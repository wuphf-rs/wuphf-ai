//! Rendering for the TUI. Pure view over [`App`] plus a couple of formatting
//! helpers; the only state it feeds back is the timeline geometry, returned from
//! [`draw`] so the event loop can bound scrolling on the next key.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use wuphf_proto::connector as pb;

use crate::app::{App, Focus};

/// Timeline geometry from a frame, fed back into [`App`] so scrolling can be
/// clamped against the real viewport and content height on the next key press.
pub struct Geometry {
    pub timeline_height: u16,
    pub max_scroll: u16,
    pub effective_scroll: u16,
}

pub fn draw(frame: &mut Frame, app: &App) -> Geometry {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(frame.area());
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(32), Constraint::Percentage(68)])
        .split(root[0]);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(columns[1]);

    draw_chats(frame, columns[0], app);
    let geometry = draw_timeline(frame, right[0], app);
    draw_input(frame, right[1], app);
    draw_status(frame, root[1], app);
    geometry
}

fn draw_chats(frame: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app
        .chats
        .iter()
        .map(|chat| {
            let name = chat.title.clone().unwrap_or_else(|| chat.chat_id.clone());
            let header = Line::from(vec![
                Span::styled(
                    format!("{} ", platform_label(chat.platform)),
                    Style::default().fg(Color::Magenta),
                ),
                Span::styled(name, Style::default().add_modifier(Modifier::BOLD)),
            ]);
            let preview = Line::from(Span::styled(
                truncate(&chat.last_message_preview, 60),
                Style::default().fg(Color::DarkGray),
            ));
            ListItem::new(vec![header, preview])
        })
        .collect();

    let mut state = ListState::default();
    if !app.chats.is_empty() {
        state.select(Some(app.selected));
    }
    let list = List::new(items)
        .block(bordered("Chats", app.focus == Focus::Chats))
        .highlight_style(
            Style::default()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_timeline(frame: &mut Frame, area: Rect, app: &App) -> Geometry {
    let title = app
        .selected_chat()
        .map(|chat| chat.title.clone().unwrap_or_else(|| chat.chat_id.clone()))
        .unwrap_or_else(|| "No chat".into());
    let inner_width = area.width.saturating_sub(2).max(1);
    let inner_height = area.height.saturating_sub(2);

    let mut lines: Vec<Line> = Vec::new();
    // First rendered line of each message, for scrolling the cursor into view.
    let mut message_starts: Vec<u16> = Vec::with_capacity(app.messages.len());
    for (index, message) in app.messages.iter().enumerate() {
        message_starts.push(lines.len() as u16);
        let selected = app.cursor == Some(index);
        render_message(message, inner_width, selected, &mut lines);
    }
    let total = lines.len() as u16;
    let max_scroll = total.saturating_sub(inner_height);
    let mut effective = if app.follow {
        max_scroll
    } else {
        app.scroll.min(max_scroll)
    };
    // Keep the cursored message visible: scroll up to its first line, or down
    // until its block (bounded by the next message's start) fits.
    if let Some(cursor) = app.cursor
        && cursor < message_starts.len()
    {
        let start = message_starts[cursor];
        let end = message_starts.get(cursor + 1).copied().unwrap_or(total);
        if start < effective {
            effective = start;
        } else if end > effective + inner_height {
            effective = end.saturating_sub(inner_height).min(start);
        }
    }

    let paragraph = Paragraph::new(lines)
        .block(bordered(&title, app.focus == Focus::Timeline))
        .scroll((effective, 0));
    frame.render_widget(paragraph, area);
    Geometry {
        timeline_height: inner_height,
        max_scroll,
        effective_scroll: effective,
    }
}

fn draw_input(frame: &mut Frame, area: Rect, app: &App) {
    let title = match &app.reply_to {
        Some(target) => format!("Reply to: {}", truncate(&target.preview, 40)),
        None => "Message".to_owned(),
    };
    let focused = app.focus == Focus::Input;
    let content: Span<'_> = if app.input.is_empty() && !focused {
        Span::styled(
            "press i to write, r to reply, Enter to send",
            Style::default().fg(Color::DarkGray),
        )
    } else {
        // Show the tail when the text overflows, with a plain block cursor.
        let visible_width = area.width.saturating_sub(3) as usize;
        let tail: String = tail_chars(&app.input, visible_width);
        Span::raw(format!("{tail}▏"))
    };
    let paragraph = Paragraph::new(content).block(bordered(&title, focused));
    frame.render_widget(paragraph, area);
}

/// The last `max_chars` characters of `text` (all of it when it fits).
fn tail_chars(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(max_chars)).collect()
}

fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let follow = if app.follow { "live" } else { "scrolled" };
    let text = format!(
        " {status}  ·  {follow}  ·  ↑/↓ select · Tab pane · i write · r reply · q quit ",
        status = app.status,
    );
    let paragraph = Paragraph::new(text).style(Style::default().bg(Color::Blue).fg(Color::White));
    frame.render_widget(paragraph, area);
}

