# bsync — MVP Architecture & Implementation Roadmap

## 1. Project Overview

**bsync** is a cross-platform P2P clipboard syncing tool. The MVP is a Rust CLI that lets two or more machines share clipboard text in real-time over a peer-to-peer network.

**Core flow**: Start the app → get your peer ID + ticket → share ticket with another peer → connect → clipboard text syncs automatically.

**License**: GPL-3.0

## 2. MVP Feature Set

| Feature | In MVP | Deferred |
|---|---|---|
| Text clipboard sync | ✅ | |
| P2P via iroh-gossip | ✅ | |
| CLI interface | ✅ | |
| Identity persistence (with file permissions) | ✅ | |
| Ticket-based peer connection (versioned) | ✅ | |
| Echo loop prevention (in core) | ✅ | |
| Event-driven clipboard watching | ✅ | |
| Graceful shutdown | ✅ | |
| `--no-clipboard` debug mode | ✅ | |
| Startup security warning | ✅ | |
| 1MB serialized message size limit | ✅ | |
| Rate limiting (4/sec) | ✅ | |
| `--room <name>` for logical isolation | ✅ | |
| Peer approval prompt | ✅ | |
| Received-message dedup | ✅ | |
| Explicit threat model | ✅ | |
| Image/HTML/file sync | | ✅ |
| UI frontends (Crux, etc.) | | ✅ |
| TUI frontend (ratatui) | ✅ |
| boltffi native bindings | | ✅ |
| Selective sync / filtering | | ✅ |
| End-to-end encryption (app-level) | | ✅ |
| Clipboard history | ✅ |
| Auto-discovery / mDNS | | ✅ |
| Content filtering (`--no-sync-patterns`) | | ✅ |
| Primary selection sync (Linux) | | ✅ |
| Echo guard ring buffer | | ✅ |

## 3. Architecture

### Core/Shell Separation

The architecture separates **pure logic** (the core) from **I/O effects** (the shell). This is critical for the Crux migration path and for supporting mobile/web frontends where clipboard I/O must use native APIs.

**Key constraint**: `BsyncCore` has **zero I/O dependencies** — no clipboard-rs, no arboard, no iroh. Only `BsyncEvent`/`BsyncEffect` cross the FFI boundary. Each shell implements its own clipboard watching and writing.

```
┌──────────────┐  BsyncEvent    ┌──────────┐  broadcast()  ┌──────────┐
│ Clipboard    │ ─────────────→ │          │ ────────────→ │          │
│ Watcher      │                │ BsyncCore│               │ bsync-net│
│ (bsync-rust) │ ←───────────── │ (pure)   │ ←──────────── │ (iroh)   │
└──────────────┘  BsyncEffect   └──────────┘  NetworkEvent └──────────┘

Shells merge the NetworkEvent stream with their own input streams
(clipboard watcher, keyboard, terminal events) in a single select! loop.
bsync-net owns: endpoint setup, gossip event loop, blob upload/download,
message serialization. Shells never touch GossipMessage, Hash, or EndpointId.
```

### Core API

```rust
pub struct BsyncCore {
    // State: connected peers, last applied hash, pending write hash (echo guard), room name
    // ZERO I/O dependencies — no clipboard-rs, no arboard, no iroh
}

impl BsyncCore {
    pub fn new(config: Config) -> Self;
    pub fn process_event(&mut self, event: BsyncEvent) -> Vec<BsyncEffect>;
    pub fn view(&self) -> BsyncViewModel;
}

pub enum BsyncEvent {
    StartEndpoint,
    ConnectToPeer { ticket: String },
    LocalClipboardChanged { content: String },
    RemoteMessageReceived { from: EndpointId, content: String },
    PeerConnected { id: EndpointId },
    PeerDisconnected { id: EndpointId },
    PeerApproved { id: EndpointId },
    PeerRejected { id: EndpointId },
    Shutdown,
}

pub enum BsyncEffect {
    WriteClipboard { content: String, write_hash: blake3::Hash },
    BroadcastGossip { message: GossipMessage },
    PrintStatus { message: String },
    StoreIdentity { key: SecretKey },
    PromptApproval { peer_id: EndpointId },
}

pub struct BsyncViewModel {
    pub peer_id: String,
    pub ticket: String,
    pub room: String,
    pub connected_peers: Vec<EndpointId>,
    pub pending_peers: Vec<EndpointId>,
    pub status: String,
    pub history: Vec<ClipboardHistoryEntry>,
}

pub struct ClipboardHistoryEntry {
    pub preview: String,       // truncated to 100 chars
    pub is_local: bool,        // true = sent, false = received
    pub origin: String,        // peer ID of origin
    pub timestamp: SystemTime, // for display
}
```

