//! Chat state (pin, mute, archive): reading it off WhatsApp's app-state
//! events and history syncs, and choosing the chat action that applies a
//! state the core sends.
//!
//! The core ends every echo by comparing a report with the stored state, so
//! each value is encoded exactly and reversibly. A mute is one number,
//! `muted_until` in Unix seconds: 0 not muted, -1 muted forever, otherwise the
//! end. WhatsApp keeps the end in milliseconds; a mute set from the core uses
//! exactly `muted_until * 1000`, so WhatsApp's echo divides back to the same
//! number. A mute whose end has passed is reported as 0.

use whatsapp_rust::types::events::Event;
use whatsapp_rust::waproto::whatsapp as wa;
use whatsapp_rust::{Jid, Server};
use wuphf_proto::connector as pb;

/// `muted_until` for a mute without an end.
pub const FOREVER: i64 = -1;

/// History syncs do not document the unit of `mute_end_time`: a value above
/// this is milliseconds, a smaller one seconds (the two ranges split before
/// the year 5138). Replace it once a real sync confirms the unit.
const MILLISECONDS_ABOVE: u64 = 100_000_000_000;

/// Now, in Unix milliseconds, from the same clock the client's chat actions
/// check a mute end against.
pub fn now_ms() -> i64 {
    whatsapp_rust::wacore::time::now_millis()
}

/// One app-state change to a chat's pin, mute or archive state.
#[derive(Debug, PartialEq)]
pub struct StateReport {
    /// The chat (without a device part).
    pub jid: Jid,
    /// The changed field, with no chat id yet: it goes out under every form
    /// of the chat.
    pub update: pb::ChatUpdate,
    /// A full app-state sync reports current state, not a live change.
    pub snapshot: bool,
}

/// The state change a pin, mute or archive event reports, or `None` for any
/// other event and for chats that are not DMs or groups.
pub fn state_report(event: &Event, now_ms: i64) -> Option<StateReport> {
    let (jid, update, snapshot) = match event {
        Event::PinUpdate(pin) => (
            &pin.jid,
            pb::ChatUpdate {
                pinned: Some(pin.action.pinned.unwrap_or(false)),
                ..Default::default()
            },
            pin.from_full_sync,
        ),
        Event::MuteUpdate(mute) => (
            &mute.jid,
            pb::ChatUpdate {
                mute: Some(pb::MuteState {
                    muted_until: muted_until(
                        mute.action.muted,
                        mute.action.mute_end_timestamp,
                        now_ms,
                    ),
                }),
                ..Default::default()
            },
            mute.from_full_sync,
        ),
        Event::ArchiveUpdate(archive) => (
            &archive.jid,
            pb::ChatUpdate {
                archived: Some(archive.action.archived.unwrap_or(false)),
                ..Default::default()
            },
            archive.from_full_sync,
        ),
        _ => return None,
    };
    let jid = jid.to_non_ad();
    matches!(jid.server, Server::Pn | Server::Lid | Server::Group).then_some(StateReport {
        jid,
        update,
        snapshot,
    })
}

/// A `MuteAction` as `muted_until`: not muted → 0; muted with a negative end
/// or no end → forever; otherwise the end in seconds, or 0 once it passed.
pub fn muted_until(muted: Option<bool>, end_ms: Option<i64>, now_ms: i64) -> i64 {
    if muted != Some(true) {
        return 0;
    }
    match end_ms {
        None => FOREVER,
        Some(end_ms) if end_ms < 0 => FOREVER,
        Some(end_ms) => unless_expired(end_ms, now_ms),
    }
}

/// A history-sync `mute_end_time` as `muted_until`. 0 or absent → not muted;
/// a value too large for an `i64` is a negative one written as unsigned
/// (-1 = forever); otherwise the unit rule, and 0 once the end passed.
pub fn history_muted_until(mute_end_time: Option<u64>, now_ms: i64) -> i64 {
    let end = mute_end_time.unwrap_or(0);
    if end == 0 {
        return 0;
    }
    let Ok(signed) = i64::try_from(end) else {
        return FOREVER;
    };
    let end_ms = if end > MILLISECONDS_ABOVE {
        signed
    } else {
        signed.saturating_mul(1000)
    };
    unless_expired(end_ms, now_ms)
}

/// A mute end in milliseconds as seconds, or 0 when it is not in the future.
fn unless_expired(end_ms: i64, now_ms: i64) -> i64 {
    if end_ms <= now_ms { 0 } else { end_ms / 1000 }
}

