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
    ClosedThread, Composer, ComposerBlocker, ComposerControls, ComposerHandlers, ComposerMenu, Handler, ModeChoice,
};
use space_ui::components::settings::{
    Catalog, FieldView, ModelChoice, PluginApproval, PluginRow, PluginSettingsView, SettingsHandlers, SettingsPage,
};
use space_ui::components::follow_scroll::FollowScrollArea;
use space_ui::components::right_pane::PaneView;
use space_ui::components::images::{Pic, ViewerHandlers, viewer};
use space_ui::components::message_actions::ActionHandlers;
use space_ui::components::trajectory::{TrajectoryHandlers, TrajectoryView, ViewTab, view_tabs};
use space_ui::components::shell::{
    banner, confirm_card, empty_heading, problem_card, sidebar_column,
};
use space_ui::components::thread_row::{RowIndicator, ThreadRow, section_header};
use space_ui::components::title_bar::{MainTitleBar, SidebarTitleBar, TitleBarNav};
use space_ui::components::find::find_bar;
use space_ui::components::transcript::{FindMarks, TranscriptView, UserExtras, transcript_items};
use space_ui::format::{self, Preview};
use space_ui::theme::{color, size, text};
use space_ui::ClickHandler;

use crate::app::{
    CloseSettings, CloseTerminal, CloseThread, CyclePermissionMode, NextThread, PreviousThread, FocusComposer, GoBack, GoForward, MaximizeTerminal, NewTerminal, NewThread,
    NextTerminal, OpenSettings, PreviousTerminal, Quit, ScrollToBottom, SpaceApp, ToggleSidebar, ToggleRightPane, ShowTerminal, ShowFiles, ToggleTrajectory,
    OpenFind, FindNext, FindPrevious, CloseFind, RenameThread, TogglePinThread,
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
    // A stopped turn's queued messages go back into the composer.
    app.restore_stopped_queue(window, cx);
    let clock = space_ui::clock::AnimationClock::global(cx);
    let (now, pinned, clock_running) = (clock.now(), clock.pinned(), clock.is_running());
    let (shown, moving) = app.sidebar_shown.at(now, pinned);
    if moving && clock_running {
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
        new_thread_active: !app.show_settings && app.current().is_some_and(Thread::is_empty),
    };

    // The terminal pane, when the current thread has it open (and a terminal plugin runs).
    let terminal_key = app.selected.filter(|k| {
        !app.show_settings && app.terminals.pane(*k).is_some_and(|p| p.open)
    });
    app.terminals.set_visible(terminal_key.and_then(|k| app.terminals.selected(k)).map(|s| s.id.clone()).as_deref());
    let terminal_open = terminal_key.is_some();
    app.terminal_slide(cx);
    let (terminal_shown, terminal_moving) = app.terminal_shown.at(now, pinned);
    if terminal_moving && clock_running {
        window.request_animation_frame();
    }
    // Drawn while open, and while it slides away after hiding.
    let terminal_drawn = terminal_key.or_else(|| {
        app.selected.filter(|k| {
            terminal_shown > 0. && !app.show_settings && app.terminals.pane(*k).is_some()
        })
    });
    let title_bar = MainTitleBar::new(if app.show_settings {
        "Settings".to_string()
    } else {
        app.current().map(|t| t.title()).unwrap_or_default()
    })
    .sidebar(shown, nav.clone())
    .when_some((!app.show_settings).then(|| app.open_in_view(cx)).flatten(), |b, o| b.open_in(o))
    .when(!app.show_settings && app.current().is_some(), |b| {
        b.right_pane(terminal_open, click(cx, |this, w, cx| this.toggle_right_pane(w, cx)))
    });
    let content = if app.show_settings {
        settings(app, window, cx)
    } else {
        let streaming = app.prepare_transcript(window, cx);
        let body = body(app, streaming, window, cx);
        match terminal_drawn {
            Some(key) => with_terminal(app, key, body, shown, terminal_shown, window, cx),
            None => body,
        }
    };
    let main = div().flex().flex_col().flex_1().min_w_0().h_full().bg(color::bg()).child(title_bar).child(content);

    div()
        .id("space-root")
        .key_context("Space")
        // A press, release or scroll may start, finish, clear or move a selection to quote.
        .capture_any_mouse_down(cx.listener(|this, _: &MouseDownEvent, w, cx| this.refresh_quote_offer(w, cx)))
        .capture_any_mouse_up(cx.listener(|this, _: &MouseUpEvent, w, cx| this.refresh_quote_offer(w, cx)))
        .on_scroll(cx.listener(|this, _: &ScrollEvent, w, cx| {
            if this.quote_offer.is_some() {
                this.refresh_quote_offer(w, cx);
                cx.notify();
            }
        }))
        // Focus sink: a click on anything non-focusable lands here and blurs the composer.
        .track_focus(&app.focus)
        .on_action(cx.listener(|this, _: &NewThread, window, cx| this.new_thread(None, window, cx)))
        .on_action(cx.listener(|this, _: &CloseThread, window, cx| this.close_current_thread(window, cx)))
        .on_action(cx.listener(|this, _: &RenameThread, window, cx| this.rename_current_thread(window, cx)))
        .on_action(cx.listener(|this, _: &TogglePinThread, _, cx| this.toggle_pin_current(cx)))
        .on_action(cx.listener(|this, _: &space_ui::actions::OpenInLastApp, _, cx| this.open_thread_folder(None, cx)))
        .on_action(cx.listener(|this, _: &NextThread, window, cx| this.step_thread(1, window, cx)))
        .on_action(cx.listener(|this, _: &PreviousThread, window, cx| this.step_thread(-1, window, cx)))
        .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
        .on_action(cx.listener(|this, _: &ToggleTrajectory, window, cx| this.toggle_trajectory(window, cx)))
        .on_action(cx.listener(|this, _: &GoBack, window, cx| this.go_back(window, cx)))
        .on_action(cx.listener(|this, _: &GoForward, window, cx| this.go_forward(window, cx)))
        .on_action(cx.listener(|this, _: &FocusComposer, window, cx| this.focus_composer(window, cx)))
        .on_action(cx.listener(|this, _: &OpenSettings, window, cx| this.toggle_settings(window, cx)))
        .on_action(cx.listener(|this, _: &ScrollToBottom, _, cx| this.scroll_to_bottom(cx)))
        .on_action(cx.listener(|this, _: &CyclePermissionMode, _, cx| this.cycle_mode(cx)))
        .on_action(cx.listener(|this, _: &ToggleRightPane, window, cx| this.toggle_right_pane(window, cx)))
        .on_action(cx.listener(|this, _: &ShowTerminal, window, cx| this.show_pane_view(PaneView::Terminal, window, cx)))
        .on_action(cx.listener(|this, _: &ShowFiles, window, cx| this.show_pane_view(PaneView::Files, window, cx)))
        .on_action(cx.listener(|this, _: &NewTerminal, window, cx| this.new_terminal(window, cx)))
        .on_action(cx.listener(|this, _: &CloseTerminal, window, cx| this.close_current_terminal(window, cx)))
        .on_action(cx.listener(|this, _: &NextTerminal, window, cx| this.step_terminal(1, window, cx)))
        .on_action(cx.listener(|this, _: &PreviousTerminal, window, cx| this.step_terminal(-1, window, cx)))
        .on_action(cx.listener(|this, _: &MaximizeTerminal, _, cx| this.toggle_terminal_maximized(cx)))
        .on_action(cx.listener(|this, _: &Quit, window, cx| this.quit(window, cx)))
        .on_action(cx.listener(|this, _: &OpenFind, window, cx| this.open_find(window, cx)))
        .on_action(cx.listener(|this, _: &FindNext, window, cx| this.find_step(true, window, cx)))
        .on_action(cx.listener(|this, _: &FindPrevious, window, cx| this.find_step(false, window, cx)))
        .on_action(cx.listener(|this, _: &CloseFind, window, cx| this.close_find(true, window, cx)))
        // ⌘ held turns the composer's queue button white (after this step), and alone numbers
        // the sidebar rows; letting go goes to the row whose number was typed (see `jump`).
        .on_modifiers_changed(cx.listener(|this, e: &ModifiersChangedEvent, window, cx| {
            let m = &e.modifiers;
            let alone = m.platform && !m.alt && !m.control && !m.shift;
            if this.cmd_held != m.platform || this.cmd_alone != alone {
                let released = this.cmd_held && !m.platform;
                if !this.cmd_held && m.platform {
                    this.cmd_digits.clear();
                    this.cmd_other = false;
                }
                this.cmd_held = m.platform;
                this.cmd_alone = alone;
                if released {
                    this.cmd_released(window, cx);
                }
                cx.notify();
            }
        }))
        // A digit with ⌘ held picks a sidebar row; it goes no further.
        .capture_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
            if e.keystroke.modifiers.platform && this.cmd_key(&e.keystroke, cx) {
                cx.stop_propagation();
            }
        }))
        // Esc in the composer while the agent works stops it (open panels and menus close first).
        .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
            if e.keystroke.key == "escape"
                && !e.keystroke.modifiers.modified()
                && this.usage_open.is_none()
                && this.current().is_some_and(|t| t.state.running)
                && this.input.read(cx).focus_handle(cx).is_focused(window)
            {
                this.cancel(cx);
                cx.stop_propagation();
            }
        }))
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
        .child(nav.overlay(shown))
        .child(space_ui::components::tip::layer(window, cx))
        .children(app.quote_button(window, cx))
        .children(app.paste_view.as_ref().map(|(body, chars)| {
            space_ui::components::pastes::text_view(body.clone(), *chars, &app.viewer_focus, handler(cx, |this, w, cx| this.close_paste_view(w, cx)))
        }))
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
    let shortcuts = app.row_shortcuts();
    // Pinned threads first, then by day (`listed_sections`); a new thread isn't listed until
    // its first message is sent (the new-thread button shows it's the current one). A section's
    // header is drawn in its first row's element, so the list keeps one element per row (⌘
    // numbers find rows by position).
    let rows = app
        .listed_sections()
        .into_iter()
        .enumerate()
        .filter_map(|(ix, (key, header))| app.thread(key).map(|t| (ix, t, header)))
        .zip(shortcuts)
        .map(|((ix, t, header), shortcut)| {
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
            let row = ThreadRow::new(("thread", key as usize), t.title())
                .selected(selected)
                .indicator(indicator)
                .terminal(!selected && app.terminals.active(key))
                .shortcut(shortcut)
                .on_click(cx.listener(move |this, _, w, cx| this.select_thread(key, w, cx)))
                .on_pin(t.pinned_ms.is_some(), !t.is_empty(), cx.listener(move |this, _, _, cx| this.toggle_pin(key, cx)))
                .on_rename(cx.listener(move |this, _, w, cx| this.rename_thread(key, w, cx)))
                .on_close(cx.listener(move |this, _, w, cx| this.confirm_close_thread(key, w, cx)));
            match header {
                Some(label) => div().child(section_header(label, ix == 0)).child(row).into_any_element(),
                None => row.into_any_element(),
            }
        })
        .collect();

    sidebar_column(SidebarTitleBar::new(nav).shown(shown), rows, &app.sidebar_scroll)
}

