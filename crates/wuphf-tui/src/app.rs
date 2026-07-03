//! TUI state and the pure transitions over it. Rendering lives in `ui`; network
//! I/O lives in `client`. Key handling returns an [`Action`] describing the
//! network work the event loop should perform, so this module stays free of
//! async and is unit-testable.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use wuphf_proto::connector as pb;

/// How many messages a page/scrollback request fetches.
pub const PAGE_SIZE: u32 = 50;

/// Which pane has focus, deciding what the navigation keys act on.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Chats,
    Timeline,
}

/// Network work the event loop should perform after a key press. Keeping I/O out
/// of the state transition makes the transitions synchronous and testable.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    Quit,
    /// Load the newest page for the selected chat (selection changed).
    LoadChat,
    /// Load a page older than the oldest currently shown message (scrollback).
    LoadOlder,
}

pub struct App {
    pub chats: Vec<pb::ObservedChat>,
    pub selected: usize,
    /// Messages for the selected chat, oldest first (display order).
    pub messages: Vec<pb::ObservedMessage>,
    pub focus: Focus,
    pub status: String,
    pub should_quit: bool,
    /// Top visible line of the timeline; ignored while `follow` is set.
    pub scroll: u16,
    /// Stick the timeline to the newest message; cleared when the user scrolls up.
    pub follow: bool,
    /// Whether the oldest message of the selected chat is loaded, so scrollback
    /// stops requesting once the history is exhausted.
    pub reached_oldest: bool,
    // Timeline geometry from the last frame, used to bound scrolling.
    pub timeline_height: u16,
    pub max_scroll: u16,
    pub effective_scroll: u16,
}

impl App {
    pub fn new() -> Self {
        Self {
            chats: Vec::new(),
            selected: 0,
            messages: Vec::new(),
            focus: Focus::Chats,
            status: "connecting…".into(),
            should_quit: false,
            scroll: 0,
            follow: true,
            reached_oldest: false,
            timeline_height: 0,
            max_scroll: 0,
            effective_scroll: 0,
        }
    }

    pub fn selected_chat(&self) -> Option<&pb::ObservedChat> {
        self.chats.get(self.selected)
    }

    /// Replace the chat list, keeping the current selection pinned to the same
    /// chat when it is still present so a background refresh does not jump the
    /// cursor.
    pub fn set_chats(&mut self, chats: Vec<pb::ObservedChat>) {
        let current = self
            .selected_chat()
            .map(|chat| (chat.connector_id.clone(), chat.chat_id.clone()));
        self.chats = chats;
        self.selected = current
            .and_then(|(connector_id, chat_id)| {
                self.chats
                    .iter()
                    .position(|chat| chat.connector_id == connector_id && chat.chat_id == chat_id)
            })
            .unwrap_or(0)
            .min(self.chats.len().saturating_sub(1));
    }

    /// Replace the selected chat's timeline with a freshly loaded newest page.
    /// The page arrives newest-first; store it oldest-first for display.
    pub fn set_messages(&mut self, mut page: Vec<pb::ObservedMessage>) {
        self.reached_oldest = (page.len() as u32) < PAGE_SIZE;
        page.reverse();
        self.messages = page;
        self.follow = true;
        self.scroll = 0;
    }

    /// Prepend an older page (scrollback). The page arrives newest-first; it sits
    /// before the current oldest message, so reversed it leads the buffer.
    pub fn prepend_messages(&mut self, mut older: Vec<pb::ObservedMessage>) {
        self.reached_oldest = (older.len() as u32) < PAGE_SIZE;
        if older.is_empty() {
            return;
        }
        older.reverse();
        older.append(&mut self.messages);
        self.messages = older;
    }

    /// The timestamp to page before for scrollback: the oldest loaded message's.
    pub fn oldest_ts(&self) -> Option<i64> {
        self.messages.first().map(|message| message.timestamp_ms)
    }

    /// Apply a live message: refresh its chat summary and, when it belongs to the
    /// open chat, upsert it into the timeline by native id (so an edit or delete
    /// replaces the row rather than duplicating it).
    pub fn on_live(&mut self, message: pb::ObservedMessage) {
        self.touch_chat(&message);
        if self.selected_chat().is_some_and(|chat| {
            chat.connector_id == message.connector_id && chat.chat_id == message.chat_id
        }) {
            match self
                .messages
                .iter_mut()
                .find(|existing| existing.platform_message_id == message.platform_message_id)
            {
                Some(existing) => *existing = message,
                // Append; when following, the draw step re-pins to the bottom.
                None => self.messages.push(message),
            }
        }
    }

    /// Bump a chat to the top of the list on new activity, creating a summary row
    /// if the chat has not been seen in the list yet.
    fn touch_chat(&mut self, message: &pb::ObservedMessage) {
        let position = self.chats.iter().position(|chat| {
            chat.connector_id == message.connector_id && chat.chat_id == message.chat_id
        });
        let selected_key = self
            .selected_chat()
            .map(|chat| (chat.connector_id.clone(), chat.chat_id.clone()));
        let preview = message_preview(message);
        match position {
            Some(index) => {
                let chat = &mut self.chats[index];
                chat.last_message_ts = message.timestamp_ms;
                chat.last_message_preview = preview;
            }
            None => self.chats.push(pb::ObservedChat {
                connector_id: message.connector_id.clone(),
                platform: message.platform,
                chat_id: message.chat_id.clone(),
                title: None,
                last_message_ts: message.timestamp_ms,
                last_message_preview: preview,
            }),
        }
        self.chats
            .sort_by_key(|chat| std::cmp::Reverse(chat.last_message_ts));
        // Re-pin the selection to the same chat after the reorder.
        if let Some((connector_id, chat_id)) = selected_key
            && let Some(index) = self
                .chats
                .iter()
                .position(|chat| chat.connector_id == connector_id && chat.chat_id == chat_id)
        {
            self.selected = index;
        }
    }

