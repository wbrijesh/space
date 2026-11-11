//! Attaching to a draft: images (from the picker, a drop or a paste) are prepared in the
//! background into draft images; other files are attached as paths; long pasted text becomes a
//! pasted-text attachment. And the viewers a sent picture or pasted text opens in.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::*;
use space_ui::components::images::Pic;

use crate::app::SpaceApp;
use crate::images::{self, MAX_PER_MESSAGE, Prepared};
use crate::threads::Thread;

/// An image in a draft: being prepared (`prepared` is `None`), or ready to send.
pub struct DraftImage {
    pub id: u64,
    pub prepared: Option<Prepared>,
    /// The thumbnail (the prepared bytes, decoded by GPUI).
    pub pic: Option<Arc<Image>>,
}

impl DraftImage {
    pub fn ready(id: u64, p: Prepared) -> Self {
        let pic = Arc::new(Image::from_bytes(p.gpui_format(), p.bytes.to_vec()));
        Self { id, prepared: Some(p), pic: Some(pic) }
    }

    pub fn pic(&self) -> Option<Pic> {
        let (p, image) = (self.prepared.as_ref()?, self.pic.clone()?);
        Some(Pic { src: image.into(), width: p.width, height: p.height })
    }
}

/// Where a thread's sent images are kept: the session store's folder for it, else a temporary
/// one.
pub fn thread_attachments_dir(thread: Option<&Thread>) -> PathBuf {
    let id = thread.map_or("unknown", |t| t.id.as_str());
    thread
        .and_then(|t| t.attachments_dir.clone())
        .unwrap_or_else(|| std::env::temp_dir().join("space-attachments").join(id))
}

enum Source {
    File(PathBuf),
    Bytes(Vec<u8>),
}

impl SpaceApp {
    /// Files from the picker, a drop or a paste: images become draft images; anything else
    /// (other files, folders) is attached as a path.
    pub(crate) fn add_files(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        let (pictures, files): (Vec<_>, Vec<_>) = paths.into_iter().partition(|p| p.is_file() && images::is_image_path(p));
        if let Some(t) = self.thread_mut(key) {
            for p in files {
                if !t.attachments.contains(&p) {
                    t.attachments.push(p);
                }
            }
        }
        self.add_images(key, pictures.into_iter().map(Source::File).collect(), cx);
        cx.notify();
    }

    /// A paste into the composer. Images, copied files and long text are taken (`true`);
    /// other text is left to the input.
    pub(crate) fn paste(&mut self, item: &ClipboardItem, cx: &mut Context<Self>) -> bool {
        let mut pictures = Vec::new();
        let mut paths = Vec::new();
        let mut text: Option<&str> = None;
        for entry in item.entries() {
            match entry {
                ClipboardEntry::Image(image) => pictures.push(Source::Bytes(image.bytes.clone())),
                ClipboardEntry::ExternalPaths(p) => paths.extend(p.paths().iter().cloned()),
                ClipboardEntry::String(s) => text = text.or(Some(s.text())),
            }
        }
        if pictures.is_empty() && paths.is_empty() {
            let Some(text) = text.filter(|t| crate::paste::is_long(t)) else { return false };
            if let Some(key) = self.selected
                && let Some(t) = self.thread_mut(key)
            {
                t.pastes.push(crate::paste::Paste::new(text.to_string()));
            }
            cx.notify();
            return true;
        }
        if let Some(key) = self.selected {
            self.add_images(key, pictures, cx);
        }
        if !paths.is_empty() {
            self.add_files(paths, cx);
        }
        cx.notify();
        true
    }