The `write_hash` in `WriteClipboard` is pre-computed by the core. When the shell later reports `LocalClipboardChanged`, the core checks the content's hash against its pending write hash — if they match, it's an echo, suppress broadcast. This works uniformly across ALL platforms (Rust, Swift, Kotlin, JS) without the shell computing hashes or sharing state.

### Data Flow

1. **Local copy** → Clipboard watcher (event-driven on Win/X11/Wayland, 500ms poll on macOS) → detects change → sends `BsyncEvent::LocalClipboardChanged` to core
2. **Core processes event** → checks echo guard (compares content hash against pending write hash) → returns `BsyncEffect::BroadcastMessage` or `BsyncEffect::BroadcastImage` → shell calls `network.broadcast(&content, &origin)` → bsync-net serializes `GossipMessage`, uploads image blob if needed, broadcasts over gossip
3. **Remote receive** → bsync-net gossip event loop gets `Event::Received` → deserializes `GossipMessage` → downloads image blob if needed → emits `NetworkEvent::MessageReceived { from, content: ClipboardContent }` → shell feeds `BsyncEvent::RemoteMessageReceived` to core. Also handles `NeighborUp` → `PeerConnected`, `NeighborDown` → `PeerDisconnected`, `Lagged` → warning.
4. **Core processes remote event** → checks `last_applied_remote_hash` for dedup → computes `write_hash` → returns `BsyncEffect::WriteClipboard { content, write_hash }` → shell writes to clipboard. Echo guard is now in core — no shared `Arc` state.
5. **New peer connects** → `NetworkEvent::PeerConnected` → `BsyncEvent::PeerConnected` → core returns `BsyncEffect::PromptApproval` → shell prompts user → `BsyncEvent::PeerApproved`/`PeerRejected`

### Network Layer (bsync-net)

All iroh gossip + blobs logic lives in `bsync-net`. Shells interact through two surfaces:

```rust
// Setup — returns a Network handle + NetworkEvent receiver
let (network, mut network_rx) = Network::setup(&room, &secret_key, bootstrap).await?;

// Broadcast — handles text serialization + image blob upload internally
network.broadcast(&content, &origin).await?;

// Event stream — shells merge this with their own input streams in select!
while let Some(event) = network_rx.recv().await {
    match event {
        NetworkEvent::MessageReceived { from, content } => { /* feed to core */ }
        NetworkEvent::PeerConnected { id } => { /* feed to core */ }
        NetworkEvent::PeerDisconnected { id } => { /* feed to core */ }
        NetworkEvent::Lagged => { /* log warning */ }
    }
}
```

The gossip event loop runs in a background task spawned via `n0_future::task::spawn` (runtime-agnostic — tokio on native, `wasm-bindgen-futures` on WASM). Shells never see `GossipMessage`, `Hash`, or `EndpointId` parsing.

### Clipboard Watching Strategy

**Single crate: clipboard-rs for watching + reading + writing.** arboard dropped for MVP.

| Platform | Mechanism | Crate | Latency |
|---|---|---|---|
| **Windows** | `WM_CLIPBOARDUPDATE` | `clipboard-rs` | ~0ms |
| **Linux X11** | `XFixes` selection notify | `clipboard-rs` | ~0ms |
| **Linux Wayland** | `ext-data-control-v1` / `wlr-data-control` | `clipboard-rs` | ~0ms |
| **macOS** | Poll `NSPasteboard.changeCount` | `clipboard-rs` (internal 500ms poll) | 250–500ms |

macOS has **no public clipboard change notification API**. This is an Apple limitation — every Mac clipboard manager polls. clipboard-rs polls at 500ms internally.

```rust
// clipboard.rs abstraction (in CLI shell, NOT in core):
trait ClipboardChangeSource: Send {
    fn next_change(&mut self) -> Result<String>;
}

// All platforms: clipboard-rs ClipboardWatcher
struct ClipboardWatcher { /* clipboard-rs internals */ }
```

