use std::collections::{HashSet, VecDeque};
use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, Event as TermEvent, KeyCode, KeyEventKind, MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Terminal;

use hyper_lib::address::{AddressRegistry, NetworkType};
use hyper_lib::node::{Event, GetaddrCacheAlgorithm, NetworkMessage, NodeId};
use hyper_lib::simulator::Simulator;
use hyper_lib::StartMode;

const EVENT_LOG_CAP: usize = 500;
const TICK_MS: u64 = 10;
/// How long (wall-clock seconds) a departed node stays visible as a ghost in the node list.
const DEPARTED_DISPLAY_SECS: u64 = 5;

/// Whether an event is an internal scheduling step or an actual network delivery.
#[derive(Clone, Copy, PartialEq)]
enum EventKind {
    /// Internal bookkeeping: NodeJoin, NodeLeave, SelfAnnounce.
    Internal,
    /// A message arrives at a node and its receive handler runs (addrman may change).
    Delivery,
}

#[derive(Clone, Copy, PartialEq)]
enum Focus {
    NodeList,
    Detail,
    NextMessage,
}

pub struct App {
    simulator: Simulator,
    event_log: VecDeque<(u64, EventKind, String)>,
    node_list: Vec<NodeId>,
    filtered_list: Vec<NodeId>,
    selected: usize,
    list_offset: usize,
    detail_scroll: usize,
    next_msg_scroll: usize,
    search_query: String,
    search_active: bool,
    running: bool,
    focus: Focus,
    // Nodes that joined/left in the most recent step — cleared at next step.
    recently_joined: HashSet<NodeId>,
    recently_left: HashSet<NodeId>,
    // Recently departed nodes shown as ghosts in the list: (id, type_str, wall-time of departure).
    // Entries are removed after DEPARTED_DISPLAY_SECS seconds of real time.
    departed_display: VecDeque<(NodeId, &'static str, Instant)>,
    // Panel rects — updated each draw, used for mouse hit-testing.
    rect_node_list: Rect,
    rect_detail: Rect,
    rect_next_msg: Rect,
    rect_event_log: Rect,
}

impl App {
    pub fn new(simulator: Simulator) -> Self {
        let node_list: Vec<NodeId> = {
            let mut ids: Vec<NodeId> = simulator.network.nodes.keys().copied().collect();
            ids.sort();
            ids
        };
        let filtered_list = node_list.clone();
        App {
            simulator,
            event_log: VecDeque::<(u64, EventKind, String)>::new(),
            node_list,
            filtered_list,
            selected: 0,
            list_offset: 0,
            detail_scroll: 0,
            next_msg_scroll: 0,
            search_query: String::new(),
            search_active: false,
            running: false,
            focus: Focus::NodeList,
            recently_joined: HashSet::new(),
            recently_left: HashSet::new(),
            departed_display: VecDeque::new(),
            rect_node_list: Rect::default(),
            rect_detail: Rect::default(),
            rect_next_msg: Rect::default(),
            rect_event_log: Rect::default(),
        }
    }

