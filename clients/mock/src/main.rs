//! Mock connector executable for manually exercising core protocol behavior.

use anyhow::{bail, Context};
use clap::Parser;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;
use wuphf_proto::v1::{
    connector_client::ConnectorClient, connector_to_core, core_to_connector, ConnectorToCore,
    CreateMessage, DispatchFailed, DispatchSucceeded, Register,
};

#[derive(Parser)]
/// Command-line options for one mock connector session and optional synthetic event.
struct Args {
    /// Core gRPC server URI.
    #[arg(long, default_value = "http://127.0.0.1:50051")]
    server: String,
    /// Configured connector identity to register.
    #[arg(long)]
    connector: String,
    /// Shared token configured for connector registration.
    #[arg(long)]
    token: String,
    /// Origin endpoint ID used for optional submitted message.
    #[arg(long)]
    endpoint: String,
    /// Optional text content for a synthetic inbound create-message event.
    #[arg(long)]
    send: Option<String>,
    /// Stable event ID override for submission and deduplication testing.
    #[arg(long)]
    event_id: Option<String>,
    /// Origin-native platform message ID override for submission.
    #[arg(long)]
    platform_message_id: Option<String>,
    /// Optional origin-native parent platform message ID for reply submission.
    #[arg(long)]
    reply_to: Option<String>,
    /// Leaves received dispatches unacknowledged to demonstrate durable replay.
    #[arg(long)]
    no_ack: bool,
    /// Stable target-native message ID to include in acknowledgements.
    #[arg(long)]
    ack_platform_id: Option<String>,
    /// Reports received dispatches as failed using this reason.
    #[arg(long)]
    fail_reason: Option<String>,
    /// Marks a reported failure terminal rather than transient.
    #[arg(long, requires = "fail_reason")]
    permanent_failure: bool,
}

#[tokio::main]
/// Registers mock connector, optionally submits one event, and handles dispatch commands.
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut client = ConnectorClient::connect(args.server.clone())
        .await
        .with_context(|| format!("connect to {}", args.server))?;
    let (sender, receiver) = mpsc::channel(32);
    sender
        .send(ConnectorToCore {
            operation: Some(connector_to_core::Operation::Register(Register {
                connector_id: args.connector.clone(),
                shared_token: args.token.clone(),
            })),
        })
        .await?;
    if let Some(text) = args.send {
        sender
            .send(ConnectorToCore {
                operation: Some(connector_to_core::Operation::CreateMessage(CreateMessage {
                    event_id: args
                        .event_id
                        .unwrap_or_else(|| format!("event-{}", Uuid::new_v4())),
                    endpoint_id: args.endpoint,
                    platform_message_id: args
                        .platform_message_id
                        .unwrap_or_else(|| format!("origin-{}", Uuid::new_v4())),
                    sender_id: "mock-user".into(),
                    sender_display_name: "Mock User".into(),
                    sent_at_unix_ms: now_ms(),
                    text,
                    reply_to_platform_message_id: args.reply_to,
                    attachments: Vec::new(),
                })),
            })
            .await?;
    }

    let mut output = client
        .connector_stream(ReceiverStream::new(receiver))
        .await?
        .into_inner();
    while let Some(message) = output.message().await? {
        match message.operation {
            Some(core_to_connector::Operation::RegistrationResult(result)) => {
                println!("registration: {}", result.message);
                if !result.accepted {
                    bail!("connector registration rejected");
                }
            }
            Some(core_to_connector::Operation::DispatchCreateMessage(dispatch)) => {
                println!(
                    "dispatch {} -> {} text={:?} reply_to={:?}",
                    dispatch.dispatch_id,
                    dispatch.endpoint_id,
                    dispatch.text,
                    dispatch.reply_to_platform_message_id
                );
                if let Some(reason) = &args.fail_reason {
                    println!("fail {} reason={:?}", dispatch.dispatch_id, reason);
                    sender
                        .send(ConnectorToCore {
                            operation: Some(connector_to_core::Operation::DispatchFailed(
                                DispatchFailed {
                                    dispatch_id: dispatch.dispatch_id,
                                    reason: reason.clone(),
                                    permanent: args.permanent_failure,
                                },
                            )),
                        })
                        .await?;
                } else if !args.no_ack {
                    let platform_id = args
                        .ack_platform_id
                        .clone()
                        .unwrap_or_else(|| format!("copy-{}", Uuid::new_v4()));
                    println!("ack {} as {}", dispatch.dispatch_id, platform_id);
                    sender
                        .send(ConnectorToCore {
                            operation: Some(connector_to_core::Operation::DispatchSucceeded(
                                DispatchSucceeded {
                                    dispatch_id: dispatch.dispatch_id,
                                    platform_message_id: platform_id,
                                },
                            )),
                        })
                        .await?;
                }
            }
            None => {}
        }
    }
    Ok(())
}

/// Returns current Unix timestamp in milliseconds for synthetic messages.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
