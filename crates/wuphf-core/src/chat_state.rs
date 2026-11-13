//! Two-way pin, mute and archive between a source chat and its mirror topics.
//!
//! A conversation has one chat state, kept in its source chat's profile under
//! the conversation's own source chat id (never under whichever address form a
//! report used, so both sides always compare against the same value). A
//! report equal to the stored state is a no-op: that is what turns every echo
//! of a dispatch this module sent into nothing. There is no suppression window.

use crate::config::RoutingConfig;
use crate::{Core, CoreError};
use std::sync::Arc;
use wuphf_model::{
    Action, ChatAddress, ChatProfile, ChatRef, ChatStateDirection, ChatStatePolicy, ChatUpdate,
    ConnectorId, Conversation, Dispatch, EndpointId, MuteState, Target,
};
use wuphf_storage::Storage;

/// The pin / mute / archive fields of one report or one dispatch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ChatState {
    pinned: Option<bool>,
    mute: Option<MuteState>,
    archived: Option<bool>,
}

impl ChatState {
    fn of(update: &ChatUpdate) -> Self {
        Self {
            pinned: update.pinned,
            mute: update.mute,
            archived: update.archived,
        }
    }

    fn is_empty(&self) -> bool {
        self.pinned.is_none() && self.mute.is_none() && self.archived.is_none()
    }

    /// The reported fields that differ from the profile and that `allowed`
    /// lets through, written into the profile as they are taken.
    fn take_changes(
        &self,
        profile: &mut ChatProfile,
        allowed: impl Fn(ChatStateDirection) -> bool,
        policy: ChatStatePolicy,
    ) -> ChatState {
        let mut changed = ChatState::default();
        if let Some(pinned) = self.pinned
            && profile.pinned != Some(pinned)
            && allowed(policy.pin)
        {
            profile.pinned = Some(pinned);
            changed.pinned = Some(pinned);
        }
        if let Some(mute) = self.mute
            && profile.mute != Some(mute)
            && allowed(policy.mute)
        {
            profile.mute = Some(mute);
            changed.mute = Some(mute);
        }
        if let Some(archived) = self.archived
            && profile.archived != Some(archived)
            && allowed(policy.archive)
        {
            profile.archived = Some(archived);
            changed.archived = Some(archived);
        }
        changed
    }

    /// Only the fields whose direction lets them cross.
    fn crossing(
        &self,
        allowed: impl Fn(ChatStateDirection) -> bool,
        policy: ChatStatePolicy,
    ) -> Self {
        Self {
            pinned: self.pinned.filter(|_| allowed(policy.pin)),
            mute: self.mute.filter(|_| allowed(policy.mute)),
            archived: self.archived.filter(|_| allowed(policy.archive)),
        }
    }

    fn action(&self) -> Action {
        Action::SetChatState {
            pinned: self.pinned,
            mute: self.mute,
            archived: self.archived,
        }
    }
}