**Note on `wayland-clipboard-listener`**: Available under GPL-3.0 (same license as bsync). Has `ListenOnSelect` for primary selection (middle-click buffer). Deferred for MVP — test `clipboard-rs` on GNOME, KDE, Sway, Hyprland first. If unreliable, swap in via `ClipboardChangeSource` trait. Primary selection sync is post-MVP.

**Note on arboard**: Dropped for MVP. clipboard-rs handles read + write + watch on all desktop platforms. Re-evaluate arboard when adding image/HTML/file sync (post-MVP) — arboard has better support for those content types.

### Echo Loop Prevention (in core, not shared state)

The critical correctness concern. When we write a remote update to the local clipboard, our own watcher will detect it as a "change". Without prevention, this creates an infinite echo loop.

**Solution**: Echo guard logic lives entirely in `BsyncCore`. The `WriteClipboard` effect carries a `write_hash`. When the shell reports `LocalClipboardChanged`, the core checks the content hash against its pending write hash.

```rust
// In BsyncCore::process_event:

// When processing RemoteMessageReceived:
BsyncEvent::RemoteMessageReceived { from, content } => {
    let hash = blake3::hash(content.as_bytes());
    if self.last_applied_hash == Some(hash) {
        return vec![]; // Already applied — skip
    }
    self.last_applied_hash = Some(hash);
    self.pending_write_hash = Some(hash); // Set echo guard
    vec![BsyncEffect::WriteClipboard { content, write_hash: hash }]
}

// When processing LocalClipboardChanged:
BsyncEvent::LocalClipboardChanged { content } => {
    let hash = blake3::hash(content.as_bytes());
    if self.pending_write_hash == Some(hash) {
        self.pending_write_hash = None; // Clear echo guard
        return vec![]; // Skip — this was our own write
    }
    vec![BsyncEffect::BroadcastGossip { message: GossipMessage::ClipboardText { origin: self.peer_id, content } }]
}
```

This works uniformly across ALL platforms — Rust, Swift, Kotlin, JS — without the shell computing hashes or sharing state.

**Known theoretical race**: Two rapid remote messages can cause the second to overwrite `pending_write_hash` before the watcher checks against the first. Result: false broadcast of remote content back to mesh (deduped by remote peer). Acceptable for MVP. Post-MVP: ring buffer of recent remote hashes.

### Performance Safeguards

```rust
const MAX_MESSAGE_SIZE: usize = 1024 * 1024; // 1MB (checked on serialized message, not raw content)
const MAX_BROADCASTS_PER_SEC: usize = 4;

// Before broadcasting:
let msg = GossipMessage::ClipboardText { origin, content };
let payload = serde_json::to_vec(&msg)?;
if payload.len() > MAX_MESSAGE_SIZE {
    eprintln!("Message too large ({} bytes). Skipping sync.", payload.len());
    continue;
}
// Rate limit: track broadcasts in a sliding window
```

### Security

**⚠️ bsync sends your clipboard contents to ALL connected peers.** This is the tool's purpose — but it means connected peers can see passwords, 2FA codes, API keys, and private text.

**MVP threat model**: bsync assumes all connected peers are trusted. A malicious peer can see all clipboard content and inject arbitrary text. Mitigations (startup warning, approval prompt) are UX-level, not cryptographic. Peer approval prevents unintended connections, not malicious peers. Post-MVP E2E encryption addresses confidentiality; content signing addresses integrity.

**MVP mitigations:**
- Startup warning printed on every launch
- Peer approval prompt on new connections (default: required, `--auto-accept` to skip — **dangerous for clipboard data**)
- Topic is NOT a security boundary — isolation comes from disjoint bootstrap peer sets
- Ticket contains only public info (EndpointId + room name) — not a secret

**Post-MVP:**
- PSK-based E2E encryption (ticket includes `psk_hint`, actual PSK entered out-of-band)
- Content filtering (`--no-sync-patterns <regex>`) to prevent syncing JWTs, Bearer tokens, etc.

## 4. Module Structure

