//! Assembles the window from `space-ui` components: maps app state to component props and
//! wires component callbacks back to [`SpaceApp`] methods. No styling lives here.

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use space_core::PluginStatus;
use gpui::component::input::InputState;
use space_core::protocol::service::SettingKind;
use space_ui::components::composer::{
    Composer, ComposerBlocker, ComposerControls, ComposerHandlers, Handler, ModeChoice,
};
use space_ui::components::settings::{Catalog, FieldView, PluginRow, PluginSettingsView, SettingsHandlers, SettingsPage};
use space_ui::components::follow_scroll::FollowScrollArea;
use space_ui::components::shell::{
    AgentFooter, AgentState, LogsPanel, banner, confirm_card, empty_heading, problem_card, sidebar_column,
};
use space_ui::components::thread_row::{RowIndicator, ThreadRow};
use space_ui::components::title_bar::{MainTitleBar, SidebarTitleBar, TitleBarNav};
use space_ui::components::transcript::{TranscriptView, transcript_items};
use space_ui::format::{self, Preview};
use space_ui::theme::{color, size, text};
use space_ui::ClickHandler;

use crate::app::{
    CloseSettings, CyclePermissionMode, FocusComposer, GoBack, GoForward, NewThread, OpenSettings, ScrollToBottom, SpaceApp,
    ToggleLogs, ToggleSidebar,
};
use crate::settings::SaveKind;
use crate::threads::Thread;

/// A click callback that runs `f` on the app.
fn click(cx: &mut Context<SpaceApp>, f: impl Fn(&mut SpaceApp, &mut Window, &mut Context<SpaceApp>) + 'static) -> ClickHandler {
    Rc::new(cx.listener(move |this, _: &ClickEvent, w, cx| f(this, w, cx)))
}

/// A plain callback that runs `f` on the app.
fn handler(cx: &mut Context<SpaceApp>, f: impl Fn(&mut SpaceApp, &mut Window, &mut Context<SpaceApp>) + 'static) -> Handler {
    let this = cx.entity().downgrade();
    Rc::new(move |w, cx| {
        let _ = this.update(cx, |this, cx| f(this, w, cx));
    })
}

pub fn render_root(app: &mut SpaceApp, window: &mut Window, cx: &mut Context<SpaceApp>) -> impl IntoElement {
    let nav = TitleBarNav {
        sidebar_open: app.sidebar_open,
        // On the Settings page, Back returns to the current thread.
        can_back: (app.show_settings && app.current().is_some()) || app.back.iter().any(|k| app.thread(*k).is_some()),
        can_forward: app.forward.iter().any(|k| app.thread(*k).is_some()),
        on_toggle_sidebar: click(cx, |this, _, cx| this.toggle_sidebar(cx)),
        on_back: click(cx, |this, w, cx| this.go_back(w, cx)),
        on_forward: click(cx, |this, w, cx| this.go_forward(w, cx)),
        on_new_thread: click(cx, |this, w, cx| this.new_thread(None, w, cx)),
    };

    let main = div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .h_full()
        .bg(color::bg())
        .child(
            MainTitleBar::new(if app.show_settings {
                "Settings".to_string()
            } else {
                app.current().map(|t| t.title()).unwrap_or_default()
            })
            .nav((!app.sidebar_open).then(|| nav.clone())),
        )
        .child(if app.show_settings { settings(app, window, cx) } else { body(app, window, cx) })
        .when(app.show_logs, |d| d.child(logs(app, cx)));

    div()
        .id("space-root")
        .key_context("Space")
        // Focus sink: a click on anything non-focusable lands here and blurs the composer.
        .track_focus(&app.focus)
        .on_action(cx.listener(|this, _: &NewThread, window, cx| this.new_thread(None, window, cx)))
        .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
        .on_action(cx.listener(|this, _: &GoBack, window, cx| this.go_back(window, cx)))
        .on_action(cx.listener(|this, _: &GoForward, window, cx| this.go_forward(window, cx)))
        .on_action(cx.listener(|this, _: &ToggleLogs, _, cx| this.toggle_logs(cx)))
        .on_action(cx.listener(|this, _: &FocusComposer, window, cx| this.focus_composer(window, cx)))
        .on_action(cx.listener(|this, _: &OpenSettings, window, cx| this.toggle_settings(window, cx)))
        .on_action(cx.listener(|this, _: &ScrollToBottom, _, cx| this.scroll_to_bottom(cx)))
        .on_action(cx.listener(|this, _: &CyclePermissionMode, _, cx| this.cycle_mode(cx)))
        // Only while the page is open, so Esc is free otherwise.
        .when(app.show_settings, |d| {
            d.on_action(cx.listener(|this, _: &CloseSettings, window, cx| this.close_settings(window, cx)))
        })
        .size_full()
        .flex()
        .bg(color::bg())
        .text_color(color::text())
        .font_family(text::UI_FONT)
        .text_size(text::MD)
        .when(app.sidebar_open, |d| d.child(sidebar(app, nav, cx)))
        .child(main)
}

