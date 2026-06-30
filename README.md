# bsync - cross-platform P2P clipboard syncing

A cross-platform P2P clipboard syncing tool built in Rust, leveraging iroh for transport and boltffi for cross-platform native bindings.  
The UI is implemented using crux, with frontends in ratatui, winui3, svelte, swiftui, gtk, and jetpack compose.

## Flow

Enter the app -> get your peer ID -> share your peer ID with others -> connect to peers -> start syncing clipboard data in real-time.

### Tech

- iroh
- boltffi
- crux
- winui3
- svelte
- swiftui
- jetpack compose
- gtk
- ratatui

### Features

- Clipboard syncing with image support
- Native UI's for Windows, Mac, Linux, Android, iOS
- File transfer
- End-to-end encryption
- Peer-to-peer transport
- Clipboard management (history, searching, etc.)
- Multi room support (for example you can have a room for work, a room for personal)
- Peer management (trusted peers, blocking, auto-connect, etc.)
- selective sync (for example you can choose to only sync text, or only sync images, or only sync files, etc.)

### TODO:

- [ ] manage peers, block a peer, change their name, trust them, etc.
- [ ] add names to peers
- [ ] persisent configuration, using sqlite or libsql or turso to save peer information, rooms, settings, etc.
- [ ] look into: https://github.com/anchalshivank/iroh-webrtc-transport,
- [ ] look into making the identity.rs stuff in bsync-rust more cross-platform, there is no need to repeat some of the code there in native platform code, basically in all platforms we will be reading the same files in the same places, we can keep it in the bsync-net
- see torrent transport
