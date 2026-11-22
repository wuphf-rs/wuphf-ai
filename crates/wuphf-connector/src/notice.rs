//! Owner notices: the counterpart of the Go `coreclient.Notice` /
//! `SendNotice`. A connector that loses its platform session otherwise just
//! goes quiet, which looks identical to "nobody is messaging you"; the core
//! relays a notice to the owner through the connectors that still work.
//!
//! Report only what happened. The wording the owner reads, which chats it
//! reaches, and whether a repeat is worth sending are the core's decisions.

use tokio::sync::mpsc;
use tracing::debug;
use wuphf_proto::connector as pb;

/// An owner-notice frame of `kind`, with platform specifics in `detail`
/// (empty when there are none).
pub fn notice(kind: pb::NoticeKind, detail: impl Into<String>) -> pb::ClientFrame {
    notice_with_media(kind, detail, Vec::new())
}

/// [`notice`] with media the connector uploaded to the core's media store
/// (through [`crate::MediaClient`]), shown with the notice: a QR code to scan.
pub fn notice_with_media(
    kind: pb::NoticeKind,
    detail: impl Into<String>,
    media_ids: Vec<String>,
) -> pb::ClientFrame {
    pb::ClientFrame {
        body: Some(pb::client_frame::Body::OwnerNotice(pb::OwnerNotice {
            kind: kind.into(),
            detail: detail.into(),
            media_ids,
        })),
    }
}

/// Queue a notice on the connector's input channel without waiting: a full or
/// closed channel drops it (logged at debug). A notice is best-effort by
/// design and must never block or fail its caller. Returns whether it was
/// queued.
pub fn send_notice(
    inputs: &mpsc::Sender<pb::ClientFrame>,
    kind: pb::NoticeKind,
    detail: impl Into<String>,
) -> bool {
    match inputs.try_send(notice(kind, detail)) {
        Ok(()) => true,
        Err(error) => {
            debug!(?kind, %error, "dropping an owner notice: the input channel is not accepting frames");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn owner_notice(frame: pb::ClientFrame) -> pb::OwnerNotice {
        match frame.body {
            Some(pb::client_frame::Body::OwnerNotice(notice)) => notice,
            other => panic!("expected an owner notice, got {other:?}"),
        }
    }

    #[test]
    fn a_notice_frame_carries_kind_and_detail() {
        let sent = owner_notice(notice(
            pb::NoticeKind::LoggedOut,
            "WhatsApp ended the session",
        ));
        assert_eq!(sent.kind(), pb::NoticeKind::LoggedOut);
        assert_eq!(sent.detail, "WhatsApp ended the session");
        assert!(sent.media_ids.is_empty());
    }

    #[test]
    fn a_notice_frame_carries_its_media() {
        let sent = owner_notice(notice_with_media(
            pb::NoticeKind::LinkRequired,
            "scan the QR code",
            vec!["m-1".into()],
        ));
        assert_eq!(sent.kind(), pb::NoticeKind::LinkRequired);
        assert_eq!(sent.media_ids, ["m-1"]);
    }

    #[tokio::test]
    async fn send_notice_queues_when_there_is_room() {
        let (inputs, mut frames) = mpsc::channel(1);
        assert!(send_notice(&inputs, pb::NoticeKind::Recovered, ""));
        let sent = owner_notice(frames.recv().await.expect("a frame"));
        assert_eq!(sent.kind(), pb::NoticeKind::Recovered);
    }

    #[tokio::test]
    async fn send_notice_never_blocks_on_a_full_channel() {
        let (inputs, _frames) = mpsc::channel(1);
        assert!(send_notice(&inputs, pb::NoticeKind::Degraded, "first"));
        let queued = tokio::time::timeout(Duration::from_millis(100), async {
            send_notice(&inputs, pb::NoticeKind::Degraded, "second")
        })
        .await
        .expect("send_notice returned at once");
        assert!(!queued);
    }

    #[tokio::test]
    async fn send_notice_tolerates_a_closed_channel() {
        let (inputs, frames) = mpsc::channel(1);
        drop(frames);
        assert!(!send_notice(&inputs, pb::NoticeKind::LoggedOut, ""));
    }
}
