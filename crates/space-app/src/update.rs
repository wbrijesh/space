//! Updates, as the app shows them: the server builds (or downloads) a new Space and says when
//! it's ready and what switching costs; a notice in the window's corner offers the switch, and
//! the server asks the app to start again from the new build when it's made.

use std::path::PathBuf;
use std::rc::Rc;

use gpui::*;
use space_core::remote::{Update, UpdateKind, UpdateStage};

use crate::app::SpaceApp;

/// What the update notice says and offers.
pub(crate) struct UpdateNotice {
    pub title: String,
    pub body: Option<String>,
    pub error: bool,
    /// (label, when idle)
    pub actions: Vec<(&'static str, bool)>,
}

impl SpaceApp {
    /// The update notice, unless closed (until the update moves on).
    pub(crate) fn update_notice(&self) -> Option<UpdateNotice> {
        let update = self.host.update()?;
        if self.update_closed.as_ref() == Some(&closed_key(&update)) {
            return None;
        }
        notice_for(&update, self.threads.iter().filter(|t| t.state.running).count())
    }

    pub(crate) fn close_update_notice(&mut self, cx: &mut Context<Self>) {
        self.update_closed = self.host.update().as_ref().map(closed_key);
        cx.notify();
    }

    pub(crate) fn apply_update(&mut self, when_idle: bool, cx: &mut Context<Self>) {
        let host = self.host.clone();
        cx.spawn(async move |this, cx| {
            if let Err(e) = host.apply_update(when_idle).await {
                let _ = this.update(cx, |this, cx| {
                    this.banner = Some(format!("Couldn't switch to the new build: {e:#}"));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// The server changed what it says about an update, or asked the app to start again from a
    /// new build.
    pub(crate) fn update_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(exe) = self.host.relaunch_into() {
            self.relaunch_from(Some(exe), cx);
        }
        cx.notify();
    }

    /// Start again (from `exe`, else this executable): what's unsaved is saved first, and the
    /// sessions stay with the server for the app that starts.
    pub(crate) fn relaunch_from(&mut self, exe: Option<PathBuf>, cx: &mut Context<Self>) {
        self.host.close();
        if !self.host.is_disconnected() {
            self.save_all_now();
        }
        self.save_drafts(cx);
        crate::app::relaunch(exe);
    }
}

fn closed_key(update: &Update) -> (String, UpdateStage, bool) {
    (update.version.clone(), update.stage, update.waiting_for_idle)
}

/// The notice for `update`, with `working` threads running. Nothing while it's on its way (the
/// menu bar item says that).
fn notice_for(update: &Update, working: usize) -> Option<UpdateNotice> {
    let v = &update.version;
    let title = format!("Space {v} is ready.");
    Some(match (update.stage, update.kind) {
        (UpdateStage::Getting, _) => return None,
        (UpdateStage::Failed, _) => UpdateNotice {
            title: if v.is_empty() { "Space can't check for updates.".into() } else { format!("Space {v} isn't ready.") },
            body: update.error.clone(),
            error: true,
            actions: Vec::new(),
        },
        (UpdateStage::Ready, Some(UpdateKind::App)) => {
            UpdateNotice { title, body: Some("The window relaunches; nothing stops.".into()), error: false, actions: vec![("Relaunch", false)] }
        }
        (UpdateStage::Ready, Some(UpdateKind::Server)) => UpdateNotice {
            title,
            body: Some("The server restarts and the window relaunches; nothing stops.".into()),
            error: false,
            actions: vec![("Restart", false)],
        },
        (UpdateStage::Ready, _) if update.waiting_for_idle => UpdateNotice {
            title: format!("Space {v} restarts when every thread is done."),
            body: None,
            error: false,
            actions: vec![("Restart now", false)],
        },
        (UpdateStage::Ready, _) if working == 0 => {
            UpdateNotice { title, body: Some("Agents restart; no thread is working.".into()), error: false, actions: vec![("Restart", false)] }
        }
        (UpdateStage::Ready, _) => UpdateNotice {
            title,
            body: Some(match working {
                1 => "Agents restart, so the working thread would stop.".into(),
                n => format!("Agents restart, so {n} working threads would stop."),
            }),
            error: false,
            actions: vec![("Restart now", false), ("Restart when idle", true)],
        },
    })
}

/// The notice element, at the window's bottom right.
pub(crate) fn element(app: &SpaceApp, cx: &mut Context<SpaceApp>) -> Option<AnyElement> {
    use gpui::assets::IconName as Lucide;
    use gpui::component::Icon;
    use space_ui::components::notice::{self, MARGIN, Notice};
    let n = app.update_notice()?;
    let this = cx.entity().downgrade();
    let actions = n
        .actions
        .iter()
        .map(|(label, when_idle)| {
            let (this, when_idle) = (this.clone(), *when_idle);
            let on: notice::Handler = Rc::new(move |_, cx| {
                let _ = this.update(cx, |this, cx| this.apply_update(when_idle, cx));
            });
            (SharedString::from(*label), on)
        })
        .collect();
    let close: notice::Handler = Rc::new(move |_, cx| {
        let _ = this.update(cx, |this, cx| this.close_update_notice(cx));
    });
    let icon = if n.error { Icon::new(gpui::component::IconName::TriangleAlert) } else { Icon::new(Lucide::RefreshCw) };
    let element = notice::notice(Notice {
        icon,
        title: n.title.into(),
        body: n.body.map(Into::into),
        kept: Vec::new(),
        error: n.error,
        undo: None,
        actions,
        on_close: close,
    });
    Some(div().absolute().right(MARGIN).bottom(MARGIN).child(element).into_any_element())
}

#[cfg(test)]
mod tests {
    use super::{Update, UpdateKind, UpdateStage, notice_for};

    fn ready(kind: UpdateKind) -> Update {
        Update { version: "e4f5a6b".into(), stage: UpdateStage::Ready, kind: Some(kind), waiting_for_idle: false, error: None }
    }

    #[test]
    fn each_kind_says_what_it_costs() {
        let app = notice_for(&ready(UpdateKind::App), 2).unwrap();
        assert_eq!(app.actions, vec![("Relaunch", false)]);
        let server = notice_for(&ready(UpdateKind::Server), 2).unwrap();
        assert_eq!(server.actions, vec![("Restart", false)]);
        let idle = notice_for(&ready(UpdateKind::Agent), 0).unwrap();
        assert_eq!(idle.actions, vec![("Restart", false)]);
        let busy = notice_for(&ready(UpdateKind::Agent), 2).unwrap();
        assert_eq!(busy.body.as_deref(), Some("Agents restart, so 2 working threads would stop."));
        assert_eq!(busy.actions, vec![("Restart now", false), ("Restart when idle", true)]);
        let waiting = notice_for(&Update { waiting_for_idle: true, ..ready(UpdateKind::Agent) }, 1).unwrap();
        assert_eq!(waiting.actions, vec![("Restart now", false)]);
        let getting = Update { stage: UpdateStage::Getting, kind: None, ..ready(UpdateKind::App) };
        assert!(notice_for(&getting, 0).is_none());
    }
}
