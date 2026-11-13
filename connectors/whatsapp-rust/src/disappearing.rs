//! Disappearing-message timers: reading them off what WhatsApp reports,
//! stamping them on what the connector sends, and the send-side fallback for
//! a dispatch that arrives without one.
//!
//! The core keeps each chat's timer (its chat profile) and hands it to every
//! send; this module only extracts and applies. The mapping functions are pure
//! so they are tested without a network client.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tracing::warn;
use whatsapp_rust::chrono::{DateTime, Utc};
use whatsapp_rust::client::Client;
use whatsapp_rust::types::message::MessageInfo;
use whatsapp_rust::wacore::proto_helpers::MessageExt;
use whatsapp_rust::wacore::stanza::groups::GroupNotificationAction;
use whatsapp_rust::waproto::whatsapp as wa;
use whatsapp_rust::{Jid, Server};
use wuphf_proto::connector as pb;

use crate::groups::GroupDirectory;
use crate::normalize::{canonical, context_info, frame};

/// The most chat updates one frame carries. A history sync reports every
/// conversation at once; batching keeps each frame well under gRPC limits.
pub const CHAT_UPDATE_BATCH: usize = 500;

/// The timer an inbound message carried: WhatsApp's `ContextInfo.expiration`
/// when greater than 0, with its setting timestamp when the message has one.
/// A message without a timer reports none (absence never turns a timer off).
pub fn message_timer(info: &MessageInfo, message: &wa::Message) -> Option<pb::DisappearingTimer> {
    let seconds = info.ephemeral_expiration.filter(|seconds| *seconds > 0)?;
    let setting_timestamp = context_info(message)
        .and_then(|context| context.ephemeral_setting_timestamp)
        .filter(|timestamp| *timestamp > 0);
    Some(pb::DisappearingTimer {
        seconds,
        setting_timestamp,
    })
}

/// The update an `EPHEMERAL_SETTING` protocol message makes to its chat:
/// the new timer (0 = off), set at the message's setting timestamp or, when
/// it has none, at the message's own time.
pub fn ephemeral_setting(
    chat_id: String,
    info: &MessageInfo,
    protocol: &wa::message::ProtocolMessage,
) -> pb::ChatUpdate {
    let setting_timestamp = protocol
        .ephemeral_setting_timestamp
        .filter(|timestamp| *timestamp > 0)
        .unwrap_or_else(|| info.timestamp.timestamp());
    pb::ChatUpdate {
        chat_id,
        disappearing: Some(pb::DisappearingTimer {
            seconds: protocol.ephemeral_expiration.unwrap_or(0),
            setting_timestamp: Some(setting_timestamp),
        }),
        ..Default::default()
    }
}

/// The update a group notification makes, when it changes the timer
/// (`not_ephemeral` arrives as an expiration of 0). Over the notification's
/// parts, so it is tested without building a `GroupUpdate`.
pub fn group_action_update(
    group: &Jid,
    at: DateTime<Utc>,
    action: &GroupNotificationAction,
) -> Option<pb::ChatUpdate> {
    let GroupNotificationAction::Ephemeral { expiration, .. } = action else {
        return None;
    };
    Some(pb::ChatUpdate {
        chat_id: canonical(group),
        disappearing: Some(pb::DisappearingTimer {
            seconds: *expiration,
            setting_timestamp: Some(at.timestamp()),
        }),
        ..Default::default()
    })
}

/// Every conversation's timer in a history sync, under its canonical id and,
/// for a DM, under its other (phone or LID) form when the sync names it. A
/// sync snapshot omits the expiration when the timer is off, so an absent one
/// reports 0. `name_of` titles each conversation (given the conversation and its
/// chat JID); its title goes on every form. Chats other than DMs and groups
/// (status, broadcasts, newsletters) are skipped.
pub fn history_updates(
    sync: &wa::HistorySync,
    mut name_of: impl FnMut(&wa::Conversation, &Jid) -> Option<String>,
) -> Vec<pb::ChatUpdate> {
    let mut updates = Vec::new();
    let mut seen = HashSet::new();
    for conversation in &sync.conversations {
        let Some(chat) = chat_jid(&conversation.id) else {
            continue;
        };
        let timer = pb::DisappearingTimer {
            seconds: conversation.ephemeral_expiration.unwrap_or(0),
            setting_timestamp: conversation
                .ephemeral_setting_timestamp
                .filter(|timestamp| *timestamp > 0),
        };
        let title = name_of(conversation, &chat);
        let primary = canonical(&chat);
        let alternates = [&conversation.pn_jid, &conversation.lid_jid]
            .into_iter()
            .flatten()
            .filter_map(|id| chat_jid(id))
            .map(|jid| canonical(&jid))
            .filter(|id| *id != primary)
            .collect::<Vec<_>>();
        for chat_id in std::iter::once(primary).chain(alternates) {
            if !seen.insert(chat_id.clone()) {
                continue;
            }
            updates.push(pb::ChatUpdate {
                chat_id,
                disappearing: Some(timer),
                title: title.clone(),
                ..Default::default()
            });
        }
    }
    updates
}

