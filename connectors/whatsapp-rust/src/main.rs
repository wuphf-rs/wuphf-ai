//! WhatsApp connector for wuphf, built on the pure-Rust whatsapp-rust client.
//! The connector is a thin pipe: it normalizes inbound WhatsApp messages into
//! core events and executes dispatch commands; all routing decisions live in
//! the core.

mod addressing;
mod avatars;
mod chat_state;
mod chats;
mod commands;
mod config;
mod contacts;
mod device;
mod disappearing;
mod groups;
mod health;
mod identity;
mod link;
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
mod setup;
mod stickermeta;
mod supervise;
#[cfg(test)]
mod testing;
mod wa_client;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};
use wuphf_connector::settings::Connection;

use crate::addressing::AddressBook;
use crate::avatars::Avatars;
use crate::commands::WhatsAppCommands;
use crate::config::{Config, Runtime};
use crate::disappearing::TimerFallback;
use crate::groups::GroupDirectory;
use crate::identity::Identity;
use crate::linking::Linking;
use crate::sender::WhatsAppSender;
use crate::session::{CurrentResync, LinkedClient, Shared};
use crate::supervise::{Stop, supervise};
use crate::wa_client::LiveClient;

/// Buffers normalized inbound events ahead of the core session so a core
/// reconnect applies backpressure instead of losing messages.
const EVENT_CAPACITY: usize = 64;

#[derive(Parser)]
#[command(
    name = "wuphf-whatsapp-rust",
    version,
    about = "WhatsApp connector for wuphf (whatsapp-rust)"
)]
struct Cli {
    /// Path to the connector TOML configuration.
    #[arg(long, default_value = "config.example.toml")]
    config: PathBuf,
    /// Fetch every WhatsApp app-state collection (contacts, pin, mute,
    /// archive, settings) in full again once connected, as if newly linked.
    #[arg(long)]
    resync_app_state: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Edit the config file and the env file its options name (tokens can be
    /// generated) on a setup screen, every option with its description.
    /// Works while the connector is down.
    Setup {
        #[arg(long, default_value = "config.local.toml")]
        config: PathBuf,
        #[arg(long, default_value = ".env.local")]
        env_file: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Some(Command::Setup { config, env_file }) = cli.command {
        return setup::run(config, env_file);
    }
    let config = Config::load(&cli.config)?;
    // Held while the connector runs, so setup's actions never use the session
    // at the same time.
    let _session_lock = wuphf_connector::session_lock::SessionLock::acquire(std::path::Path::new(
        &config.session_database,
    ))
    .context(
        "lock the WhatsApp session (is another wuphf-whatsapp-rust, or its setup, using it?)",
    )?;
    let log = wuphf_connector::logging::init(&config.runtime.log_level);

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

    // The runtime settings are declared to the core, which keeps every change
    // made from its settings panel or `wuphf settings` and pushes it back; the
    // file's values, defaults filled in, are the base.
    let settings = Arc::new(
        Connection::connect(
            &config.core_address,
            &core_token,
            &config.connector_id,
            config.runtime.clone().with_defaults(),
        )
        .context("declare the runtime settings")?,
    );

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
        // Read per conversion, so a changed timeout applies to the next one.
        timeout: Arc::new({
            let live = Arc::clone(&settings.live);
            move || live.load().media_convert_timeout()
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
    let linking = Arc::new(Linking::new(
        inputs_tx.clone(),
        media.clone(),
        config.login_method(),
    ));
    let resync = CurrentResync::default();

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
        settings: Some(settings.declaration.clone()),
        // The panel's buttons on this connector's section.
        commands: Some(Arc::new(WhatsAppCommands::new(
            Arc::clone(&linking),
            linked.clone(),
            resync.clone(),
        ))),
    });
    // With the flag off the sender stamps only what the core sends: no group
    // lookups, no owner notices.
    let fallback = config
        .supports_disappearing_timers
        .then(|| TimerFallback::new(inputs_tx.clone()));
    let cancel = CancellationToken::new();
    tokio::spawn({
        let settings = Arc::clone(&settings);
        let cancel = cancel.clone();
        async move {
            settings
                .follow(cancel, move |old: &Runtime, new: &Runtime| {
                    if old.log_level != new.log_level {
                        log.set(&new.log_level);
                    }
                })
                .await;
        }
    });
    // The core session lives as long as the process; the WhatsApp session
    // below is replaced after a logout, and the sender always reaches the
    // current one.
    let session = core_client.run(
        WhatsAppSender::new(
            LiveClient::new(linked.clone(), Arc::clone(&groups)),
            media.clone(),
            converter,
            Arc::clone(&settings.live),
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
        linking,
        avatars: Arc::new(Avatars::new(media.clone())),
        config,
        inputs: inputs_tx,
        media,
        groups,
        linked,
        resync_all: AtomicBool::new(cli.resync_app_state),
        resync,
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
