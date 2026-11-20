//! TUI state and the pure transitions over it. Rendering lives in `ui`; network
//! I/O lives in `client`. Key handling returns an [`Action`] describing the
//! network work the event loop should perform, so this module stays free of
//! async and is unit-testable.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use wuphf_proto::connector as pb;

/// How many messages a page/scrollback request fetches.
pub const PAGE_SIZE: u32 = 50;

/// Which pane has focus, deciding what the navigation keys act on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Chats,
    Timeline,
    Input,
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
    /// Send the composed text to the selected chat.
    Send {
        text: String,
        reply_to_platform_message_id: Option<String>,
    },
}

/// The message a composed send replies to, kept for the input box hint and the
/// send request.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ReplyTarget {
    pub platform_message_id: String,
    pub preview: String,
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
    /// Timeline message cursor (index into `messages`): `None` follows the
    /// bottom, `Some` highlights one message for reply targeting.
    pub cursor: Option<usize>,
    /// The message being composed in the input box.
    pub input: String,
    /// The message the composed text replies to, when any.
    pub reply_to: Option<ReplyTarget>,
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
            cursor: None,
            input: String::new(),
            reply_to: None,
            timeline_height: 0,
            max_scroll: 0,
            effective_scroll: 0,
        }
    }

    pub fn selected_chat(&self) -> Option<&pb::ObservedChat> {
        self.chats.get(self.selected)
    }

    /// Index of the chat with this identity, when present. A chat is identified
    /// by `(connector_id, chat_id)` everywhere in the list.
    fn chat_position(&self, connector_id: &str, chat_id: &str) -> Option<usize> {
        self.chats
            .iter()
            .position(|chat| chat.connector_id == connector_id && chat.chat_id == chat_id)
    }

    /// The selected chat's identity, captured before a list mutation so the
    /// selection can be re-pinned afterwards.
    fn selected_key(&self) -> Option<(String, String)> {
        self.selected_chat()
            .map(|chat| (chat.connector_id.clone(), chat.chat_id.clone()))
    }

    /// Whether a live message belongs to the chat open in the timeline.
    fn is_open_chat(&self, message: &pb::ObservedMessage) -> bool {
        self.selected_chat().is_some_and(|chat| {
            chat.connector_id == message.connector_id && chat.chat_id == message.chat_id
        })
    }

    /// Replace the chat list, keeping the current selection pinned to the same
    /// chat when it is still present so a background refresh does not jump the
    /// cursor.
    pub fn set_chats(&mut self, chats: Vec<pb::ObservedChat>) {
        let current = self.selected_key();
        self.chats = chats;
        self.selected = current
            .and_then(|(connector_id, chat_id)| self.chat_position(&connector_id, &chat_id))
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
        // A new chat's timeline: the cursor and any pending reply pointed into
        // the previous one.
        self.cursor = None;
        self.reply_to = None;
    }

    /// Prepend an older page (scrollback). The page arrives newest-first; it sits
    /// before the current oldest message, so reversed it leads the buffer.
    pub fn prepend_messages(&mut self, mut older: Vec<pb::ObservedMessage>) {
        self.reached_oldest = (older.len() as u32) < PAGE_SIZE;
        if older.is_empty() {
            return;
        }
        older.reverse();
        // Keep the cursor on the same message across the index shift.
        if let Some(cursor) = self.cursor {
            self.cursor = Some(cursor + older.len());
        }
        older.append(&mut self.messages);
        self.messages = older;
    }

    /// The timestamp to page before for scrollback: the oldest loaded message's.
    pub fn oldest_ts(&self) -> Option<i64> {
        self.messages.first().map(|message| message.timestamp_ms)
    }

    /// Apply a live message: refresh its chat summary and, when it belongs to the
    /// open chat, upsert it into the timeline by native id (so an edit or delete
    /// replaces the row rather than duplicating it). Returns `true` when the
    /// message came from a chat not in the list yet — the placeholder row it
    /// creates has no title, so the caller should refresh the chat list.
    pub fn on_live(&mut self, message: pb::ObservedMessage) -> bool {
        let unseen_chat = self.touch_chat(&message);
        if self.is_open_chat(&message) {
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
        unseen_chat
    }

    /// Bump a chat to the top of the list on new activity, creating a summary row
    /// if the chat has not been seen in the list yet. Returns `true` when the
    /// chat was not in the list.
    fn touch_chat(&mut self, message: &pb::ObservedMessage) -> bool {
        let position = self.chat_position(&message.connector_id, &message.chat_id);
        let unseen_chat = position.is_none();
        let selected_key = self.selected_key();
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
            && let Some(index) = self.chat_position(&connector_id, &chat_id)
        {
            self.selected = index;
        }
        unseen_chat
    }

    /// Translate a key press into a state change, returning the network work it
    /// implies (if any). While the input box has focus, printable keys type into
    /// it — only Ctrl-C still quits.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(Action::Quit);
        }
        if self.focus == Focus::Input {
            return self.on_input_key(key);
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Some(Action::Quit),
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Chats => Focus::Timeline,
                    Focus::Timeline => Focus::Input,
                    Focus::Input => Focus::Chats,
                };
            }
            KeyCode::Char('i') => self.focus = Focus::Input,
            KeyCode::Char('r') if self.focus == Focus::Timeline => self.start_reply(),
            KeyCode::Down | KeyCode::Char('j') => return self.move_down(),
            KeyCode::Up | KeyCode::Char('k') => return self.move_up(),
            KeyCode::PageDown => return self.scroll_timeline_down(),
            KeyCode::PageUp => return self.scroll_timeline_up(),
            _ => {}
        }
        None
    }

    /// Key handling while composing: edit the buffer, Enter sends, Esc cancels
    /// the reply (or leaves the input box when none is pending).
    fn on_input_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Enter => {
                let text = self.input.trim().to_owned();
                if text.is_empty() {
                    return None;
                }
                self.input.clear();
                let reply = self.reply_to.take();
                return Some(Action::Send {
                    text,
                    reply_to_platform_message_id: reply.map(|target| target.platform_message_id),
                });
            }
            // First Esc drops a pending reply; without one it leaves the box.
            KeyCode::Esc if self.reply_to.take().is_none() => {
                self.focus = Focus::Timeline;
            }
            KeyCode::Tab => self.focus = Focus::Chats,
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(character) => self.input.push(character),
            _ => {}
        }
        None
    }

    /// Target the cursored message (or the newest one) for a reply and switch to
    /// composing.
    fn start_reply(&mut self) {
        let Some(index) = self.cursor.or_else(|| self.messages.len().checked_sub(1)) else {
            return;
        };
        let message = &self.messages[index];
        self.reply_to = Some(ReplyTarget {
            platform_message_id: message.platform_message_id.clone(),
            preview: message_preview(message),
        });
        self.focus = Focus::Input;
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
            Focus::Timeline => {
                // Cursor down; stepping past the newest message returns to
                // following the bottom.
                match self.cursor {
                    Some(index) if index + 1 < self.messages.len() => self.cursor = Some(index + 1),
                    Some(_) => {
                        self.cursor = None;
                        self.follow = true;
                    }
                    None => {}
                }
                None
            }
            Focus::Input => None,
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
            Focus::Timeline => {
                if self.messages.is_empty() {
                    return None;
                }
                // Cursor up from the bottom selects the newest message; at the
                // top with more history, pull an older page.
                match self.cursor {
                    None => {
                        self.cursor = Some(self.messages.len() - 1);
                        self.follow = false;
                    }
                    Some(0) => {
                        if !self.reached_oldest {
                            return Some(Action::LoadOlder);
                        }
                    }
                    Some(index) => self.cursor = Some(index - 1),
                }
                None
            }
            Focus::Input => None,
        }
    }

    fn scroll_timeline_down(&mut self) -> Option<Action> {
        // Manual scrolling releases the cursor so its visibility clamp does not
        // fight the scroll position.
        self.cursor = None;
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
        self.cursor = None;
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
            sent_by: None,
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
    fn live_message_for_unseen_chat_adds_it_to_the_top_and_asks_for_a_refresh() {
        let mut app = App::new();
        app.set_chats(vec![chat("alice", 100)]);
        assert!(app.on_live(message("bob", "b1", 200, "hey")));
        assert_eq!(app.chats[0].chat_id, "bob"); // newest activity first
        assert_eq!(app.chats.len(), 2);
        // Known chat: no refresh needed.
        assert!(!app.on_live(message("alice", "a9", 300, "hi")));
    }

    #[test]
    fn typing_in_the_input_composes_and_enter_sends() {
        let mut app = App::new();
        app.set_chats(vec![chat("alice", 100)]);
        app.focus = Focus::Input;
        // 'q' must type, not quit, while composing.
        for character in ['h', 'i', ' ', 'q'] {
            assert_eq!(app.on_key(KeyEvent::from(KeyCode::Char(character))), None);
        }
        assert_eq!(app.on_key(KeyEvent::from(KeyCode::Backspace)), None);
        assert_eq!(app.input, "hi ");
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Enter)),
            Some(Action::Send {
                text: "hi".into(),
                reply_to_platform_message_id: None,
            })
        );
        assert!(app.input.is_empty());
        // Enter with nothing composed sends nothing.
        assert_eq!(app.on_key(KeyEvent::from(KeyCode::Enter)), None);
    }

    #[test]
    fn reply_targets_the_cursored_message_and_esc_cancels() {
        let mut app = App::new();
        app.set_chats(vec![chat("alice", 100)]);
        app.set_messages(vec![
            message("alice", "a2", 200, "second"),
            message("alice", "a1", 100, "first"),
        ]);
        app.focus = Focus::Timeline;
        // Cursor up twice: newest, then the older message.
        app.on_key(KeyEvent::from(KeyCode::Up));
        app.on_key(KeyEvent::from(KeyCode::Up));
        app.on_key(KeyEvent::from(KeyCode::Char('r')));
        assert_eq!(app.focus, Focus::Input);
        assert_eq!(app.reply_to.as_ref().unwrap().platform_message_id, "a1");
        // Esc drops the reply but stays composing; a second Esc leaves.
        app.on_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.reply_to.is_none());
        assert_eq!(app.focus, Focus::Input);
        app.on_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(app.focus, Focus::Timeline);
    }

    #[test]
    fn reply_without_a_cursor_targets_the_newest_message() {
        let mut app = App::new();
        app.set_chats(vec![chat("alice", 100)]);
        app.set_messages(vec![
            message("alice", "a2", 200, "second"),
            message("alice", "a1", 100, "first"),
        ]);
        app.focus = Focus::Timeline;
        app.on_key(KeyEvent::from(KeyCode::Char('r')));
        assert_eq!(app.reply_to.as_ref().unwrap().platform_message_id, "a2");
        app.input.push_str("yes");
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Enter)),
            Some(Action::Send {
                text: "yes".into(),
                reply_to_platform_message_id: Some("a2".into()),
            })
        );
        assert!(app.reply_to.is_none());
    }

    #[test]
    fn cursor_at_the_top_requests_older_history() {
        let mut app = App::new();
        app.set_chats(vec![chat("alice", 100)]);
        let page: Vec<_> = (0..PAGE_SIZE)
            .map(|n| message("alice", &format!("a{n}"), 1000 - n as i64, "x"))
            .collect();
        app.set_messages(page); // full page → more history assumed
        app.focus = Focus::Timeline;
        app.on_key(KeyEvent::from(KeyCode::Up));
        app.cursor = Some(0);
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Up)),
            Some(Action::LoadOlder)
        );
        // The prepended page keeps the cursor on the same message.
        app.prepend_messages(vec![message("alice", "old", 1, "old")]);
        assert_eq!(app.cursor, Some(1));
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