/// A DM or group JID, or `None` for anything else.
fn chat_jid(id: &str) -> Option<Jid> {
    let jid: Jid = id.parse().ok()?;
    matches!(jid.server, Server::Pn | Server::Lid | Server::Group).then_some(jid)
}

/// Chat updates as frames of at most `batch` updates each. `snapshot` says
/// they describe current state (a history sync, a first sighting) rather than
/// live changes.
pub fn chat_update_frames(
    updates: Vec<pb::ChatUpdate>,
    batch: usize,
    snapshot: bool,
) -> Vec<pb::ClientFrame> {
    updates
        .chunks(batch)
        .map(|chunk| {
            frame(pb::client_frame::Body::ChatUpdate(pb::ChatUpdateEvent {
                updates: chunk.to_vec(),
                snapshot,
            }))
        })
        .collect()
}

/// Put the chat's timer on an outgoing message. Nothing for an unknown timer
/// or one that is off. A bare `conversation` is promoted to an extended text
/// message and an existing reply context is kept: the timer is merged into
/// it. The setting timestamp goes on the same context, since the client's
/// send options have no field for it.
pub fn stamp(message: &mut wa::Message, timer: Option<&pb::DisappearingTimer>) {
    let Some(timer) = timer.filter(|timer| timer.seconds > 0) else {
        return;
    };
    if !message.set_ephemeral_expiration(timer.seconds) {
        return;
    }
    if let Some(setting_timestamp) = timer.setting_timestamp
        && let Some(context) = context_info_mut(message)
    {
        context.ephemeral_setting_timestamp = Some(setting_timestamp);
    }
}

/// The context of the message's content, in the order
/// `set_ephemeral_expiration` looks for one.
fn context_info_mut(message: &mut wa::Message) -> Option<&mut wa::ContextInfo> {
    macro_rules! first_context {
        ($($field:ident),+ $(,)?) => {
            $(
                if let Some(content) = message.$field.as_option_mut() {
                    return content.context_info.as_option_mut();
                }
            )+
        };
    }
    first_context!(
        extended_text_message,
        image_message,
        video_message,
        audio_message,
        document_message,
        sticker_message,
    );
    None
}

/// Warns once per chat: the first time a chat's timer is unknown, and again
/// only after a timer for it was known in between.
#[derive(Default)]
pub struct NoticeGate {
    warned: Mutex<HashSet<String>>,
}

impl NoticeGate {
    /// True the first time `chat` is warned about until it is known again.
    pub fn first_warning(&self, chat: &str) -> bool {
        self.warned.lock().unwrap().insert(chat.to_owned())
    }

    /// The chat's timer is known; a later unknown warns again.
    pub fn known(&self, chat: &str) {
        self.warned.lock().unwrap().remove(chat);
    }
}

/// What the sender does when a dispatch arrives without a timer: a group's
/// timer is looked up (and reported to the core), and a DM's unknown timer is
/// raised with the owner once per chat until it becomes known.
pub struct TimerFallback {
    groups: Arc<GroupDirectory>,
    inputs: mpsc::Sender<pb::ClientFrame>,
    gate: NoticeGate,
}

impl TimerFallback {
    pub fn new(groups: Arc<GroupDirectory>, inputs: mpsc::Sender<pb::ClientFrame>) -> Self {
        Self {
            groups,
            inputs,
            gate: NoticeGate::default(),
        }
    }

