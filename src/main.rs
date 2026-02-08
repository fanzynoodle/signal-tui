mod signal_cli;
mod config;
mod scrollback;

use std::collections::HashMap;
use std::io;
use std::io::IsTerminal;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, SetTitle, disable_raw_mode, enable_raw_mode,
    },
};
use ratatui::{
    Frame,
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};

use crate::signal_cli::{IncomingMessage, SignalCli};
use crate::scrollback::ScrollbackRecord;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    Insert,
    AddRecipient,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetKind {
    Contact,
    Group,
}

#[derive(Debug, Clone)]
struct Target {
    conversation_key: String,
    kind: TargetKind,
    // For contacts: E.164 number. For groups: group id.
    addr: String,
    display: String,
}

#[derive(Debug, Clone)]
struct ChatMessage {
    ts_ms: Option<i64>,
    dir: MsgDir,
    who: Option<String>,
    body: String,
}

#[derive(Debug, Clone)]
enum MsgDir {
    In,
    Out,
}

struct App {
    account: String,
    cfg: config::Config,
    notify_send: bool,
    mode: Mode,
    targets: Vec<Target>,
    selected: usize,
    pending_g: bool,
    unread: HashMap<String, usize>,
    title_dirty: bool,
    input: String,
    status: String,
    messages: HashMap<String, Vec<ChatMessage>>,
}

impl App {
    fn selected_target(&self) -> Option<&Target> {
        self.targets.get(self.selected)
    }
}

enum BgEvent {
    Received(Vec<IncomingMessage>),
    Error(String),
}

fn main() -> Result<()> {
    let args = parse_args();
    if args.help {
        print_help();
        return Ok(());
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("signal-tui must be run in an interactive TTY");
    }

    let cfg = config::load_or_create(args.config.clone().map(Into::into)).context("load config")?;
    let signal = SignalCli::with_bin(args.bin);

    let accounts = signal.list_accounts().context("list signal-cli accounts")?;
    let account = if let Some(a) = args.account {
        a
    } else {
        accounts.first().cloned().unwrap_or_default()
    };
    if account.is_empty() {
        bail!("no signal-cli accounts found (try `signal-cli register` / `signal-cli link` first)");
    }

    let mut targets = Vec::new();
    for c in signal.list_contacts(&account).unwrap_or_default() {
        let display = c.name.unwrap_or_else(|| c.number.clone());
        targets.push(Target {
            conversation_key: format!("contact:{}", c.number),
            kind: TargetKind::Contact,
            addr: c.number,
            display,
        });
    }
    for g in signal.list_groups(&account).unwrap_or_default() {
        let display = g.name.unwrap_or_else(|| format!("group {}", g.id));
        targets.push(Target {
            conversation_key: format!("group:{}", g.id),
            kind: TargetKind::Group,
            addr: g.id,
            display,
        });
    }
    targets.sort_by(|a, b| a.display.to_lowercase().cmp(&b.display.to_lowercase()));

    let status = if accounts.len() > 1 {
        format!(
            "using account {account} (found {} accounts; no selector yet)",
            accounts.len()
        )
    } else {
        format!("using account {account}")
    };

    let mut app = App {
        account,
        cfg,
        notify_send: false,
        mode: Mode::Normal,
        targets,
        selected: 0,
        pending_g: false,
        unread: HashMap::new(),
        title_dirty: true,
        input: String::new(),
        status,
        messages: HashMap::new(),
    };

    app.notify_send = app.cfg.notify && notify_send_available();
    load_initial_scrollback(&mut app).ok();

    run_tui(&signal, &mut app)
}