fn sidebar(app: &SpaceApp, nav: TitleBarNav, cx: &mut Context<SpaceApp>) -> impl IntoElement {
    let rows = app
        .threads
        .iter()
        .rev()
        .map(|t| {
            let key = t.key;
            // On the Settings page no thread is shown, so none is highlighted.
            let selected = app.selected == Some(key) && !app.show_settings;
            let failed = matches!(format::thread_preview(&t.state), Preview::Error(_)) || t.error.is_some();
            let indicator = RowIndicator::resolve_paused(
                t.state.held().is_some(),
                selected,
                t.state.running || (t.queued.is_some() && t.error.is_none()),
                t.state.pending_permission().is_some(),
                t.unread > 0,
                failed,
            );
            ThreadRow::new(("thread", key as usize), t.title())
                .selected(selected)
                .indicator(indicator)
                .on_click(cx.listener(move |this, _, w, cx| this.select_thread(key, w, cx)))
                .into_any_element()
        })
        .collect();

    let state = match &app.agent_status {
        PluginStatus::Running => AgentState::Running,
        PluginStatus::Starting => AgentState::Starting,
        PluginStatus::Stopped => AgentState::Stopped,
        PluginStatus::Crashed(msg) | PluginStatus::NotStarted(msg) => AgentState::Exited(msg.clone()),
    };
    let footer = AgentFooter::new(app.agent_title.clone(), state)
        .logs_open(app.show_logs)
        .on_toggle_logs(cx.listener(|this, _, _, cx| this.toggle_logs(cx)))
        .when(app.agent_id.is_some(), |f| f.on_restart(cx.listener(|this, _, _, cx| this.restart_agent(cx))));

    sidebar_column(SidebarTitleBar::new(nav), rows, footer)
}

fn body(app: &SpaceApp, window: &mut Window, cx: &mut Context<SpaceApp>) -> AnyElement {
    let Some(thread) = app.current() else {
        return div().flex_1().into_any_element();
    };
    let notices: Vec<AnyElement> = [agent_problem(app, thread, cx), mode_confirmation(app, thread, cx), app.banner.clone().map(|b| {
        banner(b, cx.listener(|this, _, _, cx| {
            this.banner = None;
            cx.notify();
        }))
        .into_any_element()
    })]
    .into_iter()
    .flatten()
    .collect();
    let composer = composer(app, thread, cx);
    let column = || div().w_full().max_w(size::CONTENT_MAX_W).flex().flex_col();

    if thread.is_empty() {
        let _ = window;
        return div()
            .id("empty-thread")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px_6()
            .pb(px(72.))
            .child(column().gap_6().child(empty_heading()).children(notices).child(composer))
            .into_any_element();
    }

    let this = cx.entity().downgrade();
    let this2 = this.clone();
    let items = transcript_items(TranscriptView {
        state: &thread.state,
        key: thread.key,
        expanded: &thread.expanded,
        thought_secs: &thread.thought_secs,
        queued: thread.queued.as_ref().map(|(t, a)| (t.as_str(), a.as_slice())),
        on_toggle: Rc::new(move |ix, _, cx| {
            let _ = this.update(cx, |this, cx| this.toggle_expanded(ix, cx));
        }),
        on_answer: Rc::new(move |rid, decision, _, cx| {
            let _ = this2.update(cx, |this, cx| this.answer_permission(rid, decision, cx));
        }),
    });

    let content =
        div().w_full().flex().justify_center().px_6().child(column().pt_6().pb(size::SCROLL_FADE_H).children(items));
    let transcript = FollowScrollArea::new("transcript", &app.transcript_scroll, content)
        .fade(true)
        .button(true)
        .on_follow_change(|following, _, _| {
            #[cfg(feature = "dev")]
            if crate::devscript::active() {
                eprintln!("script: transcript following={following}");
            }
            let _ = following;
        });

    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .child(transcript)
        .child(
            div()
                .flex_none()
                .w_full()
                .flex()
                .justify_center()
                .px_6()
                .pb_4()
                .child(column().gap_2().children(notices).child(composer)),
        )
        .into_any_element()
}

