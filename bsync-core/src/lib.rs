use std::collections::VecDeque;
use std::time::{Instant, SystemTime};

/// Maximum size of a serialized gossip message (1 MB).
pub const MAX_MESSAGE_SIZE: usize = 1024 * 1024;

/// Maximum clipboard broadcasts per second (rate limit).
const MAX_BROADCASTS_PER_SEC: usize = 4;

/// How many clipboard entries to keep in history.
const HISTORY_CAPACITY: usize = 50;
/// Truncate preview text to this many characters in history entries.
const HISTORY_PREVIEW_LEN: usize = 100;

/// Current ticket format version. Bump when the wire format changes.
pub const TICKET_VERSION: u8 = 1;

#[derive(Debug, Clone)]
pub struct Config {
    /// Human-readable peer ID (iroh EndpointId as string).
    pub peer_id: String,
    /// Room name for logical isolation.
    pub room: String,
    /// Skip peer approval prompts (dangerous for clipboard data).
    pub auto_accept: bool,
}

/// Versioned ticket for peer bootstrap. Base64-encoded JSON.
/// Contains only public info — EndpointId + room name. NOT a secret.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Ticket {
    pub v: u8,
    pub endpoint_addr: String,
    pub room: String,
}

impl Ticket {
    pub fn new(endpoint_addr: String, room: String) -> Self {
        Self {
            v: TICKET_VERSION,
            endpoint_addr,
            room,
        }
    }

    /// Encode ticket as base64(JSON).
    pub fn encode(&self) -> String {
        use base64::Engine;
        let json = serde_json::to_string(self).expect("Ticket serialization is infallible");
        base64::engine::general_purpose::STANDARD.encode(json)
    }

    /// Decode ticket from base64(JSON). Validates version.
    pub fn decode(encoded: &str) -> Result<Self, TicketError> {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| TicketError::InvalidEncoding)?;
        let ticket: Self = serde_json::from_slice(&bytes).map_err(|_| TicketError::InvalidJson)?;
        if ticket.v != TICKET_VERSION {
            return Err(TicketError::UnsupportedVersion(ticket.v));
        }
        Ok(ticket)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TicketError {
    #[error("invalid base64 encoding")]
    InvalidEncoding,
    #[error("invalid ticket JSON")]
    InvalidJson,
    #[error("unsupported ticket version {0}")]
    UnsupportedVersion(u8),
}

/// Wire protocol message. `origin` is required because iroh-gossip's
/// `delivered_from` is the forwarding neighbor, not the original author.
/// Enum from day one — adding variants is non-breaking.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum GossipMessage {
    ClipboardText { origin: String, content: String },
}

#[derive(Debug, Clone)]
pub enum BsyncEvent {
    /// Start the endpoint — core produces ticket + status.
    StartEndpoint,
    /// User wants to connect to a peer via ticket.
    ConnectToPeer { ticket: String },
    /// Local clipboard content changed (from watcher).
    LocalClipboardChanged { content: String },
    /// Remote peer sent clipboard content.
    RemoteMessageReceived { from: String, content: String },
    /// New peer connected (NeighborUp).
    PeerConnected { id: String },
    /// Peer disconnected (NeighborDown).
    PeerDisconnected { id: String },
    /// User approved a pending peer.
    PeerApproved { id: String },
    /// User rejected a pending peer.
    PeerRejected { id: String },
    /// Graceful shutdown requested (Ctrl+C).
    Shutdown,
}

#[derive(Debug)]
pub enum BsyncEffect {
    /// Shell writes content to local clipboard. `write_hash` is for echo guard.
    WriteClipboard {
        content: String,
        write_hash: [u8; 32],
    },
    /// Shell serializes message to JSON bytes and broadcasts via gossip.
    BroadcastMessage { message: GossipMessage },
    /// Shell prints a status line to the user.
    PrintStatus { message: String },
    /// Shell prompts the user to approve/reject a pending peer.
    PromptApproval { peer_id: String },
    /// Shell resolves the endpoint address and bootstraps a connection.
    ConnectToEndpoint { endpoint_addr: String },
    /// Shutdown signal — shell performs cleanup.
    Shutdown,
}