fn run_tui(signal: &SignalCli, app: &mut App) -> Result<()> {
    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("enter alt screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("create terminal")?;

    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<BgEvent>();
    let account = app.account.clone();
    let signal2 = signal.clone();
    let stop2 = stop.clone();
    let bg = thread::spawn(move || {
        while !stop2.load(Ordering::Relaxed) {
            match signal2.receive_once(&account, 1) {
                Ok(msgs) => {
                    if !msgs.is_empty() {
                        let _ = tx.send(BgEvent::Received(msgs));
                    }
                }
                Err(e) => {
                    let _ = tx.send(BgEvent::Error(format!("{e:#}")));
                    thread::sleep(Duration::from_secs(2));
                }
            }
        }
    });

    let res = (|| -> Result<()> {
        loop {
            while let Ok(ev) = rx.try_recv() {
                match ev {
                    BgEvent::Received(msgs) => {
                        ingest_incoming(app, msgs);
                        app.title_dirty = true;
                    }
                    BgEvent::Error(e) => app.status = format!("receive error: {e}"),
                }
            }

            if app.title_dirty {
                update_title(&mut terminal, app);
                app.title_dirty = false;
            }

            terminal.draw(|f| ui(f, app))?;

            if event::poll(Duration::from_millis(200)).context("poll events")? {
                if let Event::Key(k) = event::read().context("read event")? {
                    if handle_key(signal, app, k)? {
                        break;
                    }
                }
            }
        }
        Ok(())
    })();

    stop.store(true, Ordering::Relaxed);
    let _ = bg.join();

    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    res
}

struct Args {
    bin: String,
    account: Option<String>,
    config: Option<String>,
    help: bool,
}

fn parse_args() -> Args {
    // Tiny/forgiving arg parse:
    // `--account +1555...` or `-a +1555...`
    // `--signal-cli /path/to/signal-cli`
    // `--config /path/to/config.toml`
    // `--help` / `-h`
    let mut bin = "signal-cli".to_string();
    let mut account = None;
    let mut config = None;
    let mut help = false;

    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--account" | "-a" => {
                account = it.next();
            }
            "--signal-cli" => {
                if let Some(b) = it.next() {
                    bin = b;
                }
            }
            "--config" => {
                config = it.next();
            }
            "--help" | "-h" => help = true,
            _ => {}
        }
    }

    Args { bin, account, config, help }
}

fn ingest_incoming(app: &mut App, msgs: Vec<IncomingMessage>) {
    let selected_key = app.selected_target().map(|t| t.conversation_key.clone());
    for m in msgs {
        if !app.targets.iter().any(|t| t.conversation_key == m.conversation_key) {
            // Add unknown chats on the fly (incoming from unknown numbers / groups).
            let (kind, addr, display) = if let Some(rest) = m.conversation_key.strip_prefix("group:") {
                (TargetKind::Group, rest.to_string(), format!("group {rest}"))
            } else if let Some(rest) = m.conversation_key.strip_prefix("contact:") {
                (TargetKind::Contact, rest.to_string(), rest.to_string())
            } else {
                continue;
            };
            app.targets.push(Target {
                conversation_key: m.conversation_key.clone(),
                kind,
                addr,
                display,
            });
        }

        if selected_key.as_deref() != Some(m.conversation_key.as_str()) {
            *app.unread.entry(m.conversation_key.clone()).or_insert(0) += 1;
        }

        if app.cfg.save_scrollback {
            let rec = ScrollbackRecord {
                ts_ms: m.timestamp_ms,
                dir: "in".to_string(),
                who: m.source.clone(),
                body: m.body.clone(),
            };
            let _ = scrollback::append(&app.cfg.scrollback_dir, &m.conversation_key, &rec);
        }

        if app.notify_send {
            notify_incoming(app, &m.conversation_key, m.source.as_deref(), &m.body);
        }

        app.messages
            .entry(m.conversation_key.clone())
            .or_default()
            .push(ChatMessage {
                ts_ms: m.timestamp_ms,
                dir: MsgDir::In,
                who: m.source,
                body: m.body,
            });
    }

    // Keep list stable but reasonably ordered.
    app.targets
        .sort_by(|a, b| a.display.to_lowercase().cmp(&b.display.to_lowercase()));
    if app.selected >= app.targets.len() && !app.targets.is_empty() {
        app.selected = app.targets.len() - 1;
    }
}

