mod app;
mod cli;
mod ui;

use anyhow::Context;
use bsync_core::{
    BsyncCore, BsyncEffect, BsyncEvent, Config, GossipMessage, Ticket, MAX_MESSAGE_SIZE,
};
use bsync_rust::{clipboard, gossip, identity};
use clap::Parser;
use clipboard_rs::Clipboard;
use futures_lite::StreamExt;
use iroh_gossip::api::Event as GossipEvent;
use tokio::sync::mpsc;

const STARTUP_WARNING: &str = "\
\u{26a0}\u{fe0f}  bsync sends your clipboard contents to ALL connected peers.
    Only connect to peers you trust completely.
    Connected peers can see: passwords, 2FA codes, API keys, private text.";

// ── CLI args ──────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "bsync", version, about = "P2P clipboard sync")]
struct Cli {
    /// Run in plain CLI mode (no TUI)
    #[arg(long)]
    cli: bool,

    /// Connect to a peer using their ticket
    #[arg(short, long)]
    connect: Option<String>,

    /// Room name for logical isolation (default: "default")
    #[arg(long, default_value = "default")]
    room: String,

    /// Disable clipboard watching/writing (network-only mode)
    #[arg(long)]
    no_clipboard: bool,

    /// Skip peer approval prompt (dangerous for clipboard data)
    #[arg(long)]
    auto_accept: bool,
}

// ── Entry point ───────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let result = if cli.cli {
        cli::run(&cli).await
    } else {
        run_tui(&cli).await
    };

    eprintln!("{STARTUP_WARNING}");
    result
}

// ── TUI mode ──────────────────────────────────────────────────

async fn run_tui(cli: &Cli) -> anyhow::Result<()> {
    use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
    use crossterm::execute;
    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };
    use ratatui::{backend::CrosstermBackend, Terminal};
    use std::io::stdout;

    enable_raw_mode().context("enable raw mode")?;
    let result = async {
        let mut stdout = stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
            .context("enter alternate screen")?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend).context("create terminal")?;

        let result = run_tui_loop(
            &mut terminal,
            &cli.room,
            cli.no_clipboard,
            cli.auto_accept,
            cli.connect.clone(),
        )
        .await;

        disable_raw_mode().ok();
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )
        .ok();
        terminal.show_cursor().ok();
        result
    }
    .await;

    if result.is_err() {
        disable_raw_mode().ok();
    }
    result
}

async fn run_tui_loop(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    room: &str,
    no_clipboard: bool,
    auto_accept: bool,
    connect_ticket: Option<String>,
) -> anyhow::Result<()> {
    use app::{App, Dialog};
    use crossterm::event::{Event, KeyEventKind};

    // ── Identity ─────────────────────────────────────────
    let (secret_key, peer_id_str) = identity::load_or_create_key().await?;

    // ── Core + App ──────────────────────────────────────
    let core = BsyncCore::new(Config {
        peer_id: peer_id_str,
        room: room.to_string(),
        auto_accept,
    });
    let mut app = App::new(core);
    app.clipboard_enabled = !no_clipboard;

    let _ = app.core.process_event(BsyncEvent::StartEndpoint);

    // ── Gossip setup ────────────────────────────────────
    let mut bootstrap = Vec::new();
    if let Some(ref ticket_str) = connect_ticket {
        match Ticket::decode(ticket_str) {
            Ok(_ticket) => {
                let effects = app.core.process_event(BsyncEvent::ConnectToPeer {
                    ticket: ticket_str.clone(),
                });
                for effect in effects {
                    if let BsyncEffect::ConnectToEndpoint { endpoint_addr } = effect {
                        match gossip::parse_endpoint_addr(&endpoint_addr) {
                            Ok(id) => bootstrap.push(id),
                            Err(e) => {
                                app.dialog = Some(Dialog::Error {
                                    message: format!("Invalid ticket: {e}"),
                                });
                            }
                        }
                    }
                }
            }
            Err(e) => {
                app.dialog = Some(Dialog::Error {
                    message: format!("Invalid ticket: {e}"),
                });
            }
        }
    }

    let mut gh = gossip::setup(room, &secret_key, bootstrap).await?;

    // ── Clipboard ───────────────────────────────────────
    let clipboard_ctx: Option<clipboard_rs::ClipboardContext> = if !no_clipboard {
        clipboard_rs::ClipboardContext::new().ok()
    } else {
        None
    };

    let (clipboard_tx, mut clipboard_rx) = mpsc::channel::<String>(32);
    if clipboard_ctx.is_some() {
        clipboard::start_watcher(clipboard_tx);
    }

    let mut event_stream = crossterm::event::EventStream::new();

    terminal.draw(|frame| ui::draw(frame, &app))?;

    // ── Main TUI event loop ──────────────────────────────
    loop {
        if app.should_quit {
            break;
        }

        tokio::select! {
            maybe_event = event_stream.next() => {
                if let Some(Ok(Event::Key(key))) = maybe_event {
                    if key.kind == KeyEventKind::Press {
                        handle_tui_key(&mut app, key, &gh.sender, &clipboard_ctx).await?;
                    }
                }
            }

            Some(content) = clipboard_rx.recv() => {
                let effects = app.core.process_event(
                    BsyncEvent::LocalClipboardChanged { content }
                );
                for effect in effects {
                    dispatch_effect(effect, &gh.sender, &clipboard_ctx).await?;
                }
            }

            result = gh.receiver.next() => {
                match result {
                    Some(Ok(event)) => {
                        handle_gossip_event_tui(event, &mut app, &gh.sender, &clipboard_ctx).await?;
                    }
                    Some(Err(e)) => {
                        app.dialog = Some(Dialog::Error {
                            message: format!("Gossip error: {e}"),
                        });
                    }
                    None => break,
                }
            }

            _ = tokio::signal::ctrl_c() => {
                app.should_quit = true;
            }
        }

        terminal.draw(|frame| ui::draw(frame, &app))?;
    }

    app.core.process_event(BsyncEvent::Shutdown);
    gh.router.shutdown().await.context("shutdown router")?;
    Ok(())
}