/// A single entry in the clipboard history.
#[derive(Debug, Clone)]
pub struct ClipboardHistoryEntry {
    /// Truncated preview of the clipboard content.
    pub preview: String,
    /// Whether this originated locally (true) or from a remote peer (false).
    pub is_local: bool,
    /// Peer ID of the origin (equals our peer_id for local copies).
    pub origin: String,
    /// When this entry was recorded (system time, for display).
    pub timestamp: SystemTime,
}

#[derive(Debug, Clone)]
pub struct BsyncViewModel {
    pub peer_id: String,
    pub ticket: String,
    pub room: String,
    pub connected_peers: Vec<String>,
    pub pending_peers: Vec<String>,
    pub status: String,
    pub history: Vec<ClipboardHistoryEntry>,
}

pub struct BsyncCore {
    peer_id: String,
    room: String,
    auto_accept: bool,
    connected_peers: Vec<String>,
    pending_peers: Vec<String>,
    /// Hash of the last remote content we applied (dedup).
    last_applied_hash: Option<blake3::Hash>,
    /// Hash of the content we just wrote to clipboard (echo guard).
    pending_write_hash: Option<blake3::Hash>,
    /// Timestamps of recent broadcasts (rate limiting).
    broadcast_times: VecDeque<Instant>,
    /// Ring buffer of recent clipboard entries (newest first).
    history: VecDeque<ClipboardHistoryEntry>,
    status: String,
}

impl BsyncCore {
    pub fn new(config: Config) -> Self {
        Self {
            peer_id: config.peer_id,
            room: config.room,
            auto_accept: config.auto_accept,
            connected_peers: Vec::new(),
            pending_peers: Vec::new(),
            last_applied_hash: None,
            pending_write_hash: None,
            broadcast_times: VecDeque::with_capacity(MAX_BROADCASTS_PER_SEC),
            history: VecDeque::with_capacity(HISTORY_CAPACITY),
            status: String::new(),
        }
    }

    /// Process a single event. Returns zero or more effects the shell must dispatch.
    pub fn process_event(&mut self, event: BsyncEvent) -> Vec<BsyncEffect> {
        match event {
            BsyncEvent::StartEndpoint => self.handle_start(),
            BsyncEvent::ConnectToPeer { ticket } => self.handle_connect(ticket),
            BsyncEvent::LocalClipboardChanged { content } => self.handle_local_change(content),
            BsyncEvent::RemoteMessageReceived { from, content } => {
                self.handle_remote_message(from, content)
            }
            BsyncEvent::PeerConnected { id } => self.handle_peer_connected(id),
            BsyncEvent::PeerDisconnected { id } => self.handle_peer_disconnected(id),
            BsyncEvent::PeerApproved { id } => self.handle_peer_approved(id),
            BsyncEvent::PeerRejected { id } => self.handle_peer_rejected(id),
            BsyncEvent::Shutdown => self.handle_shutdown(),
        }
    }

    /// Read-only snapshot for UI rendering.
    pub fn view(&self) -> BsyncViewModel {
        BsyncViewModel {
            peer_id: self.peer_id.clone(),
            ticket: Ticket::new(self.peer_id.clone(), self.room.clone()).encode(),
            room: self.room.clone(),
            connected_peers: self.connected_peers.clone(),
            pending_peers: self.pending_peers.clone(),
            status: self.status.clone(),
            history: self.history.iter().rev().cloned().collect(),
        }
    }

    fn handle_start(&mut self) -> Vec<BsyncEffect> {
        let ticket = Ticket::new(self.peer_id.clone(), self.room.clone()).encode();
        self.status = "waiting for connections...".into();
        vec![BsyncEffect::PrintStatus {
            message: format!("Ticket:  {}\nRoom:    {}\n", ticket, self.room),
        }]
    }

    fn handle_connect(&mut self, ticket_str: String) -> Vec<BsyncEffect> {
        match Ticket::decode(&ticket_str) {
            Ok(ticket) => {
                self.status = format!("connecting to {}...", ticket.endpoint_addr);
                vec![BsyncEffect::ConnectToEndpoint {
                    endpoint_addr: ticket.endpoint_addr,
                }]
            }
            Err(e) => vec![BsyncEffect::PrintStatus {
                message: format!("Invalid ticket: {e}"),
            }],
        }
    }

