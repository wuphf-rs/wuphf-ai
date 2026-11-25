//! Posting on purpose (plan 084): a story to the owner's WhatsApp contacts, a
//! channel post (plan 085). 083 keeps such chats receive-only, so a message
//! typed in their topic never goes out; a post command does.
//!
//! The owner replies `/status` (or `/post`) to a message in the topic, or
//! writes `/status <text>`. The command is the owner's only, only in a mirror
//! topic, only for a source whose connector reports it takes posts. Without
//! `!` the post waits for the owner's ✅ on a preview: the core keeps it as a
//! pending post (in memory, 10 minutes) and asks the topic's connector to
//! open the settings panel there, which opens on it. With `!` it goes at once.
//!
//! What is posted is the replied-to message (the command's text, if any, as
//! its caption) or the command's own text. That message is the post's logical
//! message: its copy at the source is the post, so deleting it later deletes
//! the post (083's `mirror_deletes_kept` aside).

use std::sync::Arc;

use wuphf_model::{
    Action, ChatRef, Conversation, ConversationId, Dispatch, Endpoint, EndpointId, IngestOutcome,
    Message, MessageId, Target,
};
use wuphf_storage::Storage;

use crate::refusal::refusal_dispatch;
use crate::{Core, CoreError, RoutingConfig};

/// How long a pending post waits for the owner's ✅.
pub const PENDING_POST_TTL_MS: i64 = 10 * 60 * 1000;

/// The command words: `/status` for stories, `/post` for channels. Both post
/// the topic's source on purpose.
const COMMANDS: [&str; 2] = ["status", "post"];

/// A post command: `/status`, `/status!`, `/post`, `/post!`, with an optional
/// `@bot` after the word and text after a space.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PostCommand {
    /// `!`: post without the preview.
    pub force: bool,
    /// The text after the command, trimmed; empty when none.
    pub rest: String,
}

/// The post command `text` is, if it is one.
pub(crate) fn post_command(text: &str) -> Option<PostCommand> {
    let text = text.trim_start();
    let body = text.strip_prefix('/')?;
    let (head, rest) = body.split_once(char::is_whitespace).unwrap_or((body, ""));
    let head = head.split_once('@').map_or(head, |(word, _)| word);
    let (word, force) = match head.strip_suffix('!') {
        Some(word) => (word, true),
        None => (head, false),
    };
    COMMANDS
        .iter()
        .any(|command| command.eq_ignore_ascii_case(word))
        .then(|| PostCommand {
            force,
            rest: rest.trim().to_owned(),
        })
}

/// A post waiting for the owner's ✅ on its preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingPost {
    /// The post's id: the command message's.
    pub id: MessageId,
    pub conversation_id: ConversationId,
    /// The message posted.
    pub message_id: MessageId,
    /// The text posted instead of the message's own (the command's text).
    pub caption: Option<String>,
    /// The topic the command was typed in.
    pub origin: EndpointId,
    /// Where the post goes: the conversation's source chat.
    pub source: ChatRef,
    /// The source's title, for the preview.
    pub title: String,
    /// Who sees it, as the source's connector reported.
    pub audience: Option<String>,
    /// When the command came in (Unix ms).
    pub created_ms: i64,
}

impl PendingPost {
    pub fn expired(&self, now_ms: i64) -> bool {
        now_ms - self.created_ms > PENDING_POST_TTL_MS
    }
}

/// What a post command, a confirmation or a composed post came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PostOutcome {
    /// Dispatched: the post, and the composed message's mirror copy.
    Posted(Vec<Dispatch>),
    /// Waiting for the owner's ✅ (the panel was asked to open on it).
    Pending(PendingPost),
    /// Not posted, for this reason.
    Refused(String),
}

