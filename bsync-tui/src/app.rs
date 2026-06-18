use bsync_core::{BsyncCore, BsyncEffect, BsyncEvent, BsyncViewModel, Ticket};
use clipboard_rs::Clipboard;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Status,
    Peers,
    History,
    Help,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Status, Tab::Peers, Tab::History, Tab::Help];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Status => "Status",
            Tab::Peers => "Peers",
            Tab::History => "History",
            Tab::Help => "Help",
        }
    }

    pub fn next(self) -> Self {
        let idx = Self::ALL.iter().position(|&t| t == self).unwrap();
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }

    pub fn prev(self) -> Self {
        let idx = Self::ALL.iter().position(|&t| t == self).unwrap();
        Self::ALL[(idx + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

#[derive(Debug, Clone)]
pub enum Dialog {
    Approval { peer_id: String },
    ConnectInput { input: String },
    Error { message: String },
    Info { message: String },
}

pub struct App {
    pub core: BsyncCore,
    pub tab: Tab,
    pub history_scroll: usize,
    pub dialog: Option<Dialog>,
    pub should_quit: bool,
    pub clipboard_enabled: bool,
    pub clipboard_ctx: Option<clipboard_rs::ClipboardContext>,
    pub gossip: Option<iroh_gossip::Gossip>,
    pub room: String,
}

impl App {
    pub fn new(core: BsyncCore, room: String) -> Self {
        Self {
            core,
            tab: Tab::Status,
            history_scroll: 0,
            dialog: None,
            should_quit: false,
            clipboard_enabled: true,
            clipboard_ctx: None,
            gossip: None,
            room,
        }
    }

    pub fn view(&self) -> BsyncViewModel {
        self.core.view()
    }

    pub fn next_tab(&mut self) {
        self.tab = self.tab.next();
        self.history_scroll = 0;
    }

    pub fn prev_tab(&mut self) {
        self.tab = self.tab.prev();
        self.history_scroll = 0;
    }

    pub fn scroll_history_down(&mut self) {
        self.history_scroll = self.history_scroll.saturating_add(1);
    }

    pub fn scroll_history_up(&mut self) {
        self.history_scroll = self.history_scroll.saturating_sub(1);
    }

    pub fn open_connect_dialog(&mut self) {
        self.dialog = Some(Dialog::ConnectInput {
            input: String::new(),
        });
    }

    pub fn copy_ticket_to_clipboard(&mut self) {
        if let Some(ctx) = &self.clipboard_ctx {
            let ticket = self.view().ticket;
            let _ = ctx.set_text(ticket);
            self.dialog = Some(Dialog::Info {
                message: "Ticket copied to clipboard.".into(),
            });
        } else {
            self.dialog = Some(Dialog::Error {
                message: "Clipboard is disabled (--no-clipboard).".into(),
            });
        }
    }

    pub fn recopy_history_item(&mut self) {
        let view = self.view();
        if view.history.is_empty() {
            return;
        }
        let idx = self.history_scroll.min(view.history.len() - 1);
        if let Some(entry) = view.history.get(idx)
            && let Some(ctx) = &self.clipboard_ctx
        {
            let _ = bsync_rust::clipboard::write_clipboard(ctx, &entry.content);
            self.dialog = Some(Dialog::Info {
                message: format!(
                    "Copied to clipboard: {}",
                    &entry.preview[..entry.preview.len().min(40)]
                ),
            });
        }
    }

    pub async fn connect_to_peer(&mut self, ticket_str: String) {
        match Ticket::decode(&ticket_str) {
            Ok(ticket) => {
                let endpoint_addr = ticket.endpoint_addr.clone();
                let room = ticket.room.clone();

                let effects = self
                    .core
                    .process_event(BsyncEvent::ConnectToPeer { ticket: ticket_str });

                for effect in effects {
                    if let BsyncEffect::ConnectToEndpoint { .. } = effect {
                        match bsync_rust::gossip::parse_endpoint_addr(&endpoint_addr) {
                            Ok(peer_id) => {
                                if let Some(gossip) = &self.gossip {
                                    let topic = bsync_rust::gossip::derive_topic(&self.room);
                                    let _ = gossip.subscribe(topic, vec![peer_id]).await;

                                    self.dialog = Some(Dialog::Info {
                                        message: format!(
                                            "Connecting to peer in room '{room}'...\nThe other peer must be in the same room.",
                                        ),
                                    });
                                } else {
                                    self.dialog = Some(Dialog::Error {
                                        message: "Gossip not initialized.".into(),
                                    });
                                }
                            }
                            Err(e) => {
                                self.dialog = Some(Dialog::Error {
                                    message: format!("Invalid endpoint address: {e}"),
                                });
                            }
                        }
                    }
                }
            }
            Err(e) => {
                self.dialog = Some(Dialog::Error {
                    message: format!("Invalid ticket: {e}"),
                });
            }
        }
    }
}
