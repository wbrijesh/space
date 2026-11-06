//! Composer: input card (attach, text, send/stop) and the control row below it
//! (project folder, model, effort, context usage).

use gpui::component::button::{Button, ButtonVariants as _};
use gpui::component::input::Textarea;
use gpui::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui::component::{Disableable as _, FocusableExt as _, Icon, IconName, Sizable as _};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use space_core::protocol::ConfigureSession;

use super::thread::attachment_chip;
use super::{icon_button, tip};
use crate::app::SpaceApp;
use crate::{format, theme};

pub fn render(app: &SpaceApp, window: &mut Window, cx: &mut Context<SpaceApp>) -> impl IntoElement {
    let thread = app.current();
    let running = thread.is_some_and(|t| t.state.running);
    let queued = thread.is_some_and(|t| t.queued.is_some());
    let focused = app.input.read(cx).focus_handle(cx).is_focused(window);
    let has_text = !app.input.read(cx).value().trim().is_empty();
    let has_attachments = thread.is_some_and(|t| !t.attachments.is_empty());
    let can_send = (has_text || has_attachments) && !running && !queued;

    let mut chips = div().flex().flex_wrap().gap_1p5().px_1();
    if let Some(t) = thread {
        for (ix, p) in t.attachments.iter().enumerate() {
            chips = chips.child(
                attachment_chip(p).pr_0p5().child(
                    div()
                        .id(("remove-attachment", ix))
                        .size(px(18.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()).text_color(theme::text()))
                        .child(Icon::new(IconName::Close).size(px(11.)))
                        .on_click(cx.listener(move |this, _, _, cx| this.remove_attachment(ix, cx))),
                ),
            );
        }
    }

    let send: AnyElement = if running {
        div()
            .id("stop")
            .size(px(BUTTON))
            .flex_none()
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme::text())
            .cursor_pointer()
            .hover(|s| s.opacity(0.85))
            .child(div().size(px(10.)).rounded(px(2.)).bg(theme::bg()))
            .tooltip(tip("Stop"))
            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx)))
            .into_any_element()
    } else {
        div()
            .id("send")
            .size(px(BUTTON))
            .flex_none()
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .map(|d| {
                if can_send {
                    d.bg(theme::text()).text_color(theme::bg()).cursor_pointer().hover(|s| s.opacity(0.85))
                } else {
                    d.bg(theme::pressed()).text_color(theme::text_3())
                }
            })
            .child(Icon::new(IconName::ArrowUp).size(px(16.)))
            .when(can_send, |d| {
                d.tooltip(tip("Send  ⏎")).on_click(cx.listener(|this, _, window, cx| this.send(window, cx)))
            })
            .into_any_element()
    };

    // One row: [+] [text, grows up to 10 lines] [send]. Buttons stay pinned to the bottom
    // as the text grows. 8px padding + 32px buttons = a 50px card for one line.
    // No gap after "+": its icon already has 8px of air, plus the editor's 4px.
    let row = div()
        .flex()
        .items_end()
        .child(
            icon_button("attach", IconName::Plus, true, false)
                .size(px(BUTTON))
                .rounded_full()
                .tooltip(tip("Attach files or folders"))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.focus_composer(window, cx);
                    this.attach_files(cx);
                })),
        )
        .child(
            // Line height 20 + 6px top/bottom = the 32px button height, so a single line
            // sits on the buttons' center line.
            div()
                .flex_1()
                .min_w_0()
                .py(px((BUTTON - LINE) / 2.))
                .child(
                    // XSmall: no vertical editor padding, 4px horizontal.
                    Textarea::new(&app.input).appearance(false).xsmall().text_size(theme::TEXT_BODY),
                ),
        )
        .child(div().flex_none().ml_1p5().child(send));

    let card = div()
        .id("composer-card")
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .p(px(PAD))
        .rounded(px(18.))
        .bg(theme::surface())
        .border_1()
        .border_color(if focused { theme::border_strong() } else { theme::border() })
        .shadow_md()
        .cursor_text()
        // Clicking the card's padding focuses the input, like a real text field.
        .on_click(cx.listener(|this, _, window, cx| this.focus_composer(window, cx)))
        .when(has_attachments, |d| d.child(chips))
        .child(row);

    div().w_full().flex().flex_col().gap_2().child(card).child(controls(app, window, cx))
}

