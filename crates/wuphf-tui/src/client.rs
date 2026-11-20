//! Thin async client over the core's `ObserverTransport` service. Wraps every
//! request with the configured bearer token and exposes just the calls the TUI
//! needs. Cheap to clone (the underlying channel is shared), so the live
//! subscription runs on its own clone.

use anyhow::{Context, Result};
use tonic::Request;
use tonic::transport::Channel;
use wuphf_proto::connector as pb;
use wuphf_proto::connector::observer_transport_client::ObserverTransportClient;

/// Names the TUI on its sends (`SendMessageRequest.client`), so the core marks
/// them `sent_by = "wuphf-tui"`.
const CLIENT_NAME: &str = "wuphf-tui";

#[derive(Clone)]
pub struct ObserverClient {
    inner: ObserverTransportClient<Channel>,
    token: Option<String>,
}

impl ObserverClient {
    pub async fn connect(endpoint: String, token: Option<String>) -> Result<Self> {
        let channel = Channel::from_shared(endpoint.clone())
            .with_context(|| format!("invalid server endpoint {endpoint}"))?
            .connect()
            .await
            .with_context(|| format!("failed to connect to observer at {endpoint}"))?;
        Ok(Self {
            inner: ObserverTransportClient::new(channel),
            token,
        })
    }

    /// Wrap a message in a request carrying the bearer token, when one is set.
    fn authed<T>(&self, message: T) -> Result<Request<T>> {
        let mut request = Request::new(message);
        if let Some(token) = &self.token {
            let value = format!("Bearer {token}")
                .parse()
                .context("bearer token is not a valid header value")?;
            request.metadata_mut().insert("authorization", value);
        }
        Ok(request)
    }

    pub async fn list_chats(&mut self) -> Result<Vec<pb::ObservedChat>> {
        let request = self.authed(pb::ListChatsRequest {})?;
        Ok(self.inner.list_chats(request).await?.into_inner().chats)
    }

    pub async fn get_messages(
        &mut self,
        connector_id: String,
        chat_id: String,
        before_ms: Option<i64>,
        limit: u32,
    ) -> Result<Vec<pb::ObservedMessage>> {
        let request = self.authed(pb::GetMessagesRequest {
            connector_id,
            chat_id,
            before_ms,
            limit,
        })?;
        Ok(self
            .inner
            .get_messages(request)
            .await?
            .into_inner()
            .messages)
    }

    /// Send an owner-authored text into a chat. Returns the native message id
    /// from the connector's acknowledgement, or an empty string when the server
    /// timed out waiting for it (the send still proceeds and the message
    /// arrives on the live stream once acknowledged).
    pub async fn send_message(
        &mut self,
        connector_id: String,
        chat_id: String,
        thread_id: Option<String>,
        text: String,
        reply_to_platform_message_id: Option<String>,
    ) -> Result<String> {
        let request = self.authed(pb::SendMessageRequest {
            connector_id,
            chat_id,
            thread_id,
            text,
            reply_to_platform_message_id,
            media_ids: Vec::new(),
            client: Some(CLIENT_NAME.to_owned()),
        })?;
        Ok(self
            .inner
            .send_message(request)
            .await?
            .into_inner()
            .platform_message_id)
    }

    pub async fn subscribe(&mut self) -> Result<tonic::Streaming<pb::ObservedMessage>> {
        let request = self.authed(pb::SubscribeMessagesRequest {
            connector_id: None,
            chat_id: None,
        })?;
        Ok(self.inner.subscribe_messages(request).await?.into_inner())
    }
}
