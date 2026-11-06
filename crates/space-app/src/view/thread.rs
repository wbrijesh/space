//! Thread body: transcript (or the empty state) above the composer, and the logs panel.

use gpui::component::spinner::Spinner;
use gpui::component::text::TextView;
use gpui::component::{Icon, IconName, Sizable as _};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use space_core::{PermissionDecision, PermissionRequest, PluginStatus, ToolCallInfo, ToolStatus, TranscriptItem};

use super::{composer, icon_button, status_dot, tip};
use crate::app::{SpaceApp, Thread};
use crate::{format, theme};
use gpui::assets::IconName as Lucide;

pub fn render_body(app: &SpaceApp, window: &mut Window, cx: &mut Context<SpaceApp>) -> AnyElement {
    let Some(thread) = app.current() else {
        return div().flex_1().into_any_element();
    };
    let banner = app.banner.clone().map(|b| banner(b, cx));

    if thread.is_empty() {
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
            .child(
                div()
                    .w_full()
                    .max_w(theme::CONTENT_MAX_W)
                    .flex()
                    .flex_col()
                    .gap_6()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(26.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("What should we work on?"),
                            )
                            .child(
                                div()
                                    .text_size(theme::TEXT_MD)
                                    .text_color(theme::text_3())
                                    .child(format!("in {}", format::dir_name(&thread.cwd))),
                            ),
                    )
                    .children(agent_problem(app, thread, cx))
                    .children(banner)
                    .child(composer::render(app, window, cx)),
            )
            .into_any_element();
    }

    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .child(
            div()
                .id("transcript")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&app.transcript_scroll)
                .child(
                    div().w_full().flex().justify_center().px_6().child(
                        div()
                            .w_full()
                            .max_w(theme::CONTENT_MAX_W)
                            .pt_6()
                            .pb_8()
                            .flex()
                            .flex_col()
                            .children(transcript(app, thread, cx)),
                    ),
                ),
        )
        .child(
            div().flex_none().w_full().flex().justify_center().px_6().pb_4().child(
                div()
                    .w_full()
                    .max_w(theme::CONTENT_MAX_W)
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(agent_problem(app, thread, cx))
                    .children(banner)
                    .child(composer::render(app, window, cx)),
            ),
        )
        .into_any_element()
}

/// The agent could not start / exited: say so, with restart and logs.
fn agent_problem(app: &SpaceApp, thread: &Thread, cx: &mut Context<SpaceApp>) -> Option<AnyElement> {
    let message = match (&app.agent_status, &thread.error) {
        (PluginStatus::Crashed(msg), _) => format!("The agent exited: {}", format::one_line(msg)),
        (_, Some(err)) => format!("Couldn't start a session: {}", format::one_line(err)),
        (_, None) if app.agent_id.is_none() => "No agent plugin is installed.".to_string(),
        _ => return None,
    };
    Some(
        div()
            .flex()
            .items_start()
            .gap_2p5()
            .p_3()
            .rounded(px(10.))
            .bg(theme::red_bg())
            .border_1()
            .border_color(theme::red_border())
            .child(Icon::new(IconName::CircleAlert).size(px(16.)).text_color(theme::red()).mt(px(1.)))
            .child(div().flex_1().min_w_0().text_size(theme::TEXT_SM).line_clamp(4).child(message))
            .when(app.agent_id.is_some(), |d| {
                d.child(pill_button("problem-restart", "Restart", false).on_click(cx.listener(|this, _, _, cx| this.restart_agent(cx))))
                    .child(pill_button("problem-logs", "Logs", false).on_click(cx.listener(|this, _, _, cx| {
                        this.show_logs = true;
                        cx.notify();
                    })))
            })
            .into_any_element(),
    )
}

