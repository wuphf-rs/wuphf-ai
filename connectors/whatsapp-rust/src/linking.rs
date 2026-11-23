//! Linking the WhatsApp session to the account, at the first start and again
//! after a logout, by the configured login method. The owner is told through a
//! link-required notice — the QR code as a picture, or the pair code — so a
//! connector can be linked again from wherever the owner reads the other
//! connectors; every code also goes to the terminal.
//!
//! Not every code is told: WhatsApp rotates a QR code every 20-60 s and ends a
//! pairing round after about 160 s, and the library then starts another, so a
//! connector nobody links would otherwise post one notice a minute for as long
//! as it runs. The first code is told at once; after that a code is told only
//! once the gap since the last told one has passed ([`GAPS`]), until the
//! session is linked.
//!
//! With a pair code, every round asks for one, so a valid code is always in
//! the terminal; only a told one makes the phone show its prompt, which would
//! otherwise come every few minutes. When WhatsApp asks for a new code (the
//! owner entered one it no longer takes) a new one is asked for, told and
//! prompted at once: the owner is linking right then.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tracing::{debug, info, warn};
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

/// A pair code to ask WhatsApp for.
#[derive(Debug, PartialEq, Eq)]
pub struct PairCodeAsk {
    /// Whether the phone shows its "enter code" prompt for it.
    pub prompt_phone: bool,
    /// When the owner is told the code: the gap before the next told one.
    pub tell: Option<Duration>,
}

/// Prints every pairing code and tells the owner about some of them. Shared
/// by every WhatsApp session the process runs, so the schedule spans the
/// pairing rounds of one logout.
pub struct Linking {
    inputs: mpsc::Sender<pb::ClientFrame>,
    media: MediaClient,
    method: LoginMethod,
    state: Mutex<State>,
}

