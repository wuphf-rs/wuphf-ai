//! The WhatsApp session: one linked device, replaced after a logout. The core
//! session outlives it (see `main.rs`); this module opens the session store,
//! builds the event handlers and the bot, and runs it.
//!
//! When WhatsApp logs the device out (the phone unlinked it, or the server
//! rejected it) the library stops reconnecting and keeps the dead device in
//! the store. The store's files are then moved aside — kept next to it, never
//! deleted — and a new session links at the same path by the configured login
//! method, its codes reaching the owner through [`Linking`]. Any other end of
//! the client (another client replacing it, a ban, an outdated client) is left
//! alone: linking again would not help, and the owner has been told.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use whatsapp_rust::client::Client;
use whatsapp_rust::pair_code::PairCodeOptions;
use whatsapp_rust::prelude::*;
use whatsapp_rust::types::events::Event;
use wuphf_connector::{Failure, MediaClient};
use wuphf_proto::connector as pb;

use crate::avatars::Avatars;
use crate::chats::ChatEvents;
use crate::config::Config;
use crate::contacts::Contacts;
use crate::device;
use crate::groups::GroupDirectory;
use crate::health::Health;
use crate::identity::{Identity, MigrationGate};
use crate::linking::{Linking, PairCodeAsk};
use crate::names::ChatTitles;
use crate::normalize::Normalizer;
use crate::receipts::Receipts;
use crate::resync::Resync;

/// The client of the session that is running, for the sender: empty while a
/// new session is being set up after a logout. Cheap to clone.
#[derive(Clone, Default)]
pub struct LinkedClient(Arc<RwLock<Option<Arc<Client>>>>);

impl LinkedClient {
    /// The running session's client. Without one a dispatch fails as
    /// transient, so the core delivers it once the new session is linked.
    pub fn get(&self) -> Result<Arc<Client>, Failure> {
        self.0.read().unwrap().clone().ok_or_else(|| {
            Failure::Transient(anyhow::anyhow!(
                "WhatsApp is not linked: the connector is waiting to be linked again"
            ))
        })
    }

    fn set(&self, client: Option<Arc<Client>>) {
        *self.0.write().unwrap() = client;
    }
}

/// What every session of the process shares.
pub struct Shared {
    pub config: Config,
    pub inputs: mpsc::Sender<pb::ClientFrame>,
    /// One core media client stores inbound attachments, chat pictures and
    /// pairing QR codes.
    pub media: MediaClient,
    pub avatars: Arc<Avatars>,
    /// One group directory serves the namer (subjects), the first-sighting
    /// report (description, members) and the sender (timers).
    pub groups: Arc<GroupDirectory>,
    pub linking: Arc<Linking>,
    pub linked: LinkedClient,
    /// `--resync-app-state`: the first session resyncs every app-state
    /// collection, then this is cleared.
    pub resync_all: AtomicBool,
    /// Canonical ids for every frame the sessions send, and the identity
    /// migrations already reported; process-wide, as a mapping outlives a
    /// session.
    pub identity: Arc<Identity>,
    pub migrations: Arc<MigrationGate>,
}

/// How long a session whose client stopped waits for the logout handler before
/// taking the stop as final.
const LOGOUT_GRACE: Duration = Duration::from_secs(5);

/// How one session ended.
enum End {
    /// WhatsApp logged the device out: link a new one.
    LoggedOut,
    /// The client stopped for good for another reason.
    Stopped,
    /// The connector is shutting down.
    Cancelled,
}

/// Run sessions until `cancel` fires, or until a client stops for good for a
/// reason other than a logout (the connector then stays up without one, as
/// before linking again existed). An error is a session that could not be set
/// up at all (its store would not open).
pub async fn run(shared: Shared, cancel: CancellationToken) -> anyhow::Result<()> {
    loop {
        match run_once(&shared, &cancel).await? {
            End::LoggedOut => {
                let path = Path::new(&shared.config.session_database);
                let moved = move_aside(path, unix_seconds()).with_context(|| {
                    format!("move the logged-out session {} aside", path.display())
                })?;
                warn!(
                    kept_at = %moved.display(),
                    "WhatsApp logged this device out; its session was moved aside and a new link starts"
                );
            }
            End::Stopped | End::Cancelled => return Ok(()),
        }
    }
}