fn banner(text: String, cx: &mut Context<SpaceApp>) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_2()
        .rounded(px(10.))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
        .text_size(theme::TEXT_SM)
        .text_color(theme::text_2())
        .child(Icon::new(IconName::Info).size(px(14.)).text_color(theme::text_3()))
        .child(div().flex_1().min_w_0().child(text))
        .child(
            icon_button("dismiss-banner", IconName::Close, true, false).on_click(cx.listener(|this, _, _, cx| {
                this.banner = None;
                cx.notify();
            })),
        )
        .into_any_element()
}

fn transcript(app: &SpaceApp, thread: &Thread, cx: &mut Context<SpaceApp>) -> Vec<AnyElement> {
    let items = &thread.state.items;
    let mut out = Vec::new();
    let mut prev: Option<&TranscriptItem> = None;
    for (ix, item) in items.iter().enumerate() {
        if matches!(item, TranscriptItem::TurnEnd(r) if r == "error" || r == "end_turn") {
            continue;
        }
        // Tool calls and thoughts stack tightly; everything else gets room to breathe.
        let compact = |i: &TranscriptItem| matches!(i, TranscriptItem::Tool(_) | TranscriptItem::Thought(_));
        let gap = match prev {
            None => px(0.),
            Some(p) if compact(p) && compact(item) => px(2.),
            _ => px(18.),
        };
        let last = ix + 1 == items.len();
        out.push(div().mt(gap).child(render_item(app, thread, ix, item, last, cx)).into_any_element());
        prev = Some(item);
    }
    if let Some((text, attachments)) = &thread.queued {
        out.push(div().mt(px(18.)).child(user_bubble(text, attachments)).into_any_element());
        out.push(div().mt(px(18.)).child(working_row("Starting agent…")).into_any_element());
    } else if thread.state.running && !matches!(items.last(), Some(TranscriptItem::Agent(_) | TranscriptItem::Thought(_))) {
        let waiting = thread.state.pending_permission().is_some();
        if !waiting {
            out.push(div().mt(px(14.)).child(working_row("Working…")).into_any_element());
        }
    }
    out
}

fn working_row(label: &str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .h(px(28.))
        .px_2()
        .text_size(theme::TEXT_SM)
        .text_color(theme::text_3())
        .child(Spinner::new().xsmall().color(theme::text_3()))
        .child(label.to_string())
}

