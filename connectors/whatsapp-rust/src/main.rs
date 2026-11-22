//! WhatsApp connector for wuphf, built on the pure-Rust whatsapp-rust client.
//! The connector is a thin pipe: it normalizes inbound WhatsApp messages into
//! core events and executes dispatch commands; all routing decisions live in
//! the core.

mod addressing;
mod avatars;
mod chat_state;
mod chats;
mod config;
mod contacts;
mod device;
mod disappearing;
mod groups;
mod health;
mod identity;
mod linking;
mod media;
mod names;
mod normalize;
mod outbound;
mod qr_png;
mod receipts;
mod resync;
mod sender;
mod session;
mod stickermeta;
mod supervise;
#[cfg(test)]
mod testing;
mod wa_client;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::Context as _;
use clap::Parser;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use crate::addressing::AddressBook;
use crate::avatars::Avatars;
use crate::config::Config;
use crate::disappearing::TimerFallback;
use crate::groups::GroupDirectory;
use crate::identity::Identity;
use crate::linking::Linking;
use crate::sender::WhatsAppSender;
use crate::session::{LinkedClient, Shared};
use crate::supervise::{Stop, supervise};
use crate::wa_client::LiveClient;

/// Buffers normalized inbound events ahead of the core session so a core
/// reconnect applies backpressure instead of losing messages.
const EVENT_CAPACITY: usize = 64;

#[derive(Parser)]
#[command(about = "WhatsApp connector for wuphf (whatsapp-rust)")]
struct Cli {
    /// Path to the connector TOML configuration.
    #[arg(long, default_value = "config.example.toml")]
    config: PathBuf,
    /// Fetch every WhatsApp app-state collection (contacts, pin, mute,
    /// archive, settings) in full again once connected, as if newly linked.
    #[arg(long)]
    resync_app_state: bool,
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

    let media = wuphf_connector::MediaClient::connect(&config.core_address, &core_token)
        .context("prepare the core media client")?;
    // Animated stickers from other platforms are converted to WebP by the
    // core's media.convert provider; the invoke token, when the core sets one,
    // also comes from the environment only.
    let functions_token = config
        .functions_token_env
        .as_deref()
        .and_then(|name| std::env::var(name).ok());
    let functions = wuphf_connector::FunctionsClient::connect(
        &config.core_address,
        functions_token.as_deref(),
        &config.connector_id,
    )
    .context("prepare the core functions client")?;
    let converter = wuphf_connector::MediaConverter {
        functions,
        media: media.clone(),
        timeout: Arc::new({
            let runtime = config.runtime.clone();
            move || runtime.media_convert_timeout()
        }),
    };
    let (inputs_tx, inputs_rx) = mpsc::channel(EVENT_CAPACITY);
    let groups = Arc::new(GroupDirectory::new());
    let linked = LinkedClient::default();
    // One identity resolver and address book for the process: the sessions
    // name ids canonically and record each chat's form; the sender puts them
    // back into that form.
    let identity = Arc::new(Identity::default());
    let addresses = Arc::new(AddressBook::default());

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
        .then(|| TimerFallback::new(inputs_tx.clone()));
    let cancel = CancellationToken::new();
    // The core session lives as long as the process; the WhatsApp session
    // below is replaced after a logout, and the sender always reaches the
    // current one.
    let session = core_client.run(
        WhatsAppSender::new(
            LiveClient::new(linked.clone(), Arc::clone(&groups)),
            media.clone(),
            converter,
            config.runtime.clone(),
            fallback,
            Arc::clone(&identity),
            Arc::clone(&addresses),
        ),
        inputs_rx,
        cancel.clone(),
    );
    tokio::pin!(session);

    let whatsapp_cancel = CancellationToken::new();
    let shared = Shared {
        linking: Arc::new(Linking::new(
            inputs_tx.clone(),
            media.clone(),
            config.login_method(),
        )),
        avatars: Arc::new(Avatars::new(media.clone())),
        config,
        inputs: inputs_tx,
        media,
        groups,
        linked,
        resync_all: AtomicBool::new(cli.resync_app_state),
        identity,
        addresses,
        migrations: Arc::default(),
    };
    let mut whatsapp = tokio::spawn({
        let cancel = whatsapp_cancel.clone();
        async move {
            if let Err(error) = session::run(shared, cancel).await {
                error!(%error, "the WhatsApp session could not run");
            }
        }
    });

    let shutdown = async {
        // A failure to listen for Ctrl-C leaves the connector to its session.
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    let stop = supervise(&mut session, &mut whatsapp, shutdown, || {
        error!(
            "the WhatsApp client stopped and will not reconnect (the owner was sent a notice); \
             the connector stays up so the notice reaches the core: fix the session and restart it"
        );
    })
    .await;
    let result = match stop {
        Stop::SessionEnded(result) => result,
        Stop::Shutdown => {
            info!("shutting down");
            cancel.cancel();
            session.await
        }
    };
    // The WhatsApp session shuts its client down cleanly. supervise may have
    // seen the task end already; a finished task is not awaited twice.
    whatsapp_cancel.cancel();
    if !whatsapp.is_finished() {
        let _ = whatsapp.await;
    }
    result?;
    Ok(())
}

/// WUPHF_LOG_LEVEL overrides the configured level, matching the Go connectors.
fn init_logging(level: &str) {
    let filter =
        EnvFilter::try_from_env("WUPHF_LOG_LEVEL").unwrap_or_else(|_| EnvFilter::new(level));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