/// Append the rendered lines for one message: a header (author, time, markers)
/// then its wrapped body and any attachment labels. A selected (cursored)
/// message renders its header reversed so the reply target is visible.
fn render_message(
    message: &pb::ObservedMessage,
    width: u16,
    selected: bool,
    lines: &mut Vec<Line>,
) {
    let author = message
        .sender_name
        .clone()
        .unwrap_or_else(|| message.sender.clone());
    let selection = if selected {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    let mut header = vec![
        Span::styled(
            format_time(message.timestamp_ms),
            selection.fg(Color::DarkGray),
        ),
        Span::styled(" ", selection),
        Span::styled(
            author,
            selection.fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
    ];
    if message.direction == pb::ObservedDirection::Outgoing as i32 {
        header.insert(0, Span::styled("→ ", selection.fg(Color::Green)));
    }
    if message.edited {
        header.push(Span::styled(" (edited)", selection.fg(Color::DarkGray)));
    }
    lines.push(Line::from(header));

    if message.deleted {
        lines.push(Line::from(Span::styled(
            "  (deleted)",
            Style::default().fg(Color::Red).add_modifier(Modifier::DIM),
        )));
    } else if let Some(text) = message.text.as_deref() {
        for wrapped in wrap(text, width.saturating_sub(2)) {
            lines.push(Line::from(Span::raw(format!("  {wrapped}"))));
        }
    }
    for attachment in &message.attachments {
        let name = attachment
            .file_name
            .clone()
            .map(|file| format!(" {file}"))
            .unwrap_or_default();
        lines.push(Line::from(Span::styled(
            format!("  [{}]{name}", attachment_label(attachment.kind)),
            Style::default().fg(Color::Yellow),
        )));
    }
    lines.push(Line::from(""));
}

fn bordered(title: &str, focused: bool) -> Block<'_> {
    let style = if focused {
        Style::default().fg(Color::Blue)
    } else {
        Style::default()
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(style)
        .title(title.to_string())
}

/// Word-wrap `text` to `width` columns, hard-splitting any word longer than the
/// width so a long unbroken token still fits. Newlines in the source start new
/// lines. Approximate (counts chars, not display width) but adequate for a chat
/// log.
pub fn wrap(text: &str, width: u16) -> Vec<String> {
    let width = width.max(1) as usize;
    let mut out = Vec::new();
    for source_line in text.split('\n') {
        let mut current = String::new();
        for word in source_line.split(' ') {
            let mut word = word;
            while word.chars().count() > width {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
                let head: String = word.chars().take(width).collect();
                out.push(head);
                word = &word[char_boundary(word, width)..];
            }
            let extra = if current.is_empty() { 0 } else { 1 };
            if current.chars().count() + extra + word.chars().count() > width {
                out.push(std::mem::take(&mut current));
            }
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
        }
        out.push(current);
    }
    out
}

/// Byte offset of the `n`th character, for slicing without splitting a char.
fn char_boundary(text: &str, n: usize) -> usize {
    text.char_indices()
        .nth(n)
        .map(|(index, _)| index)
        .unwrap_or(text.len())
}

fn truncate(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// A UTC `HH:MM` clock for a millisecond timestamp, computed without a date
/// library since only the wall-clock time is shown.
fn format_time(timestamp_ms: i64) -> String {
    let seconds = timestamp_ms.div_euclid(1000);
    let minutes = seconds.div_euclid(60);
    let hours = minutes.div_euclid(60);
    format!("{:02}:{:02}", hours.rem_euclid(24), minutes.rem_euclid(60))
}

pub fn platform_label(platform: i32) -> &'static str {
    match pb::Platform::try_from(platform) {
        Ok(pb::Platform::Whatsapp) => "WA",
        Ok(pb::Platform::Telegram) => "TG",
        Ok(pb::Platform::Discord) => "DC",
        Ok(pb::Platform::Signal) => "SG",
        Ok(pb::Platform::Slack) => "SL",
        Ok(pb::Platform::Matrix) => "MX",
        Ok(pb::Platform::Instagram) => "IG",
        _ => "??",
    }
}

pub fn attachment_label(kind: i32) -> &'static str {
    match pb::attachment::Kind::try_from(kind) {
        Ok(pb::attachment::Kind::Image) => "image",
        Ok(pb::attachment::Kind::Video) => "video",
        Ok(pb::attachment::Kind::Audio) => "audio",
        Ok(pb::attachment::Kind::File) => "file",
        Ok(pb::attachment::Kind::Sticker) => "sticker",
        _ => "media",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_breaks_on_width_and_preserves_words() {
        assert_eq!(wrap("hello world foo", 11), vec!["hello world", "foo"]);
    }

    #[test]
    fn wrap_hard_splits_a_long_token() {
        assert_eq!(wrap("abcdefgh", 3), vec!["abc", "def", "gh"]);
    }

    #[test]
    fn wrap_keeps_blank_lines_from_newlines() {
        assert_eq!(wrap("a\n\nb", 5), vec!["a", "", "b"]);
    }

    #[test]
    fn format_time_is_utc_hh_mm() {
        // 1_000_000 ms = 1000 s = 00:16:40 UTC
        assert_eq!(format_time(1_000_000), "00:16");
    }
}
