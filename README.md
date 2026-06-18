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

### Architecture reviews

- take a look at iroh-blobs and check if it works on browser
- why are we using clipboard-rs and arboard? From my understanding both can read and write to the clipboard, but clipboard-rs can read events while arboard can't (correct me if im wrong)
- tradeoffs between putting the keyboard listening and writting in the rust core instead of implementing it in the native languages that will be used for the UI frontends.