impl<S> Core<S>
where
    S: Storage,
{
    /// The post command `message` (just stored, received at `origin`) is, when
    /// it is one the owner typed in a mirror topic. `None` lets ingest go on
    /// as for any message.
    pub(crate) fn owner_post_command(
        &self,
        routing: &RoutingConfig,
        origin: &Endpoint,
        message: &Message,
        author_account: Option<&str>,
    ) -> Option<PostCommand> {
        let command = post_command(message.text.as_deref()?)?;
        let author = author_account.unwrap_or(&message.author_id);
        routing
            .is_owner(&origin.connector_id, author, false)
            .then_some(command)
    }

    /// Run a post command typed at `origin`: post at once, keep it pending and
    /// open the panel on it, or refuse it with 083's note. The command itself
    /// is never routed.
    pub(crate) async fn ingest_post_command(
        &self,
        routing: &Arc<RoutingConfig>,
        origin_endpoint: EndpointId,
        command_message: &Message,
        command: PostCommand,
        actor: &str,
        now_ms: i64,
    ) -> Result<Option<IngestOutcome>, CoreError> {
        let Some(conversation) = self
            .storage
            .conversation(command_message.conversation_id)
            .await?
        else {
            return Ok(None);
        };
        let Some(source) = self.source_endpoint(&conversation).await? else {
            return Ok(None);
        };
        if source == origin_endpoint {
            // Typed in the source chat itself: an ordinary message.
            return Ok(None);
        }
        let (message_id, caption) = match (command_message.reply_to, command.rest.is_empty()) {
            (Some(parent), rest_empty) => (parent, (!rest_empty).then(|| command.rest.clone())),
            (None, false) => (command_message.id, Some(command.rest.clone())),
            (None, true) => {
                let reason =
                    "Nothing to post: reply /status to a message, or write /status <text>.";
                return self
                    .post_refused(routing, command_message, reason.to_owned())
                    .await
                    .map(Some);
            }
        };
        let profile = self.source_profile(&conversation).await?;
        let title = conversation
            .title
            .clone()
            .unwrap_or_else(|| conversation.source.address.chat_id.clone());
        if !profile
            .as_ref()
            .is_some_and(|profile| profile.accepts_posts())
        {
            let reason = format!("Not posted: {title} takes no posts.");
            return self
                .post_refused(routing, command_message, reason)
                .await
                .map(Some);
        }
        let pending = PendingPost {
            id: command_message.id,
            conversation_id: conversation.id,
            message_id,
            caption,
            origin: origin_endpoint,
            source: conversation.source.clone(),
            title,
            audience: profile.and_then(|profile| profile.posting?.audience),
            created_ms: now_ms,
        };
        let dispatches = if command.force {
            self.post(routing, &pending).await?
        } else {
            self.keep_pending(pending, now_ms);
            let origin = self.storage.endpoint(origin_endpoint).await?;
            let open = Dispatch::new(
                target_of(&origin),
                conversation.id,
                Action::OpenPanel {
                    actor: actor.to_owned(),
                },
                None,
            );
            self.save_dispatches(routing, vec![open]).await?.0
        };
        Ok(Some(IngestOutcome::Routed {
            message_id: command_message.id,
            dispatches,
            blocked: 0,
        }))
    }

    /// The pending post of the topic at `context` (a chat and maybe a thread
    /// on `connector`), the newest first, when one is still fresh.
    pub async fn pending_post_at(
        &self,
        connector: &wuphf_model::ConnectorId,
        address: &wuphf_model::ChatAddress,
        now_ms: i64,
    ) -> Result<Option<PendingPost>, CoreError> {
        let Some(endpoint) = self.storage.resolve_endpoint(connector, address).await? else {
            return Ok(None);
        };
        let posts = self.posts.lock().unwrap();
        Ok(posts
            .values()
            .filter(|post| post.origin == endpoint && !post.expired(now_ms))
            .max_by_key(|post| post.created_ms)
            .cloned())
    }

    /// The pending post `id`, while it is fresh.
    pub fn pending_post(&self, id: MessageId, now_ms: i64) -> Option<PendingPost> {
        let posts = self.posts.lock().unwrap();
        posts.get(&id).filter(|post| !post.expired(now_ms)).cloned()
    }

    /// The owner's ✅ on a pending post: post it. An expired or unknown post
    /// is refused.
    pub async fn confirm_post(&self, id: MessageId, now_ms: i64) -> Result<PostOutcome, CoreError> {
        let pending = self.posts.lock().unwrap().remove(&id);
        let Some(pending) = pending.filter(|post| !post.expired(now_ms)) else {
            return Ok(PostOutcome::Refused(
                "This post expired; send /status again.".to_owned(),
            ));
        };
        let routing = self.routing.routing();
        Ok(PostOutcome::Posted(self.post(&routing, &pending).await?))
    }

    /// The owner's ✖️ on a pending post. Whether there was one.
    pub fn cancel_post(&self, id: MessageId) -> bool {
        self.posts.lock().unwrap().remove(&id).is_some()
    }

    /// Whether the topic at `address` on `connector` mirrors a source that
    /// takes posts, and that source's title: what the panel's "Post a
    /// status" needs.
    pub async fn postable_topic(
        &self,
        connector: &wuphf_model::ConnectorId,
        address: &wuphf_model::ChatAddress,
    ) -> Result<Option<(ConversationId, String, Option<String>)>, CoreError> {
        let Some(endpoint) = self.storage.resolve_endpoint(connector, address).await? else {
            return Ok(None);
        };
        let Some(conversation) = self.storage.conversation_for_endpoint(endpoint).await? else {
            return Ok(None);
        };
        if self.source_endpoint(&conversation).await? == Some(endpoint) {
            return Ok(None);
        }
        let Some(profile) = self.source_profile(&conversation).await? else {
            return Ok(None);
        };
        if !profile.accepts_posts() {
            return Ok(None);
        }
        let title = conversation
            .title
            .clone()
            .unwrap_or_else(|| conversation.source.address.chat_id.clone());
        Ok(Some((
            conversation.id,
            title,
            profile.posting.and_then(|posting| posting.audience),
        )))
    }

    /// Post `text`, typed in the panel opened in the topic at `address`: a new
    /// owner message, mirrored into the topic and posted to the source.
    pub async fn compose_post(
        &self,
        connector: &wuphf_model::ConnectorId,
        address: &wuphf_model::ChatAddress,
        text: &str,
        now_ms: i64,
    ) -> Result<PostOutcome, CoreError> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(PostOutcome::Refused("Nothing to post.".to_owned()));
        }
        let Some((conversation_id, title, audience)) =
            self.postable_topic(connector, address).await?
        else {
            return Ok(PostOutcome::Refused(
                "This topic's chat takes no posts.".to_owned(),
            ));
        };
        let Some(origin) = self.storage.resolve_endpoint(connector, address).await? else {
            return Ok(PostOutcome::Refused("Unknown topic.".to_owned()));
        };
        let Some(conversation) = self.storage.conversation(conversation_id).await? else {
            return Ok(PostOutcome::Refused("Unknown topic.".to_owned()));
        };
        let message = Message {
            id: MessageId::new(),
            conversation_id,
            author_id: wuphf_model::OWNER_AUTHOR.to_owned(),
            participant: None,
            author_name: None,
            text: Some(text.to_owned()),
            attachments: Vec::new(),
            reply_to: None,
            timestamp_ms: now_ms,
            deleted: false,
        };
        self.storage.put_message(message.clone()).await?;
        let routing = self.routing.routing();
        // Its copy in the topic, so the story shows there like one posted by
        // command; then the post.
        let topic = self.storage.endpoint(origin).await?;
        let mirror = Dispatch::new(
            target_of(&topic),
            conversation_id,
            Action::Send {
                message: message.clone(),
                reply: None,
                disappearing: None,
            },
            None,
        );
        let mut dispatches = self.save_dispatches(&routing, vec![mirror]).await?.0;
        let pending = PendingPost {
            id: message.id,
            conversation_id,
            message_id: message.id,
            caption: None,
            origin,
            source: conversation.source.clone(),
            title,
            audience,
            created_ms: now_ms,
        };
        dispatches.extend(self.post(&routing, &pending).await?);
        Ok(PostOutcome::Posted(dispatches))
    }

    /// Dispatch `pending` to its source: the message (with its caption) as a
    /// post. 083's screen lets a post through only into a source that takes
    /// posts.
    async fn post(
        &self,
        routing: &Arc<RoutingConfig>,
        pending: &PendingPost,
    ) -> Result<Vec<Dispatch>, CoreError> {
        let Some(mut message) = self.storage.message(pending.message_id).await? else {
            return Ok(Vec::new());
        };
        if let Some(caption) = &pending.caption {
            message.text = Some(caption.clone());
        }
        let Some(conversation) = self.storage.conversation(pending.conversation_id).await? else {
            return Ok(Vec::new());
        };
        let Some(source) = self.source_endpoint(&conversation).await? else {
            return Ok(Vec::new());
        };
        let source = self.storage.endpoint(source).await?;
        let post = Dispatch::new(
            target_of(&source),
            conversation.id,
            Action::Post { message },
            None,
        );
        Ok(self.save_dispatches(routing, vec![post]).await?.0)
    }

    /// Keep `pending`, dropping the expired ones.
    fn keep_pending(&self, pending: PendingPost, now_ms: i64) {
        let mut posts = self.posts.lock().unwrap();
        posts.retain(|_, post| !post.expired(now_ms));
        posts.insert(pending.id, pending);
    }

    /// A refused post command: 083's note on the command.
    async fn post_refused(
        &self,
        routing: &Arc<RoutingConfig>,
        command_message: &Message,
        reason: String,
    ) -> Result<IngestOutcome, CoreError> {
        let note = refusal_dispatch(
            &*self.storage,
            command_message.id,
            command_message.conversation_id,
            None,
            reason,
        )
        .await?;
        let dispatches = self
            .save_dispatches(routing, note.into_iter().collect())
            .await?
            .0;
        Ok(IngestOutcome::Routed {
            message_id: command_message.id,
            dispatches,
            blocked: 0,
        })
    }

    /// The endpoint of the conversation's source chat.
    async fn source_endpoint(
        &self,
        conversation: &Conversation,
    ) -> Result<Option<EndpointId>, CoreError> {
        Ok(self
            .storage
            .resolve_endpoint(
                &conversation.source.connector_id,
                &conversation.source.address,
            )
            .await?)
    }
}

/// A dispatch target at `endpoint`.
fn target_of(endpoint: &Endpoint) -> Target {
    Target {
        endpoint_id: endpoint.id,
        connector_id: endpoint.connector_id.clone(),
        address: endpoint.address(),
        create_thread: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_commands_parse() {
        let cases = [
            ("/status", Some((false, ""))),
            ("/status!", Some((true, ""))),
            ("/post hello there", Some((false, "hello there"))),
            ("/post! hi", Some((true, "hi"))),
            ("/status@wuphf_bot", Some((false, ""))),
            ("/Status  caption ", Some((false, "caption"))),
            ("/statuses", None),
            ("status", None),
            ("/wuphf", None),
            ("hello /status", None),
        ];
        for (text, want) in cases {
            let got = post_command(text).map(|command| (command.force, command.rest));
            assert_eq!(
                got,
                want.map(|(force, rest)| (force, rest.to_owned())),
                "{text}"
            );
        }
    }
}