    /// Translate a key press into a state change, returning the network work it
    /// implies (if any).
    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(Action::Quit);
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Some(Action::Quit),
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Chats => Focus::Timeline,
                    Focus::Timeline => Focus::Chats,
                };
            }
            KeyCode::Down | KeyCode::Char('j') => return self.move_down(),
            KeyCode::Up | KeyCode::Char('k') => return self.move_up(),
            KeyCode::PageDown => return self.scroll_timeline_down(),
            KeyCode::PageUp => return self.scroll_timeline_up(),
            _ => {}
        }
        None
    }

    fn move_down(&mut self) -> Option<Action> {
        match self.focus {
            Focus::Chats => {
                if self.selected + 1 < self.chats.len() {
                    self.selected += 1;
                    return Some(Action::LoadChat);
                }
                None
            }
            Focus::Timeline => self.scroll_timeline_down(),
        }
    }

    fn move_up(&mut self) -> Option<Action> {
        match self.focus {
            Focus::Chats => {
                if self.selected > 0 {
                    self.selected -= 1;
                    return Some(Action::LoadChat);
                }
                None
            }
            Focus::Timeline => self.scroll_timeline_up(),
        }
    }

    fn scroll_timeline_down(&mut self) -> Option<Action> {
        let next = self.effective_scroll.saturating_add(1);
        if next >= self.max_scroll {
            self.follow = true;
            self.scroll = self.max_scroll;
        } else {
            self.follow = false;
            self.scroll = next;
        }
        None
    }

    fn scroll_timeline_up(&mut self) -> Option<Action> {
        self.follow = false;
        self.scroll = self.effective_scroll.saturating_sub(1);
        // At the top with more history available, ask for an older page.
        if self.effective_scroll == 0 && !self.reached_oldest && !self.messages.is_empty() {
            return Some(Action::LoadOlder);
        }
        None
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

/// A one-line preview for a chat summary derived from a live message, mirroring
/// the server's own preview rule (text, else a bracketed attachment label).
fn message_preview(message: &pb::ObservedMessage) -> String {
    if let Some(text) = message.text.as_deref() {
        let text = text.trim();
        if !text.is_empty() {
            return text.chars().take(80).collect();
        }
    }
    match message.attachments.first() {
        Some(attachment) => format!("[{}]", crate::ui::attachment_label(attachment.kind)),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(chat: &str, id: &str, ts: i64, text: &str) -> pb::ObservedMessage {
        pb::ObservedMessage {
            connector_id: "whatsapp".into(),
            platform: pb::Platform::Whatsapp as i32,
            chat_id: chat.into(),
            thread_id: None,
            platform_message_id: id.into(),
            sender: "peer".into(),
            sender_name: Some("Peer".into()),
            participant: None,
            text: Some(text.into()),
            attachments: vec![],
            reply_to_platform_message_id: None,
            direction: pb::ObservedDirection::Incoming as i32,
            timestamp_ms: ts,
            deleted: false,
            edited: false,
        }
    }

    fn chat(chat: &str, ts: i64) -> pb::ObservedChat {
        pb::ObservedChat {
            connector_id: "whatsapp".into(),
            platform: pb::Platform::Whatsapp as i32,
            chat_id: chat.into(),
            title: Some(chat.into()),
            last_message_ts: ts,
            last_message_preview: String::new(),
        }
    }

    #[test]
    fn live_edit_replaces_the_open_chats_row() {
        let mut app = App::new();
        app.set_chats(vec![chat("alice", 100)]);
        app.set_messages(vec![message("alice", "a1", 100, "hi")]);

        let mut edited = message("alice", "a1", 100, "hello");
        edited.edited = true;
        app.on_live(edited);

        assert_eq!(app.messages.len(), 1);
        assert!(app.messages[0].edited);
        assert_eq!(app.messages[0].text.as_deref(), Some("hello"));
    }

    #[test]
    fn live_message_for_unseen_chat_adds_it_to_the_top() {
        let mut app = App::new();
        app.set_chats(vec![chat("alice", 100)]);
        app.on_live(message("bob", "b1", 200, "hey"));
        assert_eq!(app.chats[0].chat_id, "bob"); // newest activity first
        assert_eq!(app.chats.len(), 2);
    }

    #[test]
    fn selecting_a_new_chat_requests_a_load() {
        let mut app = App::new();
        app.set_chats(vec![chat("alice", 200), chat("bob", 100)]);
        let action = app.on_key(KeyEvent::from(KeyCode::Down));
        assert_eq!(action, Some(Action::LoadChat));
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn set_chats_keeps_selection_on_the_same_chat() {
        let mut app = App::new();
        app.set_chats(vec![chat("alice", 200), chat("bob", 100)]);
        app.selected = 1; // bob
        // A refresh reorders alice/bob; selection should still point at bob.
        app.set_chats(vec![chat("bob", 300), chat("alice", 200)]);
        assert_eq!(app.selected_chat().unwrap().chat_id, "bob");
    }

    #[test]
    fn quit_keys() {
        let mut app = App::new();
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Char('q'))),
            Some(Action::Quit)
        );
        assert_eq!(
            app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Action::Quit)
        );
    }
}
