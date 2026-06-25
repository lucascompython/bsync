use bsync_core::{
    BsyncCore, BsyncEffect, BsyncEvent, ClipboardContent, Config, GossipMessage, Ticket,
};
use bsync_net::{Network, NetworkEvent, parse_endpoint_addr};
use bsync_rust::{clipboard, identity};
use tokio::sync::mpsc;

type CliArgs = crate::Cli;

pub async fn run(cli: &CliArgs) -> anyhow::Result<()> {
    let (secret_key, peer_id_str) = identity::load_or_create_key().await?;

    let mut core = BsyncCore::new(Config {
        peer_id: peer_id_str,
        auto_accept: cli.auto_accept,
    });

    let _ = core.process_event(BsyncEvent::StartEndpoint);

    // Determine initial room and bootstrap peer.
    // If --connect is given, use the ticket's room and add the peer as bootstrap.
    let (room_name, bootstrap) = if let Some(ticket_str) = &cli.connect {
        match Ticket::decode(ticket_str) {
            Ok(ticket) => {
                let peer_id = parse_endpoint_addr(&ticket.endpoint_addr).ok();
                (ticket.room, peer_id.into_iter().collect())
            }
            Err(e) => {
                eprintln!("Invalid ticket: {e}");
                return Ok(());
            }
        }
    } else {
        (cli.room.clone(), vec![])
    };

    let _ = core.process_event(BsyncEvent::CreateRoom {
        room: room_name.clone(),
    });

    let (network, mut network_rx) = Network::setup(&room_name, &secret_key, bootstrap).await?;

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

    let (clipboard_tx, mut clipboard_rx) = mpsc::channel::<ClipboardContent>(32);
    if clipboard_ctx.is_some() {
        clipboard::start_watcher(clipboard_tx);
    }

    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<(String, String, bool)>();

    println!("Room: {room_name}");
    println!("Waiting for connections...");

    loop {
        tokio::select! {
            Some(content) = clipboard_rx.recv() => {
                let effects = core.process_event(BsyncEvent::LocalClipboardChanged { content });
                for effect in effects {
                    dispatch_effect(effect, &network, &clipboard_ctx, &approval_tx).await?;
                }
            }

            event = network_rx.recv() => {
                match event {
                    Some(event) => {
                        handle_network_event(
                            event,
                            &room_name,
                            &mut core,
                            &network,
                            &clipboard_ctx,
                            &approval_tx,
                        )
                        .await?;
                    }
                    None => {
                        eprintln!("Network stream ended");
                        break;
                    }
                }
            }

            Some((room, peer_id, approved)) = approval_rx.recv() => {
                let event = if approved {
                    BsyncEvent::PeerApproved { room, id: peer_id }
                } else {
                    BsyncEvent::PeerRejected { room, id: peer_id }
                };
                let effects = core.process_event(event);
                for effect in effects {
                    dispatch_effect(effect, &network, &clipboard_ctx, &approval_tx).await?;
                }
            }

            _ = tokio::signal::ctrl_c() => {
                println!("\nShutting down...");
                let effects = core.process_event(BsyncEvent::Shutdown);
                for effect in effects {
                    dispatch_effect(effect, &network, &clipboard_ctx, &approval_tx).await?;
                }
                break;
            }
        }
    }

    network.shutdown().await?;
    Ok(())
}

async fn handle_network_event(
    event: NetworkEvent,
    room: &str,
    core: &mut BsyncCore,
    network: &Network,
    clipboard_ctx: &Option<clipboard_rs::ClipboardContext>,
    approval_tx: &mpsc::UnboundedSender<(String, String, bool)>,
) -> anyhow::Result<()> {
    let room = room.to_string();
    let event = match event {
        NetworkEvent::MessageReceived { from, content } => BsyncEvent::RemoteMessageReceived {
            room: room.clone(),
            from,
            content,
        },
        NetworkEvent::PeerConnected { id } => BsyncEvent::PeerConnected {
            room: room.clone(),
            id,
        },
        NetworkEvent::PeerDisconnected { id } => BsyncEvent::PeerDisconnected {
            room: room.clone(),
            id,
        },
        NetworkEvent::Lagged => {
            eprintln!("Warning: gossip stream lagged — some clipboard updates were dropped");
            return Ok(());
        }
    };

    let effects = core.process_event(event);
    for effect in effects {
        dispatch_effect(effect, network, clipboard_ctx, approval_tx).await?;
    }
    Ok(())
}

async fn dispatch_effect(
    effect: BsyncEffect,
    network: &Network,
    clipboard_ctx: &Option<clipboard_rs::ClipboardContext>,
    approval_tx: &mpsc::UnboundedSender<(String, String, bool)>,
) -> anyhow::Result<()> {
    match effect {
        BsyncEffect::WriteClipboard { content, .. } => {
            if let Some(ctx) = clipboard_ctx {
                let _ = bsync_rust::clipboard::write_clipboard(ctx, &content);
            }
        }

        BsyncEffect::BroadcastMessage {
            room: _room,
            message,
        } => match message {
            GossipMessage::ClipboardText { origin, content } => {
                network
                    .broadcast(&ClipboardContent::Text(content), &origin)
                    .await?;
            }
            GossipMessage::ClipboardImage { .. } => {
                eprintln!("Warning: unexpected BroadcastMessage with image");
            }
        },

        BsyncEffect::BroadcastImage {
            room: _room,
            origin,
            png_data,
        } => {
            network
                .broadcast(&ClipboardContent::Image(png_data), &origin)
                .await?;
        }

        BsyncEffect::PrintStatus { message } => {
            println!("{message}");
        }

        BsyncEffect::PromptApproval { room, peer_id } => {
            let tx = approval_tx.clone();
            tokio::task::spawn_blocking(move || {
                println!("Peer {peer_id} wants to connect in room '{room}'. Allow? [y/N]: ");
                let mut input = String::new();
                let approved = std::io::stdin().read_line(&mut input).is_ok()
                    && input.trim().eq_ignore_ascii_case("y");
                let _ = tx.send((room, peer_id, approved));
            });
        }

        BsyncEffect::SetupRoom { .. }
        | BsyncEffect::ShutdownRoom { .. }
        | BsyncEffect::AddPeer { .. } => {
            // CLI is single-room — these effects are handled at setup time.
        }

        BsyncEffect::Shutdown => {}
    }

    Ok(())
}