/// Card padding, button size and text line height (logical px).
const PAD: f32 = 8.;
const BUTTON: f32 = 32.;
/// The textarea's line height: 1.25rem.
const LINE: f32 = 20.;
/// Horizontal inset of the control row so its first icon centers under "+" (card border 1 +
/// padding 8 + half the button 16 = 25; minus half the 14px folder icon and the menu
/// button's 8px padding) and the context ring's right edge meets the send button's (1 + 8).
const ROW_INSET_LEFT: f32 = 1. + PAD + BUTTON / 2. - 7. - 8.;
const ROW_INSET_RIGHT: f32 = 1. + PAD;

fn menu_trigger(id: &'static str, window: &Window) -> Button {
    Button::new(id)
        .ghost()
        .small()
        // Menus hand focus back to their trigger on close; only show a ring for keyboard users.
        .focus_ring(window.last_input_was_keyboard())
        .px_2()
        .rounded(px(7.))
        .text_size(theme::TEXT_SM)
}

fn controls(app: &SpaceApp, window: &mut Window, cx: &mut Context<SpaceApp>) -> impl IntoElement {
    let Some(thread) = app.current() else { return div() };
    let weak = cx.entity().downgrade();
    let status = &thread.state.status;
    let locked = thread.state.running || thread.live_session().is_none();

    // Project folder: change it while the thread is empty; afterwards it starts a new thread.
    let dirs = app.recent_dirs();
    let cwd = thread.cwd.clone();
    let empty = thread.is_empty();
    let folder = {
        let weak = weak.clone();
        menu_trigger("project-menu", window)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_color(theme::text_2())
                    .child(Icon::new(IconName::Folder).size(px(14.)))
                    .child(div().max_w(px(220.)).truncate().child(format::dir_name(&thread.cwd)))
                    .child(Icon::new(IconName::ChevronDown).size(px(12.)).text_color(theme::text_3())),
            )
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                let mut menu = menu.min_w(px(260.)).label(if empty { "Project folder" } else { "New thread in…" });
                for dir in &dirs {
                    let w = weak.clone();
                    let d = dir.clone();
                    menu = menu.item(
                        PopupMenuItem::new(format::abbreviate_home(dir))
                            .icon(IconName::Folder)
                            .checked(empty && *dir == cwd)
                            .on_click(move |_, window, cx| {
                                let d = d.clone();
                                let _ = w.update(cx, |this, cx| this.set_thread_dir(d, window, cx));
                            }),
                    );
                }
                let w = weak.clone();
                menu.separator().item(PopupMenuItem::new("Choose folder…").icon(IconName::FolderOpen).on_click(
                    move |_, window, cx| {
                        let _ = w.update(cx, |this, cx| this.choose_folder(window, cx));
                    },
                ))
            })
    };

    let mut right = div().flex().items_center().gap_0p5();

    if let Some(model) = &status.model {
        let label = format::model_label(model, status.current_model());
        let models = status.available_models.clone().unwrap_or_default();
        let current = model.clone();
        let w = weak.clone();
        let trigger = menu_trigger("model-menu", window)
            .disabled(locked || models.is_empty())
            .child(div().text_color(theme::text_2()).child(label));
        right = right.child(if locked || models.is_empty() {
            trigger.into_any_element()
        } else {
            trigger
                .dropdown_menu_with_anchor(Anchor::BottomRight, move |menu, _, _| {
                    let mut menu = menu.min_w(px(200.)).max_h(px(420.)).scrollable(true).label("Model");
                    for m in &models {
                        let w = w.clone();
                        let (provider, id) = (m.provider.clone(), m.id.clone());
                        menu = menu.item(
                            PopupMenuItem::new(format::model_label(&m.id, Some(m))).checked(m.id == current).on_click(
                                move |_, _, cx| {
                                    let request = ConfigureSession {
                                        provider: Some(provider.clone()),
                                        model: Some(id.clone()),
                                        ..Default::default()
                                    };
                                    let _ = w.update(cx, |this, cx| this.configure(request, cx));
                                },
                            ),
                        );
                    }
                    menu
                })
                .into_any_element()
        });
    }

    if let Some(level) = &status.thinking_level {
        let levels = status.thinking_levels.clone().unwrap_or_default();
        let current = level.clone();
        let w = weak.clone();
        let trigger = menu_trigger("effort-menu", window)
            .disabled(locked || levels.is_empty())
            .child(div().text_color(theme::text_3()).child(format::effort_label(level)));
        right = right.child(if locked || levels.is_empty() {
            trigger.into_any_element()
        } else {
            trigger
                .dropdown_menu_with_anchor(Anchor::BottomRight, move |menu, _, _| {
                    let mut menu = menu.min_w(px(160.)).label("Thinking effort");
                    for l in &levels {
                        let w = w.clone();
                        let level = l.clone();
                        menu = menu.item(PopupMenuItem::new(format::effort_label(l)).checked(*l == current).on_click(
                            move |_, _, cx| {
                                let request = ConfigureSession { thinking_level: Some(level.clone()), ..Default::default() };
                                let _ = w.update(cx, |this, cx| this.configure(request, cx));
                            },
                        ));
                    }
                    menu
                })
                .into_any_element()
        });
    }

    // Context usage: always shown; exact numbers on hover.
    let tokens = status.context_tokens.unwrap_or(0);
    let tooltip = match status.context_window.filter(|w| *w > 0) {
        Some(window_size) => {
            let pct = tokens as f64 / window_size as f64 * 100.;
            let pct = if tokens > 0 && pct < 1. { "<1%".to_string() } else { format!("{pct:.0}%") };
            format!(
                "Context: {} / {} tokens · {pct}",
                format::compact_tokens(tokens),
                format::compact_tokens(window_size)
            )
        }
        None => "Context usage not reported yet".to_string(),
    };
    let frac = status.context_window.filter(|w| *w > 0).map_or(0., |w| tokens as f32 / w as f32);
    right = right.child(div().id("context-ring").ml_2().child(context_ring(frac)).tooltip(tip(tooltip)));

    div()
        .flex()
        .items_center()
        .justify_between()
        .pl(px(ROW_INSET_LEFT))
        .pr(px(ROW_INSET_RIGHT))
        .child(folder)
        .child(right)
}