impl<S> Core<S>
where
    S: Storage,
{
    /// Apply one side's report of a conversation's chat state and return the
    /// dispatches that carry it to the other side.
    ///
    /// The state lives in the source chat's profile. A report equal to it is a
    /// no-op — every echo of a dispatch this function sent ends here. A report
    /// from the source side is always stored (the profile mirrors the source);
    /// it is dispatched to each mirror endpoint with a thread when the
    /// policy's direction allows `to_mirror` and the update is not a snapshot.
    /// A report from the mirror side is stored and dispatched to the source
    /// endpoint only when the direction allows `to_source`; otherwise it is
    /// dropped.
    pub(crate) async fn sync_chat_state(
        &self,
        routing: &Arc<RoutingConfig>,
        connector_id: &ConnectorId,
        update: &ChatUpdate,
        snapshot: bool,
        conversations: &[(Conversation, EndpointId)],
    ) -> Result<Vec<Dispatch>, CoreError> {
        let reported = ChatState::of(update);
        if reported.is_empty() {
            return Ok(Vec::new());
        }
        let dispatches = match &update.thread_id {
            Some(thread_id) => {
                // A mirror side never reports a snapshot.
                if snapshot {
                    return Ok(Vec::new());
                }
                self.sync_from_mirror(routing, connector_id, update, thread_id, reported)
                    .await?
            }
            None => {
                self.sync_from_source(
                    routing,
                    connector_id,
                    update,
                    snapshot,
                    reported,
                    conversations,
                )
                .await?
            }
        };
        let (pending, _) = self.save_dispatches(dispatches).await?;
        Ok(pending)
    }

    /// A report about a chat (no thread): the source side of every
    /// conversation the chat is the source of. A chat that is the source of
    /// none still has its state recorded, on its own profile.
    async fn sync_from_source(
        &self,
        routing: &Arc<RoutingConfig>,
        connector_id: &ConnectorId,
        update: &ChatUpdate,
        snapshot: bool,
        reported: ChatState,
        conversations: &[(Conversation, EndpointId)],
    ) -> Result<Vec<Dispatch>, CoreError> {
        let mut dispatches = Vec::new();
        let mut sourced = false;
        for (conversation, endpoint) in conversations {
            if !self.is_source_endpoint(conversation, *endpoint).await? {
                continue;
            }
            sourced = true;
            let mut profile = self.state_profile(conversation).await?;
            let policy = self.policy(routing, conversation).await?;
            // The profile mirrors the source, so every difference is stored...
            let changed = reported.take_changes(&mut profile, |_| true, policy);
            if changed.is_empty() {
                continue;
            }
            self.storage.put_chat_profile(&profile).await?;
            // ...but only what may cross goes to the mirrors.
            let crossing = changed.crossing(|direction| direction.allows(true), policy);
            if snapshot || crossing.is_empty() {
                continue;
            }
            dispatches.extend(self.to_mirrors(routing, conversation, crossing).await?);
        }
        if !sourced {
            let mut profile = self
                .storage
                .chat_profile(connector_id, &update.chat_id)
                .await?
                .unwrap_or_else(|| ChatProfile::new(connector_id.clone(), update.chat_id.clone()));
            let changed = reported.take_changes(&mut profile, |_| true, ChatStatePolicy::default());
            if !changed.is_empty() {
                self.storage.put_chat_profile(&profile).await?;
            }
        }
        Ok(dispatches)
    }

    /// A report about a thread: the mirror side of the conversation whose
    /// mirror endpoint the thread is. A thread nobody maps, or one that is not
    /// a mirror topic, is dropped.
    async fn sync_from_mirror(
        &self,
        routing: &Arc<RoutingConfig>,
        connector_id: &ConnectorId,
        update: &ChatUpdate,
        thread_id: &str,
        reported: ChatState,
    ) -> Result<Vec<Dispatch>, CoreError> {
        let address = ChatAddress::new(update.chat_id.clone(), Some(thread_id.to_owned()));
        let Some(endpoint_id) = self
            .storage
            .resolve_endpoint(connector_id, &address)
            .await?
        else {
            return Ok(Vec::new());
        };
        let Some(conversation) = self.storage.conversation_for_endpoint(endpoint_id).await? else {
            return Ok(Vec::new());
        };
        let endpoint = self.storage.endpoint(endpoint_id).await?;
        if !self
            .with_routing(routing)
            .is_mirror(&conversation, &endpoint)
        {
            return Ok(Vec::new());
        }
        let mut profile = self.state_profile(&conversation).await?;
        let policy = self.policy(routing, &conversation).await?;
        let changed =
            reported.take_changes(&mut profile, |direction| direction.allows(false), policy);
        if changed.is_empty() {
            return Ok(Vec::new());
        }
        self.storage.put_chat_profile(&profile).await?;
        let source = &conversation.source;
        let Some(source_endpoint) = self
            .storage
            .resolve_endpoint(&source.connector_id, &source.address)
            .await?
        else {
            return Ok(Vec::new());
        };
        let source_endpoint = self.storage.endpoint(source_endpoint).await?;
        Ok(vec![Dispatch::new(
            Target {
                endpoint_id: source_endpoint.id,
                connector_id: source_endpoint.connector_id.clone(),
                address: source_endpoint.address(),
                create_thread: None,
            },
            conversation.id,
            changed.action(),
            None,
        )])
    }

    /// One dispatch per mirror endpoint that has a thread: a mirror without
    /// one has nothing to pin yet.
    async fn to_mirrors(
        &self,
        routing: &Arc<RoutingConfig>,
        conversation: &Conversation,
        state: ChatState,
    ) -> Result<Vec<Dispatch>, CoreError> {
        let router = self.with_routing(routing);
        let mut dispatches = Vec::new();
        for endpoint_id in &conversation.endpoints {
            let endpoint = self.storage.endpoint(*endpoint_id).await?;
            if endpoint.thread_id.is_none() || !router.is_mirror(conversation, &endpoint) {
                continue;
            }
            dispatches.push(Dispatch::new(
                Target {
                    endpoint_id: endpoint.id,
                    connector_id: endpoint.connector_id.clone(),
                    address: endpoint.address(),
                    create_thread: None,
                },
                conversation.id,
                state.action(),
                None,
            ));
        }
        Ok(dispatches)
    }

    /// The profile that holds a conversation's state: its source chat's,
    /// under the conversation's own source chat id.
    async fn state_profile(&self, conversation: &Conversation) -> Result<ChatProfile, CoreError> {
        let source = &conversation.source;
        Ok(self
            .storage
            .chat_profile(&source.connector_id, &source.address.chat_id)
            .await?
            .unwrap_or_else(|| {
                ChatProfile::new(source.connector_id.clone(), source.address.chat_id.clone())
            }))
    }

    /// The conversation's policy, from the operation's routing snapshot.
    async fn policy(
        &self,
        routing: &Arc<RoutingConfig>,
        conversation: &Conversation,
    ) -> Result<ChatStatePolicy, CoreError> {
        let mut endpoints = Vec::new();
        for endpoint_id in &conversation.endpoints {
            let endpoint = self.storage.endpoint(*endpoint_id).await?;
            endpoints.push(ChatRef {
                connector_id: endpoint.connector_id.clone(),
                address: endpoint.address(),
            });
        }
        Ok(routing.chat_state_policy(conversation, &endpoints))
    }
}