/// A synced conversation's pin, mute and archive state (absent fields are
/// the defaults: a sync omits them when off). No chat id: the caller adds it.
pub fn history_state(conversation: &wa::Conversation, now_ms: i64) -> pb::ChatUpdate {
    pb::ChatUpdate {
        pinned: Some(conversation.pinned.unwrap_or(0) > 0),
        mute: Some(pb::MuteState {
            muted_until: history_muted_until(conversation.mute_end_time, now_ms),
        }),
        archived: Some(conversation.archived.unwrap_or(false)),
        ..Default::default()
    }
}

/// The chat action that brings a chat to a `muted_until` the core sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MuteCall {
    /// `mute_chat`: muted forever.
    Forever,
    /// `mute_chat_until` with this end in milliseconds (`muted_until * 1000`).
    Until(i64),
    /// `unmute_chat`: not muted, or an end that already passed (the client
    /// refuses a past end).
    Unmute,
}

pub fn mute_call(until: i64, now_ms: i64) -> MuteCall {
    if until < 0 {
        return MuteCall::Forever;
    }
    if until == 0 {
        return MuteCall::Unmute;
    }
    match until.checked_mul(1000) {
        Some(end_ms) if end_ms > now_ms => MuteCall::Until(end_ms),
        Some(_) => MuteCall::Unmute,
        // Beyond any date the client can hold: as good as forever.
        None => MuteCall::Forever,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use whatsapp_rust::chrono::DateTime;
    use whatsapp_rust::types::events::{ArchiveUpdate, MuteUpdate, PinUpdate};

    const NOW_MS: i64 = 1_700_000_000_000;

    fn jid(id: &str) -> Jid {
        id.parse().unwrap()
    }

    fn pin(id: &str, pinned: Option<bool>, from_full_sync: bool) -> Event {
        Event::PinUpdate(
            PinUpdate::builder()
                .jid(jid(id))
                .timestamp(DateTime::from_timestamp(1_700_000_000, 0).unwrap())
                .action(Box::new(wa::sync_action_value::PinAction { pinned }))
                .from_full_sync(from_full_sync)
                .build(),
        )
    }

    fn mute(muted: Option<bool>, end_ms: Option<i64>, from_full_sync: bool) -> Event {
        Event::MuteUpdate(
            MuteUpdate::builder()
                .jid(jid("1555@s.whatsapp.net"))
                .timestamp(DateTime::from_timestamp(1_700_000_000, 0).unwrap())
                .action(Box::new(wa::sync_action_value::MuteAction {
                    muted,
                    mute_end_timestamp: end_ms,
                    ..Default::default()
                }))
                .from_full_sync(from_full_sync)
                .build(),
        )
    }

    fn archive(archived: Option<bool>, from_full_sync: bool) -> Event {
        Event::ArchiveUpdate(
            ArchiveUpdate::builder()
                .jid(jid("123@g.us"))
                .timestamp(DateTime::from_timestamp(1_700_000_000, 0).unwrap())
                .action(Box::new(wa::sync_action_value::ArchiveChatAction {
                    archived,
                    ..Default::default()
                }))
                .from_full_sync(from_full_sync)
                .build(),
        )
    }

    fn reported_mute(event: Event) -> i64 {
        state_report(&event, NOW_MS)
            .and_then(|report| report.update.mute)
            .map(|mute| mute.muted_until)
            .expect("a mute report")
    }

    #[test]
    fn a_pin_event_reports_the_pin() {
        assert_eq!(
            state_report(&pin("1555:3@s.whatsapp.net", Some(true), false), NOW_MS),
            Some(StateReport {
                jid: jid("1555@s.whatsapp.net"),
                update: pb::ChatUpdate {
                    pinned: Some(true),
                    ..Default::default()
                },
                snapshot: false,
            })
        );
        for unpinned in [Some(false), None] {
            let report = state_report(&pin("123@g.us", unpinned, false), NOW_MS).unwrap();
            assert_eq!(report.update.pinned, Some(false));
        }
        // A full app-state sync is a snapshot.
        assert!(
            state_report(&pin("123@g.us", Some(true), true), NOW_MS)
                .unwrap()
                .snapshot
        );
        // Not a chat the core mirrors.
        assert_eq!(
            state_report(&pin("status@broadcast", Some(true), false), NOW_MS),
            None
        );
    }

    #[test]
    fn a_mute_event_reports_the_end_in_seconds() {
        // An end in the future, in milliseconds: divided by 1000.
        assert_eq!(
            reported_mute(mute(Some(true), Some(1_900_000_000_123), false)),
            1_900_000_000
        );
        // Forever.
        assert_eq!(reported_mute(mute(Some(true), Some(-1), false)), FOREVER);
        assert_eq!(reported_mute(mute(Some(true), None, false)), FOREVER);
        // Expired, or not muted.
        assert_eq!(
            reported_mute(mute(Some(true), Some(1_600_000_000_000), false)),
            0
        );
        assert_eq!(reported_mute(mute(Some(true), Some(NOW_MS), false)), 0);
        assert_eq!(reported_mute(mute(Some(true), Some(0), false)), 0);
        assert_eq!(reported_mute(mute(Some(false), Some(0), false)), 0);
        assert_eq!(reported_mute(mute(None, Some(1_900_000_000_000), false)), 0);
        let report = state_report(&mute(Some(true), Some(-1), true), NOW_MS).unwrap();
        assert!(report.snapshot);
        assert_eq!(report.update.pinned, None);
        assert_eq!(report.update.archived, None);
    }

    #[test]
    fn an_archive_event_reports_the_archive() {
        let report = state_report(&archive(Some(true), false), NOW_MS).unwrap();
        assert_eq!(report.jid, jid("123@g.us"));
        assert_eq!(
            report.update,
            pb::ChatUpdate {
                archived: Some(true),
                ..Default::default()
            }
        );
        assert!(!report.snapshot);
        for open in [Some(false), None] {
            let report = state_report(&archive(open, true), NOW_MS).unwrap();
            assert_eq!(report.update.archived, Some(false));
            assert!(report.snapshot);
        }
    }

    #[test]
    fn history_sync_state_reads_either_mute_unit() {
        let conversation = |pinned, archived, mute_end_time| wa::Conversation {
            id: "1555@s.whatsapp.net".into(),
            pinned,
            archived,
            mute_end_time,
            ..Default::default()
        };
        let state = history_state(
            &conversation(Some(3), Some(true), Some(1_900_000_000_000)),
            NOW_MS,
        );
        assert_eq!(
            state,
            pb::ChatUpdate {
                pinned: Some(true),
                mute: Some(pb::MuteState {
                    muted_until: 1_900_000_000
                }),
                archived: Some(true),
                ..Default::default()
            }
        );
        // Off when absent.
        let state = history_state(&conversation(None, None, None), NOW_MS);
        assert_eq!(state.pinned, Some(false));
        assert_eq!(state.archived, Some(false));
        assert_eq!(state.mute, Some(pb::MuteState { muted_until: 0 }));
        assert_eq!(
            history_state(&conversation(Some(0), Some(false), Some(0)), NOW_MS).pinned,
            Some(false)
        );

        // Milliseconds and seconds name the same end.
        assert_eq!(
            history_muted_until(Some(1_900_000_000_000), NOW_MS),
            1_900_000_000
        );
        assert_eq!(
            history_muted_until(Some(1_900_000_000), NOW_MS),
            1_900_000_000
        );
        // Expired in either unit.
        assert_eq!(history_muted_until(Some(1_600_000_000_000), NOW_MS), 0);
        assert_eq!(history_muted_until(Some(1_600_000_000), NOW_MS), 0);
        // -1 written as an unsigned value: forever.
        assert_eq!(history_muted_until(Some(u64::MAX), NOW_MS), FOREVER);
        assert_eq!(history_muted_until(Some(0), NOW_MS), 0);
    }

    #[test]
    fn the_mute_call_matches_the_state() {
        assert_eq!(mute_call(FOREVER, NOW_MS), MuteCall::Forever);
        assert_eq!(mute_call(0, NOW_MS), MuteCall::Unmute);
        assert_eq!(
            mute_call(1_900_000_000, NOW_MS),
            MuteCall::Until(1_900_000_000_000)
        );
        // The client refuses an end that is not in the future.
        assert_eq!(mute_call(1_600_000_000, NOW_MS), MuteCall::Unmute);
        assert_eq!(mute_call(NOW_MS / 1000, NOW_MS), MuteCall::Unmute);
        assert_eq!(mute_call(i64::MAX, NOW_MS), MuteCall::Forever);
    }

    #[test]
    fn a_mute_set_from_the_core_echoes_back_unchanged() {
        for until in [FOREVER, 0, 1_900_000_000] {
            let (muted, end_ms) = match mute_call(until, NOW_MS) {
                MuteCall::Forever => (true, -1),
                MuteCall::Until(end_ms) => (true, end_ms),
                MuteCall::Unmute => (false, 0),
            };
            assert_eq!(muted_until(Some(muted), Some(end_ms), NOW_MS), until);
        }
    }
}
