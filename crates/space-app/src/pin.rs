//! Pinned threads and the sidebar's sections: pinned threads at the top, then the rest by the
//! local day they were created (Today, Yesterday, "3 Nov"). Pinning is saved with the thread
//! (`ThreadMeta::pinned_ms`) and kept while it's closed.

use gpui::*;

use crate::app::{SpaceApp, now_ms};

impl SpaceApp {
    /// The listed threads in sidebar order: pinned first, then the rest, each newest first. A
    /// new thread with nothing sent isn't listed (it isn't saved, so it can't be pinned either).
    pub(crate) fn listed_threads(&self) -> Vec<u64> {
        self.listed_sections().into_iter().map(|(key, _)| key).collect()
    }

    /// The listed threads in sidebar order, each with the header of the section it starts
    /// ("Pinned", "Today", "Yesterday", "3 Nov"). Pinned threads come first; the rest are grouped
    /// by the local day they were created, newest day first, keeping their order within a day
    /// (writing in a thread doesn't move it).
    pub(crate) fn listed_sections(&self) -> Vec<(u64, Option<String>)> {
        use space_ui::format::{day_label, local_day, local_offset_secs};
        let (now, offset) = (now_ms(), local_offset_secs());
        let listed = || self.threads.iter().rev().filter(|t| !t.is_empty());
        let mut out: Vec<(u64, Option<String>)> = listed()
            .filter(|t| t.pinned_ms.is_some())
            .enumerate()
            .map(|(ix, t)| (t.key, (ix == 0).then(|| "Pinned".to_string())))
            .collect();
        let mut rest: Vec<_> = listed().filter(|t| t.pinned_ms.is_none()).collect();
        rest.sort_by_key(|t| std::cmp::Reverse(local_day(t.created_ms, offset)));
        let mut last_day = None;
        for t in rest {
            let day = local_day(t.created_ms, offset);
            let header = (last_day != Some(day)).then(|| day_label(t.created_ms, now, offset));
            last_day = Some(day);
            out.push((t.key, header));
        }
        out
    }

    /// Render again at the next local midnight (and every one after), so "Today" becomes
    /// "Yesterday" without a restart.
    pub(crate) fn watch_midnight(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                let offset = space_ui::format::local_offset_secs() as i64;
                let now = now_ms() as i64;
                let day_ms = 86_400_000;
                let local = now + offset * 1000;
                // A second past midnight, so the new day has certainly begun.
                let wait = (day_ms - local.rem_euclid(day_ms)) + 1000;
                cx.background_executor().timer(std::time::Duration::from_millis(wait as u64)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
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