```
bsync/
├── Cargo.toml                       # workspace root
├── bsync-core/                      # pure logic — ZERO I/O deps
│   ├── Cargo.toml
│   └── src/
│       └── lib.rs                    # BsyncCore, BsyncEvent, BsyncEffect, BsyncViewModel,
│                                    #   GossipMessage, Ticket, echo guard, dedup, rate limit,
│                                    #   clipboard history (50-entry ring buffer)
├── bsync-net/                       # shared networking layer — platform-agnostic
│   ├── Cargo.toml                   #   iroh gossip + blobs, WASM-compatible
│   └── src/
│       └── lib.rs                    # Network::setup/broadcast/add_peer/shutdown,
│                                    #   NetworkEvent stream, gossip event loop,
│                                    #   blob upload/download, message serialization
├── bsync-rust/                      # Rust platform I/O — clipboard + identity only
│   ├── Cargo.toml                   #   (used by Rust shells; NOT for non-Rust shells)
│   └── src/
│       ├── lib.rs                    # re-exports
│       ├── clipboard.rs              # clipboard-rs watcher in background thread
│       └── identity.rs               # key persistence (0o600/0o700), iroh-base SecretKey
├── bsync-tui/                       # unified shell — TUI (default) + CLI (--cli flag)
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs                   # clap arg parsing, TUI event loop, dispatches to CLI
│       ├── cli.rs                    # plain CLI event loop (stdin/stdout)
│       ├── app.rs                    # TUI state: tabs, dialogs, scroll
│       └── ui.rs                     # ratatui rendering: Status, Peers, History, Help tabs
```

**Four crates, one core.** `bsync-core` has zero I/O dependencies — compiles to every target (desktop, wasm, mobile). `bsync-net` owns the entire networking layer (iroh gossip + blobs) and exposes a clean `NetworkEvent` stream + `broadcast()` API — compiles to native and WASM via target-conditional feature flags. `bsync-rust` holds Rust platform I/O (clipboard-rs, filesystem identity) — used only by Rust shells. Non-Rust shells (SwiftUI, Compose, Svelte) implement clipboard and identity via native APIs but share `bsync-net` for networking. `bsync-tui` is the unified binary: defaults to TUI, `--cli` flag runs plain CLI mode.

**Why core/net/shell from day one?** When Crux arrives, its shell calls `core.process_event()` and `core.view()` directly. When boltffi arrives, each `BsyncCore` method becomes a C function. When mobile/web frontends arrive, each shell implements its own clipboard I/O (native APIs) while sharing `bsync-net` for networking and `bsync-core` for logic including echo guard. The core has zero I/O dependencies — only `BsyncEvent`/`BsyncEffect` cross the FFI boundary. The network layer is shared across all platforms — only `NetworkEvent`/`broadcast()` cross the shell boundary.

## 5. Key Design Decisions

### Message Protocol

```rust
#[derive(Serialize, Deserialize)]
enum GossipMessage {
    ClipboardText { origin: EndpointId, content: String },
    // Future variants: FileTransfer, SelectiveSync, etc.
}
```

- **`origin` field**: Required because iroh-gossip's `delivered_from` is the forwarding neighbor, not the original author.
- **No sequence numbers**: Break on peer restart. Hash-based dedup is sufficient.
- **No timestamp**: Sub-second ordering between two people copying text is cosmetic. Reintroduce when adding clipboard history or conflict UI.
- **Enum from day one**: Adding file transfer later means adding a variant — no breaking change.
- **Wire format**: `serde_json` for MVP (human-debuggable). Post-MVP: swap to `postcard` for deterministic encoding (needed for message signing). Same `#[derive(Serialize, Deserialize)]` types work with both.

### Identity Persistence

- `SecretKey` serialized to `~/.config/bsync/identity.key` (or OS equivalent via `dirs-rs`)
- **File permissions**: Config dir `0o700`, key file `0o600` (Unix). Prevents world-readable keys on multi-user systems.
- On subsequent runs, load existing key → same `EndpointId`
- First run: generate + save + print ID + ticket
- **Browser**: Identity persisted via `localStorage` or `IndexedDB` (no filesystem)

### Ticket Format

```rust
#[derive(Serialize, Deserialize)]
struct Ticket {
    v: u8,              // Version 1 — prevents ambiguity when format evolves
    endpoint_addr: String,
    room: String,       // Room name for topic derivation
}
```

- Base64-encoded JSON
- Printed on startup alongside human-readable peer ID
- Used with `--connect <ticket>` to bootstrap a connection
- **Ticket is public info** — it contains EndpointId + room name, not a secret

### Gossip Topic

