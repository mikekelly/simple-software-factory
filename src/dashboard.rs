//! Long-running terminal view of the canonical server dashboard model.
use anyhow::{Context, Result, bail};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind},
    execute, queue,
    style::Print,
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use serde_json::Value;
use std::{
    io::{self, IsTerminal, Write},
    time::{Duration, Instant},
};
use unicode_width::UnicodeWidthChar;

#[path = "dashboard_herdr.rs"]
mod herdr;

const STALE: Duration = Duration::from_secs(10);
const CARD_HEIGHT: usize = 6;
const HEADER: usize = 3;

struct Terminal;
impl Terminal {
    fn enter() -> Result<Self> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            bail!(
                "ssf dashboard requires an interactive terminal (use ssf status --json for scripts)"
            );
        }
        terminal::enable_raw_mode()?;
        let guard = Self;
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            event::EnableMouseCapture,
            cursor::Hide
        )?;
        Ok(guard)
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            event::DisableMouseCapture,
            cursor::Show,
            LeaveAlternateScreen
        );
        let _ = terminal::disable_raw_mode();
    }
}

struct Worker(Option<tokio::task::JoinHandle<()>>);
impl Worker {
    fn new(handle: tokio::task::JoinHandle<()>) -> Self {
        Self(Some(handle))
    }
    async fn stop(mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
            let _ = handle.await;
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(handle) = &self.0 {
            handle.abort();
        }
    }
}

