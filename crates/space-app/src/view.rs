//! Assembles the window from `space-ui` components: maps app state to component props and
//! wires component callbacks back to [`SpaceApp`] methods. No styling lives here.

use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use space_core::{Approval, PluginSource, PluginStatus, grants};
use gpui::component::input::InputState;
use space_core::protocol::service::SettingKind;
use space_ui::components::model_picker::PickerPane;
use space_ui::components::composer::{
    Composer, ComposerBlocker, ComposerControls, ComposerHandlers, ComposerMenu, Handler, ModeChoice,
};
use space_ui::components::settings::{
    Catalog, FieldView, ModelChoice, PluginApproval, PluginRow, PluginSettingsView, SettingsHandlers, SettingsPage,
};
use space_ui::components::follow_scroll::FollowScrollArea;
use space_ui::components::images::{Pic, ViewerHandlers, viewer};
use space_ui::components::message_actions::ActionHandlers;
use space_ui::components::trajectory::{TrajectoryHandlers, TrajectoryView, ViewTab, view_tabs};
use space_ui::components::shell::{
    banner, confirm_card, empty_heading, problem_card, sidebar_column,
};
use space_ui::components::thread_row::{RowIndicator, ThreadRow};
use space_ui::components::title_bar::{MainTitleBar, SidebarTitleBar, TitleBarNav};
use space_ui::components::transcript::{TranscriptView, transcript_items};
use space_ui::format::{self, Preview};
use space_ui::theme::{color, size, text};
use space_ui::ClickHandler;