- Topic derived from room name: `blake3::hash("bsync-clipboard-v1:" + room_name)` → 32 bytes → `TopicId`
- Default room: `"default"` (when `--room` not specified)
- `--room <name>` flag provides logical isolation between different bsync user groups
- **Topic is NOT a security boundary** — isolation is logical, not cryptographic
- Bootstrap: the `--connect` peer's `EndpointId`

### CLI Interface

```
$ bsync                                    # Start TUI (default), wait for connections
$ bsync --cli                              # Start in plain CLI mode (no TUI)
$ bsync --connect <ticket>                 # Start + connect to peer
$ bsync --room work                        # Use room "work" instead of default
$ bsync --no-clipboard                     # Network-only mode (debugging Wayland issues)
$ bsync --auto-accept                      # Skip peer approval prompt (dangerous for clipboard data)
```

### TUI Keyboard Shortcuts

- `Tab` / `Shift+Tab` — switch tabs
- `1`-`4` — jump to tab directly
- `↑`/`↓` or `j`/`k` — scroll history
- `c` — open connect dialog (paste ticket with Ctrl+V)
- `y`/`n` — approve/reject pending peer
- `Esc` — close dialog
- `q` — quit

Startup warning (always printed):
```
⚠️  bsync sends your clipboard contents to ALL connected peers.
    Only connect to peers you trust completely.
    Connected peers can see: passwords, 2FA codes, API keys, private text.
```

### Shutdown

- `tokio::signal::ctrl_c()` in main async context
- Send `BsyncEvent::Shutdown` to core → core returns cleanup effects
- Close channel senders → signal watcher thread
- Join watcher thread with 2-second timeout (prevents zombie)
- Graceful gossip leave

### Error Handling

- `anyhow` for application errors
- `Clipboard::new()` failure → wrap with compositor guidance + exit
- Network errors → log + retry with backoff (iroh handles reconnection internally)
- Gossip `Lagged` → log warning, continue (clipboard data is ephemeral)
- Connection timeout (30s) → clear message + non-zero exit code
- iroh-gossip 0.101.0 `Event` has exactly 4 variants (not `#[non_exhaustive]`) — all matched explicitly in `bsync-net`, no wildcard arm needed

## 6. Dependencies

**`bsync-core`** (pure logic, zero I/O deps):
```toml
serde      = { version = "1", features = ["derive"] }
serde_json = "1"
blake3     = "1"
base64     = "0.22"
thiserror  = "2"
```

**`bsync-net`** (shared networking layer — iroh gossip + blobs, platform-agnostic):
```toml
bsync-core   = { path = "../bsync-core" }
iroh         = "=1.0.0"
iroh-gossip  = "=0.101.0"
iroh-blobs   = "=0.103.0"
tokio        = { version = "1", features = ["sync", "io-util"] }
anyhow       = "1"
blake3       = "1"
serde_json   = "1"
n0-future    = "0.3"

# WASM target — disable iroh defaults, re-enable minimal features
[target.'cfg(target_arch = "wasm32")'.dependencies]
iroh         = { version = "=1.0.0", default-features = false, features = ["tls-ring"] }
iroh-gossip  = { version = "=0.101.0", default-features = false, features = ["net"] }
iroh-blobs   = { version = "=0.103.0", default-features = false }
```

**`bsync-rust`** (Rust platform I/O — clipboard + identity only):
```toml
bsync-core   = { path = "../bsync-core" }
iroh-base    = { version = "=1.0.0", features = ["key"] }
clipboard-rs = { version = "0.3", features = ["wayland"] }
tokio        = { version = "1", features = ["sync", "fs"] }
anyhow       = "1"
dirs         = "6"
```

**`bsync-tui`** (unified TUI + CLI shell):
```toml
bsync-core   = { path = "../bsync-core" }
bsync-net    = { path = "../bsync-net" }
bsync-rust   = { path = "../bsync-rust" }
clipboard-rs = { version = "0.3", features = ["wayland"] }
tokio        = { version = "1", features = ["rt-multi-thread", "sync", "signal", "macros", "io-util", "time"] }
anyhow       = "1"
clap         = { version = "4", features = ["derive"] }
futures-lite = "2"
ratatui      = "0.30"
crossterm    = { version = "0.29", features = ["event-stream"] }
```

**Note on iroh version**: iroh 1.0.0 was released 2026-06-15. The 0.9x canary series loses public relay support on Sept 30, 2026. Using 1.0 from the start avoids this deadline and benefits from the wire + API stability promise.

