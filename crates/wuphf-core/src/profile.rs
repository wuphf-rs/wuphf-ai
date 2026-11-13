//! Chat profiles: what the core knows about each platform chat apart from
//! routing. Connectors report what they observe; the core merges reports,
//! hands the result to sends, and turns real changes into notes in the
//! chat's mirror threads.
//!
//! A profile is keyed by the connector and the chat's native id, never by
//! endpoint, and is recorded before routing: a chat no rule matches still
//! learns its timer, so a later send into it (an owner reply, a new rule) can
//! carry it.
//!
//! The first time a value is seen it is stored silently as the baseline; only
//! a later change of a known value is a change. A snapshot report (a history
//! sync, a first sighting) is stored without notes even when it differs.

use crate::config::RoutingConfig;
use crate::notes::{Note, change_notes};
use crate::resolve::address_forms;
use crate::{Core, CoreError};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::sync::Arc;
use wuphf_model::{
    Attachment, ChatAddress, ChatAvatar, ChatProfile, ChatUpdate, ConnectorId, Conversation,
    ConversationId, DisappearingTimer, Dispatch, EndpointId, ParticipantChange,
    ParticipantChangeKind, SourceAddress, Target,
};
use wuphf_storage::{ChatProfileStore, Storage, StorageError, TopologyStore};

/// Merge a reported timer into the stored one, returning the new value when it
/// changes. With a setting timestamp on both sides the newer wins (an equal
/// one counts as newer); otherwise the later report wins. A report that
/// repeats the stored seconds without a timestamp keeps the stored timestamp.
pub(crate) fn merge_disappearing(
    stored: Option<DisappearingTimer>,
    incoming: DisappearingTimer,
) -> Option<DisappearingTimer> {
    let next = match stored {
        None => incoming,
        Some(current) => match (current.setting_timestamp, incoming.setting_timestamp) {
            (Some(known), Some(reported)) if reported < known => return None,
            _ if incoming.seconds == current.seconds => DisappearingTimer {
                seconds: current.seconds,
                setting_timestamp: incoming.setting_timestamp.or(current.setting_timestamp),
            },
            _ => incoming,
        },
    };
    (stored != Some(next)).then_some(next)
}

/// What one report changed. The value fields hold only values that were
/// **known and differed** (a first-seen value is a silent baseline); the
/// membership changes are events and are always carried. `stored` says
/// whether the profile itself changed and must be written.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProfileChanges {
    pub title: Option<String>,
    pub description: Option<String>,
    pub avatar: Option<ChatAvatar>,
    pub disappearing: Option<DisappearingTimer>,
    pub participants: Vec<ParticipantChange>,
    pub(crate) stored: bool,
}

impl ProfileChanges {
    /// Whether the profile changed (a baseline counts) and must be written.
    pub fn changed(&self) -> bool {
        self.stored
    }
}