fn render_item(
    app: &SpaceApp,
    thread: &Thread,
    ix: usize,
    item: &TranscriptItem,
    last: bool,
    cx: &mut Context<SpaceApp>,
) -> AnyElement {
    let _ = app;
    match item {
        TranscriptItem::User { text, attachments } => user_bubble(text, attachments).into_any_element(),
        TranscriptItem::Agent(text) => div()
            .text_size(theme::TEXT_BODY)
            .line_height(relative(1.6))
            .text_color(theme::text())
            .child(TextView::markdown(SharedString::from(format!("md-{}-{ix}", thread.key)), text.trim().to_string()).selectable(true))
            .into_any_element(),
        TranscriptItem::Thought(text) => {
            let open = thread.expanded.contains(&ix);
            let label = if last && thread.state.running {
                "Thinking…".to_string()
            } else {
                match thread.thought_secs.get(&ix) {
                    Some(0) | None => "Thought briefly".to_string(),
                    Some(1) => "Thought for 1s".to_string(),
                    Some(n) => format!("Thought for {n}s"),
                }
            };
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .id(("thought", ix))
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .h(px(28.))
                        .px_2()
                        .rounded(px(8.))
                        .text_size(theme::TEXT_SM)
                        .text_color(theme::text_3())
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme::text_2()).bg(theme::surface()))
                        .child(Icon::new(Lucide::Brain).size(px(14.)))
                        .child(label)
                        .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).size(px(12.)))
                        .on_click(cx.listener(move |this, _, _, cx| this.toggle_expanded(ix, cx))),
                )
                .when(open, |d| {
                    d.child(
                        div()
                            .ml(px(15.))
                            .mt_1()
                            .mb_1()
                            .pl_3()
                            .border_l_1()
                            .border_color(theme::border_strong())
                            .text_size(theme::TEXT_SM)
                            .line_height(relative(1.5))
                            .text_color(theme::text_3())
                            .child(text.trim().to_string()),
                    )
                })
                .into_any_element()
        }
        TranscriptItem::Tool(call) => tool_row(ix, call, thread.expanded.contains(&ix), cx).into_any_element(),
        TranscriptItem::Permission { request, resolved } => permission_card(request, resolved.as_ref(), cx),
        TranscriptItem::Plan(entries) => {
            let mut card = div()
                .flex()
                .flex_col()
                .gap_1p5()
                .p_4()
                .rounded(px(12.))
                .bg(theme::surface())
                .border_1()
                .border_color(theme::border())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(theme::TEXT_SM)
                        .text_color(theme::text_2())
                        .child(Icon::new(Lucide::ListChecks).size(px(14.)))
                        .child("Plan"),
                );
            for e in entries {
                let (icon, color, text_color): (Icon, Hsla, Hsla) = match e.status.as_str() {
                    "completed" => (Icon::new(IconName::Check), theme::green(), theme::text_3()),
                    "in_progress" => (Icon::new(IconName::ArrowRight), theme::accent_hover(), theme::text()),
                    _ => (Icon::new(IconName::Dash), theme::text_3(), theme::text_2()),
                };
                card = card.child(
                    div()
                        .flex()
                        .items_start()
                        .gap_2()
                        .text_size(theme::TEXT_MD)
                        .child(icon.size(px(14.)).text_color(color).mt(px(2.)))
                        .child(div().flex_1().text_color(text_color).child(e.content.clone())),
                );
            }
            card.into_any_element()
        }
        TranscriptItem::Error(msg) => div()
            .flex()
            .items_start()
            .gap_2p5()
            .p_3()
            .rounded(px(10.))
            .bg(theme::red_bg())
            .border_1()
            .border_color(theme::red_border())
            .child(Icon::new(IconName::CircleAlert).size(px(16.)).text_color(theme::red()).mt(px(1.)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(theme::TEXT_SM)
                    .line_height(relative(1.5))
                    .text_color(theme::text())
                    .child(format::clean_error(msg)),
            )
            .into_any_element(),
        TranscriptItem::TurnEnd(reason) => {
            let text = match reason.as_str() {
                "cancelled" => "Stopped".to_string(),
                "max_tokens" | "max_turn_requests" => "Reply stopped at the output limit".to_string(),
                "refusal" => "The model declined to continue".to_string(),
                "new-session" => "The agent restarted · continuing in a new session without the earlier context".to_string(),
                other => format!("Turn ended: {other}"),
            };
            div()
                .flex()
                .items_center()
                .gap_3()
                .text_size(theme::TEXT_XS)
                .text_color(theme::text_3())
                .child(div().flex_1().h(px(1.)).bg(theme::border()))
                .child(text)
                .child(div().flex_1().h(px(1.)).bg(theme::border()))
                .into_any_element()
        }
    }
}

fn user_bubble(text: &str, attachments: &[std::path::PathBuf]) -> impl IntoElement {
    let mut bubble = div()
        .max_w(relative(0.82))
        .flex()
        .flex_col()
        .gap_2()
        .px_4()
        .py_2p5()
        .rounded(px(18.))
        .bg(theme::surface_2())
        .text_size(theme::TEXT_BODY)
        .line_height(relative(1.5));
    if !attachments.is_empty() {
        let mut chips = div().flex().flex_wrap().gap_1p5();
        for p in attachments {
            chips = chips.child(attachment_chip(p));
        }
        bubble = bubble.child(chips);
    }
    if !text.is_empty() {
        bubble = bubble.child(text.trim().to_string());
    }
    div().flex().justify_end().child(bubble)
}

pub(crate) fn attachment_chip(path: &std::path::Path) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .h(px(24.))
        .px_2()
        .rounded(px(6.))
        .bg(theme::pressed())
        .font_family(theme::MONO_FONT)
        .text_size(theme::TEXT_XS)
        .text_color(theme::text_2())
        .child(Icon::new(if path.is_dir() { IconName::Folder } else { IconName::File }).size(px(12.)))
        .child(format::dir_name(path))
}