async fn run_once(shared: &Shared, cancel: &CancellationToken) -> anyhow::Result<End> {
    let config = &shared.config;
    let store = SqliteStore::new(&config.session_database)
        .await
        .context("open WhatsApp session store")?;
    // The connector's contact names live in the session store's file, through
    // its shared pool.
    let contacts = Arc::new(
        Contacts::open(store.shared())
            .await
            .context("prepare the WhatsApp contacts table")?,
    );
    let inputs = &shared.inputs;
    let titles = Arc::new(ChatTitles::new(Arc::clone(&shared.groups), contacts));
    let normalizer = Normalizer::new(
        inputs.clone(),
        Arc::clone(&titles),
        Arc::clone(&shared.avatars),
        shared.media.clone(),
        Arc::clone(&shared.identity),
        Arc::clone(&shared.migrations),
    );
    let chat_events = ChatEvents::new(
        inputs.clone(),
        titles,
        Arc::clone(&shared.avatars),
        Arc::clone(&shared.identity),
    );
    let health = Health::new(inputs.clone());
    let receipts = Receipts::new(inputs.clone(), Arc::clone(&shared.identity));
    let resync = Resync::open(
        store.shared(),
        shared.resync_all.swap(false, Ordering::SeqCst),
    )
    .await
    .context("prepare the WhatsApp app-state seeds table")?;
    let paired = resync.clone();
    let logged_out = Arc::new(Notify::new());

    let (profile, device_props) = device::client_identity(config.client_mode);
    warn!(
        client_mode = ?config.client_mode,
        login_method = ?config.login_method(),
        "registering the WhatsApp session as an Android client (experimental)"
    );
    let (qr_linking, connected_linking) =
        (Arc::clone(&shared.linking), Arc::clone(&shared.linking));
    let pair_code_phone = config.pair_code_phone().map(str::to_owned);
    let on_logout = Arc::clone(&logged_out);
    let builder = Bot::builder()
        .with_backend(store)
        .with_device_props(device_props)
        // Every pairing round's QR codes, and in pair-code mode the round's pair
        // code: the library asks for a pair code only once per bot, so a round
        // after the first would offer none. When WhatsApp asks for a new code
        // (one entered that it no longer takes), a new one at once. Off the
        // event path, since asking waits on WhatsApp.
        .on_event_for(
            &[EventKind::PairingQrCode, EventKind::PairingCodeRefresh],
            move |event, client| {
                let linking = Arc::clone(&qr_linking);
                let phone = pair_code_phone.clone();
                async move {
                    tokio::spawn(async move {
                        let ask = match &*event {
                            Event::PairingQrCode(qr) => {
                                linking.qr(qr.code.clone(), qr.timeout).await;
                                linking.pair_code_ask()
                            }
                            Event::PairingCodeRefresh(_) => linking.pair_code_refresh(),
                            _ => None,
                        };
                        if let (Some(phone), Some(ask)) = (phone, ask) {
                            ask_pair_code(&client, &linking, phone, ask).await;
                        }
                    });
                }
            },
        )
        .on_connected(move |client| {
            connected_linking.linked();
            resync.connected(client);
            async {
                info!("connected to WhatsApp");
            }
        })
        // A link made here syncs every collection in full, through these
        // handlers: nothing is left to resync.
        .on_event_for(&[EventKind::PairSuccess], move |_event, _client| {
            let paired = paired.clone();
            async move { paired.linked().await }
        })
        .on_logged_out(move |info| {
            on_logout.notify_one();
            async move {
                error!(?info, "logged out of WhatsApp; linking a new session");
            }
        })
        .on_message(move |ctx| {
            let normalizer = normalizer.clone();
            async move { normalizer.handle(ctx).await }
        })
        // Chat profiles (titles, descriptions, members, pictures, timers, pin /
        // mute / archive) come from group notifications, picture changes,
        // app-state syncs and history syncs. DisappearingModeChanged is
        // deliberately not handled: a contact's account-wide default applies
        // only to chats started after it changes.
        .on_event_for(
            &[
                EventKind::GroupUpdate,
                EventKind::PictureUpdate,
                EventKind::ContactUpdate,
                EventKind::PinUpdate,
                EventKind::MuteUpdate,
                EventKind::ArchiveUpdate,
                EventKind::HistorySync,
            ],
            move |event, client| {
                let chat_events = chat_events.clone();
                async move { chat_events.handle(event, client).await }
            },
        )
        // Session loss and recovery become owner notices, like the Go
        // connector's.
        .on_event_for(
            &[
                EventKind::LoggedOut,
                EventKind::StreamReplaced,
                EventKind::ClientOutdated,
                EventKind::TemporaryBan,
                EventKind::Connected,
            ],
            move |event, _client| {
                let health = health.clone();
                async move { health.handle(event).await }
            },
        )
        // Delivered and read receipts in direct chats mark the mirror copies.
        .on_event_for(&[EventKind::Receipt], move |event, client| {
            let receipts = receipts.clone();
            async move { receipts.handle(event, &client).await }
        });
    let bot = builder.build().await.context("build WhatsApp bot")?;
    // The profile is in-memory only, and the library reads it for every login
    // and registration payload: set once before the first connect, it holds
    // for every reconnect of this session.
    bot.client().set_client_profile(profile).await;
    let mut handle = bot.spawn();
    shared.linked.set(Some(handle.client()));

    let end = tokio::select! {
        _ = logged_out.notified() => End::LoggedOut,
        _ = &mut handle => {
            // A logout disconnects the client, so its run can end before the
            // logout handler has run: give the handler a moment to say so.
            let logout = tokio::time::timeout(LOGOUT_GRACE, logged_out.notified()).await;
            if logout.is_err() {
                return Ok(End::Stopped);
            }
            shared.linked.set(None);
            return Ok(End::LoggedOut);
        }
        _ = cancel.cancelled() => End::Cancelled,
    };
    if let End::LoggedOut = end {
        shared.linked.set(None);
    }
    handle.shutdown().await;
    Ok(end)
}

