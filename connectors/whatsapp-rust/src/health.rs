//! Session health: the WhatsApp events that stop this session from bridging
//! become owner notices, like the Go connector's `sessionNotice`. Without them
//! a logged-out connector is silent, which looks the same as nobody writing.

use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::warn;
use whatsapp_rust::types::events::{ConnectFailureReason, Event, TemporaryBan};
use wuphf_proto::connector as pb;

/// The notice a WhatsApp event calls for, if any: its kind and the detail the
/// core appends to its own wording.
pub fn session_notice(event: &Event) -> Option<(pb::NoticeKind, String)> {
    match event {
        Event::LoggedOut(out) => Some((
            pb::NoticeKind::LoggedOut,
            if out.on_connect {
                format!(
                    "WhatsApp rejected the session on connect ({})",
                    connect_failure_text(out.reason)
                )
            } else {
                "WhatsApp ended the session".to_owned()
            },
        )),
        // Another client took the session over; this one is done until it is
        // paired again.
        Event::StreamReplaced(_) => Some((
            pb::NoticeKind::LoggedOut,
            "another WhatsApp client replaced this session".to_owned(),
        )),
        // WhatsApp refuses this client's version, so the fix is a connector
        // upgrade rather than a re-pair.
        Event::ClientOutdated(_) => Some((
            pb::NoticeKind::AuthRequired,
            "WhatsApp rejected this client as outdated; upgrade the connector".to_owned(),
        )),
        Event::TemporaryBan(ban) => Some((pb::NoticeKind::Degraded, temporary_ban_text(ban))),
        // A reconnect after trouble tells the owner the bridge is back; the core
        // drops it when nothing was reported wrong.
        Event::Connected(_) => Some((pb::NoticeKind::Recovered, String::new())),
        _ => None,
    }
}

/// whatsmeow's `ConnectFailureReason.String()`: "<code>: <message>". The
/// library has the codes but not the messages, so they are whatsmeow's.
fn connect_failure_text(reason: ConnectFailureReason) -> String {
    let message = match reason {
        ConnectFailureReason::LoggedOut => "logged out from another device",
        ConnectFailureReason::TempBanned => "account temporarily banned",
        ConnectFailureReason::AccountLocked => "primary device was logged out",
        ConnectFailureReason::ClientOutdated => "client is out of date",
        ConnectFailureReason::UnknownLogout => "logged out for unknown reason",
        ConnectFailureReason::BadUserAgent => "client user agent was rejected",
        ConnectFailureReason::CatExpired => "messenger crypto auth token has expired",
        ConnectFailureReason::CatInvalid => "messenger crypto auth token is invalid",
        _ => "unknown error",
    };
    format!("{}: {message}", reason.code())
}

/// whatsmeow's `TemporaryBan.String()`.
fn temporary_ban_text(ban: &TemporaryBan) -> String {
    let seconds = ban.expire.num_seconds();
    if seconds == 0 {
        return format!("You've been temporarily banned: {}", ban.code);
    }
    format!(
        "You've been temporarily banned: {}. The ban expires in {}",
        ban.code,
        go_duration(seconds)
    )
}

/// A whole number of seconds in Go's `time.Duration` format: "24h0m0s",
/// "5m0s", "30s", "0s".
fn go_duration(seconds: i64) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    match (hours, minutes) {
        (0, 0) => format!("{seconds}s"),
        (0, _) => format!("{minutes}m{seconds}s"),
        _ => format!("{hours}h{minutes}m{seconds}s"),
    }
}

/// Turns session health events into notices on the connector's input channel.
#[derive(Clone)]
pub struct Health {
    inputs: mpsc::Sender<pb::ClientFrame>,
}

impl Health {
    pub fn new(inputs: mpsc::Sender<pb::ClientFrame>) -> Self {
        Self { inputs }
    }

    pub async fn handle(&self, event: Arc<Event>) {
        if let Some((kind, detail)) = session_notice(&event) {
            warn!(?kind, detail, "WhatsApp session health changed");
            wuphf_connector::send_notice(&self.inputs, kind, detail);
        }
    }
}

#[cfg(test)]
mod tests {
    use whatsapp_rust::chrono::Duration;
    use whatsapp_rust::types::events::{
        ClientOutdated, Connected, LoggedOut, StreamReplaced, TempBanReason,
    };

    use super::*;

    fn logged_out(on_connect: bool, reason: ConnectFailureReason) -> Event {
        Event::LoggedOut(
            LoggedOut::builder()
                .on_connect(on_connect)
                .reason(reason)
                .build(),
        )
    }

    fn ban(code: TempBanReason, expire: Duration) -> Event {
        Event::TemporaryBan(TemporaryBan::builder().code(code).expire(expire).build())
    }

    fn kind_of(event: &Event) -> Option<pb::NoticeKind> {
        session_notice(event).map(|(kind, _)| kind)
    }

    #[test]
    fn session_loss_maps_to_go_s_notice_kinds() {
        use pb::NoticeKind::*;
        let cases = [
            (
                logged_out(false, ConnectFailureReason::LoggedOut),
                Some(LoggedOut),
            ),
            (
                Event::StreamReplaced(StreamReplaced::builder().build()),
                Some(LoggedOut),
            ),
            (
                Event::ClientOutdated(ClientOutdated::builder().build()),
                Some(AuthRequired),
            ),
            (
                ban(TempBanReason::BlockedByUsers, Duration::hours(1)),
                Some(Degraded),
            ),
            (
                Event::Connected(Connected::builder().build()),
                Some(Recovered),
            ),
        ];
        for (event, want) in cases {
            assert_eq!(kind_of(&event), want, "{event:?}");
        }
        assert_eq!(
            session_notice(&Event::Connected(Connected::builder().build())),
            Some((Recovered, String::new()))
        );
        assert_eq!(
            session_notice(&logged_out(false, ConnectFailureReason::LoggedOut)),
            Some((LoggedOut, "WhatsApp ended the session".into()))
        );
    }

    #[test]
    fn a_rejected_connect_carries_the_reason() {
        let (_, detail) =
            session_notice(&logged_out(true, ConnectFailureReason::LoggedOut)).unwrap();
        assert_eq!(
            detail,
            "WhatsApp rejected the session on connect (401: logged out from another device)"
        );
        assert_eq!(
            connect_failure_text(ConnectFailureReason::Unknown(499)),
            "499: unknown error"
        );
    }

    #[test]
    fn a_ban_reads_like_whatsmeow() {
        let (_, detail) = session_notice(&ban(
            TempBanReason::SentToTooManyPeople,
            Duration::hours(24),
        ))
        .unwrap();
        assert_eq!(
            detail,
            "You've been temporarily banned: 101: you sent too many messages to people who \
             don't have you in their address books. The ban expires in 24h0m0s"
        );
        let (_, detail) =
            session_notice(&ban(TempBanReason::BlockedByUsers, Duration::zero())).unwrap();
        assert_eq!(
            detail,
            "You've been temporarily banned: 102: too many people blocked you"
        );
    }

    #[test]
    fn durations_print_like_go() {
        for (seconds, want) in [
            (0, "0s"),
            (30, "30s"),
            (300, "5m0s"),
            (3661, "1h1m1s"),
            (86_400, "24h0m0s"),
        ] {
            assert_eq!(go_duration(seconds), want);
        }
    }
}