**Note on clipboard crate**: clipboard-rs handles watching + reading + writing on all desktop platforms. arboard was dropped — it's re-evaluated for image/HTML/file sync (post-MVP). The `ClipboardChangeSource` trait remains as an escape hatch for platform-specific swaps.

**Note on tokio features**: `rt-multi-thread` is used for the CLI/TUI shell. `bsync-net` uses only `sync` + `io-util` (no runtime reactor) — its background gossip event loop is spawned via `n0_future::task::spawn`, which maps to `tokio::spawn` on native and `wasm-bindgen-futures::spawn_local` on WASM. The browser shell (Svelte/WASM) cannot use `rt-multi-thread` — it needs a single-threaded executor. This is a shell-level concern; the core has no async runtime dependency, and `bsync-net` is runtime-agnostic.

## 7. Risks

| # | Risk | Severity | Mitigation |
|---|---|---|---|
| 1 | **Clipboard content exposure to untrusted peers** | 🔴 Critical | Startup warning + peer approval prompt. Post-MVP: E2E encryption, content filtering. Approval is UX gate, not cryptographic protection. |
| 2 | **Echo loop corrupts UX** | 🔴 Critical | Echo guard in BsyncCore (hash-based). Unit test in Phase C. |
| 3 | **Wayland clipboard fails on some compositors** | 🟠 High | Wrap errors with compositor guidance. `--no-clipboard` flag. Test on GNOME, KDE, Sway, Hyprland. Fallback: swap in `wayland-clipboard-listener` via trait. |
| 4 | **iroh-gossip pre-1.0 stability** | 🟡 Medium | Pin `=0.101.0`, commit `Cargo.lock`. iroh itself is 1.0 stable; risk is in gossip crate only. |
| 5 | **Large clipboard content saturates mesh** | ✅ Resolved | Images transfer via iroh-blobs in `bsync-net` (content-addressed, chunked, verified streaming). Only 32-byte BLAKE3 hashes travel over gossip. Text still limited to 1MB serialized. Rate limit 4 broadcasts/sec. |
| 6 | **Echo guard race with rapid remote messages** | 🟡 Low | Theoretical race — false broadcast of remote content (deduped by peer). Accept for MVP. Post-MVP: ring buffer. |
| 7 | ~~**iroh-gossip 0.101.0 WASM support unverified**~~ | ✅ Resolved | WASM build spike confirmed: iroh 1.0.0 + iroh-gossip 0.101.0 + iroh-blobs 0.103.0 compile to `wasm32-unknown-unknown` with target-conditional feature flags. `bsync-net` Cargo.toml includes the verified WASM config. `n0_future::task::spawn` provides runtime-agnostic task spawning (tokio on native, `wasm-bindgen-futures` on WASM). |

## 8. Implementation Roadmap

### Phase A: Project Scaffold + Identity + Core Types
**Goal**: `bsync` starts, generates/persists identity (with file permissions), prints peer ID + ticket, exits.

- `cargo init` with all dependencies
- `identity.rs`: `load_or_create_key(path)` → `SecretKey` (with `0o600`/`0o700` permissions), `key_to_peer_id(key)` → display string
- `lib.rs`: `BsyncCore` (zero I/O deps), `BsyncEvent`, `BsyncEffect` (with `write_hash`), `BsyncViewModel`, `Config`
- `Ticket` struct with `v: 1` version field
- Print both peer ID (human-readable) and ticket (base64 `Ticket` JSON)
- `main.rs`: clap arg parsing, calls `bsync::run(config)`
- **Automated test**: `key_is_persisted_and_stable` — load twice → same bytes
- **Manual test**: `cargo run` → prints peer ID + ticket. Run again → same peer ID.

### Phase B: iroh Endpoint + Gossip Setup
**Goal**: `bsync` creates an iroh endpoint, sets up gossip, subscribes to the topic.

- `bsync-net/src/lib.rs`: `Network::setup()` creates iroh endpoint, spawns gossip + blobs protocols, subscribes to topic, spawns background event loop task. `Network::broadcast()` handles serialization + blob upload. `NetworkEvent` stream carries `MessageReceived`/`PeerConnected`/`PeerDisconnected`/`Lagged`. All 4 `Event` variants matched explicitly (iroh-gossip 0.101.0 is not `#[non_exhaustive]`).
- Shell: call `Network::setup()`, merge `network_rx` stream with clipboard watcher in `select!` loop, print "waiting for connections..."
- **Manual test**: `cargo run` → prints peer ID, starts listening. Endpoint is alive.