    /// Prepare images in the background as draft images of thread `key`. A batch that would
    /// pass the per-message limit is refused whole; one that can't be read is dropped, saying why.
    fn add_images(&mut self, key: u64, sources: Vec<Source>, cx: &mut Context<Self>) {
        if sources.is_empty() {
            return;
        }
        let Some(have) = self.thread(key).map(|t| t.images.len()) else { return };
        if have + sources.len() > MAX_PER_MESSAGE {
            self.banner = Some(format!("A message can have up to {MAX_PER_MESSAGE} images."));
            return;
        }
        for source in sources {
            let id = self.next_image_id;
            self.next_image_id += 1;
            if let Some(t) = self.thread_mut(key) {
                t.images.push(DraftImage { id, prepared: None, pic: None });
            }
            let label = match &source {
                Source::File(p) => p.file_name().map(|n| n.to_string_lossy().into_owned()),
                Source::Bytes(_) => None,
            };
            let prepared = cx.background_spawn(async move {
                match source {
                    Source::File(path) => images::prepare_file(&path),
                    Source::Bytes(bytes) => images::prepare(&bytes, None),
                }
            });
            cx.spawn(async move |this, cx| {
                let result = prepared.await;
                let _ = this.update(cx, |this, cx| {
                    let Some(t) = this.thread_mut(key) else { return };
                    // Removed while it was being prepared.
                    let Some(ix) = t.images.iter().position(|i| i.id == id) else { return };
                    match result {
                        Ok(p) => t.images[ix] = DraftImage::ready(id, p),
                        Err(e) => {
                            t.images.remove(ix);
                            this.banner = Some(match label {
                                Some(name) => format!("{name}: {e}"),
                                None => e,
                            });
                        }
                    }
                    cx.notify();
                });
            })
            .detach();
        }
    }

    pub(crate) fn remove_image(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(key) = self.selected
            && let Some(t) = self.thread_mut(key)
            && ix < t.images.len()
        {
            t.images.remove(ix);
        }
        cx.notify();
    }

    pub(crate) fn remove_paste(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(key) = self.selected
            && let Some(t) = self.thread_mut(key)
            && ix < t.pastes.len()
        {
            t.pastes.remove(ix);
        }
        cx.notify();
    }

    /// Open the draft's pasted text `ix` to read.
    pub(crate) fn open_draft_paste(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.selected.and_then(|key| self.thread(key)).and_then(|t| t.pastes.get(ix)) else { return };
        self.paste_view = Some((p.text.clone(), p.chars));
        window.focus(&self.viewer_focus, cx);
        cx.notify();
    }

    /// Open a sent message's pasted text (read from its file).
    pub(crate) fn open_sent_paste(&mut self, text: space_core::TextRef, window: &mut Window, cx: &mut Context<Self>) {
        match std::fs::read_to_string(&text.path) {
            Ok(body) => {
                self.paste_view = Some((body.into(), text.chars));
                window.focus(&self.viewer_focus, cx);
            }
            Err(e) => self.banner = Some(format!("Couldn't open this pasted text: {e}")),
        }
        cx.notify();
    }

    pub(crate) fn close_paste_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.paste_view.take().is_some() {
            self.focus_composer(window, cx);
            cx.notify();
        }
    }

    pub(crate) fn open_viewer(&mut self, pics: Vec<Pic>, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix < pics.len() {
            self.viewer = Some((pics, ix));
            window.focus(&self.viewer_focus, cx);
            cx.notify();
        }
    }

    pub(crate) fn close_viewer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.viewer.take().is_some() {
            self.focus_composer(window, cx);
            cx.notify();
        }
    }

    /// The previous (-1) or next (+1) picture, stopping at the ends.
    pub(crate) fn step_viewer(&mut self, by: isize, cx: &mut Context<Self>) {
        if let Some((pics, ix)) = &mut self.viewer {
            *ix = ix.saturating_add_signed(by).min(pics.len() - 1);
            cx.notify();
        }
    }
}

/// How long a row shows "Copied".
const COPIED_FOR: std::time::Duration = std::time::Duration::from_millis(1500);

impl SpaceApp {
    /// A message row's copy button: the text goes to the clipboard and the row says "Copied" for
    /// a moment.
    pub(crate) fn copy_message(&mut self, row: String, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.copied = Some(row.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_FOR).await;
            let _ = this.update(cx, |this, cx| {
                if this.copied.as_deref() == Some(row.as_str()) {
                    this.copied = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Open a row's usage panel (`Some`), or close it.
    pub(crate) fn show_usage(&mut self, row: Option<String>, cx: &mut Context<Self>) {
        if self.usage_open != row {
            self.usage_open = row;
            cx.notify();
        }
    }
}

