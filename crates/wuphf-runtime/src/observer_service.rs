//! gRPC surface over the observation log for a viewer (e.g. a TUI): list
//! chats, page a chat's history, subscribe to new activity, and — when built
//! with a [`OwnerMessageSender`] — send owner-authored messages. The service
//! reads (and can write into) every captured chat, so it is guarded by an
//! optional bearer token ([`bearer_auth`]) and should be bound locally.

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};
use tokio_stream::{Stream, wrappers::ReceiverStream};
use tonic::{Request, Response, Status};
use wuphf_model::{
    ConnectorId, ObservedChat, ObservedDirection, ObservedMessage, OwnerMessage, Platform,
};
use wuphf_proto::connector as pb;
use wuphf_proto::connector::observer_transport_server::ObserverTransport;
use wuphf_storage::Storage;

use crate::grpc::attachment_to_proto;
use crate::owner_send::{self, OwnerMessageSender};
use crate::{Observer, OwnerSendError};

/// Depth of the per-subscriber forwarding queue between the shared broadcast and
/// one client's stream. Small: a client that cannot keep up should fall behind
/// and re-query rather than have the server buffer without bound.
const SUBSCRIBE_QUEUE: usize = 64;

/// How long `SendMessage` waits for the connector's acknowledgement before
/// giving up on returning the native id. The send itself is not cancelled — it
/// stays queued and its outgoing row still arrives on the live stream.
const SEND_ACK_TIMEOUT: Duration = Duration::from_secs(10);

pub struct GrpcObserverService<S> {
    observer: Arc<Observer<S>>,
    /// Send path into the running delivery loop; absent in read-only builds,
    /// where `SendMessage` reports unimplemented.
    sender: Option<OwnerMessageSender<S>>,
}

impl<S> GrpcObserverService<S> {
    pub fn new(observer: Arc<Observer<S>>) -> Self {
        Self {
            observer,
            sender: None,
        }
    }

    /// Enable sending through this service.
    pub fn with_sender(mut self, sender: OwnerMessageSender<S>) -> Self {
        self.sender = Some(sender);
        self
    }
}

