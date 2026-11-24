//! Inbound event handling: turn a normalized create/edit/delete into a stored
//! logical message and the dispatches that fan it out, including mirror-thread
//! provisioning and source-rename retitling.

use crate::reply_reaction::lone_emoji;
use crate::{Core, CoreError, RoutingConfig, notes};
use std::collections::HashMap;
use std::sync::Arc;
use wuphf_model::{
    Action, Conversation, ConversationId, DedupKey, Dispatch, Endpoint, EndpointId, IgnoreReason,
    IngestOutcome, Message, MessageCopy, MessageId, ReactorReaction, SourceAddress,
};

/// What [`Core::introduce`] decided for a routed message.
enum Introduction {
    /// The info card dispatches replace the routed ones; the message is
    /// stashed until the thread exists.
    Card(Vec<Dispatch>),
    /// No card is due: the routed dispatches go out as they are.
    NotNeeded(Vec<Dispatch>),
}
use wuphf_storage::Storage;

impl<S> Core<S>
where
    S: Storage,
{
    pub(crate) async fn ingest_create(
        &self,
        routing: &Arc<RoutingConfig>,
        source: SourceAddress,
        origin_endpoint: EndpointId,
        origin: Endpoint,
        conversation_id: ConversationId,
        inbound: wuphf_model::NewMessage,
    ) -> Result<IngestOutcome, CoreError> {
        if self
            .storage
            .is_sent_copy(origin_endpoint, &inbound.native_id)
            .await?
        {
            return Ok(IngestOutcome::Ignored(IgnoreReason::SentMessageEcho));
        }

        // Resolve the reply parent best-effort: a reply to a message the bridge
        // never saw (sent before startup, or never mirrored) simply loses its
        // reply linkage rather than failing the whole message.
        let reply_to = match inbound.reply_to_native_id.as_deref() {
            Some(native_id) => {
                self.storage
                    .message_id_for_copy(origin_endpoint, native_id)
                    .await?
            }
            None => None,
        };

        for attachment in &inbound.attachments {
            let media = self
                .storage
                .media(attachment.media_id)
                .await?
                .ok_or(CoreError::UnknownMedia(attachment.media_id))?;
            if media.attachment != *attachment {
                return Err(CoreError::MediaMetadataMismatch(attachment.media_id));
            }
        }

        let dedup_key = DedupKey {
            connector_id: origin.connector_id.clone(),
            chat_id: origin.chat_id.clone(),
            native_id: inbound.native_id.clone(),
        };
        if !self.storage.claim_event(dedup_key).await? {
            return Ok(IngestOutcome::Ignored(IgnoreReason::DuplicateEvent));
        }

        // The native id outlives the inbound form: the message drops it on
        // promotion, but the received copy is keyed by it.
        let native_id = inbound.native_id.clone();
        let author_account = inbound.author_account.clone();
        let message = Message::from_inbound(inbound, conversation_id, reply_to);
        self.storage.put_message(message.clone()).await?;
        self.storage
            .put_received_copy(MessageCopy {
                message_id: message.id,
                endpoint_id: origin_endpoint,
                native_id: native_id.clone(),
            })
            .await?;
        if let (Some(parent), Some(emoji)) = (
            message.reply_to,
            self.reply_reaction_emoji(routing, &origin, &message, author_account.as_deref()),
        ) {
            return self
                .ingest_reply_reaction(routing, &origin, parent, &message, &native_id, Some(emoji))
                .await;
        }

        let provision_title = source
            .chat_title
            .clone()
            .unwrap_or_else(|| source.address.chat_id.clone());

        // Hold the message if this conversation's mirror thread is still being
        // provisioned; it is stashed durably and replayed once the thread exists.
        // This prevents a burst of messages from each provisioning a duplicate
        // thread, and — unlike an in-memory stash — survives a crash.
        let provisioning = {
            let provisioning = self.provisioning.lock().unwrap();
            provisioning
                .get(&conversation_id)
                .is_some_and(|state| !state.pending.is_empty())
        };
        if provisioning {
            let replayed = self
                .stash_deferred(
                    routing,
                    conversation_id,
                    message.id,
                    origin_endpoint,
                    &provision_title,
                )
                .await?;
            // The provision is normally still pending, so the message waits
            // (Deferred). If it settled while we stashed, stash_deferred replayed
            // the stash instead, and those dispatches carry this message out.
            if replayed.is_empty() {
                return Ok(IngestOutcome::Deferred {
                    message_id: message.id,
                });
            }
            return Ok(IngestOutcome::Routed {
                message_id: message.id,
                dispatches: replayed,
                blocked: 0,
            });
        }

        let (mut dispatches, blocked) = self
            .route_and_record(routing, &message, origin_endpoint, &provision_title)
            .await?;
        dispatches.extend(
            self.maybe_retitle(
                routing,
                &origin,
                conversation_id,
                source.chat_title.as_deref(),
            )
            .await?,
        );
        Ok(IngestOutcome::Routed {
            message_id: message.id,
            dispatches,
            blocked,
        })
    }

    /// Detect a source chat title change and fan it out to the mirror threads.
    /// The conversation title is tracked only from its source side; a message
    /// from any other endpoint (e.g. a reply typed into a mirror topic) carries
    /// its own container's title and must not retitle the conversation.
    ///
    /// A newly recorded title only dispatches when a mirror thread already
    /// exists: on the message that first provisions a thread the mapping is not
    /// yet acknowledged, so `route_retitle` naturally yields nothing and the
    /// thread simply takes the provisioned title. When a thread already exists —
    /// including one provisioned before any title was known (named with the raw
    /// chat id) — the first real title is a genuine change and is fanned out.
    async fn maybe_retitle(
        &self,
        routing: &Arc<RoutingConfig>,
        origin: &Endpoint,
        conversation_id: ConversationId,
        new_title: Option<&str>,
    ) -> Result<Vec<Dispatch>, CoreError> {
        let Some(new_title) = new_title.map(str::trim).filter(|title| !title.is_empty()) else {
            return Ok(Vec::new());
        };
        let Some(conversation) = self.storage.conversation(conversation_id).await? else {
            return Ok(Vec::new());
        };
        if conversation.source.connector_id != origin.connector_id
            || conversation.source.address.chat_id != origin.chat_id
        {
            return Ok(Vec::new());
        }
        self.retitle_conversation(routing, conversation, new_title)
            .await
    }

    /// Record a new source title on the conversation and rename its
    /// provisioned mirror threads. The caller has checked that the title comes
    /// from the conversation's source chat. A blank or unchanged title is a
    /// no-op.
    pub(crate) async fn retitle_conversation(
        &self,
        routing: &Arc<RoutingConfig>,
        mut conversation: Conversation,
        new_title: &str,
    ) -> Result<Vec<Dispatch>, CoreError> {
        let new_title = new_title.trim();
        if new_title.is_empty() || conversation.title.as_deref() == Some(new_title) {
            return Ok(Vec::new());
        }
        self.storage
            .set_conversation_title(conversation.id, new_title)
            .await?;
        conversation.title = Some(new_title.to_owned());
        let dispatches = self
            .with_routing(routing)
            .route_retitle(&conversation, new_title)
            .await?;
        let (pending, _) = self.save_dispatches(dispatches).await?;
        Ok(pending)
    }

    /// Resolve the copy an inbound edit/delete/reaction/receipt targets to its
    /// logical message. `Ok(None)` means the copy is unknown (the caller drops
    /// the event as `Unmatched`); a copy that maps to a message that is somehow
    /// no longer stored is the per-operation `missing` error.
    async fn message_for_copy(
        &self,
        endpoint: EndpointId,
        native_id: &str,
        missing: fn(MessageId) -> CoreError,
    ) -> Result<Option<(MessageId, Message)>, CoreError> {
        let Some(message_id) = self
            .storage
            .message_id_for_copy(endpoint, native_id)
            .await?
        else {
            return Ok(None);
        };
        let message = self
            .storage
            .message(message_id)
            .await?
            .ok_or_else(|| missing(message_id))?;
        Ok(Some((message_id, message)))
    }

    /// Persist routed dispatches and return the ones ready to send, with a count
    /// of the blocked ones. Blocked dispatches are saved and surface later, when
    /// the message they wait for is acknowledged; dispatches into a paused
    /// direction are held (plan 079) and counted as blocked.
    pub(crate) async fn save_dispatches(
        &self,
        dispatches: Vec<Dispatch>,
    ) -> Result<(Vec<Dispatch>, usize), CoreError> {
        let pauses = self.health_pauses();
        let mut conversations = std::collections::HashMap::new();
        let mut pending = Vec::new();
        let mut blocked = 0;
        for dispatch in dispatches {
            if dispatch.is_pending()
                && let Some(pause) = self
                    .pause_for(&pauses, &dispatch, &mut conversations)
                    .await?
            {
                self.hold(&dispatch, &pause).await?;
                blocked += 1;
                continue;
            }
            self.storage.enqueue_dispatch(&dispatch).await?;
            if dispatch.is_pending() {
                pending.push(dispatch);
            } else {
                blocked += 1;
            }
        }
        Ok((pending, blocked))
    }

    /// Route a message, enqueue its dispatches, and register any thread
    /// provisioning it triggers so that concurrent messages to the same
    /// conversation are stashed (rather than provisioning again) until the
    /// thread exists. Returns the immediately-sendable dispatches and a
    /// count of the blocked ones.
    ///
    /// The first provisioning of a conversation's mirror thread is done by an
    /// info card instead of the message: see [`Self::introduce`].
    pub(crate) async fn route_and_record(
        &self,
        routing: &Arc<RoutingConfig>,
        message: &Message,
        origin_endpoint: EndpointId,
        provision_title: &str,
    ) -> Result<(Vec<Dispatch>, usize), CoreError> {
        let routed = self
            .with_routing(routing)
            .route(message, origin_endpoint, provision_title)
            .await?;
        let routed = match self
            .introduce(message, origin_endpoint, provision_title, routed)
            .await?
        {
            Introduction::Card(cards) => cards,
            Introduction::NotNeeded(routed) => routed,
        };
        let (dispatches, blocked) = self.save_dispatches(routed).await?;
        // Only ready dispatches register: a blocked one provisions nothing until
        // it is released.
        let provisioning_ids: Vec<_> = dispatches
            .iter()
            .filter(|dispatch| dispatch.to.create_thread.is_some())
            .map(|dispatch| dispatch.id)
            .collect();
        if !provisioning_ids.is_empty() {
            let mut provisioning = self.provisioning.lock().unwrap();
            provisioning
                .entry(message.conversation_id)
                .or_default()
                .pending
                .extend(provisioning_ids);
        }
        Ok((dispatches, blocked))
    }

    /// When routing would provision a mirror thread for a conversation that
    /// has never had an info card, post the card as the provisioning dispatch
    /// instead and hold the message in the durable provisioning stash, which
    /// replays it once the card's acknowledgement establishes the thread.
    ///
    /// The conversation is marked introduced before anything else, so a crash
    /// cannot post two cards, and a card that fails is never retried: the
    /// stash replays, and the message provisions the thread itself.
    async fn introduce(
        &self,
        message: &Message,
        origin_endpoint: EndpointId,
        provision_title: &str,
        routed: Vec<Dispatch>,
    ) -> Result<Introduction, CoreError> {
        if !routed
            .iter()
            .any(|dispatch| dispatch.to.create_thread.is_some())
        {
            return Ok(Introduction::NotNeeded(routed));
        }
        let Some(conversation) = self.storage.conversation(message.conversation_id).await? else {
            return Ok(Introduction::NotNeeded(routed));
        };
        if conversation.introduced {
            return Ok(Introduction::NotNeeded(routed));
        }
        self.storage
            .mark_conversation_introduced(conversation.id)
            .await?;

        let profile = self.source_profile(&conversation).await?;
        let avatar = match profile
            .as_ref()
            .and_then(|profile| profile.avatar.as_ref()?.media_id)
        {
            Some(media_id) => self
                .storage
                .media(media_id)
                .await?
                .map(|blob| blob.attachment),
            None => None,
        };
        let title = conversation.title.as_deref().unwrap_or(provision_title);
        let card = notes::info_card(title, profile.as_ref(), avatar);

        // Discard the routed dispatches (none was saved): the stashed message
        // is routed again once the thread exists.
        let cards = routed
            .into_iter()
            .filter(|dispatch| dispatch.to.create_thread.is_some())
            .map(|dispatch| {
                Dispatch::new(
                    dispatch.to,
                    conversation.id,
                    Action::Note {
                        text: card.text.clone(),
                        attachments: card.attachments.clone(),
                    },
                    None,
                )
            })
            .collect();
        self.storage
            .stash_provisioning_message(
                conversation.id,
                message.id,
                origin_endpoint,
                provision_title,
            )
            .await?;
        Ok(Introduction::Card(cards))
    }

    /// Apply an inbound edit: revise the canonical message text and fan the
    /// change out to the other endpoints. An edit whose text matches the stored
    /// text is dropped, which both ignores redundant reports and breaks the echo
    /// loop when our own mirrored edit is reported back by the target platform.
    pub(crate) async fn ingest_edit(
        &self,
        routing: &Arc<RoutingConfig>,
        origin_endpoint: EndpointId,
        native_id: String,
        text: Option<String>,
    ) -> Result<IngestOutcome, CoreError> {
        let Some((message_id, mut message)) = self
            .message_for_copy(origin_endpoint, &native_id, CoreError::EditTargetNotStored)
            .await?
        else {
            return Ok(IngestOutcome::Ignored(IgnoreReason::Unmatched));
        };
        if message.text == text {
            return Ok(IngestOutcome::Ignored(IgnoreReason::RedundantEdit));
        }
        message.text = text;
        self.storage.put_message(message.clone()).await?;
        if let Some(parent) = self
            .reply_reaction_parent(origin_endpoint, &message, &native_id)
            .await?
        {
            let origin = self.storage.endpoint(origin_endpoint).await?;
            let emoji = message.text.as_deref().and_then(lone_emoji);
            return self
                .ingest_reply_reaction(routing, &origin, parent, &message, &native_id, emoji)
                .await;
        }

        let routed = self
            .with_routing(routing)
            .route_edit(&message, origin_endpoint)
            .await?;
        let (dispatches, blocked) = self.save_dispatches(routed).await?;
        Ok(IngestOutcome::Routed {
            message_id,
            dispatches,
            blocked,
        })
    }

    /// Apply an inbound delete: mark the canonical message revoked and fan the
    /// deletion out to the other endpoints. A delete of an already-revoked
    /// message is dropped, which both ignores redundant reports and breaks the
    /// echo loop when our own mirrored delete is reported back by the target.
    pub(crate) async fn ingest_delete(
        &self,
        routing: &Arc<RoutingConfig>,
        origin_endpoint: EndpointId,
        native_id: String,
    ) -> Result<IngestOutcome, CoreError> {
        let Some((message_id, mut message)) = self
            .message_for_copy(
                origin_endpoint,
                &native_id,
                CoreError::DeleteTargetNotStored,
            )
            .await?
        else {
            return Ok(IngestOutcome::Ignored(IgnoreReason::Unmatched));
        };
        if message.deleted {
            return Ok(IngestOutcome::Ignored(IgnoreReason::RedundantDelete));
        }
        message.deleted = true;
        self.storage.put_message(message.clone()).await?;
        if let Some(parent) = self
            .reply_reaction_parent(origin_endpoint, &message, &native_id)
            .await?
        {
            let origin = self.storage.endpoint(origin_endpoint).await?;
            return self
                .ingest_reply_reaction(routing, &origin, parent, &message, &native_id, None)
                .await;
        }

        let routed = self
            .with_routing(routing)
            .route_delete(&message, origin_endpoint)
            .await?;
        let (dispatches, blocked) = self.save_dispatches(routed).await?;
        Ok(IngestOutcome::Routed {
            message_id,
            dispatches,
            blocked,
        })
    }

    /// Apply an inbound reaction: resolve the reacted copy to its logical
    /// message and fan the reaction out to the other endpoints, blocking targets
    /// whose copy is not delivered yet. The reaction is a toggle (`emoji` None
    /// clears the reactor's reaction); the connector decides how to render it.
    ///
    /// Each reactor's last reaction on each copy is stored, so a reaction
    /// equal to the stored one is ignored: a replay, or a platform echoing a
    /// reaction the bridge applied as the owner.
    pub(crate) async fn ingest_reaction(
        &self,
        routing: &Arc<RoutingConfig>,
        origin: &Endpoint,
        native_id: String,
        change: ReactionChange,
    ) -> Result<IngestOutcome, CoreError> {
        let Some((message_id, message)) = self
            .message_for_copy(origin.id, &native_id, CoreError::ReactionTargetNotStored)
            .await?
        else {
            return Ok(IngestOutcome::Ignored(IgnoreReason::Unmatched));
        };
        let previous = self
            .stored_reaction(message_id, origin.id, &change.key())
            .await?;
        if previous == change.emoji {
            return Ok(IngestOutcome::Ignored(IgnoreReason::RedundantReaction));
        }
        let routed = self
            .reaction_changed(routing, origin, &message, change)
            .await?;
        let (dispatches, blocked) = self.save_dispatches(routed).await?;
        Ok(IngestOutcome::Routed {
            message_id,
            dispatches,
            blocked,
        })
    }

    /// Apply the complete set of other accounts' reactions on a message: each
    /// reactor whose reaction differs from the stored one, or who is no
    /// longer in the set, is a toggle. The connector's own account (`""`)
    /// is not in a set, so its stored reaction is left alone.
    pub(crate) async fn ingest_reaction_set(
        &self,
        routing: &Arc<RoutingConfig>,
        origin: &Endpoint,
        native_id: String,
        reactions: Vec<ReactorReaction>,
    ) -> Result<IngestOutcome, CoreError> {
        let Some((message_id, message)) = self
            .message_for_copy(origin.id, &native_id, CoreError::ReactionTargetNotStored)
            .await?
        else {
            return Ok(IngestOutcome::Ignored(IgnoreReason::Unmatched));
        };
        let stored: HashMap<String, String> = self
            .storage
            .reactions_at(message_id, origin.id)
            .await?
            .into_iter()
            .filter(|reaction| {
                !reaction.reactor.is_empty() && !reaction.reactor.starts_with(REPLY_KEY_PREFIX)
            })
            .map(|reaction| (reaction.reactor, reaction.emoji))
            .collect();
        let mut changes = Vec::new();
        for reaction in &reactions {
            if reaction.reactor.is_empty() {
                continue;
            }
            if stored.get(&reaction.reactor) != Some(&reaction.emoji) {
                changes.push(ReactionChange::reported(
                    reaction.reactor.clone(),
                    reaction.reactor_name.clone(),
                    Some(reaction.emoji.clone()),
                    false,
                ));
            }
        }
        let mut cleared: Vec<&String> = stored
            .keys()
            .filter(|reactor| {
                !reactions
                    .iter()
                    .any(|reaction| reaction.reactor == **reactor)
            })
            .collect();
        cleared.sort();
        changes.extend(
            cleared
                .into_iter()
                .map(|reactor| ReactionChange::reported(reactor.clone(), None, None, false)),
        );
        if changes.is_empty() {
            return Ok(IngestOutcome::Ignored(IgnoreReason::RedundantReaction));
        }

        let mut routed = Vec::new();
        for change in changes {
            routed.extend(
                self.reaction_changed(routing, origin, &message, change)
                    .await?,
            );
        }
        let (dispatches, blocked) = self.save_dispatches(routed).await?;
        Ok(IngestOutcome::Routed {
            message_id,
            dispatches,
            blocked,
        })
    }

    /// Store one reactor's new reaction at the origin copy and route it. An
    /// owner's reaction is also stored at each target as the session
    /// account's (`""`): what a platform reacting as the owner will show, so
    /// its echo is recognised as redundant.
    ///
    /// An owner's clear removes only what it set: a target now showing
    /// another owner reaction (a later one, from a native reaction or a
    /// reply) keeps it, and gets no dispatch (plan 081).
    pub(crate) async fn reaction_changed(
        &self,
        routing: &Arc<RoutingConfig>,
        origin: &Endpoint,
        message: &Message,
        change: ReactionChange,
    ) -> Result<Vec<Dispatch>, CoreError> {
        let key = change.key();
        let previous = self.stored_reaction(message.id, origin.id, &key).await?;
        let emoji = change.emoji.clone();
        self.storage
            .set_reaction(message.id, origin.id, &key, emoji.as_deref())
            .await?;
        let by_owner = change.by_owner(routing, origin);
        let routed = self
            .with_routing(routing)
            .route_reaction(
                message,
                origin.id,
                change.reactor,
                change.reactor_name,
                emoji.clone(),
                by_owner,
            )
            .await?;
        if !by_owner {
            return Ok(routed);
        }
        let mut kept = Vec::with_capacity(routed.len());
        for dispatch in routed {
            let target = dispatch.to.endpoint_id;
            if emoji.is_none() && self.stored_reaction(message.id, target, "").await? != previous {
                continue;
            }
            self.storage
                .set_reaction(message.id, target, "", emoji.as_deref())
                .await?;
            kept.push(dispatch);
        }
        Ok(kept)
    }

    /// `reactor`'s stored reaction on `message`'s copy at `endpoint`.
    pub(crate) async fn stored_reaction(
        &self,
        message: MessageId,
        endpoint: EndpointId,
        reactor: &str,
    ) -> Result<Option<String>, CoreError> {
        Ok(self
            .storage
            .reactions_at(message, endpoint)
            .await?
            .into_iter()
            .find(|reaction| reaction.reactor == reactor)
            .map(|reaction| reaction.emoji))
    }

    /// Surface a delivery/read receipt on the message's other copies. The
    /// receipt is observed at the endpoint where the peer acknowledged it; the
    /// signal fans to every other endpoint that holds a copy (a 👍 for
    /// delivered, 👀 for read), so the side the owner reads the mirror on shows
    /// the message landed/was seen. Because it keys on the logical message and
    /// not on which side authored it, this works both for messages the bridge
    /// sent and for messages the owner sent from another of the source's devices
    /// — the latter arrive mirrored to the other endpoints, where the receipt is
    /// now surfaced. The owner's own (from_me) receipts are ignored. Targets
    /// whose copy is not delivered yet are blocked until it is. The connector
    /// decides how to render the signal.
    pub(crate) async fn ingest_receipt(
        &self,
        routing: &Arc<RoutingConfig>,
        observer_endpoint: EndpointId,
        native_id: String,
        from_me: bool,
        kind: wuphf_model::ReceiptKind,
    ) -> Result<IngestOutcome, CoreError> {
        if from_me {
            return Ok(IngestOutcome::Ignored(IgnoreReason::Unmatched));
        }
        let Some((message_id, message)) = self
            .message_for_copy(
                observer_endpoint,
                &native_id,
                CoreError::ReceiptTargetNotStored,
            )
            .await?
        else {
            return Ok(IngestOutcome::Ignored(IgnoreReason::Unmatched));
        };

        let routed = self
            .with_routing(routing)
            .route_receipt(&message, observer_endpoint, kind)
            .await?;
        let (dispatches, blocked) = self.save_dispatches(routed).await?;
        Ok(IngestOutcome::Routed {
            message_id,
            dispatches,
            blocked,
        })
    }
}

