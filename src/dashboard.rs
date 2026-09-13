//! Long-running adaptive terminal view of canonical server dashboard models.
use anyhow::{Context, Result, bail};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind},
    execute, queue,
    style::Print,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    Frame, Terminal as RatatuiTerminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};
use serde_json::Value;
use std::{
    io::{self, IsTerminal, Stdout},
    time::{Duration, Instant},
};
use unicode_width::UnicodeWidthStr;

#[path = "dashboard_herdr.rs"]
mod herdr;

const STALE: Duration = Duration::from_secs(10);
const CARD_HEIGHT: u16 = 9;
const COMPACT_CARD_HEIGHT: u16 = 6;

struct TerminalSession {
    terminal: RatatuiTerminal<CrosstermBackend<Stdout>>,
}

impl TerminalSession {
    fn enter() -> Result<Self> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            bail!(
                "ssf dashboard requires an interactive terminal (use ssf status --json for scripts)"
            );
        }
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(
            stdout,
            EnterAlternateScreen,
            event::EnableMouseCapture,
            cursor::Hide
        ) {
            let _ = execute!(
                stdout,
                event::DisableMouseCapture,
                cursor::Show,
                LeaveAlternateScreen
            );
            let _ = terminal::disable_raw_mode();
            return Err(error.into());
        }
        let terminal = match RatatuiTerminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => terminal,
            Err(error) => {
                let mut stdout = io::stdout();
                let _ = execute!(
                    stdout,
                    event::DisableMouseCapture,
                    cursor::Show,
                    LeaveAlternateScreen
                );
                let _ = terminal::disable_raw_mode();
                return Err(error.into());
            }
        };
        Ok(Self { terminal })
    }

    fn draw(&mut self, view: &mut View) -> Result<()> {
        self.terminal.draw(|frame| render(frame, view))?;
        for link in &view.links {
            queue!(
                self.terminal.backend_mut(),
                cursor::MoveTo(link.x, link.y),
                Print(format!(
                    "\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\",
                    link.url, link.label
                ))
            )?;
        }
        io::Write::flush(self.terminal.backend_mut())?;
        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = execute!(
            self.terminal.backend_mut(),
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

struct FactoryView {
    route: Option<String>,
    name: String,
    cards: Vec<Value>,
    monitored_items: Vec<Value>,
    warning: Option<String>,
    error: Option<String>,
    received: Option<Instant>,
}

impl FactoryView {
    fn new(route: Option<String>) -> Self {
        Self {
            name: route.clone().unwrap_or_else(|| "local server".into()),
            route,
            cards: Vec::new(),
            monitored_items: Vec::new(),
            warning: None,
            error: None,
            received: None,
        }
    }

    fn status(&self, now: Instant) -> (String, Color) {
        if let Some(error) = &self.error {
            return (
                format!("TRANSPORT ERROR · {error} · showing last state, retrying"),
                Color::LightRed,
            );
        }
        if let Some(warning) = &self.warning {
            return (format!("UNAVAILABLE / STALE · {warning}"), Color::Yellow);
        }
        match self.received {
            None => ("Connecting to SSF…".into(), Color::DarkGray),
            Some(at) if now.duration_since(at) >= STALE => (
                format!(
                    "STALE · {}s since response",
                    now.duration_since(at).as_secs()
                ),
                Color::Yellow,
            ),
            Some(at) => (
                format!("live · {}s ago", now.duration_since(at).as_secs()),
                Color::LightGreen,
            ),
        }
    }
}

struct View {
    factories: Vec<FactoryView>,
    selected: usize,
    first: usize,
    hitboxes: Vec<(Rect, usize)>,
    columns: usize,
    notice: String,
    links: Vec<TerminalLink>,
}

struct TerminalLink {
    x: u16,
    y: u16,
    label: String,
    url: String,
}

impl View {
    fn new(routes: Vec<Option<String>>) -> Self {
        Self {
            factories: routes.into_iter().map(FactoryView::new).collect(),
            selected: 0,
            first: 0,
            hitboxes: Vec::new(),
            columns: 1,
            notice: String::new(),
            links: Vec::new(),
        }
    }

    fn card_count(&self) -> usize {
        self.factories
            .iter()
            .map(|factory| factory.cards.len())
            .sum()
    }

    fn card(&self, index: usize) -> Option<(&FactoryView, &Value)> {
        let mut offset = 0;
        for factory in &self.factories {
            if index < offset + factory.cards.len() {
                return Some((factory, &factory.cards[index - offset]));
            }
            offset += factory.cards.len();
        }
        None
    }

    fn selected_identity(&self) -> Option<(String, String)> {
        self.card(self.selected).map(|(factory, card)| {
            (
                factory.route.clone().unwrap_or_default(),
                text(card, "owner").to_owned(),
            )
        })
    }

    fn update(&mut self, index: usize, payload: Value) -> Result<()> {
        let selected = self.selected_identity();
        let dashboard = &payload["dashboard"];
        let cards = dashboard["cards"]
            .as_array()
            .context("Server does not provide the dashboard model; update ssf-server")?;
        let factory = self
            .factories
            .get_mut(index)
            .context("status arrived for an unknown server")?;
        factory.cards = cards.clone();
        factory.monitored_items = dashboard["monitored_items"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let hostname = payload["server"]["hostname"].as_str().unwrap_or_default();
        let vm = payload["host_vm"]["name"].as_str().unwrap_or_default();
        let reported = match (hostname.is_empty(), vm.is_empty()) {
            (false, false) => format!("VM {vm} · {hostname}"),
            (false, true) => hostname.to_owned(),
            (true, false) => format!("VM {vm}"),
            (true, true) => factory.name.clone(),
        };
        factory.name = match factory.route.as_deref() {
            Some(route) if route != reported => format!("{reported} · {route}"),
            _ => reported,
        };
        factory.warning = dashboard["warning"].as_str().map(str::to_owned);
        factory.received = Some(Instant::now());
        factory.error = None;

        if let Some((route, owner)) = selected {
            let mut offset = 0;
            for candidate in &self.factories {
                if candidate.route.as_deref().unwrap_or_default() == route
                    && let Some(position) = candidate
                        .cards
                        .iter()
                        .position(|card| text(card, "owner") == owner)
                {
                    self.selected = offset + position;
                    break;
                }
                offset += candidate.cards.len();
            }
        }
        self.selected = self.selected.min(self.card_count().saturating_sub(1));
        self.first = self.first.min(self.selected);
        Ok(())
    }

    fn transport_error(&mut self, index: usize, error: String) {
        if let Some(factory) = self.factories.get_mut(index) {
            factory.error = Some(error);
        }
    }

    fn last_visible(&self) -> usize {
        self.hitboxes
            .iter()
            .map(|(_, index)| *index)
            .max()
            .unwrap_or(self.first)
    }

    fn select(&mut self, delta: isize) {
        let old = self.selected;
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.card_count().saturating_sub(1));
        if self.selected < self.first
            || (self.selected != old && self.selected > self.last_visible())
        {
            self.first = self.selected;
        }
    }

    fn page(&mut self, direction: isize) {
        self.select(direction.saturating_mul(self.hitboxes.len().max(1) as isize));
    }

    fn mouse_card(&self, column: u16, row: u16) -> Option<usize> {
        self.hitboxes
            .iter()
            .find(|(area, _)| {
                column >= area.x
                    && column < area.x + area.width
                    && row >= area.y
                    && row < area.y + area.height
            })
            .map(|(_, index)| *index)
    }
}

fn text<'a>(value: &'a Value, field: &str) -> &'a str {
    value[field].as_str().unwrap_or("—")
}

