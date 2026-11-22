//! Linking the WhatsApp session to the account, at the first start and again
//! after a logout. Every QR code and pair code the library issues goes to the
//! terminal. The owner is also told through a link-required notice — the QR
//! code as a picture, or the pair code — so a connector can be linked again
//! from wherever the owner reads the other connectors.
//!
//! Not every code is told: WhatsApp rotates a QR code every 20-60 s and ends a
//! pairing round after about 160 s, and the library then starts another, so a
//! connector nobody links would otherwise post one picture a minute for as
//! long as it runs. The first code is told at once; after that a code is told
//! only once the gap since the last told one has passed ([`GAPS`]), until the
//! session is linked.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tracing::{info, warn};
use wuphf_connector::MediaClient;
use wuphf_proto::connector as pb;

use crate::config::LoginMethod;
use crate::qr_png::qr_png;

/// How long a pair code lasts: WhatsApp ends the pairing round, and with it the
/// code, when the round's QR codes run out, about 160 s after it starts.
const PAIR_CODE_VALIDITY: Duration = Duration::from_secs(160);

/// The gap after the first, second, third… told code before the next one is
/// told; the last repeats.
const GAPS: [Duration; 4] = [
    Duration::from_secs(5 * 60),
    Duration::from_secs(15 * 60),
    Duration::from_secs(30 * 60),
    Duration::from_secs(60 * 60),
];

/// Prints every pairing code and tells the owner about some of them. Shared
/// by every WhatsApp session the process runs, so the schedule spans the
/// pairing rounds of one logout.
pub struct Linking {
    inputs: mpsc::Sender<pb::ClientFrame>,
    media: MediaClient,
    method: LoginMethod,
    schedule: Mutex<Schedule>,
}

impl Linking {
    pub fn new(
        inputs: mpsc::Sender<pb::ClientFrame>,
        media: MediaClient,
        method: LoginMethod,
    ) -> Self {
        Self {
            inputs,
            media,
            method,
            schedule: Mutex::new(Schedule::default()),
        }
    }

    /// A QR code the library issued, valid for `valid`.
    pub async fn qr(&self, code: String, valid: Duration) {
        info!(
            valid_for_seconds = valid.as_secs(),
            "scan the QR code with WhatsApp"
        );
        match qrcode::QrCode::new(code.as_bytes()) {
            Ok(image) => println!(
                "{}",
                image
                    .render::<qrcode::render::unicode::Dense1x2>()
                    .quiet_zone(false)
                    .build()
            ),
            // The raw payload still pairs when pasted into a QR tool.
            Err(_) => println!("QR payload: {code}"),
        }
        if self.method != LoginMethod::Qr {
            return;
        }
        let Some(next) = self.due() else {
            return;
        };
        let media_ids = match self.upload_qr(&code).await {
            Ok(media_id) => vec![media_id],
            Err(error) => {
                warn!(%error, "could not attach the WhatsApp QR code to the owner notice");
                Vec::new()
            }
        };
        let detail = if media_ids.is_empty() {
            qr_unattached_detail(next)
        } else {
            qr_detail(valid, next)
        };
        self.tell(detail, media_ids).await;
    }

    /// Whether this pairing round asks for a pair code, and if so the gap
    /// before the next one. A code is asked for only when the owner will be
    /// told it: each request makes the phone show a prompt, so asking every
    /// round would prompt every few minutes.
    pub fn pair_code_due(&self) -> Option<Duration> {
        (self.method == LoginMethod::PairCode)
            .then(|| self.due())
            .flatten()
    }

    /// A pair code issued for the configured phone number after
    /// [`Self::pair_code_due`]; `next` is the gap it returned.
    pub async fn pair_code(&self, code: String, next: Duration) {
        println!(
            "WhatsApp pair code (valid for about {} seconds): {code}",
            PAIR_CODE_VALIDITY.as_secs()
        );
        self.tell(
            pair_code_detail(&code, PAIR_CODE_VALIDITY, next),
            Vec::new(),
        )
        .await;
    }

    /// The session is linked: the next logout starts the schedule afresh.
    pub fn linked(&self) {
        *self.schedule.lock().unwrap() = Schedule::default();
    }

    /// Whether a code issued now is told, and if so the gap before the next.
    fn due(&self) -> Option<Duration> {
        self.schedule.lock().unwrap().due(Instant::now())
    }

    async fn upload_qr(&self, code: &str) -> anyhow::Result<String> {
        let png = qr_png(code)?;
        let metadata = pb::MediaMetadata {
            kind: pb::attachment::Kind::Image.into(),
            file_name: Some("whatsapp-link.png".into()),
            mime_type: Some("image/png".into()),
            size_bytes: Some(png.len() as u64),
            animated: false,
        };
        Ok(self.media.upload(metadata, png).await?.media_id)
    }

    /// Queue the notice, waiting for room: unlike a health notice, a dropped
    /// one would leave the owner with no way to link the session.
    async fn tell(&self, detail: String, media_ids: Vec<String>) {
        let frame =
            wuphf_connector::notice_with_media(pb::NoticeKind::LinkRequired, detail, media_ids);
        if self.inputs.send(frame).await.is_err() {
            warn!("core session input closed; dropping a WhatsApp link notice");
        }
    }
}

