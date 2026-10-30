//! WhatsApp connector for wuphf, built on the pure-Rust whatsapp-rust client.
//! The connector is a thin pipe: it normalizes inbound WhatsApp messages into
//! core events and executes dispatch commands; all routing decisions live in
//! the core.

mod avatars;
mod chat_state;
mod chats;
mod config;
mod contacts;
mod device;
mod disappearing;
mod groups;
mod health;
mod media;
mod names;
mod normalize;
mod receipts;
mod sender;
mod supervise;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use clap::Parser;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
use whatsapp_rust::pair_code::PairCodeOptions;
use whatsapp_rust::prelude::*;

use crate::avatars::Avatars;
use crate::chats::ChatEvents;
use crate::config::Config;
use crate::contacts::Contacts;
use crate::disappearing::TimerFallback;
use crate::groups::GroupDirectory;
use crate::health::Health;
use crate::names::ChatTitles;
use crate::normalize::Normalizer;
use crate::receipts::Receipts;
use crate::sender::WhatsAppSender;
use crate::supervise::{Stop, supervise};

/// Buffers normalized inbound events ahead of the core session so a core
/// reconnect applies backpressure instead of losing messages.
const EVENT_CAPACITY: usize = 64;

#[derive(Parser)]
#[command(about = "WhatsApp connector for wuphf (whatsapp-rust)")]
struct Cli {
    /// Path to the connector TOML configuration.
    #[arg(long, default_value = "config.example.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = Config::load(&cli.config)?;
    init_logging(&config.log_level);

    // The token comes from the environment only, so it stays out of the config
    // file and the process list.
    let core_token = std::env::var(&config.core_token_env)
        .ok()
        .filter(|token| !token.is_empty())
        .with_context(|| {
            format!(
                "environment variable {} (core_token_env) is unset or empty",
                config.core_token_env
            )
        })?;

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
    // One core media client stores inbound attachments and chat pictures.
    let media = wuphf_connector::MediaClient::connect(&config.core_address, &core_token)
        .context("prepare the core media client")?;
    let avatars = Arc::new(Avatars::new(media.clone()));
    let (inputs_tx, inputs_rx) = mpsc::channel(EVENT_CAPACITY);
    // One group directory serves the namer (subjects), the first-sighting
    // report (description, members) and the sender (timers).
    let groups = Arc::new(GroupDirectory::new());
    let titles = Arc::new(ChatTitles::new(Arc::clone(&groups), contacts));
    let normalizer = Normalizer::new(
        inputs_tx.clone(),
        Arc::clone(&titles),
        Arc::clone(&avatars),
        media,
    );
    let chat_events = ChatEvents::new(inputs_tx.clone(), titles, avatars);
    let health = Health::new(inputs_tx.clone());
    let receipts = Receipts::new(inputs_tx.clone());

    let (profile, device_props) = device::client_identity(config.client_mode);
    warn!(
        client_mode = ?config.client_mode,
        "registering the WhatsApp session as an Android client (experimental)"
    );
    let mut builder = Bot::builder()
        .with_backend(store)
        .with_device_props(device_props)
        .on_qr_code(|code, timeout| async move {
            info!(
                valid_for_seconds = timeout.as_secs(),
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
        })
        .on_pair_code(|code, timeout| async move {
            println!(
                "WhatsApp pair code (valid for {} seconds): {code}",
                timeout.as_secs()
            );
        })
        .on_connected(|_client| async {
            info!("connected to WhatsApp");
        })
        .on_logged_out(|info| async move {
            error!(
                ?info,
                "logged out of WhatsApp; delete the session database and restart to pair again"
            );
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
        .on_event_for(&[EventKind::Receipt], move |event, _client| {
            let receipts = receipts.clone();
            async move { receipts.handle(event).await }
        });
    if let Some(phone_number) = config.phone_number.clone() {
        builder = builder.with_pair_code(PairCodeOptions::for_phone(phone_number));
    }
    let bot = builder.build().await.context("build WhatsApp bot")?;
    // The profile is in-memory only, and the library reads it for every login
    // and registration payload: set once before the first connect, it holds
    // for every reconnect of this process.
    bot.client().set_client_profile(profile).await;
    let mut handle = bot.spawn();

    let core_client = wuphf_connector::Client::new(wuphf_connector::Options {
        core_address: config.core_address.clone(),
        connector_id: config.connector_id.clone(),
        platform: wuphf_proto::connector::Platform::Whatsapp,
        // WhatsApp has no threads/topics; the core may only use this connector
        // as a single target or an identity source, never as a threaded mirror.
        supports_threads: false,
        supports_topic_create: false,
        supports_disappearing_timers: config.supports_disappearing_timers,
        token: core_token,
        is_permanent: None,
    });
    // With the flag off the sender stamps only what the core sends: no group
    // lookups, no owner notices.
    let fallback = config
        .supports_disappearing_timers
        .then(|| TimerFallback::new(Arc::clone(&groups), inputs_tx.clone()));
    let cancel = CancellationToken::new();
    let session = core_client.run(
        WhatsAppSender::new(handle.client(), fallback),
        inputs_rx,
        cancel.clone(),
    );
    tokio::pin!(session);

    let shutdown = async {
        // A failure to listen for Ctrl-C leaves the connector to its session.
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    let stop = supervise(&mut session, &mut handle, shutdown, || {
        error!(
            "the WhatsApp client stopped and will not reconnect (the owner was sent a notice); \
             the connector stays up so the notice reaches the core: fix the session and restart it"
        );
    })
    .await;
    match stop {
        Stop::SessionEnded(result) => result?,
        Stop::Shutdown => {
            info!("shutting down");
            cancel.cancel();
            session.await?;
        }
    }
    handle.shutdown().await;
    Ok(())
}

/// WUPHF_LOG_LEVEL overrides the configured level, matching the Go connectors.
fn init_logging(level: &str) {
    let filter =
        EnvFilter::try_from_env("WUPHF_LOG_LEVEL").unwrap_or_else(|_| EnvFilter::new(level));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