// ── TUI key handling ──────────────────────────────────────────

async fn handle_tui_key(
    app: &mut app::App,
    key: crossterm::event::KeyEvent,
    sender: &iroh_gossip::api::GossipSender,
    clipboard_ctx: &Option<clipboard_rs::ClipboardContext>,
) -> anyhow::Result<()> {
    use app::Tab;
    use crossterm::event::KeyCode;

    // If a dialog is open, route keys to dialog handler
    if app.dialog.is_some() {
        return handle_dialog_key(app, key, sender, clipboard_ctx).await;
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Tab => app.next_tab(),
        KeyCode::BackTab => app.prev_tab(),
        KeyCode::Char('1') => app.tab = Tab::Status,
        KeyCode::Char('2') => app.tab = Tab::Peers,
        KeyCode::Char('3') => app.tab = Tab::History,
        KeyCode::Char('4') => app.tab = Tab::Help,
        KeyCode::Down | KeyCode::Char('j') => app.scroll_history_down(),
        KeyCode::Up | KeyCode::Char('k') => app.scroll_history_up(),
        KeyCode::Char('c') => app.open_connect_dialog(),
        KeyCode::Char('y') => {
            let view = app.view();
            if let Some(peer_id) = view.pending_peers.first() {
                let effects = app.core.process_event(BsyncEvent::PeerApproved {
                    id: peer_id.clone(),
                });
                for effect in effects {
                    dispatch_effect(effect, sender, clipboard_ctx).await?;
                }
            }
        }
        KeyCode::Char('n') => {
            let view = app.view();
            if let Some(peer_id) = view.pending_peers.first() {
                let effects = app.core.process_event(BsyncEvent::PeerRejected {
                    id: peer_id.clone(),
                });
                for effect in effects {
                    dispatch_effect(effect, sender, clipboard_ctx).await?;
                }
            }
        }
        _ => {}
    }

    Ok(())
}

