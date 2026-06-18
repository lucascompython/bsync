mod app;
mod cli;
mod ui;

use anyhow::Context;
use bsync_core::{
    BsyncCore, BsyncEffect, BsyncEvent, Config, GossipMessage, MAX_MESSAGE_SIZE, Ticket,
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

async fn run_tui(cli: &Cli) -> anyhow::Result<()> {
    use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
    use crossterm::execute;
    use crossterm::terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    };
    use ratatui::{Terminal, backend::CrosstermBackend};
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

    let (secret_key, peer_id_str) = identity::load_or_create_key().await?;

    let core = BsyncCore::new(Config {
        peer_id: peer_id_str,
        room: room.to_string(),
        auto_accept,
    });
    let mut app = App::new(core, room.to_string());
    app.clipboard_enabled = !no_clipboard;

    let _ = app.core.process_event(BsyncEvent::StartEndpoint);

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

    app.clipboard_ctx = if !no_clipboard {
        clipboard_rs::ClipboardContext::new().ok()
    } else {
        None
    };
    app.gossip = Some(gh.gossip.clone());

    let (clipboard_tx, mut clipboard_rx) = mpsc::channel::<bsync_core::ClipboardContent>(32);
    if app.clipboard_ctx.is_some() {
        clipboard::start_watcher(clipboard_tx);
    }

    let mut event_stream = crossterm::event::EventStream::new();

    terminal.draw(|frame| ui::draw(frame, &app))?;

    loop {
        if app.should_quit {
            break;
        }

        tokio::select! {
            maybe_event = event_stream.next() => {
                if let Some(Ok(Event::Key(key))) = maybe_event
                    && key.kind == KeyEventKind::Press {
                        handle_tui_key(&mut app, key, &gh).await?;
                    }

            }

            Some(content) = clipboard_rx.recv() => {
                let effects = app.core.process_event(
                    BsyncEvent::LocalClipboardChanged { content }
                );
                for effect in effects {
                    dispatch_effect(effect, &gh, &app.clipboard_ctx).await?;
                }
            }

            result = gh.receiver.next() => {
                match result {
                    Some(Ok(event)) => {
                        handle_gossip_event_tui(event, &mut app, &gh).await?;
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

async fn handle_tui_key(
    app: &mut app::App,
    key: crossterm::event::KeyEvent,
    gh: &gossip::GossipHandle,
) -> anyhow::Result<()> {
    use app::Tab;
    use crossterm::event::KeyCode;

    if app.dialog.is_some() {
        return handle_dialog_key(app, key, gh).await;
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

        // Copy your own ticket to clipboard so you can share it
        KeyCode::Char('t') => app.copy_ticket_to_clipboard(),

        KeyCode::Char('c') => app.open_connect_dialog(),

        // Re-copy a history item to clipboard (Enter on History tab)
        KeyCode::Enter if app.tab == Tab::History => app.recopy_history_item(),

        KeyCode::Char('y') => {
            let view = app.view();
            if let Some(peer_id) = view.pending_peers.first() {
                let effects = app.core.process_event(BsyncEvent::PeerApproved {
                    id: peer_id.clone(),
                });
                for effect in effects {
                    dispatch_effect(effect, gh, &app.clipboard_ctx).await?;
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
                    dispatch_effect(effect, gh, &app.clipboard_ctx).await?;
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
    gh: &gossip::GossipHandle,
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
                    dispatch_effect(effect, gh, &app.clipboard_ctx).await?;
                }
            }
            KeyCode::Char('n') => {
                let effects = app
                    .core
                    .process_event(BsyncEvent::PeerRejected { id: peer_id });
                for effect in effects {
                    dispatch_effect(effect, gh, &app.clipboard_ctx).await?;
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
                    app.connect_to_peer(input).await;
                }
            }
            KeyCode::Esc => {}
            KeyCode::Backspace => {
                input.pop();
                app.dialog = Some(Dialog::ConnectInput { input });
            }
            KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(ctx) = &app.clipboard_ctx
                    && let Ok(text) = ctx.get_text()
                {
                    input.push_str(&text);
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

async fn handle_gossip_event_tui(
    event: GossipEvent,
    app: &mut app::App,
    gh: &gossip::GossipHandle,
) -> anyhow::Result<()> {
    use app::Dialog;

    match event {
        GossipEvent::Received(msg) => {
            if let Ok(gm) = serde_json::from_slice::<GossipMessage>(&msg.content) {
                let (origin, content) = match gm {
                    GossipMessage::ClipboardText { origin, content } => {
                        (origin, bsync_core::ClipboardContent::Text(content))
                    }
                    GossipMessage::ClipboardImage { origin, hash } => {
                        let hash = iroh_blobs::Hash::from_bytes(hash);
                        let from: iroh::EndpointId = origin
                            .parse()
                            .context("invalid origin endpoint id in image message")?;
                        match gh.blobs.download(hash, from, &gh.endpoint).await {
                            Ok(png_data) => {
                                (origin, bsync_core::ClipboardContent::Image(png_data))
                            }
                            Err(e) => {
                                app.dialog = Some(Dialog::Error {
                                    message: format!("Image download failed: {e}"),
                                });
                                return Ok(());
                            }
                        }
                    }
                };
                let effects = app.core.process_event(BsyncEvent::RemoteMessageReceived {
                    from: origin,
                    content,
                });
                for effect in effects {
                    dispatch_effect(effect, gh, &app.clipboard_ctx).await?;
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
                dispatch_effect(effect, gh, &app.clipboard_ctx).await?;
            }
        }
        GossipEvent::NeighborDown(id) => {
            let effects = app
                .core
                .process_event(BsyncEvent::PeerDisconnected { id: id.to_string() });
            for effect in effects {
                dispatch_effect(effect, gh, &app.clipboard_ctx).await?;
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

async fn dispatch_effect(
    effect: BsyncEffect,
    gh: &gossip::GossipHandle,
    clipboard_ctx: &Option<clipboard_rs::ClipboardContext>,
) -> anyhow::Result<()> {
    match effect {
        BsyncEffect::WriteClipboard { content, .. } => {
            if let Some(ctx) = clipboard_ctx {
                let _ = bsync_rust::clipboard::write_clipboard(ctx, &content);
            }
        }
        BsyncEffect::BroadcastMessage { message } => {
            let payload = serde_json::to_vec(&message)?;
            if payload.len() <= MAX_MESSAGE_SIZE {
                gh.sender.broadcast(payload.into()).await?;
            }
        }
        BsyncEffect::BroadcastImage { origin, png_data } => {
            // Upload PNG bytes to the blob store, then broadcast just the hash over gossip.
            // The hash is 32 bytes — tiny compared to the raw PNG data.
            match gh.blobs.add(&png_data).await {
                Ok(hash) => {
                    let message = GossipMessage::ClipboardImage {
                        origin,
                        hash: *hash.as_bytes(),
                    };
                    let payload = serde_json::to_vec(&message)?;
                    gh.sender.broadcast(payload.into()).await?;
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("failed to add image blob: {e}"));
                }
            }
        }
        BsyncEffect::PrintStatus { .. }
        | BsyncEffect::PromptApproval { .. }
        | BsyncEffect::ConnectToEndpoint { .. }
        | BsyncEffect::Shutdown => {}
    }
    Ok(())
}
