use bsync_core::ClipboardContent;
use clipboard_rs::{
    common::RustImage, Clipboard, ClipboardContext, ClipboardHandler, ClipboardWatcher,
    ClipboardWatcherContext, ContentFormat, RustImageData,
};
use tokio::sync::mpsc;

pub fn start_watcher(tx: mpsc::Sender<ClipboardContent>) {
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
    tx: mpsc::Sender<ClipboardContent>,
}

impl ClipboardHandler for WatcherHandler {
    fn on_clipboard_change(&mut self) {
        if let Ok(content) = read_clipboard(&self.ctx) {
            let _ = self.tx.blocking_send(content);
        }
    }
}

/// Read current clipboard as either text or PNG-encoded image.
pub fn read_clipboard(ctx: &ClipboardContext) -> anyhow::Result<ClipboardContent> {
    if ctx.has(ContentFormat::Image) {
        let img = ctx.get_image().map_err(|e| anyhow::anyhow!("{e}"))?;
        let png = img.to_png().map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(ClipboardContent::Image(png.get_bytes().to_vec()))
    } else {
        let text = ctx.get_text().map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(ClipboardContent::Text(text))
    }
}

/// Write content (text or image) to the clipboard.
pub fn write_clipboard(ctx: &ClipboardContext, content: &ClipboardContent) -> anyhow::Result<()> {
    match content {
        ClipboardContent::Text(text) => {
            ctx.set_text(text.clone()).map_err(|e| anyhow::anyhow!("{e}"))
        }
        ClipboardContent::Image(png_data) => {
            let img = RustImageData::from_bytes(png_data)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            ctx.set_image(img).map_err(|e| anyhow::anyhow!("{e}"))
        }
    }
}
