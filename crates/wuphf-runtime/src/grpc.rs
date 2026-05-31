use crate::connector::ConnectorHubHandle;
use std::pin::Pin;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_stream::{Stream, wrappers::ReceiverStream};
use tonic::{Request, Response, Status};
use uuid::Uuid;
use wuphf_model::{
    Attachment, AttachmentKind, ConnectorCapabilities, ConnectorId, ConnectorInput,
    ConnectorOutput, ConnectorRegistration, DispatchAcknowledgement, DispatchPayload, EventPayload,
    InboundMessage, MediaId, NormalizedEvent, Platform, PlatformAddress, ProvisionThread,
    ReplyContext, SourceAddress,
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
        let connector_id = registration.id.clone();
        let mut connection = self
            .hub
            .connect(registration)
            .await
            .map_err(Status::failed_precondition)?;
        let input = connection.input.clone();
        tokio::spawn(async move {
            while let Ok(Some(frame)) = incoming.message().await {
                let Ok(message) = input_from_proto(frame, &connector_id) else {
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
        capabilities: registration
            .capabilities
            .map(|capabilities| ConnectorCapabilities {
                supports_threads: capabilities.supports_threads,
                supports_topic_create: capabilities.supports_topic_create,
            })
            .unwrap_or_default(),
    })
}

fn input_from_proto(
    frame: pb::ClientFrame,
    connector_id: &ConnectorId,
) -> Result<ConnectorInput, WireError> {
    match frame.body {
        Some(pb::client_frame::Body::MessageCreate(event)) => {
            Ok(ConnectorInput::Event(NormalizedEvent {
                source: SourceAddress {
                    connector_id: connector_id.clone(),
                    address: PlatformAddress::new(
                        required_string(event.chat_id, "chat_id")?,
                        event.thread_id,
                    ),
                    chat_title: event.chat_title,
                },
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
                thread_id: acknowledgement.thread_id,
            }))
        }
        Some(pb::client_frame::Body::IdentityMigrated(migration)) => {
            Ok(ConnectorInput::IdentityMigrated {
                connector_id: connector_id.clone(),
                old: PlatformAddress::new(
                    required_string(migration.old_chat_id, "old_chat_id")?,
                    migration.old_thread_id,
                ),
                new: PlatformAddress::new(
                    required_string(migration.new_chat_id, "new_chat_id")?,
                    migration.new_thread_id,
                ),
            })
        }
        Some(pb::client_frame::Body::DispatchFailed(failure)) => {
            Ok(ConnectorInput::DispatchFailed {
                dispatch_id: wuphf_model::DispatchId(parse_uuid(
                    &failure.dispatch_id,
                    "dispatch_id",
                )?),
                reason: failure.reason,
            })
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
            let target = dispatch.command.target;
            let DispatchPayload::MessageCreate {
                message,
                reply_context,
            } = dispatch.command.payload;
            let reply_to_platform_message_id = reply_context
                .as_ref()
                .map(|context| context.platform_message_id.clone());
            pb::server_frame::Body::MessageCreate(Box::new(pb::MessageCreateDispatch {
                dispatch_id: dispatch.id.to_string(),
                target_chat_id: target.address.chat_id,
                target_thread_id: target.address.thread_id,
                provision_thread: target.provision_thread.map(provision_thread_to_proto),
                sender: message.sender,
                participant: message.participant,
                text: message.text,
                attachments: message
                    .attachments
                    .into_iter()
                    .map(attachment_to_proto)
                    .collect(),
                reply_to_platform_message_id,
                timestamp_ms: message.timestamp_ms,
                reply_context: reply_context.map(reply_context_to_proto),
            }))
        }
        ConnectorOutput::Rejected { reason } => {
            pb::server_frame::Body::Rejected(pb::Rejected { reason })
        }
    };
    pb::ServerFrame { body: Some(body) }
}

fn provision_thread_to_proto(provision: ProvisionThread) -> pb::ProvisionThread {
    pb::ProvisionThread {
        container: provision.container,
        title: provision.title,
    }
}

fn reply_context_to_proto(context: ReplyContext) -> pb::ReplyContext {
    pb::ReplyContext {
        logical_message_id: context.message_id.to_string(),
        platform_message_id: context.platform_message_id,
        sender: context.sender,
        participant: context.participant,
        text: context.text,
        attachments: context
            .attachments
            .into_iter()
            .map(attachment_to_proto)
            .collect(),
    }
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
        media_id: MediaId(parse_uuid(&attachment.media_id, "attachment.media_id")?),
        kind,
        file_name: attachment.file_name,
        mime_type: attachment.mime_type,
        size_bytes: attachment.size_bytes,
    })
}

pub(crate) fn attachment_to_proto(attachment: Attachment) -> pb::Attachment {
    let kind = match attachment.kind {
        AttachmentKind::Image => pb::attachment::Kind::Image,
        AttachmentKind::Video => pb::attachment::Kind::Video,
        AttachmentKind::Audio => pb::attachment::Kind::Audio,
        AttachmentKind::File => pb::attachment::Kind::File,
        AttachmentKind::Sticker => pb::attachment::Kind::Sticker,
    };
    pb::Attachment {
        media_id: attachment.media_id.to_string(),
        kind: kind.into(),
        file_name: attachment.file_name,
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