/// When the owner was last told about a code, and how many times.
#[derive(Debug, Default)]
struct Schedule {
    told: usize,
    last: Option<Instant>,
}

impl Schedule {
    /// Whether a code issued at `now` is told (recording it when it is), and
    /// if so the gap before the next told code.
    fn due(&mut self, now: Instant) -> Option<Duration> {
        if let Some(last) = self.last {
            let gap = GAPS[(self.told - 1).min(GAPS.len() - 1)];
            if now.duration_since(last) < gap {
                return None;
            }
        }
        self.told += 1;
        self.last = Some(now);
        Some(GAPS[(self.told - 1).min(GAPS.len() - 1)])
    }
}

fn qr_detail(valid: Duration, next: Duration) -> String {
    format!(
        "scan the attached QR code in WhatsApp → Settings → Linked devices → Link a device. \
         It is valid for {}; if it expires first, a new one follows in about {}.",
        human(valid),
        human(next)
    )
}

fn qr_unattached_detail(next: Duration) -> String {
    format!(
        "the QR code to scan is in the connector's terminal (it could not be attached here); \
         a new one follows in about {}.",
        human(next)
    )
}

fn pair_code_detail(code: &str, valid: Duration, next: Duration) -> String {
    format!(
        "in WhatsApp → Settings → Linked devices → Link a device → Link with phone number \
         instead, enter {code}. It is valid for {}; if it expires first, a new one follows in \
         about {}.",
        human(valid),
        human(next)
    )
}

/// A duration as the owner reads it: seconds under two minutes, minutes under
/// an hour, then hours.
fn human(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0..120 => format!("{seconds} s"),
        120..3600 => format!("{} min", seconds / 60),
        _ => format!("{} h", seconds / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_told_with_growing_gaps() {
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        let mut schedule = Schedule::default();
        // The first code at once, then nothing for five minutes.
        assert_eq!(schedule.due(at(0)), Some(GAPS[0]));
        assert_eq!(schedule.due(at(20)), None);
        assert_eq!(schedule.due(at(299)), None);
        assert_eq!(schedule.due(at(300)), Some(GAPS[1]));
        // Then fifteen minutes, thirty, and an hour from then on.
        assert_eq!(schedule.due(at(300 + 899)), None);
        assert_eq!(schedule.due(at(300 + 900)), Some(GAPS[2]));
        assert_eq!(schedule.due(at(1200 + 1800)), Some(GAPS[3]));
        assert_eq!(schedule.due(at(3000 + 3600)), Some(GAPS[3]));
        assert_eq!(schedule.due(at(6600 + 3599)), None);
        assert_eq!(schedule.due(at(6600 + 3600)), Some(GAPS[3]));
    }

    #[tokio::test]
    async fn a_link_starts_the_schedule_afresh() {
        let (inputs, _outputs) = mpsc::channel(1);
        let media = MediaClient::connect("127.0.0.1:1", "token").unwrap();
        let linking = Linking::new(inputs, media, LoginMethod::Qr);
        assert!(linking.due().is_some());
        assert!(linking.due().is_none());
        linking.linked();
        assert!(linking.due().is_some());
    }

    #[tokio::test]
    async fn pair_codes_are_asked_for_only_when_told() {
        let (inputs, mut outputs) = mpsc::channel(4);
        let media = MediaClient::connect("127.0.0.1:1", "token").unwrap();
        let qr_mode = Linking::new(inputs.clone(), media.clone(), LoginMethod::Qr);
        assert_eq!(qr_mode.pair_code_due(), None);

        let code_mode = Linking::new(inputs, media, LoginMethod::PairCode);
        let next = code_mode
            .pair_code_due()
            .expect("the first round asks for a code");
        assert_eq!(next, GAPS[0]);
        // A second round inside the gap asks for none.
        assert_eq!(code_mode.pair_code_due(), None);
        code_mode.pair_code("ABCD-EFGH".into(), next).await;
        let Some(pb::client_frame::Body::OwnerNotice(notice)) = outputs.try_recv().unwrap().body
        else {
            panic!("expected an owner notice");
        };
        assert_eq!(notice.kind(), pb::NoticeKind::LinkRequired);
        assert_eq!(
            notice.detail,
            "in WhatsApp → Settings → Linked devices → Link a device → Link with phone number \
             instead, enter ABCD-EFGH. It is valid for 2 min; if it expires first, a new one \
             follows in about 5 min."
        );
        assert!(notice.media_ids.is_empty());
        assert!(outputs.try_recv().is_err());
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(human(Duration::from_secs(60)), "60 s");
        assert_eq!(human(Duration::from_secs(300)), "5 min");
        assert_eq!(human(Duration::from_secs(3599)), "59 min");
        assert_eq!(human(Duration::from_secs(3600)), "1 h");
        assert_eq!(human(Duration::from_secs(7200)), "2 h");
        assert!(qr_detail(Duration::from_secs(60), GAPS[0]).contains("valid for 60 s"));
    }
}
