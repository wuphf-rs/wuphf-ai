//! Inbound event handling: turn a normalized create/edit/delete into a stored
//! logical message and the dispatches that fan it out, including mirror-thread
//! provisioning and source-rename retitling.

use crate::{Core, CoreError, RoutingConfig, notes};
use std::sync::Arc;
use wuphf_model::{
    Action, Conversation, ConversationId, DedupKey, Dispatch, Endpoint, EndpointId, IgnoreReason,
    IngestOutcome, Message, MessageCopy, MessageId, SourceAddress,
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
            platform: origin.platform.clone(),
            chat_id: origin.chat_id.clone(),
            native_id: inbound.native_id.clone(),
        };
        if !self.storage.claim_event(dedup_key).await? {
            return Ok(IngestOutcome::Ignored(IgnoreReason::DuplicateEvent));
        }

        // The native id outlives the inbound form: the message drops it on
        // promotion, but the received copy is keyed by it.
        let native_id = inbound.native_id.clone();
        let message = Message::from_inbound(inbound, conversation_id, reply_to);
        self.storage.put_message(message.clone()).await?;
        self.storage
            .put_received_copy(MessageCopy {
                message_id: message.id,
                endpoint_id: origin_endpoint,
                native_id,
            })
            .await?;

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
    /// the message they wait for is acknowledged.
    pub(crate) async fn save_dispatches(
        &self,
        dispatches: Vec<Dispatch>,
    ) -> Result<(Vec<Dispatch>, usize), CoreError> {
        let mut pending = Vec::new();
        let mut blocked = 0;
        for dispatch in dispatches {
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
    /// No echo guard is needed in the source-to-text direction, as the bridge
    /// never applies a native reaction the source could report back.
    pub(crate) async fn ingest_reaction(
        &self,
        routing: &Arc<RoutingConfig>,
        origin_endpoint: EndpointId,
        native_id: String,
        reactor: String,
        reactor_name: Option<String>,
        emoji: Option<String>,
    ) -> Result<IngestOutcome, CoreError> {
        let Some((message_id, message)) = self
            .message_for_copy(
                origin_endpoint,
                &native_id,
                CoreError::ReactionTargetNotStored,
            )
            .await?
        else {
            return Ok(IngestOutcome::Ignored(IgnoreReason::Unmatched));
        };

        let routed = self
            .with_routing(routing)
            .route_reaction(&message, origin_endpoint, reactor, reactor_name, emoji)
            .await?;
        let (dispatches, blocked) = self.save_dispatches(routed).await?;
        Ok(IngestOutcome::Routed {
            message_id,
            dispatches,
            blocked,
        })
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