/// Apply one report to a profile. `avatar_hash` is the core's SHA-256 of the
/// reported picture's bytes; a reported picture without one is ignored.
pub(crate) fn apply(
    profile: &mut ChatProfile,
    update: &ChatUpdate,
    avatar_hash: Option<String>,
) -> ProfileChanges {
    let mut changes = ProfileChanges::default();

    if let Some(reported) = update.disappearing
        && let Some(next) = merge_disappearing(profile.disappearing, reported)
    {
        if profile.disappearing.is_some() {
            changes.disappearing = Some(next);
        }
        profile.disappearing = Some(next);
        changes.stored = true;
    }

    if let Some(title) = &update.title
        && profile.title.as_ref() != Some(title)
    {
        if profile.title.is_some() {
            changes.title = Some(title.clone());
        }
        profile.title = Some(title.clone());
        changes.stored = true;
    }

    if let Some(description) = &update.description
        && profile.description.as_ref() != Some(description)
    {
        if profile.description.is_some() {
            changes.description = Some(description.clone());
        }
        profile.description = Some(description.clone());
        changes.stored = true;
    }

    if let Some(reported) = &update.avatar {
        let next = if reported.id.is_empty() {
            // Removed: nothing to hash.
            Some(ChatAvatar {
                id: String::new(),
                media_id: None,
                hash: None,
            })
        } else {
            // A picture is compared by its bytes, never by the platform's id.
            avatar_hash.map(|hash| ChatAvatar {
                id: reported.id.clone(),
                media_id: reported.media_id,
                hash: Some(hash),
            })
        };
        if let Some(next) = next {
            let same = match &profile.avatar {
                None => false,
                Some(stored) if next.id.is_empty() => stored.id.is_empty(),
                Some(stored) => stored.hash.is_some() && stored.hash == next.hash,
            };
            if !same {
                if profile.avatar.is_some() {
                    changes.avatar = Some(next.clone());
                }
                profile.avatar = Some(next);
                changes.stored = true;
            }
        }
    }

    if let Some(count) = update.participant_count {
        if profile.participant_count != Some(count) {
            profile.participant_count = Some(count);
            changes.stored = true;
        }
    } else if let Some(count) = profile.participant_count {
        let adjusted = update
            .participant_changes
            .iter()
            .fold(count, |count, change| match change.kind {
                ParticipantChangeKind::Joined => count.saturating_add(1),
                ParticipantChangeKind::Left => count.saturating_sub(1),
                ParticipantChangeKind::Promoted | ParticipantChangeKind::Demoted => count,
            });
        if adjusted != count {
            profile.participant_count = Some(adjusted);
            changes.stored = true;
        }
    }

    changes.participants = update.participant_changes.clone();
    changes
}

