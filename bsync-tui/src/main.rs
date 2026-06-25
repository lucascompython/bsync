mod app;
mod cli;
mod ui;

use anyhow::Context;
use bsync_core::{BsyncCore, BsyncEffect, BsyncEvent, ClipboardContent, Config, GossipMessage};
use bsync_net::{parse_endpoint_addr, Network, NetworkEvent};
use bsync_rust::{clipboard, identity};
use clap::Parser;
use clipboard_rs::Clipboard;
use futures_lite::StreamExt;
use iroh_base::{EndpointId, SecretKey};
use std::collections::HashMap;
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

    /// Initial room name (default: "default"). Can add more rooms in the TUI.
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

struct RoomNetworkEvent {
    room: String,
    event: NetworkEvent,
}

struct RoomRegistry {
    rooms: HashMap<String, RoomHandle>,
    event_tx: mpsc::UnboundedSender<RoomNetworkEvent>,
}

struct RoomHandle {
    network: Network,
    #[allow(dead_code)]
    is_local: bool,
}

impl RoomRegistry {
    fn new(event_tx: mpsc::UnboundedSender<RoomNetworkEvent>) -> Self {
        Self {
            rooms: HashMap::new(),
            event_tx,
        }
    }

    async fn create_room(
        &mut self,
        room: &str,
        secret_key: &SecretKey,
        bootstrap: Vec<EndpointId>,
    ) -> anyhow::Result<()> {
        if self.rooms.contains_key(room) {
            return Ok(());
        }
        let (network, network_rx) = Network::setup(room, secret_key, bootstrap).await?;
        let tx = self.event_tx.clone();
        let room_name = room.to_string();
        tokio::spawn(async move {
            let mut rx = network_rx;
            while let Some(event) = rx.recv().await {
                if tx
                    .send(RoomNetworkEvent {
                        room: room_name.clone(),
                        event,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        self.rooms
            .insert(room.to_string(), RoomHandle { network, is_local: true });
        Ok(())
    }

    async fn remove_room(&mut self, room: &str) -> anyhow::Result<()> {
        if let Some(handle) = self.rooms.remove(room) {
            handle.network.shutdown().await?;
        }
        Ok(())
    }

    async fn add_peer(&self, room: &str, endpoint_addr: &str) -> anyhow::Result<()> {
        let handle = self
            .rooms
            .get(room)
            .ok_or_else(|| anyhow::anyhow!("room '{room}' not found"))?;
        let peer_id = parse_endpoint_addr(endpoint_addr)?;
        handle.network.add_peer(peer_id).await
    }

    async fn broadcast(
        &self,
        room: &str,
        content: &ClipboardContent,
        origin: &str,
    ) -> anyhow::Result<()> {
        let handle = self
            .rooms
            .get(room)
            .ok_or_else(|| anyhow::anyhow!("room '{room}' not found"))?;
        handle.network.broadcast(content, origin).await
    }

    async fn shutdown_all(&mut self) {
        for (_, handle) in self.rooms.drain() {
            let _ = handle.network.shutdown().await;
        }
    }
}

async fn run_tui_loop(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    room: &str,
    no_clipboard: bool,
    auto_accept: bool,
    connect_ticket: Option<String>,
) -> anyhow::Result<()> {
    use app::App;
    use crossterm::event::{Event, KeyEventKind};

    let (secret_key, peer_id_str) = identity::load_or_create_key().await?;

    let core = BsyncCore::new(Config {
        peer_id: peer_id_str,
        auto_accept,
    });
    let mut app = App::new(core);
    app.clipboard_enabled = !no_clipboard;

    let _ = app.core.process_event(BsyncEvent::StartEndpoint);

    let (room_event_tx, mut room_event_rx) = mpsc::unbounded_channel::<RoomNetworkEvent>();
    let mut registry = RoomRegistry::new(room_event_tx);

    // Clipboard context — moved out of app to avoid borrow conflicts.
    let clipboard_ctx: Option<clipboard_rs::ClipboardContext> = if !no_clipboard {
        clipboard_rs::ClipboardContext::new().ok()
    } else {
        None
    };

    // Create the initial room from --room
    let init_effects = app
        .core
        .process_event(BsyncEvent::CreateRoom {
            room: room.to_string(),
        });
    for effect in init_effects {
        process_effect(effect, &mut registry, &clipboard_ctx, &mut app, &secret_key).await?;
    }

    // Handle --connect
    if let Some(ticket_str) = connect_ticket {
        let effects = app
            .core
            .process_event(BsyncEvent::JoinRoom { ticket: ticket_str });
        for effect in effects {
            process_effect(effect, &mut registry, &clipboard_ctx, &mut app, &secret_key)
                .await?;
        }
    }

    let (clipboard_tx, mut clipboard_rx) = mpsc::channel::<ClipboardContent>(32);
    if clipboard_ctx.is_some() {
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
                        handle_tui_key(
                            &mut app,
                            key,
                            &mut registry,
                            &clipboard_ctx,
                            &secret_key,
                        )
                        .await?;
                    }
            }

            Some(content) = clipboard_rx.recv() => {
                let effects = app.core.process_event(
                    BsyncEvent::LocalClipboardChanged { content }
                );
                for effect in effects {
                    process_effect(
                        effect,
                        &mut registry,
                        &clipboard_ctx,
                        &mut app,
                        &secret_key,
                    )
                    .await?;
                }
            }

            Some(room_event) = room_event_rx.recv() => {
                handle_network_event_tui(
                    room_event,
                    &mut app,
                    &mut registry,
                    &clipboard_ctx,
                    &secret_key,
                )
                .await?;
            }

            _ = tokio::signal::ctrl_c() => {
                app.should_quit = true;
            }
        }

        terminal.draw(|frame| ui::draw(frame, &app))?;
    }

    app.core.process_event(BsyncEvent::Shutdown);
    registry.shutdown_all().await;
    Ok(())
}

async fn handle_tui_key(
    app: &mut app::App,
    key: crossterm::event::KeyEvent,
    registry: &mut RoomRegistry,
    clipboard_ctx: &Option<clipboard_rs::ClipboardContext>,
    secret_key: &SecretKey,
) -> anyhow::Result<()> {
    use app::Tab;
    use crossterm::event::KeyCode;

    // Clear any transient notification on key press
    app.notification = None;

    if app.dialog.is_some() {
        return handle_dialog_key(app, key, registry, clipboard_ctx, secret_key).await;
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Tab => app.next_tab(),
        KeyCode::BackTab => app.prev_tab(),
        KeyCode::Char('1') => app.tab = Tab::Status,
        KeyCode::Char('2') => app.tab = Tab::Peers,
        KeyCode::Char('3') => app.tab = Tab::Rooms,
        KeyCode::Char('4') => app.tab = Tab::History,
        KeyCode::Char('5') => app.tab = Tab::Help,
        KeyCode::Down | KeyCode::Char('j') => {
            if app.tab == Tab::History {
                app.scroll_history_down();
            } else {
                app.scroll_rooms_down();
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if app.tab == Tab::History {
                app.scroll_history_up();
            } else {
                app.scroll_rooms_up();
            }
        }

        KeyCode::Char('t') => app.copy_ticket_to_clipboard(clipboard_ctx),

        KeyCode::Char('c') => app.open_connect_dialog(),

        KeyCode::Char('r') | KeyCode::Char('n') => app.open_room_create_dialog(),

        // On Rooms tab: delete or copy ticket for selected room
        KeyCode::Char('d') if app.tab == Tab::Rooms => {
            let view = app.view();
            if let Some(room) = view.rooms.get(app.rooms_scroll) {
                let room_name = room.name.clone();
                app.open_room_delete_dialog(room_name);
            }
        }
        KeyCode::Char('T') if app.tab == Tab::Rooms => {
            let view = app.view();
            if let Some(room) = view.rooms.get(app.rooms_scroll) {
                let room_name = room.name.clone();
                app.copy_room_ticket(&room_name, clipboard_ctx);
            }
        }

        KeyCode::Enter if app.tab == Tab::History => app.recopy_history_item(clipboard_ctx),

        KeyCode::Char('y') => {
            let pending = app.view().rooms.iter().find_map(|r| {
                r.pending_peers
                    .first()
                    .map(|id| (r.name.clone(), id.clone()))
            });
            if let Some((room, peer_id)) = pending {
                let effects = app
                    .core
                    .process_event(BsyncEvent::PeerApproved { room, id: peer_id });
                for effect in effects {
                    process_effect(effect, registry, clipboard_ctx, app, secret_key).await?;
                }
            }
        }
        KeyCode::Char('N') if app.tab == Tab::Peers => {
            let pending = app.view().rooms.iter().find_map(|r| {
                r.pending_peers
                    .first()
                    .map(|id| (r.name.clone(), id.clone()))
            });
            if let Some((room, peer_id)) = pending {
                let effects = app
                    .core
                    .process_event(BsyncEvent::PeerRejected { room, id: peer_id });
                for effect in effects {
                    process_effect(effect, registry, clipboard_ctx, app, secret_key).await?;
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
    registry: &mut RoomRegistry,
    clipboard_ctx: &Option<clipboard_rs::ClipboardContext>,
    secret_key: &SecretKey,
) -> anyhow::Result<()> {
    use app::Dialog;
    use crossterm::event::{KeyCode, KeyModifiers};

    let dialog = app.dialog.take().unwrap();

    match dialog {
        Dialog::Approval { room, peer_id } => match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                let effects = app
                    .core
                    .process_event(BsyncEvent::PeerApproved { room, id: peer_id });
                for effect in effects {
                    process_effect(effect, registry, clipboard_ctx, app, secret_key).await?;
                }
            }
            KeyCode::Char('n') => {
                let effects = app
                    .core
                    .process_event(BsyncEvent::PeerRejected { room, id: peer_id });
                for effect in effects {
                    process_effect(effect, registry, clipboard_ctx, app, secret_key).await?;
                }
            }
            KeyCode::Esc => {}
            _ => {
                app.dialog = Some(Dialog::Approval { room, peer_id });
            }
        },

        Dialog::ConnectInput { mut input } => match key.code {
            KeyCode::Enter => {
                if input.is_empty() {
                    app.dialog = Some(Dialog::Error {
                        message: "Ticket cannot be empty".into(),
                    });
                } else {
                    let effects = app.core.process_event(BsyncEvent::JoinRoom { ticket: input });
                    for effect in effects {
                        process_effect(effect, registry, clipboard_ctx, app, secret_key)
                            .await?;
                    }
                }
            }
            KeyCode::Esc => {}
            KeyCode::Backspace => {
                input.pop();
                app.dialog = Some(Dialog::ConnectInput { input });
            }
            KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(ctx) = clipboard_ctx
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

        Dialog::RoomCreate { mut input } => match key.code {
            KeyCode::Enter => {
                let trimmed = input.trim().to_string();
                if trimmed.is_empty() {
                    app.dialog = Some(Dialog::Error {
                        message: "Room name cannot be empty".into(),
                    });
                } else {
                    let effects = app
                        .core
                        .process_event(BsyncEvent::CreateRoom { room: trimmed });
                    for effect in effects {
                        process_effect(effect, registry, clipboard_ctx, app, secret_key)
                            .await?;
                    }
                }
            }
            KeyCode::Esc => {}
            KeyCode::Backspace => {
                input.pop();
                app.dialog = Some(Dialog::RoomCreate { input });
            }
            KeyCode::Char(c) => {
                input.push(c);
                app.dialog = Some(Dialog::RoomCreate { input });
            }
            _ => app.dialog = Some(Dialog::RoomCreate { input }),
        },

        Dialog::RoomDelete { room } => match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                let effects = app.core.process_event(BsyncEvent::LeaveRoom { room });
                for effect in effects {
                    process_effect(effect, registry, clipboard_ctx, app, secret_key).await?;
                }
            }
            KeyCode::Char('n') | KeyCode::Esc => {}
            _ => {
                app.dialog = Some(Dialog::RoomDelete { room });
            }
        },

        Dialog::Error { .. } | Dialog::Info { .. } => {
            if !matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char(' ')) {
                app.dialog = Some(dialog);
            }
        }
    }

    Ok(())
}

async fn handle_network_event_tui(
    room_event: RoomNetworkEvent,
    app: &mut app::App,
    registry: &mut RoomRegistry,
    clipboard_ctx: &Option<clipboard_rs::ClipboardContext>,
    secret_key: &SecretKey,
) -> anyhow::Result<()> {
    use app::Dialog;

    let event = match room_event.event {
        NetworkEvent::MessageReceived { from, content } => BsyncEvent::RemoteMessageReceived {
            room: room_event.room,
            from,
            content,
        },
        NetworkEvent::PeerConnected { id } => BsyncEvent::PeerConnected {
            room: room_event.room,
            id,
        },
        NetworkEvent::PeerDisconnected { id } => BsyncEvent::PeerDisconnected {
            room: room_event.room,
            id,
        },
        NetworkEvent::Lagged => {
            app.dialog = Some(Dialog::Info {
                message: format!(
                    "[{}] Gossip stream lagged — some clipboard updates may have been dropped.",
                    room_event.room
                ),
            });
            return Ok(());
        }
    };

    let effects = app.core.process_event(event);
    for effect in effects {
        process_effect(effect, registry, clipboard_ctx, app, secret_key).await?;
    }

    Ok(())
}

async fn process_effect(
    effect: BsyncEffect,
    registry: &mut RoomRegistry,
    clipboard_ctx: &Option<clipboard_rs::ClipboardContext>,
    app: &mut app::App,
    secret_key: &SecretKey,
) -> anyhow::Result<()> {
    match effect {
        BsyncEffect::WriteClipboard { content, .. } => {
            if let Some(ctx) = clipboard_ctx {
                let _ = bsync_rust::clipboard::write_clipboard(ctx, &content);
            }
        }

        BsyncEffect::BroadcastMessage { room, message } => match message {
            GossipMessage::ClipboardText { origin, content } => {
                registry
                    .broadcast(&room, &ClipboardContent::Text(content), &origin)
                    .await?;
            }
            GossipMessage::ClipboardImage { .. } => {
                eprintln!("Warning: unexpected BroadcastMessage with image");
            }
        },

        BsyncEffect::BroadcastImage {
            room,
            origin,
            png_data,
        } => {
            registry
                .broadcast(&room, &ClipboardContent::Image(png_data), &origin)
                .await?;
        }

        BsyncEffect::PrintStatus { message } => {
            app.notification = Some(message);
        }

        BsyncEffect::PromptApproval { room, peer_id } => {
            app.dialog = Some(app::Dialog::Approval { room, peer_id });
        }

        BsyncEffect::SetupRoom { room, ticket: _ } => {
            registry.create_room(&room, secret_key, vec![]).await?;
        }

        BsyncEffect::ShutdownRoom { room } => {
            registry.remove_room(&room).await?;
        }

        BsyncEffect::AddPeer {
            room,
            endpoint_addr,
        } => {
            registry.add_peer(&room, &endpoint_addr).await?;
        }

        BsyncEffect::Shutdown => {
            app.should_quit = true;
        }
    }

    Ok(())
}
