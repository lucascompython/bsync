// Clipboard watcher using clipboard-rs.
// Runs in a separate thread, sending changes through a tokio channel.

use clipboard_rs::{
    Clipboard, ClipboardContext, ClipboardHandler, ClipboardWatcher, ClipboardWatcherContext,
};

/// Start the clipboard watcher in a background thread.
pub fn start_watcher(tx: tokio::sync::mpsc::Sender<String>) {
    std::thread::spawn(move || {
        let ctx = match ClipboardContext::new() {
            Ok(c) => c,
            Err(_) => return,
        };

        let handler = WatcherHandler { ctx, tx };

        let mut watcher = match ClipboardWatcherContext::new() {
            Ok(w) => w,
            Err(_) => return,
        };

        let _shutdown = watcher.add_handler(handler).get_shutdown_channel();
        watcher.start_watch();
    });
}

struct WatcherHandler {
    ctx: ClipboardContext,
    tx: tokio::sync::mpsc::Sender<String>,
}

impl ClipboardHandler for WatcherHandler {
    fn on_clipboard_change(&mut self) {
        if let Ok(text) = self.ctx.get_text() {
            let _ = self.tx.blocking_send(text);
        }
    }
}
