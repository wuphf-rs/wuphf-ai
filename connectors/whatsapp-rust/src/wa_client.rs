//! The sender's view of WhatsApp: sending, editing and revoking messages,
//! uploading media, chat actions, the account's own number and a group's
//! facts. The live implementation wraps the running session's whatsapp-rust
//! client; the sender's tests replace it.
//!
//! The seam sits above the library's media builders: `UploadResponse` cannot
//! be built outside the library, so [`WaClient::upload`] takes the bytes and
//! the builder's options and returns the finished message. The live adapter is
//! the only untested code on the send path; keep it a thin mapping.

use std::future::Future;
use std::sync::Arc;

use whatsapp_rust::download::MediaType;
use whatsapp_rust::media::{
    AudioOptions, DocumentOptions, ImageOptions, VideoOptions, audio_message, document_message,
    image_message, video_message,
};
use whatsapp_rust::send::RevokeType;
use whatsapp_rust::types::message::AddressingMode;
use whatsapp_rust::waproto::whatsapp as wa;
use whatsapp_rust::{AppStateError, Jid, SendError, UploadOptions};
use wuphf_connector::Failure;

use crate::addressing::Account;
use crate::chat_state::MuteCall;
use crate::groups::{GroupDirectory, GroupFacts};
use crate::identity::LidStore;
use crate::session::LinkedClient;

/// What an upload becomes: the library builder that carries it, with its
/// options.
#[derive(Clone, Debug)]
pub enum OutboundMedia {
    Image(ImageOptions),
    Video(VideoOptions),
    Audio(AudioOptions),
    Document(DocumentOptions),
}

/// One chat action of a chat-state dispatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatAction {
    Pin(bool),
    Mute(MuteCall),
    Archive(bool),
}

/// The account side (own addresses, LID migration, group addressing) and the
/// LID store come with it, for the address book and the identity resolver.
pub trait WaClient: Account + LidStore + Send + Sync + 'static {
    /// Whether a session is linked: without one every send fails as
    /// transient, so the core redelivers once a new session is linked.
    fn linked(&self) -> Result<(), Failure>;

    /// Send `message` to `to`, returning its message id.
    fn send(
        &self,
        to: Jid,
        message: wa::Message,
    ) -> impl Future<Output = Result<String, Failure>> + Send;

    /// Replace the content of our message `id` in `to`, returning its id.
    fn edit(
        &self,
        to: Jid,
        id: String,
        message: wa::Message,
    ) -> impl Future<Output = Result<String, Failure>> + Send;

    /// Revoke our message `id` in `to`.
    fn revoke(&self, to: Jid, id: String) -> impl Future<Output = Result<(), Failure>> + Send;

    /// Encrypt and upload `bytes` to WhatsApp and build the message that
    /// carries them.
    fn upload(
        &self,
        bytes: Vec<u8>,
        media: OutboundMedia,
    ) -> impl Future<Output = Result<wa::Message, Failure>> + Send;

    /// Bring `chat` to one chat action's state.
    fn chat_action(
        &self,
        chat: Jid,
        action: ChatAction,
    ) -> impl Future<Output = Result<(), Failure>> + Send;

    /// A group's facts (subject, timer, …), cached.
    fn group_facts(&self, group: &Jid) -> impl Future<Output = Option<GroupFacts>> + Send;
}

/// The live client: the running session's client (a logout replaces it, and
/// without one every call fails as transient), and the shared group
/// directory.
pub struct LiveClient {
    linked: LinkedClient,
    groups: Arc<GroupDirectory>,
}

impl LiveClient {
    pub fn new(linked: LinkedClient, groups: Arc<GroupDirectory>) -> Self {
        Self { linked, groups }
    }
}

fn transient(error: SendError) -> Failure {
    Failure::Transient(anyhow::Error::new(error))
}