/// The core's hash of a picture: SHA-256, lowercase hex.
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl<S> Core<S>
where
    S: Storage,
{
    /// Record chat properties a connector reported and return the dispatches
    /// they cause: thread renames for a new title, and (unless `snapshot`)
    /// one note per real change in every other endpoint of each conversation
    /// the chat belongs to. Runs on the connector's serial ingest worker, so a
    /// read-merge-write cannot race another update for the same connector.
    pub async fn update_chats(
        &self,
        connector_id: &ConnectorId,
        updates: Vec<ChatUpdate>,
        snapshot: bool,
    ) -> Result<Vec<Dispatch>, CoreError> {
        let routing = self.routing.routing();
        let mut dispatches = Vec::new();
        // A connector can report one chat under several address forms in one
        // batch (a WhatsApp DM as its LID and its phone number). Each form has
        // its own profile, so each can see the same change: post a note once
        // per conversation and batch.
        let mut posted = HashSet::new();
        for update in &updates {
            dispatches.extend(
                self.update_chat(&routing, connector_id, update, snapshot, &mut posted)
                    .await?,
            );
        }
        Ok(dispatches)
    }

    async fn update_chat(
        &self,
        routing: &Arc<RoutingConfig>,
        connector_id: &ConnectorId,
        update: &ChatUpdate,
        snapshot: bool,
        posted: &mut HashSet<(ConversationId, String)>,
    ) -> Result<Vec<Dispatch>, CoreError> {
        let (changes, avatar) = self.merge_report(connector_id, update).await?;
        let mut dispatches = Vec::new();
        let conversations = self
            .conversations_of_chat(connector_id, &update.chat_id)
            .await?;
        // A title is never a note: it renames the thread, snapshots included,
        // and only from the conversation's source chat.
        if let Some(title) = update.title.as_deref() {
            for (conversation, endpoint) in &conversations {
                if self.is_source_endpoint(conversation, *endpoint).await? {
                    dispatches.extend(
                        self.retitle_conversation(routing, conversation.clone(), title)
                            .await?,
                    );
                }
            }
        }
        if !snapshot {
            let notes = change_notes(&changes, avatar);
            dispatches.extend(
                self.route_notes(routing, &conversations, &notes, posted)
                    .await?,
            );
        }
        Ok(dispatches)
    }

    /// Merge one report into the stored profile, writing only on a change.
    /// Returns what changed and, for a reported picture, its attachment.
    async fn merge_report(
        &self,
        connector_id: &ConnectorId,
        update: &ChatUpdate,
    ) -> Result<(ProfileChanges, Option<Attachment>), CoreError> {
        let mut avatar_hash = None;
        let mut attachment = None;
        if let Some(media_id) = update.avatar.as_ref().and_then(|avatar| avatar.media_id)
            && let Some(blob) = self.storage.media(media_id).await?
        {
            avatar_hash = Some(sha256_hex(&blob.bytes));
            attachment = Some(blob.attachment);
        }
        let mut profile = self
            .storage
            .chat_profile(connector_id, &update.chat_id)
            .await?
            .unwrap_or_else(|| ChatProfile::new(connector_id.clone(), update.chat_id.clone()));
        let changes = apply(&mut profile, update, avatar_hash);
        if changes.changed() {
            self.storage.put_chat_profile(&profile).await?;
        }
        Ok((changes, attachment))
    }

    /// Every conversation the chat belongs to, under any address form the
    /// platform reported for it, with the chat's endpoint in it. An unrouted
    /// chat belongs to none.
    pub(crate) async fn conversations_of_chat(
        &self,
        connector_id: &ConnectorId,
        chat_id: &str,
    ) -> Result<Vec<(Conversation, EndpointId)>, CoreError> {
        let own = ChatAddress::new(chat_id.to_owned(), None);
        let forms = address_forms(&*self.storage, connector_id, &own).await?;
        let mut seen = HashSet::new();
        let mut conversations = Vec::new();
        for form in forms.iter() {
            let Some(endpoint) = self.storage.resolve_endpoint(connector_id, form).await? else {
                continue;
            };
            let Some(conversation) = self.storage.conversation_for_endpoint(endpoint).await? else {
                continue;
            };
            if seen.insert(conversation.id) {
                conversations.push((conversation, endpoint));
            }
        }
        Ok(conversations)
    }

    /// Whether `endpoint` is the conversation's source chat.
    async fn is_source_endpoint(
        &self,
        conversation: &Conversation,
        endpoint: EndpointId,
    ) -> Result<bool, CoreError> {
        Ok(self
            .storage
            .resolve_endpoint(
                &conversation.source.connector_id,
                &conversation.source.address,
            )
            .await?
            == Some(endpoint))
    }

    /// Route each note to every other endpoint of each conversation and save
    /// the dispatches; returns the ones ready to send. A note already in
    /// `posted` for a conversation is skipped there.
    async fn route_notes(
        &self,
        routing: &Arc<RoutingConfig>,
        conversations: &[(Conversation, EndpointId)],
        notes: &[Note],
        posted: &mut HashSet<(ConversationId, String)>,
    ) -> Result<Vec<Dispatch>, CoreError> {
        let mut routed = Vec::new();
        for note in notes {
            for (conversation, endpoint) in conversations {
                if !posted.insert((conversation.id, note.text.clone())) {
                    continue;
                }
                routed.extend(
                    self.with_routing(routing)
                        .route_note(conversation, *endpoint, note)
                        .await?,
                );
            }
        }
        let (pending, _) = self.save_dispatches(routed).await?;
        Ok(pending)
    }

    /// Record what an inbound message says about its chat's timer. Runs before
    /// routing, so chats no rule matches are recorded too. Returns what
    /// changed, so the caller can post a note once the message is routed.
    ///
    /// A message that carries a timer is a report like any other. A message
    /// without one, from a connector that stamps timers, settles a chat whose
    /// timer is still unknown as off: the platform would have put the timer on
    /// the message had the chat one. That fill has no setting timestamp, so
    /// any later report replaces it, and it never touches a known timer (the
    /// merge rule would let a timestamp-less 0 replace one, so it is written
    /// directly rather than merged). Being a first-seen value, it is no change.
    pub(crate) async fn observe_message_timer(
        &self,
        routing: &RoutingConfig,
        source: &SourceAddress,
        timer: Option<DisappearingTimer>,
    ) -> Result<ProfileChanges, CoreError> {
        let chat_id = &source.address.chat_id;
        if let Some(timer) = timer {
            let update = ChatUpdate {
                disappearing: Some(timer),
                ..ChatUpdate::new(chat_id.clone())
            };
            let (changes, _) = self.merge_report(&source.connector_id, &update).await?;
            return Ok(changes);
        }
        if !stamps_timers(routing, &source.connector_id) {
            return Ok(ProfileChanges::default());
        }
        let stored = self
            .storage
            .chat_profile(&source.connector_id, chat_id)
            .await?;
        if stored
            .as_ref()
            .is_some_and(|profile| profile.disappearing.is_some())
        {
            return Ok(ProfileChanges::default());
        }
        let mut profile = stored
            .unwrap_or_else(|| ChatProfile::new(source.connector_id.clone(), chat_id.clone()));
        profile.disappearing = Some(DisappearingTimer::new(0));
        self.storage.put_chat_profile(&profile).await?;
        Ok(ProfileChanges::default())
    }

    /// The notes a message's timer change posts, routed to every other
    /// endpoint of the chat's conversations.
    pub(crate) async fn message_timer_notes(
        &self,
        routing: &Arc<RoutingConfig>,
        source: &SourceAddress,
        changes: &ProfileChanges,
    ) -> Result<Vec<Dispatch>, CoreError> {
        let notes = change_notes(changes, None);
        if notes.is_empty() {
            return Ok(Vec::new());
        }
        let conversations = self
            .conversations_of_chat(&source.connector_id, &source.address.chat_id)
            .await?;
        self.route_notes(routing, &conversations, &notes, &mut HashSet::new())
            .await
    }

    /// The profile of a conversation's source chat, under any of its address
    /// forms (its own form first).
    pub(crate) async fn source_profile(
        &self,
        conversation: &Conversation,
    ) -> Result<Option<ChatProfile>, CoreError> {
        let source = &conversation.source;
        let own = ChatAddress::new(source.address.chat_id.clone(), None);
        let forms = address_forms(&*self.storage, &source.connector_id, &own).await?;
        for form in forms.iter() {
            if let Some(profile) = self
                .storage
                .chat_profile(&source.connector_id, &form.chat_id)
                .await?
            {
                return Ok(Some(profile));
            }
        }
        Ok(None)
    }
}

