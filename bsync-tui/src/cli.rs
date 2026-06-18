use anyhow::Context;
use bsync_core::{
    BsyncCore, BsyncEffect, BsyncEvent, Config, GossipMessage, Ticket, MAX_MESSAGE_SIZE,
};
use bsync_rust::{clipboard, gossip, identity};
use futures_lite::StreamExt;
use iroh_gossip::api::Event;
use tokio::sync::mpsc;

type CliArgs = crate::Cli;

pub async fn run(cli: &CliArgs) -> anyhow::Result<()> {
    let (secret_key, peer_id_str) = identity::load_or_create_key().await?;

    let mut core = BsyncCore::new(Config {
        peer_id: peer_id_str,
        room: cli.room.clone(),
        auto_accept: cli.auto_accept,
    });

    for effect in core.process_event(BsyncEvent::StartEndpoint) {
        print_effect(&effect);
    }

    let mut bootstrap = Vec::new();
    if let Some(ticket_str) = &cli.connect {
        match Ticket::decode(ticket_str) {
            Ok(_ticket) => {
                let effects = core.process_event(BsyncEvent::ConnectToPeer {
                    ticket: ticket_str.clone(),
                });
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

    let (clipboard_tx, mut clipboard_rx) = mpsc::channel::<bsync_core::ClipboardContent>(32);
    if clipboard_ctx.is_some() {
        clipboard::start_watcher(clipboard_tx);
    }

    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<(String, bool)>();

    println!("Waiting for connections...");

    loop {
        tokio::select! {
            Some(content) = clipboard_rx.recv() => {
                let effects = core.process_event(BsyncEvent::LocalClipboardChanged { content });
                for effect in effects {
                    dispatch_effect(effect, &gh.sender, &clipboard_ctx, &approval_tx).await?;
                }
            }

            result = gh.receiver.next() => {
                match result {
                    Some(Ok(event)) => {
                        handle_gossip_event(event, &mut core, &gh.sender, &clipboard_ctx, &approval_tx).await?;
                    }
                    Some(Err(e)) => {
                        eprintln!("Gossip error: {e}");
                    }
                    None => {
                        eprintln!("Gossip stream ended");
                        break;
                    }
                }
            }

            Some((peer_id, approved)) = approval_rx.recv() => {
                let event = if approved {
                    BsyncEvent::PeerApproved { id: peer_id }
                } else {
                    BsyncEvent::PeerRejected { id: peer_id }
                };
                let effects = core.process_event(event);
                for effect in effects {
                    dispatch_effect(effect, &gh.sender, &clipboard_ctx, &approval_tx).await?;
                }
            }

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

    gh.router.shutdown().await.context("shutdown router")?;
    Ok(())
}

async fn handle_gossip_event(
    event: Event,
    core: &mut BsyncCore,
    sender: &iroh_gossip::api::GossipSender,
    clipboard_ctx: &Option<clipboard_rs::ClipboardContext>,
    approval_tx: &mpsc::UnboundedSender<(String, bool)>,
) -> anyhow::Result<()> {
    match event {
        Event::Received(msg) => match serde_json::from_slice::<GossipMessage>(&msg.content) {
            Ok(gm) => {
                let (origin, content) = match gm {
                    GossipMessage::ClipboardText { origin, content } => {
                        (origin, bsync_core::ClipboardContent::Text(content))
                    }
                    GossipMessage::ClipboardImage { origin, png_data } => {
                        (origin, bsync_core::ClipboardContent::Image(png_data))
                    }
                };
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
        },
        Event::NeighborUp(id) => {
            let effects = core.process_event(BsyncEvent::PeerConnected { id: id.to_string() });
            for effect in effects {
                dispatch_effect(effect, sender, clipboard_ctx, approval_tx).await?;
            }
        }
        Event::NeighborDown(id) => {
            let effects = core.process_event(BsyncEvent::PeerDisconnected { id: id.to_string() });
            for effect in effects {
                dispatch_effect(effect, sender, clipboard_ctx, approval_tx).await?;
            }
        }
        Event::Lagged => {
            eprintln!("Warning: gossip stream lagged — some clipboard updates were dropped");
        }
    }
    Ok(())
}

async fn dispatch_effect(
    effect: BsyncEffect,
    sender: &iroh_gossip::api::GossipSender,
    clipboard_ctx: &Option<clipboard_rs::ClipboardContext>,
    approval_tx: &mpsc::UnboundedSender<(String, bool)>,
) -> anyhow::Result<()> {
    match effect {
        BsyncEffect::WriteClipboard { content, .. } => {
            if let Some(ctx) = clipboard_ctx {
                let _ = bsync_rust::clipboard::write_clipboard(ctx, &content);
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
                let approved = std::io::stdin().read_line(&mut input).is_ok()
                    && input.trim().eq_ignore_ascii_case("y");
                let _ = tx.send((peer_id, approved));
            });
        }

        BsyncEffect::ConnectToEndpoint { .. } => {}
        BsyncEffect::Shutdown => {}
    }

    Ok(())
}

fn print_effect(effect: &BsyncEffect) {
    if let BsyncEffect::PrintStatus { message } = effect {
        println!("{message}");
    }
}