    fn step(&mut self) {
        // Snapshot node set before the step so we can detect joins/leaves.
        // Also capture type strings now — departed nodes are gone after step().
        let before: HashSet<NodeId> = self.simulator.network.nodes.keys().copied().collect();
        let before_types: std::collections::HashMap<NodeId, &'static str> =
            before.iter().map(|&id| (id, self.node_type_str(id))).collect();

        if let Some((event, at)) = self.simulator.step() {
            let after: HashSet<NodeId> = self.simulator.network.nodes.keys().copied().collect();
            self.recently_joined = after.difference(&before).copied().collect();
            self.recently_left = before.difference(&after).copied().collect();

            // Add departed nodes to the ghost display list.
            let now_wall = Instant::now();
            for &id in &self.recently_left {
                let type_str = before_types.get(&id).copied().unwrap_or("?");
                self.departed_display.push_back((id, type_str, now_wall));
            }
            // Prune ghosts older than DEPARTED_DISPLAY_SECS.
            let cutoff = Duration::from_secs(DEPARTED_DISPLAY_SECS);
            while let Some(&(_, _, t)) = self.departed_display.front() {
                if now_wall.duration_since(t) > cutoff {
                    self.departed_display.pop_front();
                } else {
                    break;
                }
            }

            let kind = match &event {
                Event::SendMessage { .. } => EventKind::Delivery,
                _ => EventKind::Internal,
            };

            // Augment NodeJoin with the new node's ID, type, and outbound count
            // (the event itself carries no node_id — it's assigned inside add_node).
            let desc = match &event {
                Event::NodeJoin { .. } => {
                    if let Some(&id) = self.recently_joined.iter().next() {
                        let type_str = self.node_type_str(id);
                        let out = self.simulator.network.nodes.get(&id)
                            .map(|n| n.out_peers.len())
                            .unwrap_or(0);
                        format!("node join  node={id} ({type_str})  {out} outbound [GETADDR queued]")
                    } else {
                        "node join".to_string()
                    }
                }
                _ => event_description(&event, &self.simulator.network.registry),
            };

            if self.event_log.len() >= EVENT_LOG_CAP {
                self.event_log.pop_front();
            }
            self.event_log.push_back((at, kind, desc));
            self.next_msg_scroll = 0;

            // Refresh node list after potential node joins/leaves.
            let mut ids: Vec<NodeId> = self.simulator.network.nodes.keys().copied().collect();
            ids.sort();
            self.node_list = ids;
            self.apply_filter();
            if !self.filtered_list.is_empty() && self.selected >= self.filtered_list.len() {
                self.selected = self.filtered_list.len() - 1;
            }
        }
    }

    fn apply_filter(&mut self) {
        if self.search_query.is_empty() {
            self.filtered_list = self.node_list.clone();
        } else {
            let q = self.search_query.to_lowercase();
            self.filtered_list = self.node_list
                .iter()
                .filter(|&&id| {
                    let type_str = self.node_type_str(id);
                    format!("{id}").contains(&q) || type_str.contains(&q)
                })
                .copied()
                .collect();
        }
        if self.selected >= self.filtered_list.len() && !self.filtered_list.is_empty() {
            self.selected = self.filtered_list.len() - 1;
        }
    }

    fn node_type_str(&self, id: NodeId) -> &'static str {
        match self.simulator.network.nodes.get(&id) {
            Some(node) => {
                let has_onion = node.addresses.iter().any(|a| a.network == NetworkType::Onion);
                let has_clear = node.addresses.iter().any(|a| a.network == NetworkType::Clearnet);
                match (has_onion, has_clear) {
                    (true, true) => "dual",
                    (true, false) => "onion",
                    _ => "clear",
                }
            }
            None => "?",
        }
    }

    fn scroll_list_to_show_selected(&mut self, visible_rows: usize) {
        if self.selected < self.list_offset {
            self.list_offset = self.selected;
        } else if self.selected >= self.list_offset + visible_rows {
            self.list_offset = self.selected + 1 - visible_rows;
        }
    }

    /// Return which panel contains the terminal cell (col, row), if any.
    fn panel_at(&self, col: u16, row: u16) -> Option<Focus> {
        if rect_contains(self.rect_node_list, col, row) {
            Some(Focus::NodeList)
        } else if rect_contains(self.rect_detail, col, row) {
            Some(Focus::Detail)
        } else if rect_contains(self.rect_next_msg, col, row) {
            Some(Focus::NextMessage)
        } else {
            None
        }
    }
}

fn rect_contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height
}

fn fmt_addr(addr: &hyper_lib::address::AddressId, reg: &AddressRegistry) -> String {
    let net = match addr.network {
        NetworkType::Onion => "onion",
        NetworkType::Clearnet => "clear",
    };
    let node_id = reg.addresses.get(addr).map(|a| a.owner_node.to_string()).unwrap_or_else(|| "?".to_string());
    format!("{}({})", node_id, net)
}