    fn handle_local_change(&mut self, content: String) -> Vec<BsyncEffect> {
        let hash = blake3::hash(content.as_bytes());

        // Echo guard: if this matches our pending write, suppress broadcast.
        if self.pending_write_hash == Some(hash) {
            self.pending_write_hash = None;
            return vec![];
        }

        // Record in history regardless of rate limiting.
        let origin = self.peer_id.clone();
        self.push_history(content.clone(), &origin, true);

        // Rate limit: at most MAX_BROADCASTS_PER_SEC broadcasts per second.
        self.prune_broadcast_times();
        if self.broadcast_times.len() >= MAX_BROADCASTS_PER_SEC {
            return vec![];
        }

        self.broadcast_times.push_back(Instant::now());
        self.status = "syncing...".into();

        vec![BsyncEffect::BroadcastMessage {
            message: GossipMessage::ClipboardText {
                origin: self.peer_id.clone(),
                content,
            },
        }]
    }

    fn handle_remote_message(&mut self, from: String, content: String) -> Vec<BsyncEffect> {
        // Approval gate: only process messages from approved peers.
        // Pending, rejected, or unknown peers are silently dropped.
        if !self.connected_peers.contains(&from) {
            return vec![];
        }

        let hash = blake3::hash(content.as_bytes());

        // Dedup: skip if we already applied this exact content.
        if self.last_applied_hash == Some(hash) {
            return vec![];
        }

        self.last_applied_hash = Some(hash);
        self.pending_write_hash = Some(hash); // Set echo guard
        self.push_history(content.clone(), &from, false);

        vec![BsyncEffect::WriteClipboard {
            content,
            write_hash: hash.into(),
        }]
    }

    fn handle_peer_connected(&mut self, id: String) -> Vec<BsyncEffect> {
        if self.connected_peers.contains(&id) || self.pending_peers.contains(&id) {
            return vec![];
        }

        if self.auto_accept {
            self.connected_peers.push(id.clone());
            self.status = format!("connected to {id}");
            vec![BsyncEffect::PrintStatus {
                message: format!("Peer connected: {id} (auto-accepted)"),
            }]
        } else {
            self.pending_peers.push(id.clone());
            vec![BsyncEffect::PromptApproval { peer_id: id }]
        }
    }

    fn handle_peer_disconnected(&mut self, id: String) -> Vec<BsyncEffect> {
        self.connected_peers.retain(|p| p != &id);
        self.pending_peers.retain(|p| p != &id);
        self.status = format!("peer {id} disconnected");
        vec![BsyncEffect::PrintStatus {
            message: format!("Peer disconnected: {id}"),
        }]
    }

    fn handle_peer_approved(&mut self, id: String) -> Vec<BsyncEffect> {
        if let Some(pos) = self.pending_peers.iter().position(|p| p == &id) {
            self.pending_peers.remove(pos);
            self.connected_peers.push(id.clone());
            self.status = format!("connected to {id}");
            vec![BsyncEffect::PrintStatus {
                message: format!("Peer approved: {id}"),
            }]
        } else {
            vec![]
        }
    }

    fn handle_peer_rejected(&mut self, id: String) -> Vec<BsyncEffect> {
        self.pending_peers.retain(|p| p != &id);
        vec![BsyncEffect::PrintStatus {
            message: format!("Peer rejected: {id}"),
        }]
    }

    fn handle_shutdown(&mut self) -> Vec<BsyncEffect> {
        self.status = "shutting down...".into();
        vec![BsyncEffect::Shutdown]
    }

    // ── Helpers ──────────────────────────────────────────────

    /// Remove broadcast timestamps older than 1 second.
    fn prune_broadcast_times(&mut self) {
        let now = Instant::now();
        let one_sec = std::time::Duration::from_secs(1);
        while self
            .broadcast_times
            .front()
            .is_some_and(|t| now.duration_since(*t) >= one_sec)
        {
            self.broadcast_times.pop_front();
        }
    }

