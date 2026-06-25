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

### TODO:

- [ ] does bsync-core have some iroh/p2p logic that needs to be shared across all platforms? i think it does because if not how are we planning on having that networking logic on other platforms? I think maybe all the logic regarding the p2p communication should be shared because i think it can be compiled to all platforms (and then use boltffi where needed), later on we will need to implement encryption for example and auto-discovery and i think it makes sense to have this in only one place
- [ ] update all rust editions to 2024
- [ ] make it so you can save someone ticket, then you can change their name, and make them a trusted peer so that you dont have to accept their ticket every time
- [ ]

1. write a plan to a md file to Promote networking into a shared layer
2. look into: https://github.com/anchalshivank/iroh-webrtc-transport, since some recent version iroh now support custom transports, and one of their main developers and maintainers created a webrtc one, if this looks good, onthe plan file i asked about before, add information about implementing this