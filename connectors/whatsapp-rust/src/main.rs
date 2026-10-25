//! WhatsApp connector for wuphf, built on the pure-Rust whatsapp-rust client.
//! The connector is a thin pipe: it normalizes inbound WhatsApp messages into
//! core events and executes dispatch commands; all routing decisions live in
//! the core.

mod config;
mod normalize;
mod sender;

use std::path::PathBuf;

use anyhow::Context as _;
use clap::Parser;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;
use whatsapp_rust::pair_code::PairCodeOptions;
use whatsapp_rust::prelude::*;

use crate::config::Config;
use crate::normalize::Normalizer;
use crate::sender::WhatsAppSender;

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

    let store = SqliteStore::new(&config.session_database)
        .await
        .context("open WhatsApp session store")?;
    let (inputs_tx, inputs_rx) = mpsc::channel(EVENT_CAPACITY);
    let normalizer = Normalizer::new(inputs_tx);

    let mut builder = Bot::builder()
        .with_backend(store)
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
        });
    if let Some(phone_number) = config.phone_number.clone() {
        builder = builder.with_pair_code(PairCodeOptions::for_phone(phone_number));
    }
    let bot = builder.build().await.context("build WhatsApp bot")?;
    let mut handle = bot.spawn();

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
    let core_client = wuphf_connector::Client::new(wuphf_connector::Options {
        core_address: config.core_address.clone(),
        connector_id: config.connector_id.clone(),
        platform: wuphf_proto::connector::Platform::Whatsapp,
        // WhatsApp has no threads/topics; the core may only use this connector
        // as a single target or an identity source, never as a threaded mirror.
        supports_threads: false,
        supports_topic_create: false,
        token: core_token,
        is_permanent: None,
    });
    let cancel = CancellationToken::new();
    let session = core_client.run(
        WhatsAppSender::new(handle.client()),
        inputs_rx,
        cancel.clone(),
    );
    tokio::pin!(session);

    tokio::select! {
        result = &mut session => result?,
        _ = tokio::signal::ctrl_c() => {
            info!("shutting down");
            cancel.cancel();
            session.await?;
        }
        _ = &mut handle => anyhow::bail!("WhatsApp client stopped unexpectedly"),
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