/// What the owner was told, when a pair code was last asked for, and whether
/// the owner asked for a code now.
#[derive(Debug, Default)]
struct State {
    schedule: Schedule,
    asked: Option<Instant>,
    fresh: bool,
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
            state: Mutex::new(State::default()),
        }
    }

    /// A QR code the library issued, valid for `valid`. Pairing by code shows
    /// none: the QR codes only mark the pairing rounds.
    pub async fn qr(&self, code: String, valid: Duration) {
        if self.method != LoginMethod::Qr {
            debug!("a WhatsApp pairing round is open; waiting for the pair code to be entered");
            return;
        }
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
        let Some(next) = self.state.lock().unwrap().tell_qr(Instant::now()) else {
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

    /// The pair code to ask for now that a pairing round's QR code came, when
    /// pairing by code and the last code asked for has lapsed.
    pub fn pair_code_ask(&self) -> Option<PairCodeAsk> {
        if self.method != LoginMethod::PairCode {
            return None;
        }
        self.state.lock().unwrap().ask(Instant::now())
    }

    /// The pair code to ask for now that WhatsApp asked for a new one.
    pub fn pair_code_refresh(&self) -> Option<PairCodeAsk> {
        if self.method != LoginMethod::PairCode {
            return None;
        }
        Some(self.state.lock().unwrap().refresh(Instant::now()))
    }

    /// A pair code WhatsApp issued for an ask; `tell` is the ask's.
    pub async fn pair_code(&self, code: String, tell: Option<Duration>) {
        println!(
            "WhatsApp pair code (valid for about {} seconds): {code}",
            PAIR_CODE_VALIDITY.as_secs()
        );
        if let Some(next) = tell {
            self.tell(
                pair_code_detail(&code, PAIR_CODE_VALIDITY, next),
                Vec::new(),
            )
            .await;
        }
    }

    /// The owner asked for a code now (the panel's "send a new link code"):
    /// the next one — the next QR code, or a pair code asked for at the next
    /// QR code — is told at once whatever the gap, and a pair code prompts the
    /// phone.
    pub fn fresh_code(&self) {
        self.state.lock().unwrap().fresh = true;
    }

    /// The session is linked: the next logout starts afresh.
    pub fn linked(&self) {
        *self.state.lock().unwrap() = State::default();
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

impl State {
    /// The pair code a round opening at `now` asks for, unless the last one
    /// asked for is still valid.
    fn ask(&mut self, now: Instant) -> Option<PairCodeAsk> {
        if std::mem::take(&mut self.fresh) {
            return Some(self.refresh(now));
        }
        if self
            .asked
            .is_some_and(|asked| now.duration_since(asked) < PAIR_CODE_VALIDITY)
        {
            return None;
        }
        self.asked = Some(now);
        let tell = self.schedule.due(now);
        Some(PairCodeAsk {
            prompt_phone: tell.is_some(),
            tell,
        })
    }

    /// Whether a QR code issued at `now` is told, and if so the gap before the
    /// next told one: at once when the owner asked for one.
    fn tell_qr(&mut self, now: Instant) -> Option<Duration> {
        if std::mem::take(&mut self.fresh) {
            return Some(self.schedule.told(now));
        }
        self.schedule.due(now)
    }

    /// The pair code to ask for at `now` because WhatsApp asked for a new one:
    /// prompted and told whatever the gap.
    fn refresh(&mut self, now: Instant) -> PairCodeAsk {
        self.asked = Some(now);
        PairCodeAsk {
            prompt_phone: true,
            tell: Some(self.schedule.told(now)),
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
        Some(self.told(now))
    }

    /// Record a code told at `now`, whatever the gap, returning the gap before
    /// the next told code.
    fn told(&mut self, now: Instant) -> Duration {
        self.told += 1;
        self.last = Some(now);
        GAPS[(self.told - 1).min(GAPS.len() - 1)]
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

    #[test]
    fn a_fresh_qr_code_is_told_at_once_then_the_gaps_resume() {
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        let mut state = State::default();
        assert_eq!(state.tell_qr(at(0)), Some(GAPS[0]));
        assert_eq!(state.tell_qr(at(20)), None);
        state.fresh = true;
        assert_eq!(state.tell_qr(at(40)), Some(GAPS[1]));
        // Once: the next one waits for the gap again.
        assert_eq!(state.tell_qr(at(60)), None);
    }

    #[test]
    fn a_fresh_pair_code_is_asked_prompted_and_told_at_once() {
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        let mut state = State::default();
        assert!(state.ask(at(0)).is_some());
        // The code holds, so the round's next QR code asks for nothing...
        assert_eq!(state.ask(at(20)), None);
        // ...unless the owner asked for one.
        state.fresh = true;
        assert_eq!(
            state.ask(at(40)),
            Some(PairCodeAsk {
                prompt_phone: true,
                tell: Some(GAPS[1]),
            })
        );
        assert_eq!(state.ask(at(60)), None);
    }

    #[test]
    fn every_round_asks_for_a_pair_code_but_only_a_told_one_prompts() {
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        let mut state = State::default();
        // The first round: told and prompted.
        assert_eq!(
            state.ask(at(0)),
            Some(PairCodeAsk {
                prompt_phone: true,
                tell: Some(GAPS[0]),
            })
        );
        // The round's later QR codes ask for nothing while the code holds.
        assert_eq!(state.ask(at(20)), None);
        assert_eq!(state.ask(at(159)), None);
        // The next round asks again, quietly, inside the gap.
        assert_eq!(
            state.ask(at(160)),
            Some(PairCodeAsk {
                prompt_phone: false,
                tell: None,
            })
        );
        // Past the gap the code is told and prompted again.
        assert_eq!(
            state.ask(at(320)),
            Some(PairCodeAsk {
                prompt_phone: true,
                tell: Some(GAPS[1]),
            })
        );
    }

    #[test]
    fn a_refresh_asks_prompts_and_tells_at_once() {
        let start = Instant::now();
        let mut state = State::default();
        state.ask(start);
        let refreshed = state.refresh(start + Duration::from_secs(30));
        assert_eq!(
            refreshed,
            PairCodeAsk {
                prompt_phone: true,
                tell: Some(GAPS[1]),
            }
        );
        // The refreshed code holds for its own 160 s.
        assert_eq!(state.ask(start + Duration::from_secs(150)), None);
    }

    #[tokio::test]
    async fn a_link_starts_afresh_and_qr_mode_asks_for_no_pair_code() {
        let (inputs, _outputs) = mpsc::channel(1);
        let media = MediaClient::connect("127.0.0.1:1", "token").unwrap();
        let qr_mode = Linking::new(inputs.clone(), media.clone(), LoginMethod::Qr);
        assert_eq!(qr_mode.pair_code_ask(), None);
        assert_eq!(qr_mode.pair_code_refresh(), None);

        let code_mode = Linking::new(inputs, media, LoginMethod::PairCode);
        assert!(code_mode.pair_code_ask().is_some());
        assert!(code_mode.pair_code_ask().is_none());
        code_mode.linked();
        assert!(code_mode.pair_code_ask().is_some());
    }

    #[tokio::test]
    async fn a_told_pair_code_reaches_the_owner() {
        let (inputs, mut outputs) = mpsc::channel(4);
        let media = MediaClient::connect("127.0.0.1:1", "token").unwrap();
        let linking = Linking::new(inputs, media, LoginMethod::PairCode);
        linking.pair_code("ABCD-EFGH".into(), Some(GAPS[0])).await;
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
        // An untold code goes to the terminal only.
        linking.pair_code("WXYZ-1234".into(), None).await;
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
