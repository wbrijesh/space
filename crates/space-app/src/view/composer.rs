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
            .size(px(30.))
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
            .size(px(30.))
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

    let card = div()
        .id("composer-card")
        .w_full()
        .flex()
        .flex_col()
        .gap_1()
        .px_2()
        .pt_2p5()
        .pb_2()
        .rounded(px(16.))
        .bg(theme::surface())
        .border_1()
        .border_color(if focused { theme::border_strong() } else { theme::border() })
        .shadow_md()
        .cursor_text()
        // Clicking the card's padding focuses the input, like a real text field.
        .on_click(cx.listener(|this, _, window, cx| this.focus_composer(window, cx)))
        .when(has_attachments, |d| d.child(chips))
        .child(
            div()
                .px_1p5()
                .text_size(theme::TEXT_BODY)
                .line_height(relative(1.5))
                .child(Textarea::new(&app.input).appearance(false)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    icon_button("attach", IconName::Plus, true, false)
                        .rounded_full()
                        .tooltip(tip("Attach files or folders"))
                        .on_click(cx.listener(|this, _, _, cx| this.attach_files(cx))),
                )
                .child(send),
        );

    div().w_full().flex().flex_col().gap_1p5().child(card).child(controls(app, window, cx))
}

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

    if let (Some(tokens), Some(window_size)) = (status.context_tokens, status.context_window)
        && window_size > 0
        && tokens > 0
    {
        let frac = (tokens as f32 / window_size as f32).clamp(0., 1.);
        let pct = (frac * 100.).round() as u32;
        let color = if frac > 0.85 {
            theme::red()
        } else if frac > 0.6 {
            theme::yellow()
        } else {
            theme::text_3()
        };
        right = right.child(
            div()
                .id("context-meter")
                .flex()
                .items_center()
                .gap_1p5()
                .h(px(24.))
                .px_2()
                .rounded(px(7.))
                .hover(|s| s.bg(theme::hover()))
                .child(
                    div()
                        .w(px(28.))
                        .h(px(4.))
                        .rounded_full()
                        .bg(theme::pressed())
                        .child(div().h_full().rounded_full().bg(color).w(px((28. * frac).max(2.)))),
                )
                .child(div().font_family(theme::MONO_FONT).text_size(theme::TEXT_XS).text_color(theme::text_3()).child(if pct == 0 { "<1%".to_string() } else { format!("{pct}%") }))
                .tooltip(tip(format!(
                    "Context: {} of {} tokens",
                    format::compact_tokens(tokens),
                    format::compact_tokens(window_size)
                ))),
        );
    }

    div().flex().items_center().justify_between().px_0p5().child(folder).child(right)
}