/// One reactor's reaction change: as a connector reported it, or from the
/// owner's lone-emoji reply (plan 081).
pub(crate) struct ReactionChange {
    pub(crate) reactor: String,
    pub(crate) reactor_name: Option<String>,
    /// `None` clears the reactor's reaction.
    pub(crate) emoji: Option<String>,
    /// The connector's own session account.
    pub(crate) from_me: bool,
    /// The native id of the owner's reply this reaction is, when it is one.
    pub(crate) via_reply: Option<String>,
}

/// The reaction store's key prefix for a reaction made by replying with a
/// lone emoji, followed by the reply's native id. A reaction set never
/// reports these, so a diff leaves them alone.
pub(crate) const REPLY_KEY_PREFIX: &str = "reply:";

impl ReactionChange {
    /// A reaction the platform reported.
    pub(crate) fn reported(
        reactor: String,
        reactor_name: Option<String>,
        emoji: Option<String>,
        from_me: bool,
    ) -> Self {
        Self {
            reactor,
            reactor_name,
            emoji,
            from_me,
            via_reply: None,
        }
    }

    /// The reactor's key in the reaction store: `""` for the session account,
    /// `reply:<native id>` for a reply.
    pub(crate) fn key(&self) -> String {
        match &self.via_reply {
            Some(reply) => format!("{REPLY_KEY_PREFIX}{reply}"),
            None if self.from_me => String::new(),
            None => self.reactor.clone(),
        }
    }

    /// Whether the reactor is the owner. A reply reaction is stored only for
    /// the owner's replies, so it always is.
    fn by_owner(&self, routing: &RoutingConfig, origin: &Endpoint) -> bool {
        self.via_reply.is_some()
            || routing.is_owner(&origin.connector_id, &self.reactor, self.from_me)
    }
}