/// Ask WhatsApp for a pair code for `phone` and hand it to `linking`.
async fn ask_pair_code(client: &Arc<Client>, linking: &Linking, phone: String, ask: PairCodeAsk) {
    let options = PairCodeOptions {
        show_push_notification: ask.prompt_phone,
        ..PairCodeOptions::for_phone(phone)
    };
    match client.pair_with_code(options).await {
        Ok(code) => linking.pair_code(code, ask.tell).await,
        Err(error) => warn!(%error, "could not get a WhatsApp pair code"),
    }
}

/// Rename the session database at `path` and its `-wal` / `-shm` files with
/// `.logged-out-<at>` before the extension (`session.db` becomes
/// `session.logged-out-<at>.db`, so ignore rules for `*.db` still match),
/// returning the database's new path. Files that do not exist are skipped.
fn move_aside(path: &Path, at: u64) -> std::io::Result<PathBuf> {
    let mut name = path.file_stem().unwrap_or_default().to_os_string();
    name.push(format!(".logged-out-{at}"));
    if let Some(extension) = path.extension() {
        name.push(".");
        name.push(extension);
    }
    let kept = path.with_file_name(name);
    for suffix in ["", "-wal", "-shm"] {
        let from = PathBuf::from(format!("{}{suffix}", path.display()));
        if from.exists() {
            std::fs::rename(&from, format!("{}{suffix}", kept.display()))?;
        }
    }
    Ok(kept)
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_logged_out_session_is_kept_beside_the_new_one() {
        let dir = std::env::temp_dir().join(format!(
            "wuphf-move-aside-{}-{}",
            std::process::id(),
            unix_seconds()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("session.db");
        std::fs::write(&db, b"db").unwrap();
        std::fs::write(dir.join("session.db-wal"), b"wal").unwrap();

        let kept = move_aside(&db, 7).unwrap();
        assert_eq!(kept, dir.join("session.logged-out-7.db"));
        assert_eq!(std::fs::read(&kept).unwrap(), b"db");
        assert_eq!(
            std::fs::read(dir.join("session.logged-out-7.db-wal")).unwrap(),
            b"wal"
        );
        // No -shm existed; the path is free for the new session.
        assert!(!dir.join("session.logged-out-7.db-shm").exists());
        assert!(!db.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_dispatch_without_a_linked_session_is_retried() {
        let linked = LinkedClient::default();
        let Err(failure) = linked.get() else {
            panic!("no session is linked");
        };
        assert!(!failure.is_permanent());
        assert!(failure.to_string().contains("not linked"), "{failure}");
    }
}
