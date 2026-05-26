use crate::connector::ConnectorHubHandle;
use std::pin::Pin;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_stream::{Stream, wrappers::ReceiverStream};
use tonic::{Request, Response, Status};
use uuid::Uuid;
use wuphf_model::{
    Attachment, AttachmentKind, ConnectorId, ConnectorInput, ConnectorOutput,
    ConnectorRegistration, DispatchAcknowledgement, DispatchPayload, EndpointId, EventPayload,
    GroupId, InboundMessage, NormalizedEvent, Platform,
};
use wuphf_proto::connector as pb;
use wuphf_proto::connector::connector_transport_server::ConnectorTransport;

pub struct GrpcConnectorService {
    hub: ConnectorHubHandle,
}

impl GrpcConnectorService {
    pub fn new(hub: ConnectorHubHandle) -> Self {
        Self { hub }
    }
}

#[tonic::async_trait]
impl ConnectorTransport for GrpcConnectorService {
    type OpenSessionStream =
        Pin<Box<dyn Stream<Item = Result<pb::ServerFrame, Status>> + Send + 'static>>;

    async fn open_session(
        &self,
        request: Request<tonic::Streaming<pb::ClientFrame>>,
    ) -> Result<Response<Self::OpenSessionStream>, Status> {
        let mut incoming = request.into_inner();
        let first = incoming
            .message()
            .await?
            .ok_or_else(|| Status::invalid_argument("registration frame required"))?;
        let registration = match first.body {
            Some(pb::client_frame::Body::Registration(registration)) => {
                registration_from_proto(registration)
                    .map_err(|error| Status::invalid_argument(error.to_string()))?
            }
            _ => return Err(Status::invalid_argument("first frame must be registration")),
        };
        let mut connection = self
            .hub
            .connect(registration)
            .await
            .map_err(Status::failed_precondition)?;
        let input = connection.input.clone();
        tokio::spawn(async move {
            while let Ok(Some(frame)) = incoming.message().await {
                let Ok(message) = input_from_proto(frame) else {
                    return;
                };
                if input.try_send(message).is_err() {
                    return;
                }
            }
        });
        let (outgoing_tx, outgoing_rx) = mpsc::channel(32);
        tokio::spawn(async move {
            while let Some(output) = connection.output.recv().await {
                if outgoing_tx.send(Ok(output_to_proto(output))).await.is_err() {
                    return;
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(outgoing_rx))))
    }
}

#[derive(Debug, Error)]
#[error("{0}")]
struct WireError(String);

fn registration_from_proto(
    registration: pb::Registration,
) -> Result<ConnectorRegistration, WireError> {
    Ok(ConnectorRegistration {
        id: ConnectorId(required_string(registration.connector_id, "connector_id")?),
        platform: platform_from_proto(registration.platform)?,
        endpoints: registration
            .endpoint_ids
            .into_iter()
            .map(|id| endpoint_id(&id))
            .collect::<Result<_, _>>()?,
    })
}

fn input_from_proto(frame: pb::ClientFrame) -> Result<ConnectorInput, WireError> {
    match frame.body {
        Some(pb::client_frame::Body::MessageCreate(event)) => {
            Ok(ConnectorInput::Event(NormalizedEvent {
                origin_endpoint: endpoint_id(&event.origin_endpoint_id)?,
                group_id: group_id(&event.group_id)?,
                payload: EventPayload::MessageCreate(InboundMessage {
                    platform_message_id: required_string(
                        event.platform_message_id,
                        "platform_message_id",
                    )?,
                    sender: required_string(event.sender, "sender")?,
                    participant: event.participant,
                    text: event.text,
                    attachments: event
                        .attachments
                        .into_iter()
                        .map(attachment_from_proto)
                        .collect::<Result<_, _>>()?,
                    reply_to_platform_message_id: event.reply_to_platform_message_id,
                    timestamp_ms: event.timestamp_ms,
                }),
            }))
        }
        Some(pb::client_frame::Body::Acknowledgement(acknowledgement)) => {
            Ok(ConnectorInput::Acknowledgement(DispatchAcknowledgement {
                dispatch_id: wuphf_model::DispatchId(parse_uuid(
                    &acknowledgement.dispatch_id,
                    "dispatch_id",
                )?),
                platform_message_id: required_string(
                    acknowledgement.platform_message_id,
                    "platform_message_id",
                )?,
            }))
        }
        Some(pb::client_frame::Body::Registration(_)) => {
            Err(WireError("registration already completed".into()))
        }
        None => Err(WireError("client frame body required".into())),
    }
}

fn output_to_proto(output: ConnectorOutput) -> pb::ServerFrame {
    let body = match output {
        ConnectorOutput::Dispatch(dispatch) => {
            let DispatchPayload::MessageCreate {
                message,
                reply_to_platform_message_id,
            } = dispatch.command.payload;
            pb::server_frame::Body::MessageCreate(Box::new(pb::MessageCreateDispatch {
                dispatch_id: dispatch.id.to_string(),
                target_endpoint_id: dispatch.command.target_endpoint.to_string(),
                logical_message_id: message.id.to_string(),
                group_id: message.group_id.to_string(),
                sender: message.sender,
                participant: message.participant,
                text: message.text,
                attachments: message
                    .attachments
                    .into_iter()
                    .map(attachment_to_proto)
                    .collect(),
                reply_to_logical_message_id: message.reply_to.map(|id| id.to_string()),
                reply_to_platform_message_id,
                timestamp_ms: message.timestamp_ms,
            }))
        }
        ConnectorOutput::Rejected { reason } => {
            pb::server_frame::Body::Rejected(pb::Rejected { reason })
        }
    };
    pb::ServerFrame { body: Some(body) }
}

fn attachment_from_proto(attachment: pb::Attachment) -> Result<Attachment, WireError> {
    let kind = match pb::attachment::Kind::try_from(attachment.kind)
        .map_err(|_| WireError("invalid attachment kind".into()))?
    {
        pb::attachment::Kind::Image => AttachmentKind::Image,
        pb::attachment::Kind::Video => AttachmentKind::Video,
        pb::attachment::Kind::Audio => AttachmentKind::Audio,
        pb::attachment::Kind::File => AttachmentKind::File,
        pb::attachment::Kind::Sticker => AttachmentKind::Sticker,
        pb::attachment::Kind::Unspecified => {
            return Err(WireError("attachment kind required".into()));
        }
    };
    Ok(Attachment {
        kind,
        url: required_string(attachment.url, "attachment.url")?,
        mime_type: attachment.mime_type,
        size_bytes: attachment.size_bytes,
    })
}

fn attachment_to_proto(attachment: Attachment) -> pb::Attachment {
    let kind = match attachment.kind {
        AttachmentKind::Image => pb::attachment::Kind::Image,
        AttachmentKind::Video => pb::attachment::Kind::Video,
        AttachmentKind::Audio => pb::attachment::Kind::Audio,
        AttachmentKind::File => pb::attachment::Kind::File,
        AttachmentKind::Sticker => pb::attachment::Kind::Sticker,
    };
    pb::Attachment {
        kind: kind.into(),
        url: attachment.url,
        mime_type: attachment.mime_type,
        size_bytes: attachment.size_bytes,
    }
}

fn platform_from_proto(value: i32) -> Result<Platform, WireError> {
    match pb::Platform::try_from(value).map_err(|_| WireError("invalid platform".into()))? {
        pb::Platform::Whatsapp => Ok(Platform::WhatsApp),
        pb::Platform::Telegram => Ok(Platform::Telegram),
        pb::Platform::Discord => Ok(Platform::Discord),
        pb::Platform::Signal => Ok(Platform::Signal),
        pb::Platform::Slack => Ok(Platform::Slack),
        pb::Platform::Matrix => Ok(Platform::Matrix),
        pb::Platform::Unspecified => Err(WireError("platform required".into())),
    }
}

fn endpoint_id(value: &str) -> Result<EndpointId, WireError> {
    Ok(EndpointId(parse_uuid(value, "endpoint_id")?))
}

fn group_id(value: &str) -> Result<GroupId, WireError> {
    Ok(GroupId(parse_uuid(value, "group_id")?))
}

fn parse_uuid(value: &str, name: &str) -> Result<Uuid, WireError> {
    Uuid::parse_str(value).map_err(|_| WireError(format!("{name} must be valid UUID")))
}

fn required_string(value: String, name: &str) -> Result<String, WireError> {
    if value.is_empty() {
        Err(WireError(format!("{name} required")))
    } else {
        Ok(value)
    }
}