fn event_description(event: &Event, reg: &AddressRegistry) -> String {
    match event {
        Event::NodeJoin { .. } => "node join".to_string(),
        Event::NodeLeave { node_id, .. } => format!("node leave  node={}", node_id),
        Event::NodeReconnect { node_id, network, .. } => {
            let net = match network {
                NetworkType::Onion => "onion",
                NetworkType::Clearnet => "clear",
            };
            format!("reconnect  node={} net={}", node_id, net)
        }
        Event::SelfAnnounce { node_id, peer_addr, .. } => {
            format!("announce-timer  node={} → {}", node_id, fmt_addr(peer_addr, reg))
        }
        Event::SendMessage { from, to, msg, .. } => match msg {
            NetworkMessage::GetAddr => format!("GetAddr  {} → {}", fmt_addr(from, reg), fmt_addr(to, reg)),
            NetworkMessage::Addr(v) if v.is_empty() => {
                format!("getaddr-reply(0) [empty addrman]  {} → {}", fmt_addr(from, reg), fmt_addr(to, reg))
            }
            NetworkMessage::Addr(v) => {
                format!("getaddr-reply({})  {} → {}", v.len(), fmt_addr(from, reg), fmt_addr(to, reg))
            }
            NetworkMessage::AddrAnnounce(v) => {
                let is_self = v.first().map_or(false, |p| p.address == *from);
                if is_self {
                    format!("self-announce  {} → {}", fmt_addr(from, reg), fmt_addr(to, reg))
                } else {
                    format!("relay-announce({})  {} → {}", v.len(), fmt_addr(from, reg), fmt_addr(to, reg))
                }
            }
        },
    }
}

fn format_time(unix: u64) -> String {
    let days = unix / 86400;
    let rem = unix % 86400;
    let h = rem / 3600;
    let m = (rem % 3600) / 60;
    let s = rem % 60;
    format!("day {} {:02}:{:02}:{:02}", days, h, m, s)
}

fn age_str(now: u64, ts: u64) -> String {
    if ts > now {
        return "future".to_string();
    }
    let diff = now - ts;
    let days = diff / 86400;
    let hours = (diff % 86400) / 3600;
    let mins = (diff % 3600) / 60;
    if days > 0 {
        format!("{}d {}h", days, hours)
    } else if hours > 0 {
        format!("{}h {}m", hours, mins)
    } else {
        format!("{}m", mins)
    }
}

pub fn run(simulator: Simulator) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    // Enable only basic button-press/release (?1000h) with SGR extended
    // coordinates (?1006h).  Deliberately omit ?1002h (button-motion) and
    // ?1003h (all-motion) — those flood the queue with Moved events that
    // some terminals encode ambiguously, producing spurious Down matches.
    {
        use io::Write;
        write!(stdout, "\x1b[?1000h\x1b[?1006h")?;
        stdout.flush()?;
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_inner(&mut terminal, simulator);

    disable_raw_mode()?;
    {
        use io::Write;
        let mut out = io::stdout();
        write!(out, "\x1b[?1006l\x1b[?1000l")?;
        out.flush()?;
    }
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    result
}