fn handle_key(signal: &SignalCli, app: &mut App, k: KeyEvent) -> Result<bool> {
    if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
        return Ok(true);
    }

    match app.mode {
        Mode::Normal => handle_key_normal(signal, app, k),
        Mode::Insert => handle_key_insert(signal, app, k),
        Mode::AddRecipient => handle_key_add_recipient(app, k),
    }
}

fn handle_key_normal(signal: &SignalCli, app: &mut App, k: KeyEvent) -> Result<bool> {
    // vim-ish key chords
    if !matches!(k.code, KeyCode::Char('g')) {
        app.pending_g = false;
    }

    match k.code {
        KeyCode::Char('q') => return Ok(true),
        KeyCode::Char('j') | KeyCode::Down => {
            if !app.targets.is_empty() {
                app.selected = (app.selected + 1).min(app.targets.len() - 1);
                mark_selected_read(app);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if !app.targets.is_empty() {
                app.selected = app.selected.saturating_sub(1);
                mark_selected_read(app);
            }
        }
        KeyCode::Char('g') => {
            if app.pending_g {
                app.selected = 0;
                app.pending_g = false;
                mark_selected_read(app);
            } else {
                app.pending_g = true;
            }
        }
        KeyCode::Char('G') => {
            if !app.targets.is_empty() {
                app.selected = app.targets.len() - 1;
                mark_selected_read(app);
            }
        }
        KeyCode::Char('i') => {
            if app.selected_target().is_some() {
                app.mode = Mode::Insert;
                app.input.clear();
            } else {
                app.status = "no target selected; press 'a' to add a recipient".to_string();
            }
        }
        KeyCode::Char('a') => {
            app.mode = Mode::AddRecipient;
            app.input.clear();
            app.status = "add recipient: type E.164 number like +15551234567, Enter to add, Esc to cancel".to_string();
        }
        KeyCode::Char('r') => {
            match signal.receive_once(&app.account, 1) {
                Ok(msgs) => {
                    if msgs.is_empty() {
                        app.status = "sync: no new messages".to_string();
                    } else {
                        app.status = format!("sync: received {} message(s)", msgs.len());
                        ingest_incoming(app, msgs);
                        app.title_dirty = true;
                    }
                }
                Err(e) => app.status = format!("sync error: {e:#}"),
            }
        }
        _ => {}
    }
    Ok(false)
}

fn handle_key_insert(signal: &SignalCli, app: &mut App, k: KeyEvent) -> Result<bool> {
    match k.code {
        KeyCode::Esc => {
            app.mode = Mode::Normal;
            app.input.clear();
        }
        KeyCode::Enter => {
            let body = app.input.trim().to_string();
            if body.is_empty() {
                app.status = "empty message; nothing sent".to_string();
                return Ok(false);
            }
            let Some(t) = app.selected_target().cloned() else {
                app.status = "no target selected".to_string();
                return Ok(false);
            };

            let send_res = match t.kind {
                TargetKind::Contact => signal.send_message_to_number(&app.account, &t.addr, &body),
                TargetKind::Group => signal.send_message_to_group(&app.account, &t.addr, &body),
            };
            match send_res {
                Ok(()) => {
                    if app.cfg.save_scrollback {
                        let rec = ScrollbackRecord {
                            ts_ms: None,
                            dir: "out".to_string(),
                            who: Some(app.account.clone()),
                            body: body.clone(),
                        };
                        let _ = scrollback::append(&app.cfg.scrollback_dir, &t.conversation_key, &rec);
                    }
                    app.messages.entry(t.conversation_key.clone()).or_default().push(ChatMessage {
                        ts_ms: None,
                        dir: MsgDir::Out,
                        who: Some(app.account.clone()),
                        body: body.clone(),
                    });
                    app.status = "sent".to_string();
                    app.input.clear();
                    app.mode = Mode::Normal;
                }
                Err(e) => app.status = format!("send error: {e:#}"),
            }
        }
        KeyCode::Backspace => {
            app.input.pop();
        }
        KeyCode::Char(c) => {
            if !k.modifiers.contains(KeyModifiers::CONTROL) && !k.modifiers.contains(KeyModifiers::ALT) {
                app.input.push(c);
            }
        }
        _ => {}
    }
    Ok(false)
}

fn handle_key_add_recipient(app: &mut App, k: KeyEvent) -> Result<bool> {
    match k.code {
        KeyCode::Esc => {
            app.mode = Mode::Normal;
            app.input.clear();
            app.status = "cancelled".to_string();
        }
        KeyCode::Enter => {
            let num = app.input.trim().to_string();
            if !num.starts_with('+') || num.len() < 8 {
                app.status = "recipient must look like +15551234567".to_string();
                return Ok(false);
            }
            let key = format!("contact:{num}");
            if !app.targets.iter().any(|t| t.conversation_key == key) {
                app.targets.push(Target {
                    conversation_key: key,
                    kind: TargetKind::Contact,
                    addr: num.clone(),
                    display: num.clone(),
                });
                app.targets
                    .sort_by(|a, b| a.display.to_lowercase().cmp(&b.display.to_lowercase()));
            }
            if let Some(i) = app.targets.iter().position(|t| t.addr == num) {
                app.selected = i;
            }
            mark_selected_read(app);
            app.mode = Mode::Normal;
            app.input.clear();
            app.status = "recipient added (press 'i' to message)".to_string();
        }
        KeyCode::Backspace => {
            app.input.pop();
        }
        KeyCode::Char(c) => {
            if !k.modifiers.contains(KeyModifiers::CONTROL) && !k.modifiers.contains(KeyModifiers::ALT) {
                app.input.push(c);
            }
        }
        _ => {}
    }
    Ok(false)
}

fn ui(f: &mut Frame, app: &App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(4)])
        .split(f.area());

    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(root[0]);

    draw_targets(f, app, main[0]);
    draw_chat(f, app, main[1]);
    draw_status(f, app, root[1]);
}

