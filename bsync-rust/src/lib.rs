// bsync-rust — shared Rust platform implementation for bsync shells.
//
// Used by Rust-based shells (TUI, GTK-rs). NOT used by non-Rust shells
// (SwiftUI, Compose, Svelte) — those implement clipboard and identity
// via native APIs.

pub mod clipboard;
pub mod gossip;
pub mod identity;