### Phase C: Clipboard Watcher + Writer
**Goal**: `bsync` monitors local clipboard changes (event-driven where possible) and can write to clipboard.

- `clipboard.rs`: `ClipboardChangeSource` trait, `ClipboardWatcher` (clipboard-rs), `start_watcher(...)` — spawns thread with platform-appropriate backend
- `--no-clipboard` flag: skip clipboard init
- Wayland error wrapping with compositor guidance
- Shutdown: close channel sender + 2-second timeout on join
- **Automated test**: `local_change_during_remote_write_is_suppressed` — echo guard in core (in memory)
- **Manual test**: `cargo run -- --no-clipboard` → networking only. Without flag → watcher starts.

### Phase D: End-to-End Sync
**Goal**: Two `bsync` instances sync clipboard text in real-time.

- Wire clipboard events → `BsyncCore::process_event()` → `BsyncEffect::BroadcastGossip` → serialize to `Bytes` → `gossip_sender.broadcast(bytes)`
- Wire gossip receiver → `BsyncCore::process_event()` → `BsyncEffect::WriteClipboard { content, write_hash }` → clipboard-rs write
- Echo guard in core: `pending_write_hash` checked on `LocalClipboardChanged`
- Received-message dedup via `last_applied_remote_hash` in core
- `--connect <ticket>`: parse ticket, bootstrap, subscribe
- `--room <name>`: derive topic from room name
- Peer approval prompt on `NeighborUp` (default: required, `--auto-accept` to skip)
- Connection timeout: 30s
- Startup security warning + threat model
- 1MB serialized message size limit + 4/sec rate limit
- **Manual test**: Two terminals. A: `bsync` → prints ticket. B: `bsync --connect <ticket>`. Copy in A → appears in B. Copy in B → appears in A. No echo loop.

### Phase E: Shutdown + Polish
**Goal**: Clean Ctrl+C handling, status messages, UX polish.

- `tokio::signal::ctrl_c()` → `BsyncEvent::Shutdown` → cleanup effects → join with timeout
- Graceful gossip leave
- Status messages via `BsyncViewModel`: "Connected to peer X", "Syncing clipboard...", "Shutting down..."
- **Manual test**: Ctrl+C → clean shutdown. Two peers → status messages show connection state.

## 9. Frontend / Platform Matrix

| Frontend | Clipboard watching | Clipboard writing | Echo guard | Async runtime |
|---|---|---|---|---|
| CLI (ratatui/none) | clipboard-rs (Rust) | clipboard-rs (Rust) | Core (hash-based) | tokio `rt-multi-thread` |
| winui3 (Windows) | clipboard-rs (Rust via boltffi) | clipboard-rs (Rust via boltffi) | Core (hash-based) | tokio `rt-multi-thread` |
| swiftui (macOS) | clipboard-rs or native NSPasteboard | clipboard-rs or native NSPasteboard | Core (hash-based) | tokio `rt-multi-thread` |
| swiftui (iOS) | UIPasteboard (native Swift) | UIPasteboard (native Swift) | Core (hash-based) | native async |
| jetpack compose (Android) | ClipboardManager (native Kotlin) | ClipboardManager (native Kotlin) | Core (hash-based) | native async |
| svelte (web) | navigator.clipboard (native JS) | navigator.clipboard (native JS) | Core (hash-based) | `wasm-bindgen-futures` |

**Key insight**: Rust clipboard crates (clipboard-rs, arboard) do NOT compile for iOS, Android, or wasm. Mobile/web frontends MUST use native clipboard APIs. The echo guard lives in `BsyncCore` so it works uniformly across all platforms.

## 10. Browser / Svelte Frontend

iroh, iroh-gossip, and iroh-blobs all compile to `wasm32-unknown-unknown` and work in-browser via WebSocket relay transport (since iroh v0.33; iroh-blobs since Nov 2025). The Svelte frontend compiles `bsync-core` + `bsync-net` directly to WASM — no Tauri wrapper or relay bridge required. `bsync-net`'s `Cargo.toml` includes target-conditional feature flags verified by a build spike.

