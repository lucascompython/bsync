# bsync - cross-platform P2P clipboard syncing

A cross-platform P2P clipboard syncing tool built in Rust, leveraging iroh for transport and boltffi for cross-platform native bindings. The UI is implemented using crux, with frontends in ratatui, winui3, svelte, swiftui, and jetpack compose.

## Flow

Enter the app -> get your peer ID -> share your peer ID with others -> connect to peers -> start syncing clipboard data in real-time.

## Platforms

- UI -> cross-platform rust core with crux; ratatui, winui3, svelte, swiftui, jetpack compose frontends; iroh for p2p transport; boltffi for cross-platform native bindings

## Phase 1:

- Rust CLI
- iroh
- arboard
- clipboard sync only

## After MVP:

Wire up the UI frontends, add features like selective sync, file transfer, end-to-end encryption, and more.

### Tech to use

- iroh
- boltffi
- crux
- winui3
- svelte
- swiftui
- jetpack compose
- gtk
- ratatui