/// 16px ring: muted track, accent arc from 12 o'clock clockwise for `frac` of the circle.
fn context_ring(frac: f32) -> impl IntoElement {
    const SIZE: f32 = 16.;
    const STROKE: f32 = 2.;
    let frac = frac.clamp(0., 1.);
    // Any usage at all gets a visible sliver.
    let sweep = if frac > 0. { frac.max(0.04) } else { 0. };
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let center = bounds.center();
            let r = (SIZE - STROKE) / 2.;
            let arc = |from: f32, to: f32| {
                let steps = ((to - from) * 64.).ceil().max(2.) as usize;
                let mut path = PathBuilder::stroke(px(STROKE));
                for i in 0..=steps {
                    let t = from + (to - from) * i as f32 / steps as f32;
                    let a = -std::f32::consts::FRAC_PI_2 + t * std::f32::consts::TAU;
                    let p = point(center.x + px(r * a.cos()), center.y + px(r * a.sin()));
                    if i == 0 { path.move_to(p) } else { path.line_to(p) }
                }
                path.build().ok()
            };
            if let Some(track) = arc(0., 1.) {
                window.paint_path(track, theme::border_strong());
            }
            if sweep > 0.
                && let Some(progress) = arc(0., sweep)
            {
                window.paint_path(progress, theme::accent());
            }
        },
    )
    .size(px(SIZE))
}
