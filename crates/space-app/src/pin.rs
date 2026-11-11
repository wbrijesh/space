//! Pinned threads: listed at the top of the sidebar, in its usual order among themselves.
//! Pinning is saved with the thread (`ThreadMeta::pinned_ms`) and kept while it's closed.

use gpui::*;

use crate::app::{SpaceApp, now_ms};

impl SpaceApp {
    /// The listed threads in sidebar order: pinned first, then the rest, each newest first. A
    /// new thread with nothing sent isn't listed (it isn't saved, so it can't be pinned either).
    pub(crate) fn listed_threads(&self) -> Vec<u64> {
        let listed = || self.threads.iter().rev().filter(|t| !t.is_empty());
        listed().filter(|t| t.pinned_ms.is_some()).chain(listed().filter(|t| t.pinned_ms.is_none())).map(|t| t.key).collect()
    }

    /// `TogglePinThread`: pin or unpin the selected thread.
    pub(crate) fn toggle_pin_current(&mut self, cx: &mut Context<Self>) {
        if self.show_settings {
            return;
        }
        if let Some(key) = self.selected {
            self.toggle_pin(key, cx);
        }
    }

    /// Pin the thread, or unpin it, and save that the way a rename is saved. A thread with
    /// nothing sent isn't saved, so it isn't pinned.
    pub(crate) fn toggle_pin(&mut self, key: u64, cx: &mut Context<Self>) {
        let Some(thread) = self.thread_mut(key).filter(|t| !t.is_empty()) else { return };
        let pinned = thread.pinned_ms.is_none().then(now_ms);
        thread.pinned_ms = pinned;
        if !self.saving_threads() {
            cx.notify();
            return;
        }
        let thread = self.thread_mut(key).expect("thread exists");
        if thread.unloaded || thread.loading {
            // Its events haven't been read: write the summary it was saved with, pinned.
            let Some(meta) = thread.meta.as_mut() else { return };
            meta.pinned_ms = pinned;
            meta.updated_ms = meta.updated_ms.max(now_ms());
            let (meta, first, host) = (meta.clone(), thread.saved, self.host.clone());
            cx.spawn(async move |this, cx| {
                if let Err(e) = host.save_thread(&meta.id, Some(meta.clone()), first, &[]).await {
                    let _ = this.update(cx, |this, cx| {
                        this.banner = Some(format!("Couldn't save the pin: {e:#}"));
                        cx.notify();
                    });
                }
            })
            .detach();
        } else {
            self.save_thread(key, cx);
        }
        cx.notify();
    }
}
