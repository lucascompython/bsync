use bsync_core::{
    BsyncCore, BsyncEffect, BsyncEvent, ClipboardContent, Config, GossipMessage, Ticket,
};
use bsync_net::{parse_endpoint_addr, Network, NetworkEvent};
use bsync_rust::{clipboard, identity};
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
                            match parse_endpoint_addr(&endpoint_addr) {
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

    let (network, mut network_rx) = Network::setup(&cli.room, &secret_key, bootstrap).await?;

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

    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<(String, bool)>();

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

            Some((peer_id, approved)) = approval_rx.recv() => {
                let event = if approved {
                    BsyncEvent::PeerApproved { id: peer_id }
                } else {
                    BsyncEvent::PeerRejected { id: peer_id }
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
    core: &mut BsyncCore,
    network: &Network,
    clipboard_ctx: &Option<clipboard_rs::ClipboardContext>,
    approval_tx: &mpsc::UnboundedSender<(String, bool)>,
) -> anyhow::Result<()> {
    match event {
        NetworkEvent::MessageReceived { from, content } => {
            let effects = core.process_event(BsyncEvent::RemoteMessageReceived { from, content });
            for effect in effects {
                dispatch_effect(effect, network, clipboard_ctx, approval_tx).await?;
            }
        }
        NetworkEvent::PeerConnected { id } => {
            let effects = core.process_event(BsyncEvent::PeerConnected { id });
            for effect in effects {
                dispatch_effect(effect, network, clipboard_ctx, approval_tx).await?;
            }
        }
        NetworkEvent::PeerDisconnected { id } => {
            let effects = core.process_event(BsyncEvent::PeerDisconnected { id });
            for effect in effects {
                dispatch_effect(effect, network, clipboard_ctx, approval_tx).await?;
            }
        }
        NetworkEvent::Lagged => {
            eprintln!("Warning: gossip stream lagged — some clipboard updates were dropped");
        }
    }
    Ok(())
}

async fn dispatch_effect(
    effect: BsyncEffect,
    network: &Network,
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
            match message {
                GossipMessage::ClipboardText { origin, content } => {
                    network
                        .broadcast(&ClipboardContent::Text(content), &origin)
                        .await?;
                }
                GossipMessage::ClipboardImage { .. } => {
                    // Core emits BroadcastImage for images, not BroadcastMessage.
                    // This arm is unreachable in practice.
                    eprintln!("Warning: unexpected BroadcastMessage with image — skipping");
                }
            }
        }

        BsyncEffect::BroadcastImage { origin, png_data } => {
            network
                .broadcast(&ClipboardContent::Image(png_data), &origin)
                .await?;
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