fn tool_icon(kind: &str) -> Icon {
    match kind {
        "execute" => Icon::new(Lucide::Terminal),
        "read" => Icon::new(IconName::FileText),
        "edit" | "delete" | "move" => Icon::new(Lucide::Pencil),
        "search" => Icon::new(IconName::Search),
        "fetch" => Icon::new(IconName::Globe),
        "think" => Icon::new(Lucide::Brain),
        _ => Icon::new(Lucide::Wrench),
    }
}

fn tool_row(ix: usize, call: &ToolCallInfo, open: bool, cx: &mut Context<SpaceApp>) -> impl IntoElement {
    let title = if call.title.is_empty() { call.id.clone() } else { call.title.clone() };
    let (name, detail) = format::tool_parts(&title);
    let has_body = !call.output.trim().is_empty() || !call.locations.is_empty();
    let status: AnyElement = match call.status {
        ToolStatus::InProgress => Spinner::new().xsmall().color(theme::text_3()).into_any_element(),
        ToolStatus::Pending => status_dot(theme::text_3()).into_any_element(),
        ToolStatus::Completed => status_dot(theme::green()).into_any_element(),
        ToolStatus::Failed => div()
            .flex()
            .items_center()
            .gap_1p5()
            .text_size(theme::TEXT_XS)
            .text_color(theme::red())
            .child("Failed")
            .child(status_dot(theme::red()))
            .into_any_element(),
    };

    let header = div()
        .id(("tool", ix))
        .flex()
        .items_center()
        .gap_2()
        .h(px(30.))
        .px_2()
        .rounded(px(8.))
        .when(has_body, |d| d.cursor_pointer().hover(|s| s.bg(theme::surface())))
        .child(tool_icon(&call.kind).size(px(14.)).text_color(theme::text_3()))
        .child(div().flex_none().text_size(theme::TEXT_SM).font_weight(FontWeight::MEDIUM).text_color(theme::text_2()).child(name))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family(theme::MONO_FONT)
                .text_size(px(12.))
                .text_color(theme::text_3())
                .child(detail),
        )
        .child(status)
        .when(has_body, |d| {
            d.child(
                Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                    .size(px(12.))
                    .text_color(theme::text_3()),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_expanded(ix, cx)))
        });

    let body = open.then(|| {
        const MAX_LINES: usize = 40;
        let lines: Vec<&str> = call.output.lines().collect();
        let shown = lines.iter().take(MAX_LINES).copied().collect::<Vec<_>>().join("\n");
        let more = lines.len().saturating_sub(MAX_LINES);
        div()
            .ml(px(30.))
            .mt_1()
            .mb_1()
            .p_3()
            .rounded(px(8.))
            .bg(theme::bg_sidebar())
            .border_1()
            .border_color(theme::border())
            .font_family(theme::MONO_FONT)
            .text_size(px(12.))
            .line_height(relative(1.5))
            .text_color(theme::text_2())
            .flex()
            .flex_col()
            .gap_1()
            .when(!call.locations.is_empty(), |d| d.child(div().text_color(theme::text_3()).child(call.locations.join("\n"))))
            .when(!shown.is_empty(), |d| d.child(shown))
            .when(more > 0, |d| d.child(div().text_color(theme::text_3()).child(format!("… {more} more lines"))))
    });

    div().flex().flex_col().child(header).children(body)
}

