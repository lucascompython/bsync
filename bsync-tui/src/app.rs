use bsync_core::{BsyncCore, BsyncViewModel};

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

/// Modal dialog overlay state.
#[derive(Debug, Clone)]
pub enum Dialog {
    /// Peer approval prompt.
    Approval { peer_id: String },
    /// Ticket input for connecting to a peer.
    ConnectInput { input: String },
    /// Error message display.
    Error { message: String },
    /// Info message display.
    Info { message: String },
}

pub struct App {
    pub core: BsyncCore,
    pub tab: Tab,
    pub history_scroll: usize,
    pub dialog: Option<Dialog>,
    pub should_quit: bool,
    /// Clipboard context for writing (None if --no-clipboard).
    pub clipboard_enabled: bool,
}

impl App {
    pub fn new(core: BsyncCore) -> Self {
        Self {
            core,
            tab: Tab::Status,
            history_scroll: 0,
            dialog: None,
            should_quit: false,
            clipboard_enabled: true,
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
}