/// The thread's body with the terminal pane beside it (or instead of it, maximized). `shown`:
/// how far the pane has slid in (0..1); its column grows to its width while the pane inside
/// keeps that width, so it slides in from the right without its terminal resizing.
fn with_terminal(
    app: &mut SpaceApp,
    key: u64,
    body: AnyElement,
    sidebar: f32,
    shown: f32,
    window: &mut Window,
    cx: &mut Context<SpaceApp>,
) -> AnyElement {
    let Some(pane) = app.terminals.pane(key) else { return body };
    let maximized = pane.maximized;
    let main_width = f32::from(window.viewport_size().width) - f32::from(size::SIDEBAR_W) * sidebar;
    let width = px((main_width * app.terminals.width).round());
    let sliding = !maximized && shown < 1.;
    let pane = terminal_pane(app, key, window, cx);
    let pane = if sliding {
        div().absolute().top_0().left_0().h_full().w(width).flex().flex_col().child(pane).into_any_element()
    } else {
        pane
    };
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
                .map(|d| if maximized { d.flex_1().min_w_0() } else { d.flex_none().w(width * shown) })
                .when(sliding, |d| d.overflow_hidden())
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

/// The right pane: its header (view switch, the view's controls, hide), then the view, or the
/// chooser when the thread hasn't picked one.
fn terminal_pane(app: &mut SpaceApp, key: u64, window: &mut Window, cx: &mut Context<SpaceApp>) -> AnyElement {
    use space_ui::components::right_pane::{ViewHandler, chooser, files_placeholder, header};
    let view = app.terminals.pane(key).and_then(|p| p.view);
    let on_view: ViewHandler = {
        let this = cx.entity().downgrade();
        Rc::new(move |v, w, cx| {
            let _ = this.update(cx, |this, cx| this.show_pane_view(v, w, cx));
        })
    };
    let on_hide = handler(cx, |this, w, cx| this.hide_terminal(w, cx));
    let (controls, content) = match view {
        None => (None, chooser(on_view.clone(), window).into_any_element()),
        Some(PaneView::Files) => (None, files_placeholder().into_any_element()),
        Some(PaneView::Terminal) => {
            let (strip, content) = terminal_view_parts(app, key, window, cx);
            (Some(strip), content)
        }
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(color::bg())
        .child(header(view, controls, on_view, on_hide))
        .child(content)
        .into_any_element()
}

/// The Terminal view: its tabs (for the header), then the selected terminal (or why there's none).
fn terminal_view_parts(app: &mut SpaceApp, key: u64, window: &mut Window, cx: &mut Context<SpaceApp>) -> (Div, AnyElement) {
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
                .child(if opening > 0 {
                    "Starting a shell…"
                } else if !app.host.has_terminal() {
                    "The terminal plugin isn't running."
                } else {
                    ""
                })
                .into_any_element(),
        },
    };
    (tab_strip(&tabs, selected, maximized, &handlers), content)
}