fn draw_targets(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .targets
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let unread = *app.unread.get(&t.conversation_key).unwrap_or(&0);
            let mut style = Style::default();
            if i == app.selected {
                style = style
                    .fg(Color::Black)
                    .bg(Color::LightGreen)
                    .add_modifier(Modifier::BOLD);
            }
            let prefix = match t.kind {
                TargetKind::Contact => "@",
                TargetKind::Group => "#",
            };
            let badge = if unread > 0 { format!(" ({unread})") } else { String::new() };
            ListItem::new(Line::from(vec![
                Span::styled(prefix, style),
                Span::raw(" "),
                Span::styled(format!("{}{}", t.display, badge), style),
            ]))
        })
        .collect();

    let title = format!("Chats ({})", app.targets.len());
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(list, area);
}

fn draw_chat(f: &mut Frame, app: &App, area: Rect) {
    let title = if let Some(t) = app.selected_target() {
        format!("{}  [{}]", t.display, t.addr)
    } else {
        "No chat selected".to_string()
    };

    let key = app.selected_target().map(|t| t.conversation_key.clone());
    let msgs = key
        .as_deref()
        .and_then(|k| app.messages.get(k))
        .map(|v| v.as_slice())
        .unwrap_or(&[]);

    // Render last N lines. Keep it simple: no scroll yet.
    let mut lines = Vec::new();
    for m in msgs.iter().rev().take(200).rev() {
        let ts = m
            .ts_ms
            .map(|t| format!("{}", t / 1000))
            .unwrap_or_else(|| "-".to_string());
        let dir = match m.dir {
            MsgDir::In => "<",
            MsgDir::Out => ">",
        };
        let who = m.who.clone().unwrap_or_else(|| "?".to_string());
        lines.push(Line::from(vec![
            Span::styled(format!("{ts} {dir} {who}: "), Style::default().fg(Color::Gray)),
            Span::raw(m.body.clone()),
        ]));
    }

    let p = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let help = match app.mode {
        Mode::Normal => "normal: j/k move, i insert, a add-recipient, r sync, q quit",
        Mode::Insert => "insert: type, Enter send, Esc cancel",
        Mode::AddRecipient => "add-recipient: type +E164, Enter add, Esc cancel",
    };

    let l1 = Line::from(vec![
        Span::styled(&app.account, Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::raw(help),
    ]);

    let l2 = match app.mode {
        Mode::Insert | Mode::AddRecipient => Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Yellow)),
            Span::raw(app.input.clone()),
        ]),
        Mode::Normal => Line::from(vec![Span::raw(app.status.clone())]),
    };

    let p = Paragraph::new(vec![l1, l2])
        .block(Block::default().borders(Borders::ALL).title("Status"))
        .wrap(Wrap { trim: true });
    f.render_widget(p, area);
}