#[tonic::async_trait]
impl<S> ObserverTransport for GrpcObserverService<S>
where
    S: Storage,
{
    async fn list_chats(
        &self,
        _request: Request<pb::ListChatsRequest>,
    ) -> Result<Response<pb::ListChatsResponse>, Status> {
        let chats = self
            .observer
            .storage()
            .list_observed_chats()
            .await
            .map_err(|error| Status::internal(error.to_string()))?;
        Ok(Response::new(pb::ListChatsResponse {
            chats: chats.into_iter().map(observed_chat_to_proto).collect(),
        }))
    }

    async fn get_messages(
        &self,
        request: Request<pb::GetMessagesRequest>,
    ) -> Result<Response<pb::GetMessagesResponse>, Status> {
        let request = request.into_inner();
        if request.connector_id.is_empty() || request.chat_id.is_empty() {
            return Err(Status::invalid_argument(
                "connector_id and chat_id are required",
            ));
        }
        if request.limit == 0 {
            return Err(Status::invalid_argument("limit must be positive"));
        }
        let messages = self
            .observer
            .storage()
            .observed_messages(
                &ConnectorId(request.connector_id),
                &request.chat_id,
                request.before_ms,
                request.limit,
            )
            .await
            .map_err(|error| Status::internal(error.to_string()))?;
        Ok(Response::new(pb::GetMessagesResponse {
            messages: messages
                .into_iter()
                .map(observed_message_to_proto)
                .collect(),
        }))
    }

    type SubscribeMessagesStream =
        Pin<Box<dyn Stream<Item = Result<pb::ObservedMessage, Status>> + Send + 'static>>;

    async fn subscribe_messages(
        &self,
        request: Request<pb::SubscribeMessagesRequest>,
    ) -> Result<Response<Self::SubscribeMessagesStream>, Status> {
        let filter = request.into_inner();
        let mut feed = self.observer.subscribe();
        let (sender, receiver) = mpsc::channel(SUBSCRIBE_QUEUE);
        tokio::spawn(async move {
            loop {
                match feed.recv().await {
                    Ok(observed) => {
                        if !matches_filter(&filter, &observed) {
                            continue;
                        }
                        if sender
                            .send(Ok(observed_message_to_proto(observed)))
                            .await
                            .is_err()
                        {
                            return; // client hung up
                        }
                    }
                    // A lagging subscriber missed messages; it can re-query
                    // history, so keep streaming rather than tearing down.
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(receiver))))
    }

    async fn send_message(
        &self,
        request: Request<pb::SendMessageRequest>,
    ) -> Result<Response<pb::SendMessageResponse>, Status> {
        let Some(sender) = &self.sender else {
            return Err(Status::unimplemented(
                "this observer service is read-only; sending is not enabled",
            ));
        };
        let request = request.into_inner();
        if request.connector_id.is_empty() || request.chat_id.is_empty() {
            return Err(Status::invalid_argument(
                "connector_id and chat_id are required",
            ));
        }
        if request.text.trim().is_empty() {
            return Err(Status::invalid_argument("text must not be empty"));
        }
        let settled = sender
            .send(OwnerMessage {
                connector_id: ConnectorId(request.connector_id),
                chat_id: request.chat_id,
                thread_id: request.thread_id,
                text: request.text,
                reply_to_native_id: request.reply_to_platform_message_id,
                timestamp_ms: owner_send::now_ms(),
            })
            .await
            .map_err(|error| match error {
                OwnerSendError::Core(error) => Status::failed_precondition(error.to_string()),
                OwnerSendError::RuntimeStopped => Status::unavailable(error.to_string()),
            })?;
        // The dispatch is queued; wait (bounded) for the connector to report
        // the native id. On timeout the send still proceeds — the outgoing row
        // reaches subscribers whenever the acknowledgement lands.
        match tokio::time::timeout(SEND_ACK_TIMEOUT, settled).await {
            Ok(Ok(observed)) => Ok(Response::new(pb::SendMessageResponse {
                platform_message_id: observed.native_id,
            })),
            // The runtime dropped the tracker: the dispatch failed permanently.
            Ok(Err(_)) => Err(Status::aborted("the connector rejected the send")),
            Err(_) => Ok(Response::new(pb::SendMessageResponse {
                platform_message_id: String::new(),
            })),
        }
    }
}

/// Only deliver messages matching the subscription filter: a set `connector_id`
/// and/or `chat_id` must equal the message's; an unset field matches anything.
fn matches_filter(filter: &pb::SubscribeMessagesRequest, observed: &ObservedMessage) -> bool {
    let connector_ok = filter
        .connector_id
        .as_ref()
        .is_none_or(|id| observed.connector_id.0 == *id);
    let chat_ok = filter
        .chat_id
        .as_ref()
        .is_none_or(|id| observed.chat_id == *id);
    connector_ok && chat_ok
}

fn observed_chat_to_proto(chat: ObservedChat) -> pb::ObservedChat {
    pb::ObservedChat {
        connector_id: chat.connector_id.0,
        platform: platform_to_proto(&chat.platform).into(),
        chat_id: chat.chat_id,
        title: chat.title,
        last_message_ts: chat.last_message_ts,
        last_message_preview: chat.last_message_preview,
    }
}

fn observed_message_to_proto(observed: ObservedMessage) -> pb::ObservedMessage {
    pb::ObservedMessage {
        connector_id: observed.connector_id.0,
        platform: platform_to_proto(&observed.platform).into(),
        chat_id: observed.chat_id,
        thread_id: observed.thread_id,
        platform_message_id: observed.native_id,
        sender: observed.author_id,
        sender_name: observed.author_name,
        participant: observed.participant,
        text: observed.text,
        attachments: observed
            .attachments
            .into_iter()
            .map(attachment_to_proto)
            .collect(),
        reply_to_platform_message_id: observed.reply_to_native_id,
        direction: direction_to_proto(observed.direction).into(),
        timestamp_ms: observed.timestamp_ms,
        deleted: observed.deleted,
        edited: observed.edited,
    }
}

/// Map the model platform to its wire enum. `Other` (an unknown connector) has
/// no wire variant, so it maps to `Unspecified`; the connector id still labels
/// the chat, so no information is lost.
fn platform_to_proto(platform: &Platform) -> pb::Platform {
    match platform {
        Platform::WhatsApp => pb::Platform::Whatsapp,
        Platform::Telegram => pb::Platform::Telegram,
        Platform::Discord => pb::Platform::Discord,
        Platform::Signal => pb::Platform::Signal,
        Platform::Slack => pb::Platform::Slack,
        Platform::Matrix => pb::Platform::Matrix,
        Platform::Instagram => pb::Platform::Instagram,
        Platform::Other(_) => pb::Platform::Unspecified,
    }
}

fn direction_to_proto(direction: ObservedDirection) -> pb::ObservedDirection {
    match direction {
        ObservedDirection::Incoming => pb::ObservedDirection::Incoming,
        ObservedDirection::Outgoing => pb::ObservedDirection::Outgoing,
    }
}

/// Build a tonic interceptor that enforces a bearer token on every request. With
/// no token configured (`None`) every request is allowed, for local development;
/// with a token, requests must carry `authorization: Bearer <token>`.
// tonic's Interceptor signature fixes the `Err` type to the large `Status`, so
// the large-err lint is unavoidable here.
#[allow(clippy::result_large_err)]
pub fn bearer_auth(token: Option<String>) -> impl tonic::service::Interceptor + Clone {
    move |request: Request<()>| -> Result<Request<()>, Status> {
        let Some(expected) = token.as_deref() else {
            return Ok(request);
        };
        match crate::auth::bearer_token(request.metadata()) {
            Some(value) if value == expected => Ok(request),
            _ => Err(Status::unauthenticated("valid bearer token required")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_stream::StreamExt;
    use wuphf_model::{ChatAddress, Event, EventKind, NewMessage, SourceAddress};
    use wuphf_storage::InMemoryStorage;

    fn service() -> GrpcObserverService<InMemoryStorage> {
        GrpcObserverService::new(Arc::new(Observer::new(
            Arc::new(InMemoryStorage::default()),
        )))
    }

    fn create(chat: &str, id: &str, text: &str) -> Event {
        Event {
            source: SourceAddress {
                connector_id: ConnectorId("whatsapp".into()),
                address: ChatAddress::new(chat, None),
                chat_title: Some("Chat".into()),
            },
            payload: EventKind::NewMessage(NewMessage {
                native_id: id.into(),
                author_id: "peer".into(),
                participant: None,
                author_name: Some("Peer".into()),
                text: Some(text.into()),
                attachments: vec![],
                reply_to_native_id: None,
                timestamp_ms: 100,
            }),
        }
    }

    #[tokio::test]
    async fn lists_chats_and_pages_messages() {
        let service = service();
        service
            .observer
            .observe(&create("alice", "a1", "hi"), Platform::WhatsApp)
            .await
            .unwrap();

        let chats = service
            .list_chats(Request::new(pb::ListChatsRequest {}))
            .await
            .unwrap()
            .into_inner()
            .chats;
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].chat_id, "alice");
        assert_eq!(chats[0].platform, pb::Platform::Whatsapp as i32);

        let page = service
            .get_messages(Request::new(pb::GetMessagesRequest {
                connector_id: "whatsapp".into(),
                chat_id: "alice".into(),
                before_ms: None,
                limit: 10,
            }))
            .await
            .unwrap()
            .into_inner()
            .messages;
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].text.as_deref(), Some("hi"));
    }

    #[tokio::test]
    async fn get_messages_rejects_bad_arguments() {
        let service = service();
        let status = service
            .get_messages(Request::new(pb::GetMessagesRequest {
                connector_id: "whatsapp".into(),
                chat_id: "alice".into(),
                before_ms: None,
                limit: 0,
            }))
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn subscribe_streams_live_messages_for_the_filtered_chat() {
        let service = service();
        let stream = service
            .subscribe_messages(Request::new(pb::SubscribeMessagesRequest {
                connector_id: Some("whatsapp".into()),
                chat_id: Some("alice".into()),
            }))
            .await
            .unwrap()
            .into_inner();
        tokio::pin!(stream);

        // A message for a different chat is filtered out; the awaited one arrives.
        service
            .observer
            .observe(&create("bob", "b1", "nope"), Platform::WhatsApp)
            .await
            .unwrap();
        service
            .observer
            .observe(&create("alice", "a1", "yes"), Platform::WhatsApp)
            .await
            .unwrap();

        let first = stream.next().await.unwrap().unwrap();
        assert_eq!(first.chat_id, "alice");
        assert_eq!(first.text.as_deref(), Some("yes"));
    }

    #[tokio::test]
    async fn send_message_without_a_sender_is_unimplemented() {
        let status = service()
            .send_message(Request::new(pb::SendMessageRequest {
                connector_id: "whatsapp".into(),
                chat_id: "alice".into(),
                thread_id: None,
                text: "hi".into(),
                reply_to_platform_message_id: None,
            }))
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::Unimplemented);
    }
}