use crate::app::{
    CloseSettings, CloseTerminal, CyclePermissionMode, FocusComposer, GoBack, GoForward, MaximizeTerminal, NewTerminal, NewThread,
    NextTerminal, OpenSettings, PreviousTerminal, Quit, ScrollToBottom, SpaceApp, ToggleSidebar, ToggleTerminal, ToggleTrajectory,
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
    let clock = space_ui::clock::AnimationClock::global(cx);
    let (shown, moving) = app.sidebar_shown.at(clock.now(), clock.pinned());
    if moving && clock.is_running() {
        window.request_animation_frame();
    }
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

    // The terminal pane, when the current thread has it open (and a terminal plugin runs).
    let terminal_key = app.selected.filter(|k| {
        !app.show_settings && app.host.has_terminal() && app.terminals.pane(*k).is_some_and(|p| p.open)
    });
    app.terminals.set_visible(terminal_key.and_then(|k| app.terminals.selected(k)).map(|s| s.id.clone()).as_deref());
    let terminal_open = terminal_key.is_some();
    let title_bar = MainTitleBar::new(if app.show_settings {
        "Settings".to_string()
    } else {
        app.current().map(|t| t.title()).unwrap_or_default()
    })
    .sidebar(shown, nav.clone())
    .when(!app.show_settings && app.host.has_terminal(), |b| {
        b.terminal(terminal_open, click(cx, |this, w, cx| this.toggle_terminal(w, cx)))
    });
    let content = if app.show_settings {
        settings(app, window, cx)
    } else {
        let body = body(app, window, cx);
        match terminal_key {
            Some(key) => with_terminal(app, key, body, shown, window, cx),
            None => body,
        }
    };
    let main = div().flex().flex_col().flex_1().min_w_0().h_full().bg(color::bg()).child(title_bar).child(content);

    div()
        .id("space-root")
        .key_context("Space")
        // Focus sink: a click on anything non-focusable lands here and blurs the composer.
        .track_focus(&app.focus)
        .on_action(cx.listener(|this, _: &NewThread, window, cx| this.new_thread(None, window, cx)))
        .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
        .on_action(cx.listener(|this, _: &ToggleTrajectory, window, cx| this.toggle_trajectory(window, cx)))
        .on_action(cx.listener(|this, _: &GoBack, window, cx| this.go_back(window, cx)))
        .on_action(cx.listener(|this, _: &GoForward, window, cx| this.go_forward(window, cx)))
        .on_action(cx.listener(|this, _: &FocusComposer, window, cx| this.focus_composer(window, cx)))
        .on_action(cx.listener(|this, _: &OpenSettings, window, cx| this.toggle_settings(window, cx)))
        .on_action(cx.listener(|this, _: &ScrollToBottom, _, cx| this.scroll_to_bottom(cx)))
        .on_action(cx.listener(|this, _: &CyclePermissionMode, _, cx| this.cycle_mode(cx)))
        .on_action(cx.listener(|this, _: &ToggleTerminal, window, cx| this.toggle_terminal(window, cx)))
        .on_action(cx.listener(|this, _: &NewTerminal, window, cx| this.new_terminal(window, cx)))
        .on_action(cx.listener(|this, _: &CloseTerminal, window, cx| this.close_current_terminal(window, cx)))
        .on_action(cx.listener(|this, _: &NextTerminal, window, cx| this.step_terminal(1, window, cx)))
        .on_action(cx.listener(|this, _: &PreviousTerminal, window, cx| this.step_terminal(-1, window, cx)))
        .on_action(cx.listener(|this, _: &MaximizeTerminal, _, cx| this.toggle_terminal_maximized(cx)))
        .on_action(cx.listener(|this, _: &Quit, window, cx| this.quit(window, cx)))
        // Dragging the terminal pane's divider.
        .when(app.terminals.dragging, |d| {
            d.cursor_col_resize()
                .on_mouse_move(cx.listener(move |this, e: &MouseMoveEvent, window, cx| {
                    let left = f32::from(size::SIDEBAR_W) * this.sidebar_shown.target();
                    let width = f32::from(window.viewport_size().width) - left;
                    if width > 0. {
                        this.terminals.set_width((f32::from(window.viewport_size().width) - f32::from(e.position.x)) / width);
                        cx.notify();
                    }
                }))
                .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.terminals.dragging = false;
                    cx.notify();
                }))
        })
        // Esc closes a message's usage panel, the context panel or a composer menu (the model
        // picker steps back from one of its lists first).
        .when(app.usage_open.is_some(), |d| {
            d.on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    let back = this.usage_open.as_deref().and_then(composer_menu).and_then(|m| m.back());
                    this.show_usage(back.map(menu_slot), cx);
                }
            }))
        })
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
        .relative()
        // The sidebar slides in from the left at full width, revealed by its column growing,
        // while the main column narrows beside it.
        .when(shown > 0., |d| {
            d.child(
                div()
                    .relative()
                    .flex_none()
                    .h_full()
                    .overflow_hidden()
                    .w(size::SIDEBAR_W * shown)
                    .child(div().absolute().top_0().right_0().h_full().child(sidebar(app, nav.clone(), shown, cx))),
            )
        })
        .child(main)
        .child(nav.overlay())
        .child(space_ui::components::tip::layer(window, cx))
        .children(app.viewer.as_ref().map(|(pics, ix)| {
            viewer(pics, *ix, &app.viewer_focus, ViewerHandlers {
                on_close: handler(cx, |this, w, cx| this.close_viewer(w, cx)),
                on_step: {
                    let this = cx.entity().downgrade();
                    Rc::new(move |by, _: &mut Window, cx: &mut App| {
                        let _ = this.update(cx, |this, cx| this.step_viewer(by, cx));
                    })
                },
            })
        }))
}

/// A queued message's picture (prepared, not saved yet).
fn queued_pic(p: &crate::images::Prepared) -> Pic {
    Pic { src: Arc::new(Image::from_bytes(p.gpui_format(), p.bytes.to_vec())).into(), width: p.width, height: p.height }
}

fn sidebar(app: &SpaceApp, nav: TitleBarNav, shown: f32, cx: &mut Context<SpaceApp>) -> impl IntoElement {
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
                .terminal(!selected && app.terminals.active(key))
                .on_click(cx.listener(move |this, _, w, cx| this.select_thread(key, w, cx)))
                .on_delete(cx.listener(move |this, _, w, cx| this.delete_thread(key, w, cx)))
                .into_any_element()
        })
        .collect();

    sidebar_column(SidebarTitleBar::new(nav).shown(shown), rows)
}

