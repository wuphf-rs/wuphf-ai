//! A terminal client over the wuphf observation log. Connects to the core's
//! observer gRPC service, lists the chats seen across every platform, shows a
//! selected chat's history, follows new activity live, and sends owner-authored
//! messages (plain or as replies) back into a chat.

mod app;
mod client;
mod ui;

use std::io::{self, Stdout};

use anyhow::Result;
use clap::Parser;
use crossterm::event::{Event, EventStream, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use wuphf_proto::connector as pb;

use crate::app::{Action, App, PAGE_SIZE};
use crate::client::ObserverClient;

#[derive(Parser)]
#[command(version, about = "Terminal viewer for the wuphf observation log")]
struct Arguments {
    /// Observer gRPC endpoint of the running core.
    #[arg(long, default_value = "http://127.0.0.1:50051")]
    server: String,
    /// Bearer token, when the observer service requires one.
    #[arg(long)]
    token: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let arguments = Arguments::parse();
    let mut client = ObserverClient::connect(arguments.server, arguments.token).await?;
    let mut app = App::new();

    match client.list_chats().await {
        Ok(chats) => {
            app.set_chats(chats);
            app.status = format!("{} chats", app.chats.len());
        }
        Err(error) => app.status = format!("list chats failed: {error}"),
    }
    if app.selected_chat().is_some() {
        load_chat(&mut client, &mut app).await;
    }

    // Live activity streams in on its own client clone, decoupled from the
    // request/response calls the event loop makes.
    let (live_tx, mut live_rx) = mpsc::channel::<pb::ObservedMessage>(256);
    let mut subscription = client.clone();
    tokio::spawn(async move {
        if let Ok(mut stream) = subscription.subscribe().await {
            while let Ok(Some(message)) = stream.message().await {
                if live_tx.send(message).await.is_err() {
                    break;
                }
            }
        }
    });

    // Send results come back from spawned tasks so a slow acknowledgement never
    // freezes the event loop.
    let (status_tx, mut status_rx) = mpsc::channel::<String>(16);

    let mut terminal = setup_terminal()?;
    let result = run(
        &mut terminal,
        &mut client,
        &mut app,
        &mut live_rx,
        status_tx,
        &mut status_rx,
    )
    .await;
    restore_terminal(&mut terminal)?;
    result
}

async fn run(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    client: &mut ObserverClient,
    app: &mut App,
    live_rx: &mut mpsc::Receiver<pb::ObservedMessage>,
    status_tx: mpsc::Sender<String>,
    status_rx: &mut mpsc::Receiver<String>,
) -> Result<()> {
    let mut events = EventStream::new();
    loop {
        let mut geometry = None;
        terminal.draw(|frame| geometry = Some(ui::draw(frame, app)))?;
        if let Some(geometry) = geometry {
            app.timeline_height = geometry.timeline_height;
            app.max_scroll = geometry.max_scroll;
            app.effective_scroll = geometry.effective_scroll;
        }
        if app.should_quit {
            return Ok(());
        }

        tokio::select! {
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                    match app.on_key(key) {
                        Some(Action::Quit) => app.should_quit = true,
                        Some(Action::LoadChat) => load_chat(client, app).await,
                        Some(Action::LoadOlder) => load_older(client, app).await,
                        Some(Action::Send { text, reply_to_platform_message_id }) => {
                            send_message(client, app, &status_tx, text, reply_to_platform_message_id);
                        }
                        None => {}
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => app.should_quit = true,
            },
            message = live_rx.recv() => {
                if let Some(message) = message {
                    // A message from a chat not in the list yet arrives without
                    // its title; refresh the list so the placeholder gets one.
                    if app.on_live(message)
                        && let Ok(chats) = client.list_chats().await
                    {
                        app.set_chats(chats);
                    }
                }
            }
            status = status_rx.recv() => {
                if let Some(status) = status {
                    app.status = status;
                }
            }
        }
    }
}

/// Fire a send on its own task: the server may wait up to its acknowledgement
/// window before responding, and the UI must keep drawing meanwhile. The result
/// lands in the status bar; the sent message itself arrives over the live
/// subscription once the connector acknowledges it.
fn send_message(
    client: &ObserverClient,
    app: &mut App,
    status_tx: &mpsc::Sender<String>,
    text: String,
    reply_to_platform_message_id: Option<String>,
) {
    let Some(chat) = app.selected_chat() else {
        app.status = "no chat selected".into();
        return;
    };
    let (connector_id, chat_id) = (chat.connector_id.clone(), chat.chat_id.clone());
    app.status = "sending…".into();
    let mut client = client.clone();
    let status_tx = status_tx.clone();
    tokio::spawn(async move {
        let status = match client
            .send_message(
                connector_id,
                chat_id,
                None,
                text,
                reply_to_platform_message_id,
            )
            .await
        {
            Ok(id) if id.is_empty() => "sent (awaiting acknowledgement)".to_owned(),
            Ok(id) => format!("sent ({id})"),
            Err(error) => format!("send failed: {error}"),
        };
        let _ = status_tx.send(status).await;
    });
}

/// Load the newest page for the selected chat, replacing the timeline.
async fn load_chat(client: &mut ObserverClient, app: &mut App) {
    let Some(chat) = app.selected_chat() else {
        return;
    };
    let (connector_id, chat_id) = (chat.connector_id.clone(), chat.chat_id.clone());
    match client
        .get_messages(connector_id, chat_id, None, PAGE_SIZE)
        .await
    {
        Ok(page) => {
            app.set_messages(page);
            app.status = format!("{} messages", app.messages.len());
        }
        Err(error) => app.status = format!("load failed: {error}"),
    }
}

/// Fetch a page older than the oldest shown message and prepend it (scrollback).
async fn load_older(client: &mut ObserverClient, app: &mut App) {
    let Some(chat) = app.selected_chat() else {
        return;
    };
    let before = app.oldest_ts();
    let (connector_id, chat_id) = (chat.connector_id.clone(), chat.chat_id.clone());
    match client
        .get_messages(connector_id, chat_id, before, PAGE_SIZE)
        .await
    {
        Ok(older) => app.prepend_messages(older),
        Err(error) => app.status = format!("scrollback failed: {error}"),
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}
