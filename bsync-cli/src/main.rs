// bsync — P2P clipboard sync CLI.
//
// Architecture: main.rs is the shell loop. All state lives in BsyncCore.
// Clipboard watching, gossip I/O, and stdin prompts happen here.
// The core has zero I/O dependencies — only BsyncEvent / BsyncEffect cross.

mod clipboard;
mod gossip;
mod identity;

use anyhow::Context;
use bsync_core::{
    BsyncCore, BsyncEffect, BsyncEvent, Config, GossipMessage, Ticket, MAX_MESSAGE_SIZE,
};
use clap::Parser;
use clipboard_rs::Clipboard;
use futures_lite::StreamExt;
use iroh_gossip::api::Event;

// ── Startup warning (printed on every launch) ─────────────────

const STARTUP_WARNING: &str = "\
\u{26a0}\u{fe0f}  bsync sends your clipboard contents to ALL connected peers.
    Only connect to peers you trust completely.
    Connected peers can see: passwords, 2FA codes, API keys, private text.
";

// ── CLI ───────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "bsync", version, about = "P2P clipboard sync")]
struct Cli {
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
    run(cli).await
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    // ── Startup warning ──────────────────────────────────
    eprintln!("{STARTUP_WARNING}");

    // ── Identity ─────────────────────────────────────────
    let (secret_key, peer_id_str) = identity::load_or_create_key().await?;

    // ── Core ─────────────────────────────────────────────
    let mut core = BsyncCore::new(Config {
        peer_id: peer_id_str,
        room: cli.room.clone(),
        auto_accept: cli.auto_accept,
    });

    // Print peer ID + ticket
    for effect in core.process_event(BsyncEvent::StartEndpoint) {
        print_effect(&effect);
    }

    // ── Gossip setup ─────────────────────────────────────
    let mut bootstrap = Vec::new();
    if let Some(ticket_str) = &cli.connect {
        match Ticket::decode(ticket_str) {
            Ok(_ticket) => {
                let effects = core
                    .process_event(BsyncEvent::ConnectToPeer { ticket: ticket_str.clone() });
                for effect in effects {
                    match effect {
                        BsyncEffect::ConnectToEndpoint { endpoint_addr } => {
                            match gossip::parse_endpoint_addr(&endpoint_addr) {
                                Ok(id) => bootstrap.push(id),
                                Err(e) => eprintln!("Invalid endpoint address in ticket: {e}"),
                            }
                        }
                        other => print_effect(&other),
                    }
                }
            }
            Err(e) => eprintln!("Invalid ticket: {e}"),
        }
    }

    let mut gh = gossip::setup(&cli.room, &secret_key, bootstrap).await?;

    // ── Clipboard ────────────────────────────────────────
    let clipboard_ctx: Option<clipboard_rs::ClipboardContext> = if !cli.no_clipboard {
        match clipboard_rs::ClipboardContext::new() {
            Ok(ctx) => Some(ctx),
            Err(e) => {
                eprintln!("Failed to open clipboard: {e}");
                eprintln!(
                    "Wayland: ensure compositor supports wlr-data-control or ext-data-control-v1."
                );
                eprintln!("Use --no-clipboard to run without clipboard sync.");
                None
            }
        }
    } else {
        None
    };

    let (clipboard_tx, mut clipboard_rx) = tokio::sync::mpsc::channel::<String>(32);
    if clipboard_ctx.is_some() {
        clipboard::start_watcher(clipboard_tx);
    }

    // ── Approval channel ─────────────────────────────────
    let (approval_tx, mut approval_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, bool)>();

    println!("Waiting for connections...");

    // ── Main event loop ──────────────────────────────────
    loop {
        tokio::select! {
            // ── Clipboard change ──────────────────────
            Some(content) = clipboard_rx.recv() => {
                let effects = core.process_event(BsyncEvent::LocalClipboardChanged { content });
                for effect in effects {
                    dispatch_effect(effect, &gh.sender, &clipboard_ctx, &approval_tx).await?;
                }
            }

            // ── Gossip event ──────────────────────────
            result = gh.receiver.next() => {
                match result {
                    Some(Ok(event)) => handle_gossip_event(
                        event,
                        &mut core,
                        &gh.sender,
                        &clipboard_ctx,
                        &approval_tx,
                    ).await?,
                    Some(Err(e)) => {
                        eprintln!("Gossip error: {e}");
                    }
                    None => {
                        eprintln!("Gossip stream ended");
                        break;
                    }
                }
            }

            // ── Approval response ──────────────────────
            Some((peer_id, approved)) = approval_rx.recv() => {
                let event = if approved {
                    BsyncEvent::PeerApproved { id: peer_id }
                } else {
                    BsyncEvent::PeerRejected { id: peer_id }
                };
                for effect in core.process_event(event) {
                    dispatch_effect(effect, &gh.sender, &clipboard_ctx, &approval_tx).await?;
                }
            }

            // ── Ctrl+C ─────────────────────────────────
            _ = tokio::signal::ctrl_c() => {
                println!("\nShutting down...");
                let effects = core.process_event(BsyncEvent::Shutdown);
                for effect in effects {
                    dispatch_effect(effect, &gh.sender, &clipboard_ctx, &approval_tx).await?;
                }
                break;
            }
        }
    }