    /// The timer to stamp on a send to `chat`.
    pub async fn resolve(
        &self,
        client: &Client,
        chat_id: &str,
        chat: &Jid,
        dispatched: Option<pb::DisappearingTimer>,
    ) -> Option<pb::DisappearingTimer> {
        if let Some(timer) = dispatched {
            self.gate.known(chat_id);
            return Some(timer);
        }
        if chat.server == Server::Group
            && let Some(facts) = self.groups.facts(client, chat).await
        {
            let timer = pb::DisappearingTimer {
                seconds: facts.disappearing_seconds,
                setting_timestamp: None,
            };
            self.gate.known(chat_id);
            self.emit(frame(pb::client_frame::Body::ChatUpdate(
                pb::ChatUpdateEvent {
                    updates: vec![pb::ChatUpdate {
                        chat_id: chat_id.to_owned(),
                        disappearing: Some(timer),
                        ..Default::default()
                    }],
                    snapshot: false,
                },
            )));
            return Some(timer);
        }
        if self.gate.first_warning(chat_id) {
            warn!(
                chat = chat_id,
                "sending without a disappearing timer: the chat's timer is unknown"
            );
            self.emit(frame(pb::client_frame::Body::OwnerNotice(
                pb::OwnerNotice {
                    kind: pb::NoticeKind::DisappearingTimerUnknown.into(),
                    detail: format!("chat {chat_id}"),
                },
            )));
        }
        None
    }

    /// Queue a frame for the core without waiting: the session loop that runs
    /// the sender is the one draining this channel, so awaiting a full channel
    /// here would deadlock it.
    fn emit(&self, frame: pb::ClientFrame) {
        emit_without_blocking(&self.inputs, frame);
    }
}