fn run_inner(
    terminal: &mut ratatui::Terminal<CrosstermBackend<io::Stdout>>,
    simulator: Simulator,
) -> anyhow::Result<()> {
    let mut app = App::new(simulator);
    let tick = Duration::from_millis(TICK_MS);

    loop {
        terminal.draw(|f| draw(f, &mut app))?;

        if event::poll(tick)? {
            match event::read()? {
                TermEvent::Key(key) => {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    if app.search_active {
                        match key.code {
                            KeyCode::Esc | KeyCode::Enter => {
                                app.search_active = false;
                            }
                            KeyCode::Backspace => {
                                app.search_query.pop();
                                app.apply_filter();
                            }
                            KeyCode::Char(c) => {
                                app.search_query.push(c);
                                app.apply_filter();
                            }
                            _ => {}
                        }
                    } else {
                        match key.code {
                            KeyCode::Char('q') => break,
                            KeyCode::Char(' ') => app.step(),
                            KeyCode::Char('r') => app.running = !app.running,
                            KeyCode::Char('/') => {
                                app.search_active = true;
                                app.search_query.clear();
                                app.apply_filter();
                            }
                            KeyCode::Tab => {
                                app.focus = match app.focus {
                                    Focus::NodeList => Focus::Detail,
                                    Focus::Detail => Focus::NextMessage,
                                    Focus::NextMessage => Focus::NodeList,
                                };
                            }
                            KeyCode::Up => match app.focus {
                                Focus::NodeList => {
                                    if app.selected > 0 {
                                        app.selected -= 1;
                                    }
                                }
                                Focus::Detail => {
                                    if app.detail_scroll > 0 {
                                        app.detail_scroll -= 1;
                                    }
                                }
                                Focus::NextMessage => {
                                    if app.next_msg_scroll > 0 {
                                        app.next_msg_scroll -= 1;
                                    }
                                }
                            },
                            KeyCode::Down => match app.focus {
                                Focus::NodeList => {
                                    if app.selected + 1 < app.filtered_list.len() {
                                        app.selected += 1;
                                    }
                                }
                                Focus::Detail => {
                                    app.detail_scroll += 1;
                                }
                                Focus::NextMessage => {
                                    app.next_msg_scroll += 1;
                                }
                            },
                            _ => {}
                        }
                    }
                }
                TermEvent::Mouse(mouse) => {
                    let col = mouse.column;
                    let row = mouse.row;
                    match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            if let Some(panel) = app.panel_at(col, row) {
                                app.focus = panel;
                            }
                        }
                        MouseEventKind::ScrollUp => {
                            match app.panel_at(col, row) {
                                Some(Focus::NodeList) | None => {
                                    if app.selected > 0 {
                                        app.selected -= 1;
                                    }
                                }
                                Some(Focus::Detail) => {
                                    if app.detail_scroll > 0 {
                                        app.detail_scroll -= 1;
                                    }
                                }
                                Some(Focus::NextMessage) => {
                                    if app.next_msg_scroll > 0 {
                                        app.next_msg_scroll -= 1;
                                    }
                                }
                            }
                        }
                        MouseEventKind::ScrollDown => {
                            match app.panel_at(col, row) {
                                Some(Focus::NodeList) | None => {
                                    if app.selected + 1 < app.filtered_list.len() {
                                        app.selected += 1;
                                    }
                                }
                                Some(Focus::Detail) => {
                                    app.detail_scroll += 1;
                                }
                                Some(Focus::NextMessage) => {
                                    app.next_msg_scroll += 1;
                                }
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        if app.running {
            app.step();
        }
    }

    Ok(())
}

fn draw(f: &mut ratatui::Frame, app: &mut App) {
    let area = f.area();

    // Outer layout: top_bar | params_bar | main | event_panel | help_bar
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(10),
            Constraint::Length(1),
        ])
        .split(area);

    draw_top_bar(f, app, outer[0]);
    draw_params_bar(f, app, outer[1]);

    // Main: node list | node detail
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(30), Constraint::Min(0)])
        .split(outer[2]);

    app.rect_node_list = main[0];
    app.rect_detail = main[1];

    draw_node_list(f, app, main[0]);
    draw_node_detail(f, app, main[1]);

    // Event area: log on the left, next-message detail on the right
    let event_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(44)])
        .split(outer[3]);

    app.rect_event_log = event_cols[0];
    app.rect_next_msg = event_cols[1];

    draw_event_panel(f, app, event_cols[0]);
    draw_next_message_panel(f, app, event_cols[1]);
    draw_help_bar(f, app, outer[4]);
}