fn permission_card(request: &PermissionRequest, resolved: Option<&PermissionDecision>, cx: &mut Context<SpaceApp>) -> AnyElement {
    let title = request.tool_call.title.clone().unwrap_or_else(|| request.tool_call.id.clone());
    let (name, detail) = format::tool_parts(&title);
    if let Some(decision) = resolved {
        let (icon, text) = match decision {
            PermissionDecision::Selected(id) => {
                let opt = request.options.iter().find(|o| &o.id == id);
                let allow = opt.is_some_and(|o| o.kind.is_allow());
                let label = opt.map_or(id.as_str(), |o| o.name.as_str());
                (if allow { IconName::Check } else { IconName::Close }, format!("{label} · {name} {detail}"))
            }
            PermissionDecision::Cancelled => (IconName::Close, format!("Permission request cancelled · {name} {detail}")),
        };
        return div()
            .flex()
            .items_center()
            .gap_2()
            .h(px(28.))
            .px_2()
            .text_size(theme::TEXT_SM)
            .text_color(theme::text_3())
            .child(Icon::new(icon).size(px(14.)))
            .child(div().min_w_0().truncate().child(text))
            .into_any_element();
    }

    let mut buttons = div().flex().gap_2();
    for (n, opt) in request.options.iter().enumerate() {
        let rid = request.request_id;
        let oid = opt.id.clone();
        buttons = buttons.child(
            pill_button(SharedString::from(format!("perm-{rid}-{n}")), opt.name.clone(), opt.kind.is_allow()).on_click(
                cx.listener(move |this, _, _, cx| this.answer_permission(rid, PermissionDecision::Selected(oid.clone()), cx)),
            ),
        );
    }
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .rounded(px(12.))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::yellow_border())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(theme::TEXT_SM)
                .text_color(theme::yellow())
                .child(Icon::new(IconName::TriangleAlert).size(px(14.)))
                .child("Permission needed"),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().text_size(theme::TEXT_MD).font_weight(FontWeight::MEDIUM).child(name))
                .child(div().min_w_0().truncate().font_family(theme::MONO_FONT).text_size(px(12.5)).text_color(theme::text_2()).child(detail)),
        )
        .child(buttons)
        .into_any_element()
}

/// 28px rounded text button: filled accent (`primary`) or quiet outline.
pub(crate) fn pill_button(id: impl Into<ElementId>, label: impl Into<SharedString>, primary: bool) -> Stateful<Div> {
    div()
        .id(id.into())
        .flex_none()
        .h(px(28.))
        .px_3()
        .flex()
        .items_center()
        .rounded(px(8.))
        .text_size(theme::TEXT_SM)
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .map(|d| {
            if primary {
                d.bg(theme::accent()).text_color(theme::on_accent()).hover(|s| s.bg(theme::accent_hover()))
            } else {
                d.border_1()
                    .border_color(theme::border_strong())
                    .text_color(theme::text())
                    .hover(|s| s.bg(theme::hover()))
            }
        })
        .active(|s| s.opacity(0.85))
        .child(label.into())
}

pub fn render_logs(app: &SpaceApp, cx: &mut Context<SpaceApp>) -> impl IntoElement {
    let mut col = div().flex().flex_col().px_4().py_2().font_family(theme::MONO_FONT).text_size(px(11.5)).line_height(relative(1.5));
    if app.logs.is_empty() {
        col = col.child(div().text_color(theme::text_3()).child("No output yet."));
    }
    for line in app.logs.iter().skip(app.logs.len().saturating_sub(500)) {
        col = col.child(div().text_color(theme::text_2()).child(line.clone()));
    }
    div()
        .flex_none()
        .flex()
        .flex_col()
        .h(px(220.))
        .border_t_1()
        .border_color(theme::border())
        .bg(theme::bg_sidebar())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .h(px(36.))
                .pl_4()
                .pr_2()
                .border_b_1()
                .border_color(theme::border())
                .text_size(theme::TEXT_SM)
                .text_color(theme::text_2())
                .child(Icon::new(Lucide::Terminal).size(px(14.)).text_color(theme::text_3()))
                .child(format!("{} · stderr", app.agent_id.clone().unwrap_or_else(|| "agent".into())))
                .child(div().flex_1())
                .child(
                    icon_button("close-logs", IconName::Close, true, false)
                        .tooltip(tip("Close logs"))
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_logs(cx))),
                ),
        )
        .child(div().id("logs").flex_1().min_h_0().overflow_y_scroll().track_scroll(&app.log_scroll).child(col))
}