/// Whether the connector declares that it stamps disappearing timers.
fn stamps_timers(routing: &RoutingConfig, connector_id: &ConnectorId) -> bool {
    routing
        .connector(connector_id)
        .is_some_and(|connector| connector.capabilities.supports_disappearing_timers)
}

/// The timer a send to `target` carries: `None` when the target's connector
/// does not declare supports_disappearing_timers, or when no profile of the
/// chat (under its own id, then under any linked form, newest timestamp
/// first) has one.
pub(crate) async fn target_disappearing<S: TopologyStore + ChatProfileStore>(
    storage: &S,
    routing: &RoutingConfig,
    target: &Target,
) -> Result<Option<DisappearingTimer>, StorageError> {
    if !stamps_timers(routing, &target.connector_id) {
        return Ok(None);
    }
    let own = ChatAddress::new(target.address.chat_id.clone(), None);
    let forms = address_forms(storage, &target.connector_id, &own).await?;
    let mut seen = HashSet::new();
    let mut best: Option<DisappearingTimer> = None;
    // The set starts with the target's own address, so its profile is read
    // first and a linked form only wins with a strictly newer timestamp.
    for form in forms.iter() {
        if !seen.insert(form.chat_id.as_str()) {
            continue;
        }
        let Some(timer) = storage
            .chat_profile(&target.connector_id, &form.chat_id)
            .await?
            .and_then(|profile| profile.disappearing)
        else {
            continue;
        };
        if best.is_none_or(|current| timer.setting_timestamp > current.setting_timestamp) {
            best = Some(timer);
        }
    }
    Ok(best)
}