fn draw_top_bar(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let sim_time = if app.simulator.start_time > 0 {
        let now = app.event_log.back().map(|(t, _, _)| *t).unwrap_or(app.simulator.start_time);
        format_time(now.saturating_sub(app.simulator.start_time))
    } else {
        "—".to_string()
    };
    let queue_depth = app.simulator.event_queue.len();
    let title = format!(
        " HYPERION   Time: {}   Events: {}   Logged: {} ",
        sim_time, queue_depth, app.event_log.len()
    );
    let style = Style::default()
        .fg(Color::Black)
        .bg(Color::LightCyan)
        .add_modifier(Modifier::BOLD);
    f.render_widget(Paragraph::new(title).style(style), area);
}

fn draw_params_bar(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let c = &app.simulator.config;
    let algo = match c.cache_algo {
        GetaddrCacheAlgorithm::Current => "current",
        GetaddrCacheAlgorithm::FixedOffset => "fixed-offset",
        GetaddrCacheAlgorithm::NetworkBased => "network-based",
    };
    let start = match c.start_mode {
        StartMode::Warm => "warm",
        StartMode::Cold => "cold",
        StartMode::Peers => "peers",
        StartMode::Dns => "dns",
    };
    let total = c.onion + c.clearnet + c.dual_stack;
    let text = format!(
        " algo: {}   nodes: {} (onion={} clear={} dual={})   start: {}   days: {}   churn: +{}/−{}/day ",
        algo, total, c.onion, c.clearnet, c.dual_stack, start, c.days, c.joins_per_day, c.leaves_per_day,
    );
    let style = Style::default().fg(Color::DarkGray);
    f.render_widget(Paragraph::new(text).style(style), area);
}

fn draw_node_list(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::NodeList;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };

    let title = if app.search_active {
        format!(" NODES [/{}] ", app.search_query)
    } else if !app.search_query.is_empty() {
        format!(" NODES [{}] ", app.search_query)
    } else {
        " NODES [/search] ".to_string()
    };

    let inner_height = area.height.saturating_sub(2) as usize;
    app.scroll_list_to_show_selected(inner_height);

    // Prune departed ghosts that have expired (done here so it works even when paused).
    let cutoff = Duration::from_secs(DEPARTED_DISPLAY_SECS);
    let now_wall = Instant::now();
    while let Some(&(_, _, t)) = app.departed_display.front() {
        if now_wall.duration_since(t) > cutoff {
            app.departed_display.pop_front();
        } else {
            break;
        }
    }

    // Build departed ghost rows (may be fewer than departed_display if space runs out).
    let departed_snapshot: Vec<(NodeId, &'static str)> = app
        .departed_display
        .iter()
        .map(|&(id, type_str, _)| (id, type_str))
        .collect();

    let live_items: Vec<ListItem> = app
        .filtered_list
        .iter()
        .skip(app.list_offset)
        .take(inner_height)
        .enumerate()
        .map(|(i, &id)| {
            let abs_idx = i + app.list_offset;
            let type_str = app.node_type_str(id);
            let conns = app.simulator.network.nodes.get(&id).map(|n| {
                n.out_peers.len() + n.in_peers.len()
            }).unwrap_or(0);
            let new_marker = if app.recently_joined.contains(&id) { "+" } else { " " };
            let text = format!("{}{:>5}  {:5}  {:>3}c", new_marker, id, type_str, conns);
            let style = if abs_idx == app.selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else if app.recently_joined.contains(&id) {
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(text).style(style)
        })
        .collect();

    let live_shown = live_items.len();
    let ghost_budget = inner_height.saturating_sub(live_shown);
    let ghost_items: Vec<ListItem> = departed_snapshot
        .iter()
        .take(ghost_budget)
        .map(|(id, type_str)| {
            let text = format!("-{:>5}  {:5}  gone", id, type_str);
            ListItem::new(text).style(
                Style::default()
                    .fg(Color::Red)
                    .add_modifier(Modifier::DIM),
            )
        })
        .collect();

    let mut items = live_items;
    items.extend(ghost_items);

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);
    f.render_widget(List::new(items).block(block), area);
}