#[derive(Default)]
struct View {
    cards: Vec<Value>,
    monitored_items: Vec<Value>,
    server: String,
    selected: usize,
    first: usize,
    warning: Option<String>,
    error: Option<String>,
    received: Option<Instant>,
    notice: String,
}
impl View {
    fn update(&mut self, payload: Value) -> Result<()> {
        let dashboard = &payload["dashboard"];
        let cards = dashboard["cards"]
            .as_array()
            .context("Server does not provide the dashboard model; update ssf-server")?;
        let owner = self
            .cards
            .get(self.selected)
            .map(|card| text(card, "owner").to_owned());
        self.selected = owner
            .and_then(|owner| cards.iter().position(|card| text(card, "owner") == owner))
            .unwrap_or(self.selected)
            .min(cards.len().saturating_sub(1));
        self.cards = cards.clone();
        self.monitored_items = dashboard["monitored_items"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let hostname = payload["server"]["hostname"].as_str().unwrap_or_default();
        let vm = payload["host_vm"]["name"].as_str().unwrap_or_default();
        self.server = match (hostname.is_empty(), vm.is_empty()) {
            (false, false) => format!("VM {vm} on {hostname}"),
            (false, true) => hostname.to_owned(),
            (true, false) => format!("VM {vm}"),
            (true, true) => "unknown server".into(),
        };
        self.warning = dashboard["warning"].as_str().map(str::to_owned);
        self.received = Some(Instant::now());
        self.error = None;
        Ok(())
    }
    fn select(&mut self, delta: isize) {
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.cards.len().saturating_sub(1));
    }
    fn visible(&mut self, height: usize) -> usize {
        let count = ((height.saturating_sub(HEADER + 2)) / CARD_HEIGHT).max(1);
        if self.selected < self.first {
            self.first = self.selected;
        }
        if self.selected >= self.first + count {
            self.first = self.selected + 1 - count;
        }
        count
    }
    fn mouse_card(&self, row: u16, height: usize) -> Option<usize> {
        let row = usize::from(row);
        if row < HEADER || row >= height.saturating_sub(2) {
            return None;
        }
        let offset = (row - HEADER) / CARD_HEIGHT;
        let count = ((height.saturating_sub(HEADER + 2)) / CARD_HEIGHT).max(1);
        let index = self.first + offset;
        (offset < count && index < self.cards.len()).then_some(index)
    }
    fn status(&self, now: Instant) -> String {
        if let Some(error) = &self.error {
            return format!("TRANSPORT ERROR — {error}; showing last received state, retrying");
        }
        if let Some(warning) = &self.warning {
            return format!("UNAVAILABLE / STALE — {warning}");
        }
        match self.received {
            None => "Connecting to SSF…".into(),
            Some(at) if now.duration_since(at) >= STALE => format!(
                "STALE — last response {}s ago; refresh pending",
                now.duration_since(at).as_secs()
            ),
            Some(at) => format!("Live — refreshed {}s ago", now.duration_since(at).as_secs()),
        }
    }
    fn lines(&mut self, height: usize) -> Vec<String> {
        let count = self.visible(height);
        let mut lines = vec![
            format!("SSF active agents ({}) — {}", self.cards.len(), self.server),
            self.status(Instant::now()),
            String::new(),
        ];
        for (index, card) in self.cards.iter().enumerate().skip(self.first).take(count) {
            let marker = if index == self.selected { '>' } else { ' ' };
            let origin = &card["origin"];
            let additional = card["additional"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .map(|item| format!("{} {}", text(item, "id"), text(item, "title")))
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .unwrap_or_default();
            lines.push(format!(
                "{marker} {} {}",
                text(origin, "id"),
                text(origin, "title")
            ));
            lines.push(format!(
                "  Assigned: {}",
                if additional.is_empty() {
                    "none"
                } else {
                    &additional
                }
            ));
            lines.push(format!(
                "  State: {} | {} {}",
                text(card, "agent_state"),
                text(card, "harness"),
                text(card, "model")
            ));
            lines.push(format!(
                "  Last activity: {}",
                text(card, "last_activity_at")
            ));
            lines.push(format!(
                "  Latest: {}",
                text(card, "last_assistant_message")
            ));
            lines.push(String::new());
        }
        if self.cards.is_empty()
            && self.received.is_some()
            && self.warning.is_none()
            && self.error.is_none()
        {
            lines.push("No active agents".into());
        }
        if !self.monitored_items.is_empty() {
            lines.push(format!(
                "Monitored without an agent ({}): {}",
                self.monitored_items.len(),
                self.monitored_items
                    .iter()
                    .map(|item| text(item, "id"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        lines.truncate(height.saturating_sub(2));
        lines.resize(height.saturating_sub(2), String::new());
        lines.push(if self.notice.is_empty() {
            if herdr::available() {
                "Enter/click: focus matching agent on this Herdr server".into()
            } else {
                "Standalone terminal; pane navigation requires Herdr".into()
            }
        } else {
            self.notice.clone()
        });
        lines.push(
            "↑/↓ j/k: select  PgUp/PgDn: page  Home/End  Enter: focus  q/Ctrl-C: quit".into(),
        );
        lines
    }
}
fn text<'a>(value: &'a Value, field: &str) -> &'a str {
    value[field].as_str().unwrap_or("—")
}

// Treat all server/agent strings as text, including terminal escape sequences.
fn clipped(input: &str, width: usize) -> String {
    let mut used = 0;
    input
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take_while(|c| {
            used += c.width().unwrap_or(0);
            used <= width
        })
        .collect()
}
// OSC 8 is an optional terminal capability; unsupported terminals still show IDs.
// Add trusted escapes only after clipping and sanitizing the untrusted text.
fn linked_line(line: &str, issues: &[Value]) -> String {
    let mut result = String::new();
    let mut remaining = line;
    for issue in issues {
        let Some(id) = issue["id"]
            .as_str()
            .filter(|id| !id.is_empty() && !id.chars().any(char::is_control))
        else {
            continue;
        };
        let Some(url) = issue["url"]
            .as_str()
            .filter(|url| !url.chars().any(char::is_control))
            .and_then(|url| reqwest::Url::parse(url).ok())
            .filter(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
        else {
            continue;
        };
        let Some(position) = remaining.find(id) else {
            continue;
        };
        result.push_str(&remaining[..position]);
        result.push_str(&format!("\x1b]8;;{url}\x1b\\{id}\x1b]8;;\x1b\\"));
        remaining = &remaining[position + id.len()..];
    }
    result.push_str(remaining);
    result
}

fn draw(view: &mut View) -> Result<()> {
    let (width, height) = terminal::size()?;
    let mut stdout = io::stdout().lock();
    for (row, line) in view
        .lines(usize::from(height))
        .iter()
        .take(usize::from(height))
        .enumerate()
    {
        let line = clipped(line, usize::from(width).saturating_sub(1));
        let card_row = row.saturating_sub(HEADER);
        let card = (row >= HEADER && row < usize::from(height).saturating_sub(2))
            .then(|| view.cards.get(view.first + card_row / CARD_HEIGHT))
            .flatten();
        let line = match (card, card_row % CARD_HEIGHT) {
            (Some(card), 0) => linked_line(&line, std::slice::from_ref(&card["origin"])),
            (Some(card), 1) => linked_line(
                &line,
                card["additional"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            ),
            _ => line,
        };
        queue!(
            stdout,
            cursor::MoveTo(0, row as u16),
            Clear(ClearType::CurrentLine),
            Print(line)
        )?;
    }
    stdout.flush()?;
    Ok(())
}

pub(crate) async fn run(server: Option<String>) -> Result<()> {
    let mut source = crate::dashboard_transport::StatusSource::new(server)?;
    let _terminal = Terminal::enter()?;
    let (sender, mut snapshots) = tokio::sync::mpsc::channel(1);
    let worker = Worker::new(tokio::spawn(async move {
        loop {
            let result = source
                .next_snapshot()
                .await
                .map_err(|error| format!("{error:#}"));
            let failed = result.is_err();
            if sender.send(result).await.is_err() {
                break;
            }
            if failed {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }));
    let (focus_sender, mut focus_results) = tokio::sync::mpsc::channel(1);
    let mut focus_worker: Option<Worker> = None;
    let mut view = View::default();
    let mut ticks = tokio::time::interval(Duration::from_millis(50));
    let mut termination =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    draw(&mut view)?;
    'dashboard: loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = termination.recv() => break,
            _ = hangup.recv() => break,
            result = snapshots.recv() => {
                match result {
                    Some(Ok(payload)) => { if let Err(error) = view.update(payload) { view.error = Some(error.to_string()); } }
                    Some(Err(error)) => view.error = Some(error),
                    None => bail!("Dashboard refresh worker stopped"),
                }
            }
            Some(result) = focus_results.recv() => { view.notice = result; focus_worker = None; }
            _ = ticks.tick() => {
                // Poll rather than blocking stdin: transport and signal futures keep running.
                for _ in 0..32 {
                    if !event::poll(Duration::ZERO)? { break; }
                    let height = usize::from(terminal::size()?.1);
                    let mut activate = false;
                    match event::read()? {
                        Event::Key(key) if key.kind != event::KeyEventKind::Release => match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => break 'dashboard,
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break 'dashboard,
                            KeyCode::Down | KeyCode::Char('j') => view.select(1),
                            KeyCode::Up | KeyCode::Char('k') => view.select(-1),
                            KeyCode::PageDown => { let count = view.visible(height); view.select(count as isize); }
                            KeyCode::PageUp => { let count = view.visible(height); view.select(-(count as isize)); }
                            KeyCode::Home => view.selected = 0,
                            KeyCode::End => view.selected = view.cards.len().saturating_sub(1),
                            KeyCode::Enter => activate = true,
                            _ => {},
                        },
                        Event::Mouse(mouse) => match mouse.kind {
                            MouseEventKind::ScrollDown => view.select(1),
                            MouseEventKind::ScrollUp => view.select(-1),
                            MouseEventKind::Down(MouseButton::Left) => if let Some(index) = view.mouse_card(mouse.row, height) { view.selected = index; activate = true; },
                            _ => {},
                        },
                        _ => {},
                    }
                    if activate && focus_worker.is_none()
                        && let Some(card) = view.cards.get(view.selected) {
                            let session = card["agent_session_id"].as_str().unwrap_or_default().to_owned();
                            let harness = card["harness"].as_str().unwrap_or_default().to_owned();
                            let sender = focus_sender.clone();
                            view.notice = "Finding agent on this Herdr server…".into();
                            focus_worker = Some(Worker::new(tokio::spawn(async move {
                                let message = herdr::focus(&session, &harness).await.unwrap_or_else(|error| format!("{error:#}"));
                                let _ = sender.send(message).await;
                            })));
                    }
                }
            }
        }
        draw(&mut view)?;
    }
    worker.stop().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn payload(owners: &[&str]) -> Value {
        json!({"dashboard":{"cards":owners.iter().map(|owner|json!({"owner":owner,"origin":{"id":owner,"title":"Issue"},"agent_state":"working","last_activity_at":"today","last_assistant_message":"Latest","additional":[{"id":"r#3","title":"Assigned"}]})).collect::<Vec<_>>()}})
    }
    #[test]
    fn selection_survives_reordering_and_scrolling_and_mouse_maps_visible_cards() {
        let mut view = View::default();
        view.update(payload(&["r#1", "r#2", "r#3"])).unwrap();
        view.select(2);
        assert_eq!(view.visible(17), 2);
        assert_eq!(view.first, 1);
        assert_eq!(view.mouse_card(3, 17), Some(1));
        assert_eq!(view.mouse_card(2, 17), None);
        assert_eq!(view.mouse_card(16, 17), None);
        view.update(payload(&["r#3", "r#1"])).unwrap();
        assert_eq!(view.selected, 0);
        view.select(-3);
        assert_eq!(view.selected, 0);
        view.update(payload(&[])).unwrap();
        assert_eq!(view.mouse_card(3, 17), None);
    }
    #[test]
    fn blank_rows_never_select_an_offscreen_agent() {
        let mut view = View::default();
        view.update(payload(&["r#1", "r#2", "r#3", "r#4"])).unwrap();
        assert_eq!(view.visible(24), 3);
        assert_eq!(view.mouse_card(20, 24), Some(2));
        assert_eq!(view.mouse_card(21, 24), None);
    }
    #[test]
    fn hyperlinks_allow_only_safe_web_urls_and_visible_complete_ids() {
        let issue = json!({"id":"r#1","url":"https://github.com/o/r/issues/1"});
        assert!(
            linked_line("> r#1 Issue", std::slice::from_ref(&issue))
                .contains("\x1b]8;;https://github.com/o/r/issues/1")
        );
        assert_eq!(linked_line("> r#", &[issue]), "> r#");
        for url in ["javascript:alert(1)", "https://example.org/\x1b]malicious"] {
            assert_eq!(linked_line("r#1", &[json!({"id":"r#1","url":url})]), "r#1");
        }
    }
    #[test]
    fn canonical_cards_errors_stale_state_and_terminal_escapes_are_explicit() {
        let mut view = View::default();
        view.update(payload(&["r#1"])).unwrap();
        let lines = view.lines(24).join("\n");
        for field in ["r#1 Issue", "r#3 Assigned", "working", "today", "Latest"] {
            assert!(lines.contains(field));
        }
        assert!(view.status(Instant::now() + STALE).contains("STALE"));
        view.error = Some("connection refused".into());
        assert!(view.status(Instant::now()).contains("TRANSPORT ERROR"));
        assert!(view.update(json!({"sessions":[]})).is_err());
        assert_eq!(view.cards.len(), 1);
        view.update(json!({"dashboard":{"cards":[],"warning":"VM stopped"}}))
            .unwrap();
        assert!(view.lines(24).join("\n").contains("VM stopped"));
        assert!(!view.lines(24).join("\n").contains("No active agents"));
        assert_eq!(clipped("\u{1b}[31m界x\n", 8), " [31m界x");
        assert_eq!(clipped("界x", 1), "");
    }
}