/// The Settings page, centered in a scrolling column like the transcript.
fn settings(app: &mut SpaceApp, window: &mut Window, cx: &mut Context<SpaceApp>) -> AnyElement {
    let plugin_sections = plugin_settings(app, window, cx);
    let plugin_rows = plugin_rows(app);
    let app = &*app;
    let catalog = match &app.agent_models {
        Some(models) => Catalog::Ready { models: models.clone(), levels: app.agent_levels.clone().unwrap_or_default() },
        None if app.agent_id.is_none()
            || matches!(app.agent_status, PluginStatus::Crashed(_) | PluginStatus::NotStarted(_) | PluginStatus::Stopped) =>
        {
            Catalog::Unavailable
        }
        None => Catalog::Loading,
    };
    let this = cx.entity().downgrade();
    let handlers = SettingsHandlers {
        on_model: {
            let this = this.clone();
            Rc::new(move |model, _, cx| {
                let _ = this.update(cx, |this, cx| this.set_default_model(model, cx));
            })
        },
        on_thinking: {
            let this = this.clone();
            Rc::new(move |level, _, cx| {
                let _ = this.update(cx, |this, cx| this.set_default_thinking(level, cx));
            })
        },
        on_notify: {
            let this = this.clone();
            Rc::new(move |on, _, cx| {
                let _ = this.update(cx, |this, cx| this.set_notify_on_finish(on, cx));
            })
        },
        on_sound: Rc::new(move |on, _, cx| {
            let _ = this.update(cx, |this, cx| this.set_sound(on, cx));
        }),
        on_allow: handler(cx, |this, _, cx| this.allow_notifications(cx)),
        on_open_system_settings: Rc::new(|_, _| crate::notify::open_system_settings()),
        on_mode: {
            let this = cx.entity().downgrade();
            Rc::new(move |mode, _, cx| {
                let _ = this.update(cx, |this, cx| this.set_default_mode(mode, cx));
            })
        },
        on_save_field: {
            let this = cx.entity().downgrade();
            Rc::new(move |plugin, key, w, cx| {
                let _ = this.update(cx, |this, cx| this.save_plugin_setting(plugin, key, SaveKind::Checked, w, cx));
            })
        },
        on_save_anyway: {
            let this = cx.entity().downgrade();
            Rc::new(move |plugin, key, w, cx| {
                let _ = this.update(cx, |this, cx| this.save_plugin_setting(plugin, key, SaveKind::Anyway, w, cx));
            })
        },
        on_clear_field: {
            let this = cx.entity().downgrade();
            Rc::new(move |plugin, key, w, cx| {
                let _ = this.update(cx, |this, cx| this.save_plugin_setting(plugin, key, SaveKind::Clear, w, cx));
            })
        },
        on_plugin_enabled: {
            let this = cx.entity().downgrade();
            Rc::new(move |plugin, on, _, cx| {
                let _ = this.update(cx, |this, cx| this.set_plugin_enabled(plugin, on, cx));
            })
        },
    };
    let s = &app.settings.settings;
    let page = SettingsPage::new(catalog)
        .model(s.new_threads.model.clone().map(|m| (m.provider, m.id)))
        .thinking(s.new_threads.thinking_level.clone())
        .notify(s.notifications.on_finish)
        .sound(s.notifications.sound)
        .permission(app.permission)
        .modes(app.permission_modes.clone(), s.new_threads.permission_mode.clone())
        .plugin_settings(plugin_sections)
        .plugins(plugin_rows)
        .handlers(handlers);
    div()
        .id("settings")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .child(
            div()
                .w_full()
                .flex()
                .justify_center()
                .px_6()
                .child(div().w_full().max_w(size::CONTENT_MAX_W).pt_8().pb_12().child(page)),
        )
        .into_any_element()
}