    /// Push a clipboard entry to history, evicting oldest if at capacity.
    fn push_history(&mut self, content: String, origin: &str, is_local: bool) {
        let preview: String = if content.chars().count() > HISTORY_PREVIEW_LEN {
            format!(
                "{}\u{2026}",
                content
                    .chars()
                    .take(HISTORY_PREVIEW_LEN)
                    .collect::<String>()
            )
        } else {
            content
        };
        let entry = ClipboardHistoryEntry {
            preview,
            is_local,
            origin: origin.to_string(),
            timestamp: SystemTime::now(),
        };
        if self.history.len() >= HISTORY_CAPACITY {
            self.history.pop_front();
        }
        self.history.push_back(entry);
    }
}

// ── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_core() -> BsyncCore {
        BsyncCore::new(Config {
            peer_id: "test-peer-1".into(),
            room: "test-room".into(),
            auto_accept: false,
        })
    }

    /// Helper: connect + approve a peer so its messages pass the approval gate.
    fn connect_peer(core: &mut BsyncCore, id: &str) {
        core.process_event(BsyncEvent::PeerConnected { id: id.into() });
        core.process_event(BsyncEvent::PeerApproved { id: id.into() });
    }

    #[test]
    fn start_produces_ticket() {
        let mut core = test_core();
        let effects = core.process_event(BsyncEvent::StartEndpoint);
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            BsyncEffect::PrintStatus { message } => {
                assert!(message.contains("Ticket:"));
                assert!(message.contains("Room:"));
            }
            _ => panic!("expected PrintStatus"),
        }
    }

    #[test]
    fn ticket_roundtrip() {
        let ticket = Ticket::new("test-addr".into(), "my-room".into());
        let encoded = ticket.encode();
        let decoded = Ticket::decode(&encoded).unwrap();
        assert_eq!(decoded.endpoint_addr, "test-addr");
        assert_eq!(decoded.room, "my-room");
        assert_eq!(decoded.v, 1);
    }

    #[test]
    fn ticket_rejects_invalid_version() {
        let ticket = Ticket {
            v: 99,
            endpoint_addr: "addr".into(),
            room: "room".into(),
        };
        let encoded = ticket.encode();
        let result = Ticket::decode(&encoded);
        assert!(result.is_err());
    }

    #[test]
    fn echo_guard_suppresses_own_write() {
        let mut core = test_core();

        // Connect peer first (approval gate requires it)
        connect_peer(&mut core, "peer-2");

        // Simulate remote message → WriteClipboard sets pending_write_hash
        let effects = core.process_event(BsyncEvent::RemoteMessageReceived {
            from: "peer-2".into(),
            content: "hello".into(),
        });
        assert_eq!(effects.len(), 1); // One WriteClipboard

        // Local change with same content → should be suppressed
        let effects = core.process_event(BsyncEvent::LocalClipboardChanged {
            content: "hello".into(),
        });
        assert!(effects.is_empty(), "echo guard should suppress own write");
    }

    #[test]
    fn local_change_after_echo_guard_clear_broadcasts() {
        let mut core = test_core();
        connect_peer(&mut core, "peer-2");

        // Write from remote → pending_write_hash set
        core.process_event(BsyncEvent::RemoteMessageReceived {
            from: "peer-2".into(),
            content: "hello".into(),
        });

        // Echo guard clears on first local detection
        core.process_event(BsyncEvent::LocalClipboardChanged {
            content: "hello".into(),
        });

        // Different content → should broadcast normally
        let effects = core.process_event(BsyncEvent::LocalClipboardChanged {
            content: "world".into(),
        });
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            BsyncEffect::BroadcastMessage {
                message: GossipMessage::ClipboardText { origin, content },
            } => {
                assert_eq!(origin, "test-peer-1");
                assert_eq!(content, "world");
            }
            _ => panic!("expected BroadcastMessage"),
        }
    }

    #[test]
    fn remote_dedup_skips_identical_content() {
        let mut core = test_core();
        connect_peer(&mut core, "peer-2");

        let effects1 = core.process_event(BsyncEvent::RemoteMessageReceived {
            from: "peer-2".into(),
            content: "dup".into(),
        });
        assert_eq!(effects1.len(), 1);

        let effects2 = core.process_event(BsyncEvent::RemoteMessageReceived {
            from: "peer-2".into(),
            content: "dup".into(),
        });
        assert!(effects2.is_empty(), "dedup should skip identical content");
    }

    #[test]
    fn peer_approval_flow() {
        let mut core = test_core();

        // Peer connects → prompt
        let effects = core.process_event(BsyncEvent::PeerConnected { id: "p2".into() });
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], BsyncEffect::PromptApproval { .. }));

        // User approves → peer moves to connected
        let effects = core.process_event(BsyncEvent::PeerApproved { id: "p2".into() });
        assert_eq!(effects.len(), 1);

        let view = core.view();
        assert!(view.connected_peers.contains(&"p2".to_string()));
        assert!(view.pending_peers.is_empty());
    }

    #[test]
    fn peer_rejected_does_not_connect() {
        let mut core = test_core();

        core.process_event(BsyncEvent::PeerConnected { id: "p2".into() });
        core.process_event(BsyncEvent::PeerRejected { id: "p2".into() });

        let view = core.view();
        assert!(view.connected_peers.is_empty());
        assert!(view.pending_peers.is_empty());
    }

    #[test]
    fn auto_accept_skips_prompt() {
        let mut core = BsyncCore::new(Config {
            peer_id: "p1".into(),
            room: "r".into(),
            auto_accept: true,
        });

        let effects = core.process_event(BsyncEvent::PeerConnected { id: "p2".into() });
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], BsyncEffect::PrintStatus { .. }));

        let view = core.view();
        assert!(view.connected_peers.contains(&"p2".to_string()));
    }

    #[test]
    fn unapproved_peer_messages_are_dropped() {
        let mut core = test_core();

        // Peer connects but is NOT approved
        core.process_event(BsyncEvent::PeerConnected { id: "p2".into() });

        // Message from pending peer → should be dropped
        let effects = core.process_event(BsyncEvent::RemoteMessageReceived {
            from: "p2".into(),
            content: "secret".into(),
        });
        assert!(
            effects.is_empty(),
            "messages from unapproved peers must be dropped"
        );

        // Reject the peer
        core.process_event(BsyncEvent::PeerRejected { id: "p2".into() });

        // Message from rejected peer → still dropped
        let effects = core.process_event(BsyncEvent::RemoteMessageReceived {
            from: "p2".into(),
            content: "secret".into(),
        });
        assert!(
            effects.is_empty(),
            "messages from rejected peers must be dropped"
        );
    }

    #[test]
    fn history_records_local_and_remote() {
        let mut core = test_core();
        connect_peer(&mut core, "peer-2");

        // Local copy
        core.process_event(BsyncEvent::LocalClipboardChanged {
            content: "local text".into(),
        });

        // Remote receive
        core.process_event(BsyncEvent::RemoteMessageReceived {
            from: "peer-2".into(),
            content: "remote text".into(),
        });

        let view = core.view();
        assert_eq!(view.history.len(), 2);

        // Newest first (reversed from internal deque)
        assert_eq!(view.history[0].preview, "remote text");
        assert!(!view.history[0].is_local);
        assert_eq!(view.history[0].origin, "peer-2");

        assert_eq!(view.history[1].preview, "local text");
        assert!(view.history[1].is_local);
        assert_eq!(view.history[1].origin, "test-peer-1");
    }

    #[test]
    fn history_truncates_long_content() {
        let mut core = test_core();
        let long = "x".repeat(200);
        core.process_event(BsyncEvent::LocalClipboardChanged { content: long });

        let view = core.view();
        assert_eq!(view.history.len(), 1);
        assert!(view.history[0].preview.ends_with('\u{2026}'));
        assert!(view.history[0].preview.chars().count() <= 101); // 100 + ellipsis
    }

    #[test]
    fn history_caps_at_max_entries() {
        let mut core = test_core();
        for i in 0..60 {
            core.process_event(BsyncEvent::LocalClipboardChanged {
                content: format!("item {i}"),
            });
        }
        let view = core.view();
        assert_eq!(view.history.len(), 50);
    }

    #[test]
    fn view_includes_room() {
        let core = test_core();
        let view = core.view();
        assert_eq!(view.room, "test-room");
        assert!(!view.ticket.is_empty());
    }
}