async fn handle_dialog_key(
    app: &mut app::App,
    key: crossterm::event::KeyEvent,
    sender: &iroh_gossip::api::GossipSender,
    clipboard_ctx: &Option<clipboard_rs::ClipboardContext>,
) -> anyhow::Result<()> {
    use app::Dialog;
    use crossterm::event::{KeyCode, KeyModifiers};

    let dialog = app.dialog.take().unwrap();

    match dialog {
        Dialog::Approval { peer_id } => match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                let effects = app
                    .core
                    .process_event(BsyncEvent::PeerApproved { id: peer_id });
                for effect in effects {
                    dispatch_effect(effect, sender, clipboard_ctx).await?;
                }
            }
            KeyCode::Char('n') => {
                let effects = app
                    .core
                    .process_event(BsyncEvent::PeerRejected { id: peer_id });
                for effect in effects {
                    dispatch_effect(effect, sender, clipboard_ctx).await?;
                }
            }
            KeyCode::Esc => {}
            _ => app.dialog = Some(Dialog::Approval { peer_id }),
        },

        Dialog::ConnectInput { mut input } => match key.code {
            KeyCode::Enter => {
                if input.is_empty() {
                    app.dialog = Some(Dialog::Error {
                        message: "Ticket cannot be empty".into(),
                    });
                } else {
                    match Ticket::decode(&input) {
                        Ok(ticket) => {
                            let effects = app
                                .core
                                .process_event(BsyncEvent::ConnectToPeer { ticket: input });
                            for effect in effects {
                                if let BsyncEffect::ConnectToEndpoint { endpoint_addr } = &effect {
                                    match gossip::parse_endpoint_addr(endpoint_addr) {
                                        Ok(_id) => {
                                            app.dialog = Some(Dialog::Info {
                                                message: format!(
                                                    "Connecting to {} in room {}...",
                                                    &ticket.endpoint_addr
                                                        [..ticket.endpoint_addr.len().min(20)],
                                                    ticket.room
                                                ),
                                            });
                                        }
                                        Err(e) => {
                                            app.dialog = Some(Dialog::Error {
                                                message: format!("Invalid endpoint: {e}"),
                                            });
                                        }
                                    }
                                } else {
                                    dispatch_effect(effect, sender, clipboard_ctx).await?;
                                }
                            }
                        }
                        Err(e) => {
                            app.dialog = Some(Dialog::Error {
                                message: format!("Invalid ticket: {e}"),
                            });
                        }
                    }
                }
            }
            KeyCode::Esc => {}
            KeyCode::Backspace => {
                input.pop();
                app.dialog = Some(Dialog::ConnectInput { input });
            }
            KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(ctx) = clipboard_ctx {
                    if let Ok(text) = ctx.get_text() {
                        input.push_str(&text);
                    }
                }
                app.dialog = Some(Dialog::ConnectInput { input });
            }
            KeyCode::Char(c) => {
                input.push(c);
                app.dialog = Some(Dialog::ConnectInput { input });
            }
            _ => app.dialog = Some(Dialog::ConnectInput { input }),
        },

        Dialog::Error { .. } | Dialog::Info { .. } => {
            if !matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char(' ')) {
                app.dialog = Some(dialog);
            }
        }
    }

    Ok(())
}

// ── Gossip event handling (TUI) ───────────────────────────────

async fn handle_gossip_event_tui(
    event: GossipEvent,
    app: &mut app::App,
    sender: &iroh_gossip::api::GossipSender,
    clipboard_ctx: &Option<clipboard_rs::ClipboardContext>,
) -> anyhow::Result<()> {
    use app::Dialog;

    match event {
        GossipEvent::Received(msg) => {
            if let Ok(GossipMessage::ClipboardText { origin, content }) =
                serde_json::from_slice::<GossipMessage>(&msg.content)
            {
                let effects = app.core.process_event(BsyncEvent::RemoteMessageReceived {
                    from: origin,
                    content,
                });
                for effect in effects {
                    dispatch_effect(effect, sender, clipboard_ctx).await?;
                }
            }
        }
        GossipEvent::NeighborUp(id) => {
            let effects = app
                .core
                .process_event(BsyncEvent::PeerConnected { id: id.to_string() });
            for effect in effects {
                if let BsyncEffect::PromptApproval { peer_id } = &effect {
                    app.dialog = Some(Dialog::Approval {
                        peer_id: peer_id.clone(),
                    });
                }
                dispatch_effect(effect, sender, clipboard_ctx).await?;
            }
        }
        GossipEvent::NeighborDown(id) => {
            let effects = app
                .core
                .process_event(BsyncEvent::PeerDisconnected { id: id.to_string() });
            for effect in effects {
                dispatch_effect(effect, sender, clipboard_ctx).await?;
            }
        }
        GossipEvent::Lagged => {
            app.dialog = Some(Dialog::Info {
                message: "Gossip stream lagged — some clipboard updates may have been dropped."
                    .into(),
            });
        }
    }
    Ok(())
}

// ── Shared effect dispatch ────────────────────────────────────

async fn dispatch_effect(
    effect: BsyncEffect,
    sender: &iroh_gossip::api::GossipSender,
    clipboard_ctx: &Option<clipboard_rs::ClipboardContext>,
) -> anyhow::Result<()> {
    match effect {
        BsyncEffect::WriteClipboard { content, .. } => {
            if let Some(ctx) = clipboard_ctx {
                let _ = ctx.set_text(content);
            }
        }
        BsyncEffect::BroadcastMessage { message } => {
            let payload = serde_json::to_vec(&message)?;
            if payload.len() <= MAX_MESSAGE_SIZE {
                sender.broadcast(payload.into()).await?;
            }
        }
        BsyncEffect::PrintStatus { .. }
        | BsyncEffect::PromptApproval { .. }
        | BsyncEffect::ConnectToEndpoint { .. }
        | BsyncEffect::Shutdown => {}
    }
    Ok(())
}