fn draw_node_detail(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Detail;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };

    let selected_id = app.filtered_list.get(app.selected).copied();
    let node = selected_id.and_then(|id| app.simulator.network.nodes.get(&id));

    let now = app.event_log.back().map(|(t, _, _)| *t).unwrap_or(app.simulator.start_time);

    let mut lines: Vec<Line> = vec![];

    match (selected_id, node) {
        (Some(id), Some(node)) => {
            let type_str = {
                let has_onion = node.addresses.iter().any(|a| a.network == NetworkType::Onion);
                let has_clear = node.addresses.iter().any(|a| a.network == NetworkType::Clearnet);
                match (has_onion, has_clear) {
                    (true, true) => "Dual-stack",
                    (true, false) => "Onion",
                    _ => "Clearnet",
                }
            };
            let reachable_str = {
                let nets: Vec<&str> = [NetworkType::Onion, NetworkType::Clearnet]
                    .iter()
                    .filter(|&&n| node.reachable_networks.contains(&n))
                    .map(|n| match n {
                        NetworkType::Onion => "onion",
                        NetworkType::Clearnet => "clearnet",
                    })
                    .collect();
                if nets.is_empty() { "not reachable".to_string() } else { nets.join(", ") }
            };
            lines.push(Line::from(vec![
                Span::styled(format!("Node {} — {}  ", id, type_str), Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(format!("(reachable: {})", reachable_str), Style::default().fg(Color::DarkGray)),
            ]));
            lines.push(Line::from(""));

            let out_peers: Vec<_> = node.out_peers.keys().copied().collect();
            let in_peers: Vec<_> = node.in_peers.keys().copied().collect();

            let peer_line = |addr: hyper_lib::address::AddressId| -> Line {
                let net = match addr.network {
                    NetworkType::Onion => "onion",
                    NetworkType::Clearnet => "clear",
                };
                let owner = app.simulator.network.registry.addresses
                    .get(&addr)
                    .map(|a| format!("node {}", a.owner_node))
                    .unwrap_or_else(|| "?".to_string());
                Line::from(format!("  {}  ({})", owner, net))
            };

            lines.push(Line::from(Span::styled("Outbound peers:", Style::default().add_modifier(Modifier::UNDERLINED))));
            if out_peers.is_empty() {
                lines.push(Line::from("  (none)"));
            } else {
                for addr in &out_peers {
                    lines.push(peer_line(*addr));
                }
            }
            lines.push(Line::from(""));

            lines.push(Line::from(Span::styled("Inbound peers:", Style::default().add_modifier(Modifier::UNDERLINED))));
            if in_peers.is_empty() {
                lines.push(Line::from("  (none)"));
            } else {
                for addr in &in_peers {
                    lines.push(peer_line(*addr));
                }
            }
            lines.push(Line::from(""));

            lines.push(Line::from(Span::styled(
                format!("Addrman ({} entries)", node.addrman.entries.len()),
                Style::default().add_modifier(Modifier::UNDERLINED),
            )));
            lines.push(Line::from(Span::styled(
                format!("  {:<7}  {:<8}  {:<8}", "node", "network", "age"),
                Style::default().fg(Color::DarkGray),
            )));

            let mut addrman_entries: Vec<_> = node.addrman.entries.values()
                .map(|e| (e.address, e.timestamp, e.is_terrible(now)))
                .collect();
            addrman_entries.sort_by_key(|(addr, _, _)| *addr);
            for (addr, timestamp, terrible) in addrman_entries {
                let net = match addr.network {
                    NetworkType::Onion => "onion",
                    NetworkType::Clearnet => "clear",
                };
                let owner = app.simulator.network.registry.addresses
                    .get(&addr)
                    .map(|a| a.owner_node.to_string())
                    .unwrap_or_else(|| "?".to_string());
                let stale = if terrible { " [stale]" } else { "" };
                let age = age_str(now, timestamp);
                lines.push(Line::from(format!(
                    "  {:<7}  {:<8}  {:<8}{}",
                    owner, net, age, stale
                )));
            }
            lines.push(Line::from(""));

            let s = &node.node_statistics;
            lines.push(Line::from(Span::styled("Message stats:", Style::default().add_modifier(Modifier::UNDERLINED))));
            lines.push(Line::from(format!("  GETADDR   sent {:>4}  recv {:>4}", s.getaddr_sent, s.getaddr_received)));
            lines.push(Line::from(format!("  ADDR      sent {:>4}  recv {:>4}", s.addr_sent, s.addr_received)));
            lines.push(Line::from(format!("  ANNOUNCE  sent {:>4}  recv {:>4}", s.addr_announce_sent, s.addr_announce_received)));
        }
        _ => {
            lines.push(Line::from("(no node selected)"));
        }
    }

    let total = lines.len();
    let inner_h = area.height.saturating_sub(2) as usize;
    if app.detail_scroll > total.saturating_sub(inner_h) {
        app.detail_scroll = total.saturating_sub(inner_h);
    }
    let visible: Vec<Line> = lines.into_iter().skip(app.detail_scroll).collect();

    let block = Block::default()
        .title(" Node Detail ")
        .borders(Borders::ALL)
        .border_style(border_style);
    f.render_widget(Paragraph::new(visible).block(block), area);
}