/// Send `frame` now if the channel has room, else from a task of its own.
fn emit_without_blocking(inputs: &mpsc::Sender<pb::ClientFrame>, frame: pb::ClientFrame) {
    match inputs.try_send(frame) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(frame)) => {
            let inputs = inputs.clone();
            tokio::spawn(async move {
                let _ = inputs.send(frame).await;
            });
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            warn!("core session input closed; dropping a chat update or notice");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use whatsapp_rust::wacore::proto_helpers::MessageBuilderExt;

    fn timer(seconds: u32, setting_timestamp: Option<i64>) -> pb::DisappearingTimer {
        pb::DisappearingTimer {
            seconds,
            setting_timestamp,
        }
    }

    fn context(message: &wa::Message) -> &wa::ContextInfo {
        message
            .extended_text_message
            .as_option()
            .and_then(|text| text.context_info.as_option())
            .expect("an extended text message with a context")
    }

    #[test]
    fn stamp_promotes_bare_text_and_sets_both_fields() {
        let mut message = wa::Message::text("hi");
        stamp(&mut message, Some(&timer(86_400, Some(1_700_000_000))));
        assert_eq!(message.conversation, None);
        let text = message.extended_text_message.as_option().unwrap();
        assert_eq!(text.text.as_deref(), Some("hi"));
        let context = context(&message);
        assert_eq!(context.expiration, Some(86_400));
        assert_eq!(context.ephemeral_setting_timestamp, Some(1_700_000_000));
    }

    #[test]
    fn stamp_merges_into_a_reply_context() {
        let reply = wa::ContextInfo {
            stanza_id: Some("quoted".into()),
            participant: Some("1@s.whatsapp.net".into()),
            ..Default::default()
        };
        let mut message = wa::Message::text_with_context("re", reply);
        stamp(&mut message, Some(&timer(604_800, None)));
        let context = context(&message);
        assert_eq!(context.stanza_id.as_deref(), Some("quoted"));
        assert_eq!(context.participant.as_deref(), Some("1@s.whatsapp.net"));
        assert_eq!(context.expiration, Some(604_800));
        assert_eq!(context.ephemeral_setting_timestamp, None);
    }

    #[test]
    fn an_off_or_unknown_timer_leaves_the_message_alone() {
        for unchanged in [None, Some(timer(0, Some(5)))] {
            let mut message = wa::Message::text("hi");
            stamp(&mut message, unchanged.as_ref());
            assert_eq!(message, wa::Message::text("hi"));
        }
    }

    #[test]
    fn the_message_timer_needs_a_running_timer() {
        let message = wa::Message::text_with_context(
            "hi",
            wa::ContextInfo {
                expiration: Some(86_400),
                ephemeral_setting_timestamp: Some(1_700_000_000),
                ..Default::default()
            },
        );
        let info = |seconds| MessageInfo {
            ephemeral_expiration: seconds,
            ..Default::default()
        };
        assert_eq!(
            message_timer(&info(Some(86_400)), &message),
            Some(timer(86_400, Some(1_700_000_000)))
        );
        assert_eq!(message_timer(&info(Some(0)), &message), None);
        assert_eq!(message_timer(&info(None), &message), None);
        assert_eq!(
            message_timer(&info(Some(86_400)), &wa::Message::text("hi")),
            Some(timer(86_400, None))
        );
    }

    fn group_notification(action: GroupNotificationAction) -> Option<pb::ChatUpdate> {
        group_action_update(
            &"123@g.us".parse().unwrap(),
            DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            &action,
        )
    }

    #[test]
    fn group_notifications_report_the_timer() {
        for seconds in [604_800, 0] {
            let update = group_notification(GroupNotificationAction::Ephemeral {
                expiration: seconds,
                trigger: None,
            });
            assert_eq!(
                update,
                Some(pb::ChatUpdate {
                    chat_id: "123@g.us".into(),
                    disappearing: Some(timer(seconds, Some(1_700_000_000))),
                    ..Default::default()
                })
            );
        }
        assert_eq!(group_notification(GroupNotificationAction::Unlocked), None);
    }

    fn conversation(id: &str, expiration: Option<u32>) -> wa::Conversation {
        wa::Conversation {
            id: id.into(),
            ephemeral_expiration: expiration,
            ..Default::default()
        }
    }

    #[test]
    fn history_sync_reports_every_chat_and_its_other_form() {
        let sync = wa::HistorySync {
            conversations: vec![
                conversation("1@g.us", Some(86_400)),
                wa::Conversation {
                    ephemeral_setting_timestamp: Some(1_700_000_000),
                    pn_jid: Some("15550001111@s.whatsapp.net".into()),
                    ..conversation("88887777@lid", Some(604_800))
                },
                conversation("2@s.whatsapp.net", None),
                conversation("status@broadcast", Some(86_400)),
            ],
            ..Default::default()
        };
        assert_eq!(
            history_updates(&sync, |_, _| None),
            vec![
                pb::ChatUpdate {
                    chat_id: "1@g.us".into(),
                    disappearing: Some(timer(86_400, None)),
                    ..Default::default()
                },
                pb::ChatUpdate {
                    chat_id: "88887777@lid".into(),
                    disappearing: Some(timer(604_800, Some(1_700_000_000))),
                    ..Default::default()
                },
                pb::ChatUpdate {
                    chat_id: "15550001111@s.whatsapp.net".into(),
                    disappearing: Some(timer(604_800, Some(1_700_000_000))),
                    ..Default::default()
                },
                pb::ChatUpdate {
                    chat_id: "2@s.whatsapp.net".into(),
                    disappearing: Some(timer(0, None)),
                    ..Default::default()
                },
            ]
        );
    }

    #[test]
    fn chat_updates_go_out_in_batches() {
        let sync = wa::HistorySync {
            conversations: (0..1201)
                .map(|n| conversation(&format!("{n}@s.whatsapp.net"), None))
                .collect(),
            ..Default::default()
        };
        let sizes =
            chat_update_frames(history_updates(&sync, |_, _| None), CHAT_UPDATE_BATCH, true)
                .into_iter()
                .map(|frame| match frame.body {
                    Some(pb::client_frame::Body::ChatUpdate(event)) => event.updates.len(),
                    other => panic!("expected a chat update frame, got {other:?}"),
                })
                .collect::<Vec<_>>();
        assert_eq!(sizes, [500, 500, 201]);
    }

    #[test]
    fn the_gate_warns_once_until_the_chat_is_known() {
        let gate = NoticeGate::default();
        assert!(gate.first_warning("1@s.whatsapp.net"));
        assert!(!gate.first_warning("1@s.whatsapp.net"));
        assert!(gate.first_warning("2@s.whatsapp.net"));
        gate.known("1@s.whatsapp.net");
        assert!(gate.first_warning("1@s.whatsapp.net"));
    }

    #[tokio::test]
    async fn a_full_channel_never_blocks_the_sender() {
        let (inputs, mut receiver) = mpsc::channel(1);
        emit_without_blocking(
            &inputs,
            frame(pb::client_frame::Body::ChatUpdate(Default::default())),
        );
        // The channel is full now; this must return at once.
        emit_without_blocking(
            &inputs,
            frame(pb::client_frame::Body::ChatUpdate(Default::default())),
        );
        assert!(receiver.recv().await.is_some());
        assert!(receiver.recv().await.is_some());
    }
}