/// Settings sections for service plugins that declare settings, with their fields' inputs.
fn plugin_settings(app: &mut SpaceApp, window: &mut Window, cx: &mut Context<SpaceApp>) -> Vec<PluginSettingsView> {
    let mut sections = Vec::new();
    for (plugin_id, states) in app.plugin_settings.clone() {
        if states.is_empty() || app.host.is_disabled(&plugin_id) {
            continue;
        }
        let title = app.host.plugin(&plugin_id).map_or_else(|| plugin_id.clone(), |p| p.manifest.name);
        let fields = states
            .into_iter()
            .map(|st| {
                let secret = st.field.kind == SettingKind::Secret;
                let placeholder = st.field.placeholder.clone().unwrap_or_default();
                let placeholder =
                    if st.set && secret { "Enter a new key to replace it".to_string() } else { placeholder };
                let (input, shown) = app
                    .setting_inputs
                    .entry((plugin_id.clone(), st.field.key.clone()))
                    .or_insert_with(|| (cx.new(|cx| InputState::new(window, cx).masked(secret)), None));
                if shown.as_deref() != Some(placeholder.as_str()) {
                    input.update(cx, |s, cx| s.set_placeholder(placeholder.clone(), window, cx));
                    *shown = Some(placeholder);
                }
                let input = input.clone();
                let check = app.field_checks.get(&(plugin_id.clone(), st.field.key.clone())).cloned().unwrap_or_default();
                FieldView {
                    plugin_id: plugin_id.clone(),
                    key: st.field.key.clone(),
                    label: st.field.label.clone().into(),
                    description: st.field.description.clone().into(),
                    secret,
                    set: st.set,
                    input,
                    check,
                    status: st.set.then(|| app.plugin_health(&plugin_id)).flatten(),
                }
            })
            .collect();
        sections.push(PluginSettingsView { title: title.into(), fields });
    }
    sections
}

/// Every plugin, with what it's doing; service plugins can be turned off.
fn plugin_rows(app: &SpaceApp) -> Vec<PluginRow> {
    app.host
        .plugins()
        .into_iter()
        .map(|p| {
            let agent = p.manifest.is_agent();
            let enabled = !app.host.is_disabled(p.id());
            let status = if agent {
                (app.agent_id.as_deref() == Some(p.id())).then_some(&app.agent_status)
            } else {
                app.plugin_status.get(p.id())
            };
            let status = match (enabled, status) {
                (false, _) => "Off".to_string(),
                (true, Some(PluginStatus::Running)) => "Running".into(),
                (true, Some(PluginStatus::Starting)) => "Starting…".into(),
                (true, Some(PluginStatus::Crashed(m))) => format!("Exited: {}", format::one_line(m)),
                (true, Some(PluginStatus::NotStarted(m))) => format!("Not started: {}", format::one_line(m)),
                (true, _) => "Not running".into(),
            };
            PluginRow {
                id: p.id().to_string(),
                name: p.manifest.name.clone().into(),
                description: p.manifest.description.clone().into(),
                agent,
                required: p.manifest.required,
                enabled,
                status: status.into(),
            }
        })
        .collect()
}

/// "Switch this thread to Full access?" while a mode waits for confirmation.
fn mode_confirmation(app: &SpaceApp, thread: &Thread, cx: &mut Context<SpaceApp>) -> Option<AnyElement> {
    let (key, id) = app.pending_confirm.as_ref()?;
    if *key != thread.key {
        return None;
    }
    let entry = app.permission_modes.iter().find(|e| &e.id == id)?;
    let message = entry.confirm.clone().unwrap_or_default();
    let card = confirm_card(
        message,
        format!("Use {}", entry.name),
        cx.listener(|this, _, _, cx| this.confirm_mode(true, cx)),
        cx.listener(|this, _, _, cx| this.confirm_mode(false, cx)),
    );
    Some(card.into_any_element())
}