fn body(app: &SpaceApp, streaming: Option<(usize, usize)>, window: &mut Window, cx: &mut Context<SpaceApp>) -> AnyElement {
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
        // The same as the gap between the composer's card and its controls row above it.
        .pb_2()
        .child(column().gap_2().children(notices).child(composer));

    if app.view_tab() == ViewTab::Trajectory
        && let Some(view) = trajectory_view(app, cx)
    {
        // No composer here: the ledger gets the whole height.
        return div().flex_1().min_h_0().flex().flex_col().children(tabs).child(view).into_any_element();
    }

    #[cfg(feature = "dev")]
    let built = std::time::Instant::now();
    let this = cx.entity().downgrade();
    let (this2, this3, this4) = (this.clone(), this.clone(), this.clone());
    let items = transcript_items(TranscriptView {
        state: &thread.state,
        key: thread.key,
        expanded: &thread.expanded,
        thought_secs: &thread.thought_secs,
        queued: thread.queued.as_ref().map(|q| {
            let extras = UserExtras {
                attachments: q.attachments.as_slice(),
                pics: q.images.iter().map(queued_pic).collect(),
                unsaved_texts: q.pastes.iter().map(|p| p.chars).collect(),
                quote: q.quote.as_deref(),
                ..Default::default()
            };
            (q.text.as_str(), extras)
        }),
        on_toggle: Rc::new(move |ix, _, cx| {
            let _ = this.update(cx, |this, cx| this.toggle_expanded(ix, cx));
        }),
        on_answer: Rc::new(move |rid, decision, _, cx| {
            let _ = this2.update(cx, |this, cx| this.answer_permission(rid, decision, cx));
        }),
        on_open_image: Some(Rc::new(move |pics, ix, w, cx| {
            let _ = this3.update(cx, |this, cx| this.open_viewer(pics, ix, w, cx));
        })),
        on_open_text: {
            let this = cx.entity().downgrade();
            Some(Rc::new(move |text, w, cx| {
                let _ = this.update(cx, |this, cx| this.open_sent_paste(text, w, cx));
            }))
        },
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
        replies: &app.replies.states,
        find: app.find.as_ref().filter(|f| f.open).map(|f| FindMarks {
            user: &f.user,
            current: f.current_match().map(|m| (m.ix, m.range.clone())),
            reveal: f.reveal.clone(),
        }),
        on_reveal: {
            let scroll = app.transcript_scroll.clone();
            Some(Rc::new(move |bounds, window, _| {
                scroll.reveal(bounds);
                window.refresh();
            }))
        },
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

    // The find bar floats at the top right of the transcript.
    let find_bar = app.find.as_ref().filter(|f| f.open).map(|f| {
        let on_step: space_ui::components::find::StepHandler = {
            let this = cx.entity().downgrade();
            Rc::new(move |forward, w, cx| {
                let _ = this.update(cx, |this, cx| this.find_step(forward, w, cx));
            })
        };
        let on_close: space_ui::components::find::CloseHandler = {
            let this = cx.entity().downgrade();
            Rc::new(move |w, cx| {
                let _ = this.update(cx, |this, cx| this.close_find(true, w, cx));
            })
        };
        div().absolute().top_2().right_4().child(find_bar(&f.input, f.count(cx), on_step, on_close))
    });
    // Where a selection must end to be quoted.
    let area = app.transcript_area.clone();
    let measure = canvas(move |bounds, _, _| area.set(Some(bounds)), |_, _, _, _| {}).absolute().size_full();
    let transcript = div().relative().flex_1().min_h_0().flex().flex_col().child(measure).child(transcript).children(find_bar);

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
pub(crate) fn streaming_reply(app: &SpaceApp, thread: &Thread, window: &mut Window, cx: &mut Context<SpaceApp>) -> Option<(usize, usize)> {
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
    let keep_closed = app.saving_threads().then(|| app.keep_closed_input(window, cx));
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
        .keep_closed(keep_closed)
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
        on_send_after_step: handler(cx, |this, w, cx| this.send_after_step(w, cx)),
        on_queued: {
            let this = cx.entity().downgrade();
            Rc::new(move |ix, action, w: &mut Window, cx: &mut App| {
                use space_ui::components::composer::QueuedAction;
                let action = match action {
                    QueuedAction::Edit => crate::queue::PendingAction::Edit,
                    QueuedAction::Remove => crate::queue::PendingAction::Remove,
                    QueuedAction::SendNow => crate::queue::PendingAction::SendNow,
                };
                let _ = this.update(cx, |this, cx| this.pending_action(ix, action, w, cx));
            })
        },
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
        on_open_paste: {
            let this = cx.entity().downgrade();
            Rc::new(move |ix, w: &mut Window, cx: &mut App| {
                let _ = this.update(cx, |this, cx| this.open_draft_paste(ix, w, cx));
            })
        },
        on_remove_paste: {
            let this = cx.entity().downgrade();
            Rc::new(move |ix, _: &mut Window, cx: &mut App| {
                let _ = this.update(cx, |this, cx| this.remove_paste(ix, cx));
            })
        },
        on_remove_quote: handler(cx, |this, _, cx| this.remove_quote(cx)),
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
        on_resume: {
            let this = cx.entity().downgrade();
            Rc::new(move |id, w: &mut Window, cx: &mut App| {
                let _ = this.update(cx, |this, cx| this.resume_thread(&id, w, cx));
            })
        },
    };
    // A new thread with nothing typed offers this folder's closed threads.
    let untouched = thread.is_empty()
        && thread.attachments.is_empty()
        && thread.images.is_empty()
        && thread.pastes.is_empty()
        && thread.quote.is_none()
        && app.input.read(cx).value().trim().is_empty();
    let now = crate::app::now_ms();
    let resumable = if untouched {
        app.closed_threads_in(&thread.cwd)
            .into_iter()
            .map(|m| ClosedThread {
                id: m.id.clone(),
                title: m.title.clone().into(),
                closed: format::time_ago(m.closed_ms.unwrap_or(now), now).into(),
            })
            .collect()
    } else {
        Vec::new()
    };
    let mode = app
        .effective_mode(thread)
        .map(|current| ModeChoice { entries: app.permission_modes.clone(), current });
    let blocker = app.blocker(thread).map(|b| ComposerBlocker {
        times: format::attempt_times(b.last_attempt_ms, b.next_attempt_ms, crate::app::now_ms()).map(Into::into),
        text: b.text.into(),
        actions: b.actions.into_iter().map(|a| (a.label.into(), a.action.into())).collect(),
    });
    let can_steer = thread.session.as_ref().is_some_and(|s| s.agent().can_steer());
    let queued_messages = thread
        .pending
        .iter()
        .map(|p| space_ui::components::composer::QueuedView {
            text: p.message.text.clone().into(),
            after_step: p.when == crate::queue::When::Step,
            extras: p.message.attachments.len() + p.message.images.len() + p.message.pastes.len() + usize::from(p.message.quote.is_some()),
        })
        .collect();
    Composer::new("composer", &app.input)
        .running(thread.state.running)
        .queued(thread.queued.is_some())
        .queued_messages(queued_messages)
        .after_step(app.cmd_held && can_steer && thread.state.running)
        .attachments(thread.attachments.clone())
        .images(thread.images.iter().map(|i| i.pic().map(|p| p.src)).collect())
        .pastes(thread.pastes.iter().map(|p| p.chars).collect())
        .quote(thread.quote.clone().map(Into::into))
        .controls(ComposerControls {
            cwd: thread.cwd.clone(),
            dirs: app.recent_dirs(),
            thread_empty: thread.is_empty(),
            status: {
                // A thread not loaded yet offers what the agent offers now, not what it saved.
                let mut status = thread.state.status.clone();
                if thread.live_session().is_none() {
                    status.available_models = app.agent_models.clone().or(status.available_models);
                    status.thinking_levels = app.agent_levels.clone().or(status.thinking_levels);
                }
                status
            },
            // Without a live session (a saved thread not loaded yet, a sleeping agent) the choice
            // is kept on the thread and applied when its session is loaded. A new thread's first
            // session gets Settings' defaults instead, so it waits for its session.
            locked: thread.state.running
                || thread.creating
                || (thread.live_session().is_none() && thread.is_empty() && !app.agent_asleep),
            mode,
            blocker,
            stats: app.host.has_trajectory().then(|| app.trajectories.stats.get(&thread.id).cloned()).flatten(),
            context_open: app.usage_open.as_deref() == Some(CONTEXT_PANEL),
            menu: app.usage_open.as_deref().and_then(composer_menu),
            resumable,
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
        ComposerMenu::Resume => "composer:resume",
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
        .chain([ComposerMenu::Folder, ComposerMenu::Mode, ComposerMenu::Resume])
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