    // ── Cleanup ─────────────────────────────────────────
    gh.router.shutdown().await.context("shutdown router")?;
    Ok(())
}

// ── Gossip event handler ──────────────────────────────────────

async fn handle_gossip_event(
    event: Event,
    core: &mut BsyncCore,
    sender: &iroh_gossip::api::GossipSender,
    clipboard_ctx: &Option<clipboard_rs::ClipboardContext>,
    approval_tx: &tokio::sync::mpsc::UnboundedSender<(String, bool)>,
) -> anyhow::Result<()> {
    match event {
        Event::Received(msg) => {
            match serde_json::from_slice::<GossipMessage>(&msg.content) {
                Ok(GossipMessage::ClipboardText { origin, content }) => {
                    let effects = core.process_event(BsyncEvent::RemoteMessageReceived {
                        from: origin,
                        content,
                    });
                    for effect in effects {
                        dispatch_effect(effect, sender, clipboard_ctx, approval_tx).await?;
                    }
                }
                Err(e) => {
                    eprintln!("Failed to deserialize gossip message: {e}");
                }
            }
        }
        Event::NeighborUp(id) => {
            let effects =
                core.process_event(BsyncEvent::PeerConnected { id: id.to_string() });
            for effect in effects {
                dispatch_effect(effect, sender, clipboard_ctx, approval_tx).await?;
            }
        }
        Event::NeighborDown(id) => {
            let effects =
                core.process_event(BsyncEvent::PeerDisconnected { id: id.to_string() });
            for effect in effects {
                dispatch_effect(effect, sender, clipboard_ctx, approval_tx).await?;
            }
        }
        Event::Lagged => {
            eprintln!(
                "Warning: gossip stream lagged — some clipboard updates were dropped"
            );
        }
    }
    Ok(())
}

// ── Effect dispatch ───────────────────────────────────────────

async fn dispatch_effect(
    effect: BsyncEffect,
    sender: &iroh_gossip::api::GossipSender,
    clipboard_ctx: &Option<clipboard_rs::ClipboardContext>,
    approval_tx: &tokio::sync::mpsc::UnboundedSender<(String, bool)>,
) -> anyhow::Result<()> {
    match effect {
        BsyncEffect::WriteClipboard { content, .. } => {
            if let Some(ctx) = clipboard_ctx {
                ctx.set_text(content).ok();
            }
        }

        BsyncEffect::BroadcastMessage { message } => {
            let payload = serde_json::to_vec(&message)?;
            if payload.len() > MAX_MESSAGE_SIZE {
                eprintln!(
                    "Message too large ({} bytes). Skipping sync.",
                    payload.len()
                );
                return Ok(());
            }
            sender.broadcast(payload.into()).await?;
        }

        BsyncEffect::PrintStatus { message } => {
            println!("{message}");
        }

        BsyncEffect::PromptApproval { peer_id } => {
            let tx = approval_tx.clone();
            tokio::task::spawn_blocking(move || {
                println!("Peer {peer_id} wants to connect. Allow? [y/N]: ");
                let mut input = String::new();
                let approved = std::io::stdin()
                    .read_line(&mut input)
                    .is_ok()
                    && input.trim().eq_ignore_ascii_case("y");
                let _ = tx.send((peer_id, approved));
            });
        }

        BsyncEffect::ConnectToEndpoint { .. } => {
            // Handled at startup — ignored during runtime.
        }

        BsyncEffect::Shutdown => {
            // Cleanup handled after event loop exit.
        }
    }

    Ok(())
}

fn print_effect(effect: &BsyncEffect) {
    if let BsyncEffect::PrintStatus { message } = effect {
        println!("{message}");
    }
}