**Limitations:**
1. **Relay-only transport** — no direct UDP/hole-punching. All traffic via WebSocket relay. Latency ~50–200ms relay hop. Irrelevant for clipboard text (tiny payloads). Matters for large file transfer post-MVP.
2. **MemStore only** (no FsStore) — iroh-blobs content lives in WASM memory only. No persistence across page reloads. Fine for clipboard text (ephemeral). Custom IndexedDB-backed `Store` needed for persistent file sharing.
3. **No mDNS / no DHT** — browser peers cannot auto-discover on LAN. Ticket-based connection only (already the MVP design). The deferred "auto-discovery / mDNS" feature will NEVER work in-browser.
4. **tokio `rt-multi-thread` not WASM-compatible** — browser shell uses single-threaded executor (`wasm-bindgen-futures` / `spawn_local`).
5. **Identity persistence** — browser has no filesystem. Identity key saved to `localStorage` or `IndexedDB`.
6. ~~**iroh-gossip 0.101.0 WASM support unverified**~~ — ✅ **Verified.** Build spike confirmed compilation to `wasm32-unknown-unknown`. `bsync-net` Cargo.toml ships the verified WASM feature config. `n0_future::task::spawn` handles runtime-agnostic task spawning.

## 11. Post-MVP Migration Path

| Feature | How the MVP architecture supports it |
|---|---|
| Crux UI frontends | `BsyncCore` is a pure state machine (`process_event` + `view`). Crux shell calls these directly. Mechanical refactor: extract state to `Model` struct, wrap `Vec<BsyncEffect>` in `Command`. Logic unchanged. |
| boltffi native bindings | Each `BsyncCore` method becomes a C function. `BsyncEvent`/`BsyncEffect` map to C enums. Needs concrete plan for: enum representation (tagged unions), memory management (string ownership), async operations (callbacks or runtime). |
| File transfer | Send small iroh-blobs ticket/hash over gossip, then fetch content via iroh-blobs. Don't stuff large payloads into gossip messages. |
| Selective sync | Per-room topics already supported via `--room`. Add `room_id` field to `GossipMessage` for cross-room messages. |
| E2E encryption | Add encryption layer in `bsync-net` before serialization. Ticket includes `psk_hint`; actual PSK entered out-of-band. Swap `serde_json` → `postcard` for deterministic encoding (needed for signing). |
| Clipboard history | Add `timestamp` field back to `GossipMessage::ClipboardText`. Store in local SQLite or in-memory ring buffer. |
| Image sync | ✅ **Implemented.** `GossipMessage::ClipboardImage { origin, hash: [u8; 32] }` carries only the BLAKE3 blob hash (32 bytes). PNG bytes are uploaded to `iroh-blobs` `MemStore` via `store.blobs().add_slice()`, then fetched by the receiver via `store.downloader().download(hash, [sender_endpoint_id])`. All blob upload/download logic lives in `bsync-net` — shells call `network.broadcast(&ClipboardContent::Image(png_data), &origin)` and receive `NetworkEvent::MessageReceived { content: ClipboardContent::Image(bytes) }`. Core emits `BsyncEffect::BroadcastImage { origin, png_data }` for local images. No raw image bytes ever touch gossip. |
| Content filtering | Add `--no-sync-patterns <regex>` flag. Filter in `BsyncCore::process_event()` before generating `BroadcastGossip` effect. |
| Primary selection sync | Swap in `wayland-clipboard-listener` (GPL-3.0, now available) via `ClipboardChangeSource` trait. Uses `ListenOnSelect` for middle-click buffer. |
| Echo guard ring buffer | Replace single `pending_write_hash` with `ArrayVec<blake3::Hash, 5>` in core. Check against all recent hashes before clearing guard. |
| WebRTC direct transport | `iroh-webrtc-transport` implements `iroh::endpoint::transports::CustomTransport` (behind `unstable-custom-transports`). Integration point: `Network::setup()` in `bsync-net` — add a `webrtc` cargo feature, conditionally call `builder.add_custom_transport(Arc::new(webrtc_transport))` before `.bind()`. JSEP signaling (SDP offer/answer) rides on iroh's existing relay QUIC — no extra signaling server. Browser side uses JS `RTCPeerConnection` via wasm-bindgen. Current state: v0.1.0, depends on iroh 0.97 (needs porting to 1.0.0), pulls str0m + axum. Post-MVP because relay-only adds ~50-200ms latency — irrelevant for clipboard text, mild for images. |
