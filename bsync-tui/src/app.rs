use bsync_core::{BsyncCore, BsyncViewModel};
use clipboard_rs::Clipboard;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Status,
    Peers,
    Rooms,
    History,
    Help,
}

impl Tab {
    pub const ALL: [Tab; 5] = [Tab::Status, Tab::Peers, Tab::Rooms, Tab::History, Tab::Help];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Status => "Status",
            Tab::Peers => "Peers",
            Tab::Rooms => "Rooms",
            Tab::History => "History",
            Tab::Help => "Help",
        }
    }

    pub fn next(self) -> Self {
        let idx = Self::ALL.iter().position(|t| *t == self).unwrap();
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }

    pub fn prev(self) -> Self {
        let idx = Self::ALL.iter().position(|t| *t == self).unwrap();
        Self::ALL[(idx + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

#[derive(Debug, Clone)]
pub enum Dialog {
    Approval { room: String, peer_id: String },
    ConnectInput { input: String },
    RoomCreate { input: String },
    RoomDelete { room: String },
    Error { message: String },
    Info { message: String },
}

pub struct App {
    pub core: BsyncCore,
    pub tab: Tab,
    pub history_scroll: usize,
    pub rooms_scroll: usize,
    pub dialog: Option<Dialog>,
    pub should_quit: bool,
    pub clipboard_enabled: bool,
    /// Transient notification shown in the bottom-right.
    pub notification: Option<String>,
}

impl App {
    pub fn new(core: BsyncCore) -> Self {
        Self {
            core,
            tab: Tab::Status,
            history_scroll: 0,
            rooms_scroll: 0,
            dialog: None,
            should_quit: false,
            clipboard_enabled: true,
            notification: None,
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

    pub fn scroll_rooms_down(&mut self) {
        self.rooms_scroll = self.rooms_scroll.saturating_add(1);
    }

    pub fn scroll_rooms_up(&mut self) {
        self.rooms_scroll = self.rooms_scroll.saturating_sub(1);
    }

    pub fn open_connect_dialog(&mut self) {
        self.dialog = Some(Dialog::ConnectInput {
            input: String::new(),
        });
    }

    pub fn open_room_create_dialog(&mut self) {
        self.dialog = Some(Dialog::RoomCreate {
            input: String::new(),
        });
    }

    pub fn open_room_delete_dialog(&mut self, room: String) {
        self.dialog = Some(Dialog::RoomDelete { room });
    }

    pub fn copy_ticket_to_clipboard(&mut self, clipboard_ctx: &Option<clipboard_rs::ClipboardContext>) {
        if let Some(ctx) = clipboard_ctx {
            if let Some(room) = self.view().rooms.iter().find(|r| r.is_local)
                && let Some(ticket) = &room.ticket {
                    let _ = ctx.set_text(ticket.clone());
                    self.dialog = Some(Dialog::Info {
                        message: format!("Ticket for room '{}' copied to clipboard.", room.name),
                    });
                    return;
                }
            self.dialog = Some(Dialog::Error {
                message: "No local rooms available.".into(),
            });
        } else {
            self.dialog = Some(Dialog::Error {
                message: "Clipboard is disabled (--no-clipboard).".into(),
            });
        }
    }

    pub fn copy_room_ticket(
        &mut self,
        room_name: &str,
        clipboard_ctx: &Option<clipboard_rs::ClipboardContext>,
    ) {
        if let Some(ctx) = clipboard_ctx {
            if let Some(room) = self.view().rooms.iter().find(|r| r.name == room_name) {
                if let Some(ticket) = &room.ticket {
                    let _ = ctx.set_text(ticket.clone());
                    self.dialog = Some(Dialog::Info {
                        message: format!("Ticket for '{room_name}' copied to clipboard."),
                    });
                    return;
                }
                self.dialog = Some(Dialog::Error {
                    message: format!("Room '{room_name}' is not a local room — no ticket to copy."),
                });
                return;
            }
            self.dialog = Some(Dialog::Error {
                message: format!("Room '{room_name}' not found."),
            });
        } else {
            self.dialog = Some(Dialog::Error {
                message: "Clipboard is disabled (--no-clipboard).".into(),
            });
        }
    }

    pub fn recopy_history_item(
        &mut self,
        clipboard_ctx: &Option<clipboard_rs::ClipboardContext>,
    ) {
        let view = self.view();
        if view.history.is_empty() {
            return;
        }
        let idx = self.history_scroll.min(view.history.len() - 1);
        if let Some(entry) = view.history.get(idx)
            && let Some(ctx) = clipboard_ctx
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
}