fn clean(input: &str) -> String {
    input
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn state_color(state: &str) -> Color {
    match state {
        "working" => Color::LightGreen,
        "blocked" => Color::LightRed,
        "waiting" | "idle" | "done" => Color::Yellow,
        _ => Color::Gray,
    }
}

fn column_count(width: u16) -> usize {
    match width {
        150.. => 3,
        92.. => 2,
        _ => 1,
    }
}

fn safe_link(value: &Value, x: u16, y: u16, max_width: u16) -> Option<TerminalLink> {
    let label = clean(text(value, "id"));
    let raw_url = text(value, "url");
    if raw_url.chars().any(char::is_control) {
        return None;
    }
    let url = reqwest::Url::parse(raw_url).ok()?;
    if label == "—"
        || UnicodeWidthStr::width(label.as_str()) > usize::from(max_width)
        || !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
    {
        return None;
    }
    Some(TerminalLink {
        x,
        y,
        label,
        url: url.to_string(),
    })
}

fn render_card(
    frame: &mut Frame<'_>,
    area: Rect,
    card: &Value,
    selected: bool,
    compact: bool,
) -> Vec<TerminalLink> {
    let state = clean(text(card, "agent_state"));
    let color = state_color(&state);
    let owner = clean(text(card, "owner"));
    let border = if selected {
        Color::LightCyan
    } else {
        Color::DarkGray
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .title(Line::from(vec![
            Span::styled(
                format!(" {} ", state.to_uppercase()),
                Style::default()
                    .fg(Color::Black)
                    .bg(color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {owner} "),
                Style::default().fg(if selected {
                    Color::LightCyan
                } else {
                    Color::White
                }),
            ),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut links = safe_link(&card["origin"], inner.x, inner.y + 1, inner.width)
        .into_iter()
        .collect::<Vec<_>>();

    let origin = &card["origin"];
    let stack = [text(card, "harness"), text(card, "model")]
        .into_iter()
        .filter(|part| *part != "—" && !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    let mut lines = vec![
        Line::from(Span::styled(
            clean(text(origin, "title")),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled(
                clean(text(origin, "id")),
                Style::default().fg(Color::LightCyan),
            ),
            Span::raw(format!("  {stack}")),
        ]),
        Line::from(vec![
            Span::styled("Last active  ", Style::default().fg(Color::DarkGray)),
            Span::raw(clean(text(card, "last_activity_at"))),
        ]),
    ];
    if !compact {
        lines.push(Line::from(vec![
            Span::styled("Latest       ", Style::default().fg(Color::DarkGray)),
            Span::raw(clean(text(card, "last_assistant_message"))),
        ]));
        let additional = card["additional"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .map(|item| clean(text(item, "id")))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        lines.push(Line::from(vec![
            Span::styled("Assigned     ", Style::default().fg(Color::DarkGray)),
            Span::raw(if additional.is_empty() {
                "none".into()
            } else {
                additional
            }),
        ]));
        let mut x = inner.x.saturating_add(13);
        for item in card["additional"].as_array().into_iter().flatten() {
            if let Some(link) = safe_link(item, x, inner.y + 4, inner.right().saturating_sub(x)) {
                x = x.saturating_add(UnicodeWidthStr::width(link.label.as_str()) as u16 + 2);
                links.push(link);
            }
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
    links
}

fn render_compact(frame: &mut Frame<'_>, area: Rect, view: &mut View) {
    let mut lines = vec![Line::from(Span::styled(
        "SSF factory · compact view",
        Style::default()
            .fg(Color::LightCyan)
            .add_modifier(Modifier::BOLD),
    ))];
    let now = Instant::now();
    let mut global = 0;
    for factory in &view.factories {
        let (status, color) = factory.status(now);
        lines.push(Line::from(vec![
            Span::styled(
                format!("{}  ", clean(&factory.name)),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(clean(&status), Style::default().fg(color)),
        ]));
        for card in &factory.cards {
            let marker = if global == view.selected { "›" } else { " " };
            lines.push(Line::from(format!(
                "{marker} {}  {}  {}",
                clean(text(&card["origin"], "id")),
                clean(text(card, "agent_state")),
                clean(text(&card["origin"], "title"))
            )));
            global += 1;
        }
        if !factory.monitored_items.is_empty() {
            lines.push(Line::from(format!(
                "  monitored without agent: {}",
                factory
                    .monitored_items
                    .iter()
                    .map(|item| clean(text(item, "id")))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), area);
    view.hitboxes.clear();
    view.links.clear();
    view.columns = 1;
}

fn render(frame: &mut Frame<'_>, view: &mut View) {
    let area = frame.area();
    if area.width < 48 || area.height < 12 {
        render_compact(frame, area, view);
        return;
    }
    let [header, main, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Fill(1),
        Constraint::Length(2),
    ])
    .areas(area);
    let total = view.card_count();
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " SSF FACTORY ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::LightCyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    "  {total} live agent{} · {} server{}",
                    if total == 1 { "" } else { "s" },
                    view.factories.len(),
                    if view.factories.len() == 1 { "" } else { "s" }
                ),
                Style::default().fg(Color::Gray),
            ),
        ])),
        header,
    );

    let content = main.inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    let columns = column_count(content.width);
    view.columns = columns;
    view.hitboxes.clear();
    view.links.clear();
    let compact_cards = content.height < 18;
    let card_height = if compact_cards {
        COMPACT_CARD_HEIGHT
    } else {
        CARD_HEIGHT
    };
    let mut y = content.y;
    let bottom = content.y + content.height;
    let mut global = 0;
    let now = Instant::now();
    let mut rendered_any_factory = false;

    for factory in &view.factories {
        let start = global;
        let end = start + factory.cards.len();
        global = end;
        if end <= view.first && total > 0 {
            continue;
        }
        if y >= bottom {
            break;
        }
        rendered_any_factory = true;
        let (status, status_color) = factory.status(now);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!("◆ {}", clean(&factory.name)),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  {status}"), Style::default().fg(status_color)),
            ])),
            Rect::new(content.x, y, content.width, 1),
        );
        y += 1;

        if factory.cards.is_empty()
            && factory.received.is_some()
            && factory.warning.is_none()
            && factory.error.is_none()
            && y < bottom
        {
            frame.render_widget(
                Paragraph::new("No live agents").style(Style::default().fg(Color::DarkGray)),
                Rect::new(content.x, y, content.width, 1),
            );
            y += 1;
        }

        let first_local = view.first.saturating_sub(start).min(factory.cards.len());
        let locals: Vec<_> = (first_local..factory.cards.len()).collect();
        for chunk in locals.chunks(columns) {
            if y + card_height > bottom {
                break;
            }
            let row = Rect::new(content.x, y, content.width, card_height);
            let constraints = vec![Constraint::Ratio(1, columns as u32); columns];
            let cells = Layout::horizontal(constraints).spacing(1).split(row);
            for (column, local) in chunk.iter().enumerate() {
                let index = start + *local;
                let cell = cells[column];
                view.links.extend(render_card(
                    frame,
                    cell,
                    &factory.cards[*local],
                    index == view.selected,
                    compact_cards,
                ));
                view.hitboxes.push((cell, index));
            }
            y += card_height + 1;
        }
        if !factory.monitored_items.is_empty() && y < bottom {
            let items = factory
                .monitored_items
                .iter()
                .map(|item| clean(text(item, "id")))
                .collect::<Vec<_>>()
                .join(", ");
            let width = usize::from(content.width.max(1));
            let display_width = UnicodeWidthStr::width("Monitored without an agent  ")
                + UnicodeWidthStr::width(items.as_str());
            let monitored = Paragraph::new(Line::from(vec![
                Span::styled(
                    "Monitored without an agent  ",
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(items),
            ]))
            .wrap(Wrap { trim: true });
            let height = display_width
                .div_ceil(width)
                .saturating_add(1)
                .min(usize::from(bottom - y)) as u16;
            frame.render_widget(monitored, Rect::new(content.x, y, content.width, height));
            y += height;
        }
    }

    if !rendered_any_factory {
        frame.render_widget(Paragraph::new("No server status to display"), content);
    }

    let notice = if view.notice.is_empty() {
        if herdr::available() {
            "Enter/click: focus in this Herdr server"
        } else {
            "Standalone terminal · pane navigation requires Herdr"
        }
    } else {
        &view.notice
    };
    frame.render_widget(
        Paragraph::new(clean(notice)).style(Style::default().fg(Color::DarkGray)),
        Rect::new(footer.x, footer.y, footer.width, 1),
    );
    frame.render_widget(
        Paragraph::new("←/→/↑/↓ or h/j/k/l select · PgUp/PgDn page · Enter focus · q quit")
            .style(Style::default().fg(Color::DarkGray)),
        Rect::new(footer.x, footer.y + 1, footer.width, 1),
    );
}

pub(crate) async fn run(servers: Vec<String>) -> Result<()> {
    let routes: Vec<Option<String>> = if servers.is_empty() {
        vec![None]
    } else {
        servers.into_iter().map(Some).collect()
    };
    let mut terminal = TerminalSession::enter()?;
    let (sender, mut snapshots) = tokio::sync::mpsc::channel(routes.len().max(1));
    let mut workers = Vec::new();
    for (index, route) in routes.iter().cloned().enumerate() {
        let mut source = crate::dashboard_transport::StatusSource::new(route)?;
        let sender = sender.clone();
        workers.push(Worker::new(tokio::spawn(async move {
            loop {
                let result = source
                    .next_snapshot()
                    .await
                    .map_err(|error| format!("{error:#}"));
                let failed = result.is_err();
                if sender.send((index, result)).await.is_err() {
                    break;
                }
                if failed {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        })));
    }
    drop(sender);
    let (focus_sender, mut focus_results) = tokio::sync::mpsc::channel(1);
    let mut focus_worker: Option<Worker> = None;
    let mut view = View::new(routes);
    let mut ticks = tokio::time::interval(Duration::from_millis(50));
    let mut termination =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    terminal.draw(&mut view)?;
    'dashboard: loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = termination.recv() => break,
            _ = hangup.recv() => break,
            result = snapshots.recv() => {
                match result {
                    Some((index, Ok(payload))) => if let Err(error) = view.update(index, payload) { view.transport_error(index, error.to_string()); },
                    Some((index, Err(error))) => view.transport_error(index, error),
                    None => bail!("Dashboard refresh workers stopped"),
                }
            }
            Some(result) = focus_results.recv() => { view.notice = result; focus_worker = None; }
            _ = ticks.tick() => {
                for _ in 0..32 {
                    if !event::poll(Duration::ZERO)? { break; }
                    let mut activate = false;
                    match event::read()? {
                        Event::Key(key) if key.kind != event::KeyEventKind::Release => match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => break 'dashboard,
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break 'dashboard,
                            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('j') => view.select(1),
                            KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('k') => view.select(-1),
                            KeyCode::Down => view.select(view.columns as isize),
                            KeyCode::Up => view.select(-(view.columns as isize)),
                            KeyCode::PageDown => view.page(1),
                            KeyCode::PageUp => view.page(-1),
                            KeyCode::Home => { view.selected = 0; view.first = 0; },
                            KeyCode::End => { view.selected = view.card_count().saturating_sub(1); view.first = view.selected; },
                            KeyCode::Enter => activate = true,
                            _ => {},
                        },
                        Event::Mouse(mouse) => match mouse.kind {
                            MouseEventKind::ScrollDown => view.select(view.columns as isize),
                            MouseEventKind::ScrollUp => view.select(-(view.columns as isize)),
                            MouseEventKind::Down(MouseButton::Left) => if let Some(index) = view.mouse_card(mouse.column, mouse.row) { view.selected = index; activate = true; },
                            _ => {},
                        },
                        _ => {},
                    }
                    if activate && focus_worker.is_none()
                        && let Some((_, card)) = view.card(view.selected)
                    {
                        let session = card["agent_session_id"].as_str().unwrap_or_default().to_owned();
                        let harness = card["harness"].as_str().unwrap_or_default().to_owned();
                        let sender = focus_sender.clone();
                        view.notice = "Finding agent on this Herdr server…".into();
                        focus_worker = Some(Worker::new(tokio::spawn(async move {
                            let message = herdr::focus(&session, &harness)
                                .await
                                .unwrap_or_else(|error| format!("{error:#}"));
                            let _ = sender.send(message).await;
                        })));
                    }
                }
            }
        }
        terminal.draw(&mut view)?;
    }
    for worker in workers {
        worker.stop().await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use serde_json::json;

    fn card(owner: &str, state: &str, title: &str) -> Value {
        json!({
            "owner":owner,
            "origin":{"id":owner,"title":title,"url":"https://example.test/issue"},
            "agent_state":state,
            "last_activity_at":"2026-09-12T20:00:00Z",
            "last_assistant_message":"Latest summary with useful detail",
            "additional":[{"id":"r#99","title":"Assigned","url":"https://example.test/assigned"}],
            "agent_session_id":format!("session-{owner}"),
            "harness":"codex","model":"gpt-5"
        })
    }

    fn payload(server: &str, cards: Vec<Value>, monitored: Vec<Value>) -> Value {
        json!({
            "server":{"hostname":server},
            "dashboard":{"cards":cards,"monitored_items":monitored,"warning":null}
        })
    }

    fn rendered(view: &mut View, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, view)).unwrap();
        terminal.backend().to_string()
    }

    #[test]
    fn adaptive_grid_uses_one_two_and_three_columns() {
        for (width, expected) in [(70, 1), (110, 2), (180, 3)] {
            let mut view = View::new(vec![None]);
            view.update(
                0,
                payload(
                    "factory-a",
                    (1..=4)
                        .map(|n| card(&format!("r#{n}"), "working", "An agent card"))
                        .collect(),
                    vec![],
                ),
            )
            .unwrap();
            let screen = rendered(&mut view, width, 35);
            assert_eq!(view.columns, expected, "{width} columns\n{screen}");
            let first_row = view.hitboxes.first().unwrap().0.y;
            assert_eq!(
                view.hitboxes
                    .iter()
                    .take(expected)
                    .filter(|(area, _)| area.y == first_row)
                    .count(),
                expected,
                "{screen}"
            );
            assert!(screen.contains('╭') && screen.contains('╯'));
        }
    }

    #[test]
    fn states_servers_and_monitored_work_render_from_the_model() {
        let mut view = View::new(vec![Some("one.example".into()), Some("two.example".into())]);
        view.update(
            0,
            payload(
                "factory-one",
                vec![card("r#1", "working", "Build the API")],
                vec![json!({"id":"r#10","title":"Queued"})],
            ),
        )
        .unwrap();
        view.update(
            1,
            payload(
                "factory-two",
                vec![card("r#2", "blocked", "Deploy safely")],
                vec![],
            ),
        )
        .unwrap();
        let screen = rendered(&mut view, 160, 38);
        for expected in [
            "factory-one · one.example",
            "factory-two · two.example",
            "WORKING",
            "BLOCKED",
            "Build the API",
            "Deploy safely",
            "Monitored without an agent",
            "r#10",
        ] {
            assert!(screen.contains(expected), "missing {expected:?}\n{screen}");
        }
        assert_eq!(view.links.len(), 4);
    }

    #[test]
    fn compact_and_long_unicode_content_are_bounded_and_safe() {
        let mut view = View::new(vec![None]);
        view.update(
            0,
            payload(
                "工場-界",
                vec![card(
                    "r#界",
                    "waiting",
                    "A very long 界 title with\ncontrol\u{1b}[31m content that must wrap safely",
                )],
                vec![],
            ),
        )
        .unwrap();
        for (width, height) in [(30, 8), (47, 11), (60, 14), (100, 24)] {
            let screen = rendered(&mut view, width, height);
            assert!(!screen.contains('\u{1b}'));
            assert_eq!(screen.lines().count(), usize::from(height));
        }
    }

    #[test]
    fn terminal_links_accept_only_visible_http_issue_ids() {
        let valid = json!({"id":"r#1","url":"https://example.test/issues/1"});
        assert!(safe_link(&valid, 1, 2, 10).is_some());
        assert!(safe_link(&valid, 1, 2, 2).is_none());
        assert!(
            safe_link(
                &json!({"id":"r#1","url":"https://example.test/\u{1b}]8;;bad"}),
                1,
                2,
                10
            )
            .is_none()
        );
        assert!(
            safe_link(
                &json!({"id":"r#1","url":"file:///tmp/not-allowed"}),
                1,
                2,
                10
            )
            .is_none()
        );
    }

    #[test]
    fn resize_reordering_and_mouse_keep_selection_correct() {
        let mut view = View::new(vec![None]);
        view.update(
            0,
            payload(
                "factory",
                vec![card("r#1", "idle", "One"), card("r#2", "working", "Two")],
                vec![],
            ),
        )
        .unwrap();
        view.select(1);
        let _ = rendered(&mut view, 80, 28);
        assert_eq!(text(view.card(view.selected).unwrap().1, "owner"), "r#2");
        let selected_area = view
            .hitboxes
            .iter()
            .find(|(_, index)| *index == view.selected)
            .unwrap()
            .0;
        assert_eq!(
            view.mouse_card(selected_area.x + 1, selected_area.y + 1),
            Some(view.selected)
        );
        view.update(
            0,
            payload(
                "factory",
                vec![card("r#2", "working", "Two"), card("r#1", "idle", "One")],
                vec![],
            ),
        )
        .unwrap();
        let wide = rendered(&mut view, 170, 30);
        assert_eq!(text(view.card(view.selected).unwrap().1, "owner"), "r#2");
        assert!(wide.contains("Two"));
    }

    #[test]
    fn per_server_errors_do_not_replace_other_server_cards() {
        let mut view = View::new(vec![Some("one".into()), Some("two".into())]);
        view.update(
            0,
            payload("one", vec![card("r#1", "working", "Healthy")], vec![]),
        )
        .unwrap();
        view.update(1, payload("two", vec![], vec![])).unwrap();
        view.transport_error(1, "connection refused".into());
        let screen = rendered(&mut view, 120, 28);
        assert!(screen.contains("Healthy"));
        assert!(screen.contains("connection refused"));
    }

    #[test]
    fn unavailable_empty_server_is_not_presented_as_a_healthy_empty_factory() {
        let mut view = View::new(vec![None]);
        let mut unavailable = payload("factory", vec![], vec![]);
        unavailable["dashboard"]["warning"] = json!("VM stopped");
        view.update(0, unavailable).unwrap();
        let screen = rendered(&mut view, 100, 24);
        assert!(screen.contains("UNAVAILABLE / STALE"));
        assert!(screen.contains("VM stopped"));
        assert!(!screen.contains("No live agents"));
    }
}
