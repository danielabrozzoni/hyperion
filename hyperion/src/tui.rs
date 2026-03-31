use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind};
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

/// Whether an event is an internal scheduling step or an actual network delivery.
#[derive(Clone, Copy, PartialEq)]
enum EventKind {
    /// Internal bookkeeping: NodeJoin, NodeLeave, SelfAnnounce.
    /// When these fire nothing has been delivered yet; they may enqueue further events.
    Internal,
    /// A message arrives at a node and its receive handler runs (addrman may change).
    Delivery,
}

#[derive(Clone, Copy, PartialEq)]
enum Focus {
    NodeList,
    Detail,
}

pub struct App {
    simulator: Simulator,
    event_log: VecDeque<(u64, EventKind, String)>,
    node_list: Vec<NodeId>,
    filtered_list: Vec<NodeId>,
    selected: usize,
    list_offset: usize,
    detail_scroll: usize,
    search_query: String,
    search_active: bool,
    running: bool,
    focus: Focus,
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
            search_query: String::new(),
            search_active: false,
            running: false,
            focus: Focus::NodeList,
        }
    }

    fn step(&mut self) {
        if let Some((event, at)) = self.simulator.step() {
            let kind = match &event {
                Event::SendMessage { .. } => EventKind::Delivery,
                _ => EventKind::Internal,
            };
            let desc = event_description(&event, &self.simulator.network.registry);
            if self.event_log.len() >= EVENT_LOG_CAP {
                self.event_log.pop_front();
            }
            self.event_log.push_back((at, kind, desc));

            // Refresh node list after potential node joins/leaves
            let mut ids: Vec<NodeId> = self.simulator.network.nodes.keys().copied().collect();
            ids.sort();
            self.node_list = ids;
            self.apply_filter();
            // Clamp selection
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
        // Clamp selection after filter
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
        // Internal events — when these fire, nothing has been delivered yet.
        Event::NodeJoin { .. } => "node join".to_string(),
        Event::NodeLeave { node_id, .. } => format!("node leave  node={}", node_id),
        // SelfAnnounce: node decides to announce itself → creates SendMessage(AddrAnnounce).
        // Addrman on the peer does NOT update until that SendMessage is processed.
        Event::SelfAnnounce { node_id, peer_addr, .. } => {
            format!("sched AddrAnnounce  node={} → {}", node_id, fmt_addr(peer_addr, reg))
        }
        // Delivery events — the `to` node's receive handler runs right now.
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
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_inner(&mut terminal, simulator);

    disable_raw_mode()?;
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

        let timeout = tick;
        if event::poll(timeout)? {
            if let TermEvent::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if app.search_active {
                    match key.code {
                        KeyCode::Esc => {
                            app.search_active = false;
                        }
                        KeyCode::Enter => {
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
                                Focus::Detail => Focus::NodeList,
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
                        },
                        _ => {}
                    }
                }
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

    draw_node_list(f, app, main[0]);
    draw_node_detail(f, app, main[1]);
    draw_event_panel(f, app, outer[3]);
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

    let items: Vec<ListItem> = app
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
            let text = format!("{:>5}  {:5}  {:>3}c", id, type_str, conns);
            let style = if abs_idx == app.selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(text).style(style)
        })
        .collect();

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

            // Collect peer lists before releasing the node borrow so we can hit the registry.
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

            // Addrman
            lines.push(Line::from(Span::styled(
                format!("Addrman ({} entries)", node.addrman.entries.len()),
                Style::default().add_modifier(Modifier::UNDERLINED),
            )));
            lines.push(Line::from(Span::styled(
                format!("  {:<7}  {:<8}  {:<8}", "node", "network", "age"),
                Style::default().fg(Color::DarkGray),
            )));

            let addrman_entries: Vec<_> = node.addrman.entries.values()
                .map(|e| (e.address, e.timestamp, e.is_terrible(now)))
                .collect();
            let mut addrman_entries = addrman_entries;
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

            // Message stats
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

    // Apply scroll
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

    // Executed event log: most recent first. Reserve last row for "next".
    // Delivery events (SendMessage) are bright — state actually changed.
    // Internal events (SelfAnnounce, NodeJoin/Leave) are muted — only scheduling.
    let log_rows = inner_h.saturating_sub(1);
    let log_lines = app.event_log.iter().rev().take(log_rows).map(|(t, kind, desc)| {
        let rel = t.saturating_sub(start);
        let (prefix, desc_style) = match kind {
            EventKind::Delivery => (
                "→ ",
                Style::default().fg(Color::White),
            ),
            EventKind::Internal => (
                "· ",
                Style::default().fg(Color::DarkGray),
            ),
        };
        Line::from(vec![
            Span::styled(format!("T+{:<8}", format!("{}s", rel)), Style::default().fg(Color::DarkGray)),
            Span::styled(prefix, desc_style),
            Span::styled(desc.as_str(), desc_style),
        ])
    });

    // "Next" line last: dim cyan to signal "pending / not yet executed"
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

    // Pad log lines so "next" always sits at the bottom
    let mut lines: Vec<Line> = log_lines.collect();
    lines.reverse(); // oldest first, most recent just above "next"
    while lines.len() < log_rows {
        lines.insert(0, Line::from(""));
    }
    lines.push(next_line);

    let block = Block::default()
        .title(" Event Log ")
        .borders(Borders::ALL);
    f.render_widget(Paragraph::new(lines).block(block), area);
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
