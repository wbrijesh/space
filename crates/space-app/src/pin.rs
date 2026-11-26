//! Pinned threads and the sidebar's order: pinned threads at the top, then the rest, each
//! newest created first (writing in a thread doesn't move it, so ⌘-numbers stay put), the rest
//! under date headers by when each was made. Done threads are listed apart, under Done, by when
//! they were marked done, under the same headers. Pinning is saved with
//! the thread (`ThreadMeta::pinned_ms`) and kept while it's done.

use gpui::*;
use space_core::protocol::service::ThreadMeta;

use crate::app::{SpaceApp, now_ms};

impl SpaceApp {
    /// The listed threads in sidebar order: pinned first, then the rest, each newest created
    /// first. A new thread with nothing sent isn't listed (it isn't saved, so it can't be pinned
    /// either).
    pub(crate) fn listed_threads(&self) -> Vec<u64> {
        let mut listed: Vec<_> = self.threads.iter().filter(|t| !t.is_empty()).collect();
        listed.sort_by_key(|t| (t.pinned_ms.is_none(), std::cmp::Reverse(t.created_ms)));
        listed.into_iter().map(|t| t.key).collect()
    }

    /// The Done list: done threads, last done first, each with the header of the section it
    /// starts ("Today", "Previous 7 Days", "October", "2025").
    pub(crate) fn done_sections(&self) -> Vec<(&ThreadMeta, Option<String>)> {
        let cutoff = self.done_cutoff();
        let mut done: Vec<_> = self.done_threads.iter().filter(|m| !cutoff.is_some_and(|c| crate::persist::expired(m, c))).collect();
        done.sort_by_key(|m| std::cmp::Reverse(m.done_ms));
        let now = now_ms();
        date_headers(done, |m| m.done_ms.unwrap_or(now))
    }

    /// Show or hide the Done list (collapsed at launch).
    pub(crate) fn toggle_done_list(&mut self, cx: &mut Context<Self>) {
        self.done_open = !self.done_open;
        cx.notify();
    }

    /// Render again at the next local midnight (and every one after), so a "Today" header becomes
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

/// Each item (newest first) with the date header of the section it starts, if it starts one:
/// sections as [`space_ui::format::date_section`], from the time `at` gives. A list that's all
/// one section has no header: it would only repeat what every row shares.
pub(crate) fn date_headers<T>(items: impl IntoIterator<Item = T>, at: impl Fn(&T) -> u64) -> Vec<(T, Option<String>)> {
    let (now, offset) = (now_ms(), space_ui::format::local_offset_secs());
    let mut last = None;
    let mut items: Vec<_> = items
        .into_iter()
        .map(|item| {
            let section = space_ui::format::date_section(at(&item), now, offset);
            let header = (last.as_ref() != Some(&section)).then(|| section.clone());
            last = Some(section);
            (item, header)
        })
        .collect();
    if items.iter().filter(|(_, header)| header.is_some()).count() == 1 {
        items.iter_mut().for_each(|(_, header)| *header = None);
    }
    items
}

#[cfg(test)]
mod tests {
    use super::{date_headers, now_ms};

    #[test]
    fn one_section_has_no_header() {
        let now = now_ms();
        let day = 86_400_000;
        let headers = |times: &[u64]| date_headers(times.iter().copied(), |t| *t).into_iter().map(|(_, h)| h).collect::<Vec<_>>();
        assert_eq!(headers(&[now, now]), vec![None, None]);
        assert_eq!(headers(&[now, now, now - 3 * day]), vec![Some("Today".into()), None, Some("Previous 7 Days".into())]);
        assert!(headers(&[]).is_empty());
    }
}