/// The thread's body with the terminal pane beside it (or instead of it, maximized).
fn with_terminal(app: &mut SpaceApp, key: u64, body: AnyElement, sidebar: f32, window: &mut Window, cx: &mut Context<SpaceApp>) -> AnyElement {
    let Some(pane) = app.terminals.pane(key) else { return body };
    let maximized = pane.maximized;
    let main_width = f32::from(window.viewport_size().width) - f32::from(size::SIDEBAR_W) * sidebar;
    let width = px((main_width * app.terminals.width).round());
    let pane = terminal_pane(app, key, window, cx);
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .child(div().flex_1().min_w_0().flex().flex_col().when(maximized, |d| d.hidden()).child(body))
        .child(
            div()
                .relative()
                .h_full()
                .flex()
                .flex_col()
                .map(|d| if maximized { d.flex_1().min_w_0() } else { d.flex_none().w(width) })
                .child(pane)
                // Beside the chat: a hairline like the title bar's, and a handle on it to resize.
                .when(!maximized, |d| {
                    d.border_l_1().border_color(color::border()).child(
                        div()
                            .id("terminal-divider")
                            .absolute()
                            .top_0()
                            .left(px(-4.))
                            .w(px(7.))
                            .h_full()
                            .cursor_col_resize()
                            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                                this.terminals.dragging = true;
                                cx.stop_propagation();
                                cx.notify();
                            })),
                    )
                }),
        )
        .into_any_element()
}