/// An invalid chat-action request (a malformed chat, a past mute end) can
/// never succeed on retry; anything else (encoding, keys, the send) may.
pub fn app_state_failure(error: AppStateError) -> Failure {
    match error {
        AppStateError::InvalidRequest(_) => Failure::permanent(error.to_string()),
        other => Failure::Transient(anyhow::Error::new(other)),
    }
}

impl WaClient for LiveClient {
    async fn send(&self, to: Jid, message: wa::Message) -> Result<String, Failure> {
        let result = self
            .linked
            .get()?
            .send_message(to, message)
            .await
            .map_err(transient)?;
        Ok(result.message_id)
    }

    async fn edit(&self, to: Jid, id: String, message: wa::Message) -> Result<String, Failure> {
        self.linked
            .get()?
            .edit_message(to, id, message)
            .await
            .map_err(transient)
    }

    async fn revoke(&self, to: Jid, id: String) -> Result<(), Failure> {
        self.linked
            .get()?
            .revoke_message(to, id, RevokeType::Sender)
            .await
            .map_err(transient)
            .map(|_| ())
    }

    async fn upload(&self, bytes: Vec<u8>, media: OutboundMedia) -> Result<wa::Message, Failure> {
        let client = self.linked.get()?;
        let kind = match &media {
            OutboundMedia::Image(_) => MediaType::Image,
            OutboundMedia::Video(_) => MediaType::Video,
            OutboundMedia::Audio(_) => MediaType::Audio,
            OutboundMedia::Document(_) => MediaType::Document,
        };
        let upload = client
            .upload(bytes, kind, UploadOptions::new())
            .await
            .map_err(|error| Failure::Transient(error.context("upload media to WhatsApp")))?;
        Ok(match media {
            OutboundMedia::Image(options) => image_message(upload, options),
            OutboundMedia::Video(options) => video_message(upload, options),
            OutboundMedia::Audio(options) => audio_message(upload, options),
            OutboundMedia::Document(options) => document_message(upload, options),
        })
    }

    async fn chat_action(&self, chat: Jid, action: ChatAction) -> Result<(), Failure> {
        let client = self.linked.get()?;
        let actions = client.chat_actions();
        let result = match action {
            ChatAction::Pin(true) => actions.pin_chat(&chat).await,
            ChatAction::Pin(false) => actions.unpin_chat(&chat).await,
            ChatAction::Mute(MuteCall::Forever) => actions.mute_chat(&chat).await,
            ChatAction::Mute(MuteCall::Until(end_ms)) => {
                actions.mute_chat_until(&chat, end_ms).await
            }
            ChatAction::Mute(MuteCall::Unmute) => actions.unmute_chat(&chat).await,
            ChatAction::Archive(true) => actions.archive_chat(&chat, None).await,
            ChatAction::Archive(false) => actions.unarchive_chat(&chat, None).await,
        };
        result.map_err(app_state_failure)
    }

    fn linked(&self) -> Result<(), Failure> {
        self.linked.get().map(|_| ())
    }

    async fn group_facts(&self, group: &Jid) -> Option<GroupFacts> {
        let client = self.linked.get().ok()?;
        self.groups.facts(&client, group).await
    }
}

impl LidStore for LiveClient {
    async fn lid_for_pn(&self, pn: &Jid) -> Option<Jid> {
        self.linked.get().ok()?.lid_for_pn(pn).await
    }

    async fn pn_for_lid(&self, lid: &Jid) -> Option<Jid> {
        self.linked.get().ok()?.pn_for_lid(lid).await
    }
}

impl Account for LiveClient {
    fn own_pn(&self) -> Option<Jid> {
        self.linked.get().ok()?.get_pn()
    }

    fn own_lid(&self) -> Option<Jid> {
        self.linked.get().ok()?.get_lid()
    }

    async fn lid_migrated(&self) -> bool {
        match self.linked.get() {
            Ok(client) => client.is_lid_migrated().await,
            Err(_) => false,
        }
    }

    async fn group_mode(&self, group: &Jid) -> Option<AddressingMode> {
        Account::group_mode(&*self.linked.get().ok()?, group).await
    }
}