fn composer(app: &SpaceApp, thread: &Thread, cx: &mut Context<SpaceApp>) -> AnyElement {
    let configure = {
        let this = cx.entity().downgrade();
        Rc::new(move |request, _: &mut Window, cx: &mut App| {
            let _ = this.update(cx, |this, cx| this.configure(request, cx));
        })
    };
    let remove = {
        let this = cx.entity().downgrade();
        Rc::new(move |ix, _: &mut Window, cx: &mut App| {
            let _ = this.update(cx, |this, cx| this.remove_attachment(ix, cx));
        })
    };
    let pick_dir = {
        let this = cx.entity().downgrade();
        Rc::new(move |dir, w: &mut Window, cx: &mut App| {
            let _ = this.update(cx, |this, cx| this.set_thread_dir(dir, w, cx));
        })
    };
    let handlers = ComposerHandlers {
        on_send: handler(cx, |this, w, cx| this.send(w, cx)),
        on_stop: handler(cx, |this, _, cx| this.cancel(cx)),
        on_attach: handler(cx, |this, _, cx| this.attach_files(cx)),
        on_focus: handler(cx, |this, w, cx| this.focus_composer(w, cx)),
        on_remove_attachment: remove,
        on_pick_dir: pick_dir,
        on_choose_folder: handler(cx, |this, w, cx| this.choose_folder(w, cx)),
        on_configure: configure,
        on_mode: {
            let this = cx.entity().downgrade();
            Rc::new(move |id, _: &mut Window, cx: &mut App| {
                let _ = this.update(cx, |this, cx| this.set_mode(id, cx));
            })
        },
        on_blocker_action: {
            let this = cx.entity().downgrade();
            Rc::new(move |action, w: &mut Window, cx: &mut App| {
                let _ = this.update(cx, |this, cx| this.blocker_action(&action, w, cx));
            })
        },
    };
    let mode = app
        .effective_mode(thread)
        .map(|current| ModeChoice { entries: app.permission_modes.clone(), current });
    let blocker = app.blocker(thread).map(|b| ComposerBlocker {
        times: format::attempt_times(b.last_attempt_ms, b.next_attempt_ms, crate::app::now_ms()).map(Into::into),
        text: b.text.into(),
        actions: b.actions.into_iter().map(|a| (a.label.into(), a.action.into())).collect(),
    });
    Composer::new("composer", &app.input)
        .running(thread.state.running)
        .queued(thread.queued.is_some())
        .attachments(thread.attachments.clone())
        .controls(ComposerControls {
            cwd: thread.cwd.clone(),
            dirs: app.recent_dirs(),
            thread_empty: thread.is_empty(),
            status: thread.state.status.clone(),
            locked: thread.state.running || thread.live_session().is_none(),
            mode,
            blocker,
        })
        .handlers(handlers)
        .into_any_element()
}

/// The agent could not start or exited: say so, with Restart and Logs.
fn agent_problem(app: &SpaceApp, thread: &Thread, cx: &mut Context<SpaceApp>) -> Option<AnyElement> {
    let message = match (&app.agent_status, &thread.error) {
        (PluginStatus::Crashed(msg), _) => format!("The agent exited: {}", format::one_line(msg)),
        (PluginStatus::NotStarted(msg), _) => format!("The agent can't run: {}", format::one_line(msg)),
        (_, Some(err)) => format!("Couldn't start a session: {}", format::one_line(err)),
        (_, None) if app.agent_id.is_none() => "No agent plugin is installed.".to_string(),
        _ => return None,
    };
    let actions = app.agent_id.is_some().then(|| {
        (
            click(cx, |this, _, cx| this.restart_agent(cx)),
            click(cx, |this, _, cx| {
                this.show_logs = true;
                cx.notify();
            }),
        )
    });
    Some(problem_card(message, actions).into_any_element())
}

fn logs(app: &SpaceApp, cx: &mut Context<SpaceApp>) -> impl IntoElement {
    let lines = app.logs.iter().skip(app.logs.len().saturating_sub(500)).map(|l| SharedString::from(l.clone())).collect();
    LogsPanel::new("Plugin logs · stderr", lines, &app.log_scroll)
        .on_close(cx.listener(|this, _, _, cx| this.toggle_logs(cx)))
}