/// The pane: tabs, then the selected terminal (or why there's none).
fn terminal_pane(app: &mut SpaceApp, key: u64, window: &mut Window, cx: &mut Context<SpaceApp>) -> AnyElement {
    use space_ui::components::terminal::{StripHandlers, stopped_notice, tab_strip};
    let (tabs, selected, maximized, opening, error) = {
        let pane = app.terminals.pane(key).expect("open pane");
        let tabs: Vec<_> = pane.tabs.iter().filter_map(|id| app.terminals.session(id)).map(|s| s.tab()).collect();
        (tabs, pane.selected, pane.maximized, pane.opening, pane.error.clone())
    };
    let handlers = StripHandlers {
        on_select: {
            let this = cx.entity().downgrade();
            Rc::new(move |ix, w, cx| {
                let _ = this.update(cx, |this, cx| this.select_terminal(ix, w, cx));
            })
        },
        on_close: {
            let this = cx.entity().downgrade();
            Rc::new(move |ix, w, cx| {
                let _ = this.update(cx, |this, cx| this.close_terminal(ix, w, cx));
            })
        },
        on_new: handler(cx, |this, w, cx| this.new_terminal(w, cx)),
        on_maximize: handler(cx, |this, _, cx| this.toggle_terminal_maximized(cx)),
        on_hide: handler(cx, |this, w, cx| this.hide_terminal(w, cx)),
    };
    let session = app.terminals.selected(key);
    let stopped = session.as_ref().is_some_and(|s| s.state.lock().unwrap().stopped);
    let content = match session {
        Some(s) if stopped => {
            let id = s.id.clone();
            stopped_notice(
                "The terminal plugin stopped, and this terminal's shell with it.",
                handler(cx, move |this, w, cx| this.restart_terminal(&id, w, cx)),
            )
            .into_any_element()
        }
        Some(s) => match app.terminal_view(&s.id, window, cx) {
            Some(view) => div().flex_1().min_h_0().child(view).into_any_element(),
            None => div().flex_1().into_any_element(),
        },
        None => match error {
            Some(e) => stopped_notice(e, handler(cx, |this, w, cx| this.new_terminal(w, cx))).into_any_element(),
            None => div()
                .flex_1()
                .p_3()
                .text_size(text::SM)
                .text_color(color::text_3())
                .child(if opening > 0 { "Starting a shell…" } else { "" })
                .into_any_element(),
        },
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(color::bg())
        .child(tab_strip(&tabs, selected, maximized, &handlers))
        .child(content)
        .into_any_element()
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

    // The tabs: only with a trajectory plugin.
    let has_trajectory = app.host.has_trajectory();
    let tabs = has_trajectory.then(|| {
        let this = cx.entity().downgrade();
        view_tabs(app.view_tab(), Rc::new(move |tab, w, cx| {
            let _ = this.update(cx, |this, cx| this.show_tab(tab, w, cx));
        }))
    });
    let bottom = div()
        .flex_none()
        .w_full()
        .flex()
        .justify_center()
        .px_6()
        .pb_4()
        .child(column().gap_2().children(notices).child(composer));

    if app.view_tab() == ViewTab::Trajectory
        && let Some(view) = trajectory_view(app, cx)
    {
        // No composer here: the ledger gets the whole height.
        return div().flex_1().min_h_0().flex().flex_col().children(tabs).child(view).into_any_element();
    }

    // The streaming reply (the last item of a running turn) shows at an even pace.
    let streaming = streaming_reply(app, thread, window, cx);
    #[cfg(feature = "dev")]
    let built = std::time::Instant::now();
    let this = cx.entity().downgrade();
    let (this2, this3, this4) = (this.clone(), this.clone(), this.clone());
    let items = transcript_items(TranscriptView {
        state: &thread.state,
        key: thread.key,
        expanded: &thread.expanded,
        thought_secs: &thread.thought_secs,
        queued: thread.queued.as_ref().map(|q| (q.text.as_str(), q.attachments.as_slice(), q.images.iter().map(queued_pic).collect())),
        on_toggle: Rc::new(move |ix, _, cx| {
            let _ = this.update(cx, |this, cx| this.toggle_expanded(ix, cx));
        }),
        on_answer: Rc::new(move |rid, decision, _, cx| {
            let _ = this2.update(cx, |this, cx| this.answer_permission(rid, decision, cx));
        }),
        on_open_image: Some(Rc::new(move |pics, ix, w, cx| {
            let _ = this3.update(cx, |this, cx| this.open_viewer(pics, ix, w, cx));
        })),
        now_ms: crate::app::now_ms(),
        copied: app.copied.as_deref(),
        usage_open: app.usage_open.as_deref(),
        actions: ActionHandlers {
            on_copy: {
                let this = cx.entity().downgrade();
                Rc::new(move |row, text, _, cx| {
                    let _ = this.update(cx, |this, cx| this.copy_message(row.to_string(), text.to_string(), cx));
                })
            },
            on_usage: {
                let this = cx.entity().downgrade();
                Rc::new(move |row, _, cx| {
                    let _ = this.update(cx, |this, cx| this.show_usage(row.map(|r| r.to_string()), cx));
                })
            },
        },
        open_turns: &thread.open_turns,
        on_toggle_turn: Rc::new(move |ix, _, cx| {
            let _ = this4.update(cx, |this, cx| this.toggle_turn(ix, cx));
        }),
        streaming,
    }, window, cx);
    #[cfg(feature = "dev")]
    perf::frame(built.elapsed());

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

    div().flex_1().min_h_0().flex().flex_col().children(tabs).child(transcript).child(bottom).into_any_element()
}

/// Dev: `SPACE_PERF=1` prints, every 2 s, how often the transcript was built (frame interval)
/// and how long building its elements took.
#[cfg(feature = "dev")]
mod perf {
    use std::cell::RefCell;
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct Stats {
        last: Option<Instant>,
        since: Option<Instant>,
        frames: u32,
        max_gap: Duration,
        build: Duration,
        max_build: Duration,
    }

    thread_local!(static STATS: RefCell<Stats> = RefCell::default());

    pub fn frame(build: Duration) {
        if std::env::var_os("SPACE_PERF").is_none() {
            return;
        }
        STATS.with(|s| {
            let mut s = s.borrow_mut();
            let now = Instant::now();
            if let Some(last) = s.last {
                s.max_gap = s.max_gap.max(now - last);
            }
            s.last = Some(now);
            let since = *s.since.get_or_insert(now);
            s.frames += 1;
            s.build += build;
            s.max_build = s.max_build.max(build);
            if now - since >= Duration::from_secs(2) {
                let n = s.frames.max(1);
                eprintln!(
                    "perf: {} frames in {:.1}s (avg {:.1} ms apart, max {:.1} ms); transcript build avg {:.2} ms, max {:.2} ms",
                    s.frames,
                    (now - since).as_secs_f64(),
                    (now - since).as_secs_f64() * 1000. / n as f64,
                    s.max_gap.as_secs_f64() * 1000.,
                    s.build.as_secs_f64() * 1000. / n as f64,
                    s.max_build.as_secs_f64() * 1000.,
                );
                *s = Stats { last: Some(now), ..Default::default() };
            }
        });
    }
}

/// The streaming reply of `thread`, if a turn is running and its last item is a reply: its index
/// and how much of it to show now (asking for the next frame while more is to come).
fn streaming_reply(app: &SpaceApp, thread: &Thread, window: &mut Window, cx: &mut Context<SpaceApp>) -> Option<(usize, usize)> {
    let mut reveal = app.stream_reveal.borrow_mut();
    let last = thread.state.items.len().checked_sub(1);
    let text = match (thread.state.running, last.and_then(|i| thread.state.items.get(i))) {
        (true, Some(space_core::TranscriptItem::Agent(text))) => text,
        _ => {
            reveal.idle(thread.key);
            return None;
        }
    };
    let clock = space_ui::clock::AnimationClock::global(cx);
    let instant = cx.reduce_motion() || clock.pinned().is_some();
    let (shown, more) = reveal.advance(thread.key, last?, text, clock.now(), instant);
    if more {
        window.request_animation_frame();
    }
    Some((last?, shown))
}

/// The Trajectory tab's view of the selected thread (`None` until its state exists).
fn trajectory_view(app: &SpaceApp, cx: &mut Context<SpaceApp>) -> Option<AnyElement> {
    let state = app.trajectories.state.as_ref()?;
    let this = cx.entity().downgrade();
    let h = TrajectoryHandlers {
        on_select: {
            let this = this.clone();
            Rc::new(move |id, _, cx| {
                let _ = this.update(cx, |this, cx| this.select_trajectory_row(id, cx));
            })
        },
        on_close_inspector: handler(cx, |this, _, cx| this.close_trajectory_inspector(cx)),
        on_toggle_fold: {
            let this = this.clone();
            Rc::new(move |key, _, cx| {
                let _ = this.update(cx, |this, cx| this.toggle_trajectory_fold(key, cx));
            })
        },
        on_fold_all: {
            let this = this.clone();
            Rc::new(move |scope, _, cx| {
                let _ = this.update(cx, |this, cx| this.fold_all_trajectory(scope, cx));
            })
        },
        on_duration_mode: {
            let this = this.clone();
            Rc::new(move |mode, _, cx| {
                let _ = this.update(cx, |this, cx| this.set_trajectory_mode(mode, cx));
            })
        },
        on_range: {
            let this = this.clone();
            Rc::new(move |range, _, cx| {
                let _ = this.update(cx, |this, cx| this.set_trajectory_range(range, cx));
            })
        },
        on_load_earlier: handler(cx, |this, _, cx| this.load_earlier_trajectory(cx)),
        on_copy: Rc::new(|text, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()))),
        on_open_image: {
            let this = this.clone();
            Rc::new(move |pics, ix, w, cx| {
                let _ = this.update(cx, |this, cx| this.open_trajectory_images(pics, ix, w, cx));
            })
        },
    };
    Some(
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                TrajectoryView::new(state.page.clone(), &state.scroll)
                    .selected(state.selected.clone(), state.detail.clone())
                    .folds(state.folds.clone())
                    .search(&app.trajectories.search, state.matches.clone())
                    .mode(state.mode)
                    .range(state.range)
                    .now(crate::app::now_ms())
                    .handlers(h),
            )
            .into_any_element(),
    )
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
        on_thinking: Rc::new(move |level, _, cx| {
            let _ = this.update(cx, |this, cx| this.set_default_thinking(level, cx));
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
        on_toggle_field: {
            let this = cx.entity().downgrade();
            Rc::new(move |plugin, key, on, _, cx| {
                let _ = this.update(cx, |this, cx| this.set_plugin_value(plugin, key, Some(on.to_string()), cx));
            })
        },
        on_choose_field: {
            let this = cx.entity().downgrade();
            Rc::new(move |plugin, key, value, _, cx| {
                let _ = this.update(cx, |this, cx| this.set_plugin_value(plugin, key, value, cx));
            })
        },
        on_plugin_enabled: {
            let this = cx.entity().downgrade();
            Rc::new(move |plugin, on, _, cx| {
                let _ = this.update(cx, |this, cx| this.set_plugin_enabled(plugin, on, cx));
            })
        },
        on_plugin_approval: {
            let this = cx.entity().downgrade();
            Rc::new(move |plugin, approved, _, cx| {
                let _ = this.update(cx, |this, cx| this.answer_plugin_approval(plugin, approved, cx));
            })
        },
    };
    let s = &app.settings.settings;
    let page = SettingsPage::new(catalog)
        .model(s.new_threads.model.clone().map(|m| (m.provider, m.id)))
        .thinking(s.new_threads.thinking_level.clone())
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
                let switch = (st.field.kind == SettingKind::Switch)
                    .then(|| st.value.as_deref().or(st.field.default.as_deref()) == Some("true"));
                let model = (st.field.kind == SettingKind::Model)
                    .then(|| ModelChoice { chosen: st.value.clone(), models: app.agent_models.clone().unwrap_or_default() });
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
                    switch,
                    model,
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
            let approval = app.host.approval(p.id());
            let status = match (enabled, status) {
                (false, _) => "Off".to_string(),
                (true, _) if approval == Approval::Pending => "Needs your approval".into(),
                (true, _) if approval == Approval::Denied => "Not approved".into(),
                (true, Some(PluginStatus::Running)) => "Running".into(),
                (true, _) if agent && app.agent_asleep => "Asleep (starts when you write)".into(),
                (true, Some(PluginStatus::Starting)) => "Starting…".into(),
                (true, Some(PluginStatus::Crashed(m))) => format!("Exited: {}", format::one_line(m)),
                (true, Some(PluginStatus::NotStarted(m))) => format!("Not started: {}", format::one_line(m)),
                (true, _) => "Not running".into(),
            };
            PluginRow {
                id: p.id().to_string(),
                name: p.manifest.name.clone().into(),
                description: match (approval, p.source) {
                    (Approval::Pending | Approval::Denied, PluginSource::User) => format!("From ~/.space/plugins. {}", p.manifest.description),
                    (Approval::Pending | Approval::Denied, _) => format!("From this project's .space/plugins. {}", p.manifest.description),
                    _ => p.manifest.description.clone(),
                }
                .trim_end()
                .to_string()
                .into(),
                agent,
                required: p.manifest.required,
                enabled,
                status: status.into(),
                approval: matches!(approval, Approval::Pending | Approval::Denied).then(|| PluginApproval {
                    permissions: p.manifest.permissions().iter().map(|perm| grants::describe(perm).into()).collect(),
                    denied: approval == Approval::Denied,
                }),
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
        on_remove_image: {
            let this = cx.entity().downgrade();
            Rc::new(move |ix, _: &mut Window, cx: &mut App| {
                let _ = this.update(cx, |this, cx| this.remove_image(ix, cx));
            })
        },
        on_drop: {
            let this = cx.entity().downgrade();
            Rc::new(move |paths, _: &mut Window, cx: &mut App| {
                let _ = this.update(cx, |this, cx| this.add_files(paths, cx));
            })
        },
        on_recall: {
            let this = cx.entity().downgrade();
            Rc::new(move |step, w: &mut Window, cx: &mut App| this.update(cx, |this, cx| this.recall(step, w, cx)).unwrap_or(false))
        },
        on_paste: {
            let this = cx.entity().downgrade();
            Rc::new(move |item: &ClipboardItem, _: &mut Window, cx: &mut App| this.update(cx, |this, cx| this.paste(item, cx)).unwrap_or(false))
        },
        on_context: {
            let this = cx.entity().downgrade();
            Rc::new(move |open, _: &mut Window, cx: &mut App| {
                let _ = this.update(cx, |this, cx| this.show_usage(open.then(|| CONTEXT_PANEL.to_string()), cx));
            })
        },
        on_menu: {
            let this = cx.entity().downgrade();
            Rc::new(move |menu, _: &mut Window, cx: &mut App| {
                let _ = this.update(cx, |this, cx| this.show_usage(menu.map(menu_slot), cx));
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
        .images(thread.images.iter().map(|i| i.pic().map(|p| p.src)).collect())
        .controls(ComposerControls {
            cwd: thread.cwd.clone(),
            dirs: app.recent_dirs(),
            thread_empty: thread.is_empty(),
            status: thread.state.status.clone(),
            // While the agent sleeps the choice is kept and applied when the session is loaded again.
            locked: thread.state.running || (thread.live_session().is_none() && !app.agent_asleep),
            mode,
            blocker,
            stats: app.host.has_trajectory().then(|| app.trajectories.stats.get(&thread.id).cloned()).flatten(),
            context_open: app.usage_open.as_deref() == Some(CONTEXT_PANEL),
            menu: app.usage_open.as_deref().and_then(composer_menu),
        })
        .handlers(handlers)
        .into_any_element()
}


/// `usage_open` while the context ring's panel is open (rows use their ids).
const CONTEXT_PANEL: &str = "composer:context";

/// `usage_open` while one of the composer's menus is open.
fn menu_slot(menu: ComposerMenu) -> String {
    match menu {
        ComposerMenu::Folder => "composer:folder",
        ComposerMenu::Mode => "composer:mode",
        ComposerMenu::Model(PickerPane::Root) => "composer:picker",
        ComposerMenu::Model(PickerPane::Models) => "composer:picker:models",
        ComposerMenu::Model(PickerPane::Efforts) => "composer:picker:efforts",
    }
    .into()
}

pub(crate) fn composer_menu(slot: &str) -> Option<ComposerMenu> {
    [PickerPane::Root, PickerPane::Models, PickerPane::Efforts]
        .map(ComposerMenu::Model)
        .into_iter()
        .chain([ComposerMenu::Folder, ComposerMenu::Mode])
        .find(|m| menu_slot(*m) == slot)
}

/// The agent could not start or exited: say so, with Restart.
fn agent_problem(app: &SpaceApp, thread: &Thread, cx: &mut Context<SpaceApp>) -> Option<AnyElement> {
    // pi-agent talks to Claude through the Claude CLI; without it nothing works.
    if app.agent_id.as_deref() == Some("pi-agent") && !app.claude_cli {
        let message = "Space needs the Claude CLI: install it (brew install --cask claude-code) and run `claude` once to sign in.";
        return Some(problem_card(message, Some(click(cx, |this, _, cx| this.restart_agent(cx)))).into_any_element());
    }
    let message = match (&app.agent_status, &thread.error) {
        (PluginStatus::Crashed(msg), _) => format!("The agent exited: {}", format::one_line(msg)),
        (PluginStatus::NotStarted(msg), _) => format!("The agent can't run: {}", format::one_line(msg)),
        (_, Some(err)) => format!("Couldn't start a session: {}", format::one_line(err)),
        (_, None) if app.agent_id.is_none() => "No agent plugin is installed.".to_string(),
        _ => return None,
    };
    let restart = app.agent_id.is_some().then(|| click(cx, |this, _, cx| this.restart_agent(cx)));
    Some(problem_card(message, restart).into_any_element())
}

