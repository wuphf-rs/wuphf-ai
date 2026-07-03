//! A read-only terminal viewer over the wuphf observation log. Connects to the
//! core's observer gRPC service, lists the chats seen across every platform,
//! shows a selected chat's history, and follows new activity live.

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
#[command(about = "Terminal viewer for the wuphf observation log")]
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

    let mut terminal = setup_terminal()?;
    let result = run(&mut terminal, &mut client, &mut app, &mut live_rx).await;
    restore_terminal(&mut terminal)?;
    result
}

async fn run(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    client: &mut ObserverClient,
    app: &mut App,
    live_rx: &mut mpsc::Receiver<pb::ObservedMessage>,
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
                        None => {}
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => app.should_quit = true,
            },
            message = live_rx.recv() => {
                if let Some(message) = message {
                    app.on_live(message);
                }
            }
        }
    }
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