fn mark_selected_read(app: &mut App) {
    let key = app.selected_target().map(|t| t.conversation_key.clone());
    if let Some(k) = key {
        if app.unread.remove(&k).is_some() {
            app.title_dirty = true;
        }
    }
}

fn update_title(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &App) {
    let unread_total: usize = app.unread.values().sum();
    let title = if unread_total > 0 {
        format!("signal-tui ({unread_total})")
    } else {
        "signal-tui".to_string()
    };
    let _ = execute!(terminal.backend_mut(), SetTitle(title));
}

fn notify_send_available() -> bool {
    let has_display =
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
    if !has_display {
        return false;
    }
    if let Some(path) = std::env::var_os("PATH") {
        for p in std::env::split_paths(&path) {
            if p.join("notify-send").exists() {
                return true;
            }
        }
    }
    false
}

fn notify_incoming(app: &App, conversation_key: &str, source: Option<&str>, body: &str) {
    let chat = app
        .targets
        .iter()
        .find(|t| t.conversation_key == conversation_key)
        .map(|t| t.display.as_str())
        .unwrap_or(conversation_key);
    let from = source.unwrap_or("unknown");

    let mut msg = body.to_string();
    if msg.len() > 200 {
        msg.truncate(200);
        msg.push_str("...");
    }

    let _ = std::process::Command::new("notify-send")
        .args([
            "-a",
            "signal-tui",
            "-t",
            "4000",
            &format!("Signal: {chat}"),
            &format!("{from}: {msg}"),
        ])
        .spawn();
}

fn load_initial_scrollback(app: &mut App) -> Result<()> {
    for t in &app.targets {
        let recs = scrollback::load_tail(
            &app.cfg.scrollback_dir,
            &t.conversation_key,
            app.cfg.scrollback_load_limit,
        )?;
        if recs.is_empty() {
            continue;
        }
        let v = app.messages.entry(t.conversation_key.clone()).or_default();
        for r in recs {
            v.push(ChatMessage {
                ts_ms: r.ts_ms,
                dir: if r.dir == "out" { MsgDir::Out } else { MsgDir::In },
                who: r.who,
                body: r.body,
            });
        }
    }
    Ok(())
}

fn print_help() {
    println!(
        "signal-tui

USAGE:
  signal-tui [--account +15551234567] [--signal-cli /path/to/signal-cli] [--config /path/to/config.toml]

FILES:
  Config:      $XDG_CONFIG_HOME/signal-tui/config.toml (default: ~/.config/signal-tui/config.toml)
  Scrollback:  $XDG_STATE_HOME/signal-tui/scrollback (default: ~/.local/state/signal-tui/scrollback)

KEYS:
  j/k (or arrows)  Move
  gg / G           Top / bottom
  a                Add recipient (+E164)
  i                Compose message
  Enter            Send (insert mode)
  Esc              Cancel
  r                Sync once
  q                Quit"
    );
}