fn draw_event_panel(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let start = app.simulator.start_time;
    let inner_h = area.height.saturating_sub(2) as usize;

    let log_rows = inner_h.saturating_sub(1);
    let log_lines = app.event_log.iter().rev().take(log_rows).map(|(t, kind, desc)| {
        let rel = t.saturating_sub(start);
        let (prefix, desc_style) = match kind {
            EventKind::Delivery => ("→ ", Style::default().fg(Color::White)),
            EventKind::Internal => ("· ", Style::default().fg(Color::DarkGray)),
        };
        Line::from(vec![
            Span::styled(format!("T+{:<8}", format!("{}s", rel)), Style::default().fg(Color::DarkGray)),
            Span::styled(prefix, desc_style),
            Span::styled(desc.as_str(), desc_style),
        ])
    });

    let next_line = match app.simulator.event_queue.peek() {
        Some(se) => {
            let rel = se.time().saturating_sub(start);
            let desc = event_description(&se.inner, &app.simulator.network.registry);
            let (prefix, style) = match se.inner {
                Event::SendMessage { .. } => (
                    "→ ",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
                ),
                _ => (
                    "· ",
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                ),
            };
            Line::from(vec![
                Span::styled("next  ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("T+{:<8}", format!("{}s", rel)), Style::default().fg(Color::DarkGray)),
                Span::styled(prefix, style),
                Span::styled(desc, style),
            ])
        }
        None => Line::from(Span::styled("next  (queue empty)", Style::default().fg(Color::DarkGray))),
    };

    let mut lines: Vec<Line> = log_lines.collect();
    lines.reverse();
    while lines.len() < log_rows {
        lines.insert(0, Line::from(""));
    }
    lines.push(next_line);

    let block = Block::default()
        .title(" Event Log ")
        .borders(Borders::ALL);
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn next_message_lines(app: &App) -> Vec<Line<'static>> {
    let start = app.simulator.start_time;
    let now = app.event_log.back().map(|(t, _, _)| *t).unwrap_or(start);
    let mut lines: Vec<Line<'static>> = vec![];

    let se = match app.simulator.event_queue.peek() {
        None => {
            lines.push(Line::from(Span::styled("(queue empty)", Style::default().fg(Color::DarkGray))));
            return lines;
        }
        Some(se) => se,
    };

    let rel = se.time().saturating_sub(start);
    lines.push(Line::from(vec![
        Span::styled("T+", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}s", rel),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
    ]));

    match &se.inner {
        Event::SendMessage { from, to, msg, .. } => {
            let reg = &app.simulator.network.registry;
            lines.push(Line::from(vec![
                Span::styled("from  ", Style::default().fg(Color::DarkGray)),
                Span::styled(fmt_addr(from, reg), Style::default().fg(Color::White)),
            ]));
            lines.push(Line::from(vec![
                Span::styled("to    ", Style::default().fg(Color::DarkGray)),
                Span::styled(fmt_addr(to, reg), Style::default().fg(Color::White)),
            ]));
            lines.push(Line::from(""));

            match msg {
                NetworkMessage::GetAddr => {
                    lines.push(Line::from(Span::styled(
                        "GETADDR",
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    )));
                }
                NetworkMessage::Addr(entries) => {
                    lines.push(Line::from(vec![
                        Span::styled("ADDR ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                        Span::styled(
                            format!("({} entries)", entries.len()),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]));
                    lines.push(Line::from(Span::styled(
                        format!("  {:<12}  {:<7}  age", "node", "net"),
                        Style::default().fg(Color::DarkGray),
                    )));
                    for p in entries {
                        let net = match p.address.network {
                            NetworkType::Onion => "onion",
                            NetworkType::Clearnet => "clear",
                        };
                        let owner = reg.addresses.get(&p.address)
                            .map(|a| a.owner_node.to_string())
                            .unwrap_or_else(|| "?".to_string());
                        let age = age_str(now, p.timestamp);
                        lines.push(Line::from(format!(
                            "  {:<12}  {:<7}  {}",
                            owner, net, age
                        )));
                    }
                }
                NetworkMessage::AddrAnnounce(entries) => {
                    let is_self = entries.first().map_or(false, |p| p.address == *from);
                    let label = if is_self { "SELF-ANNOUNCE" } else { "ADDR-ANNOUNCE" };
                    lines.push(Line::from(vec![
                        Span::styled(label, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                        Span::styled(
                            format!(" ({} entries)", entries.len()),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]));
                    lines.push(Line::from(Span::styled(
                        format!("  {:<12}  {:<7}  age", "node", "net"),
                        Style::default().fg(Color::DarkGray),
                    )));
                    for p in entries {
                        let net = match p.address.network {
                            NetworkType::Onion => "onion",
                            NetworkType::Clearnet => "clear",
                        };
                        let owner = reg.addresses.get(&p.address)
                            .map(|a| a.owner_node.to_string())
                            .unwrap_or_else(|| "?".to_string());
                        let age = age_str(now, p.timestamp);
                        lines.push(Line::from(format!(
                            "  {:<12}  {:<7}  {}",
                            owner, net, age
                        )));
                    }
                }
            }
        }
        other => {
            let desc = event_description(other, &app.simulator.network.registry);
            lines.push(Line::from(Span::styled(desc, Style::default().fg(Color::DarkGray))));
        }
    }

    lines
}

fn draw_next_message_panel(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::NextMessage;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let lines = next_message_lines(app);
    let total = lines.len();
    let inner_h = area.height.saturating_sub(2) as usize;

    // Clamp scroll
    if app.next_msg_scroll > total.saturating_sub(inner_h) {
        app.next_msg_scroll = total.saturating_sub(inner_h);
    }

    let scroll_indicator = if total > inner_h {
        format!(" Next Message [{}/{}] ", app.next_msg_scroll + 1, total)
    } else {
        " Next Message ".to_string()
    };

    let visible: Vec<Line> = lines.into_iter().skip(app.next_msg_scroll).collect();

    let block = Block::default()
        .title(scroll_indicator)
        .borders(Borders::ALL)
        .border_style(border_style);
    f.render_widget(Paragraph::new(visible).block(block), area);
}

fn draw_help_bar(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let run_label = if app.running { "[r] Pause" } else { "[r] Run" };
    let help = format!(
        " [SPACE] Step  {}  [/] Search  [↑↓] Navigate  [Tab] Pane  [q] Quit ",
        run_label
    );
    let style = Style::default().fg(Color::White).bg(Color::DarkGray);
    f.render_widget(Paragraph::new(help).style(style), area);
}
