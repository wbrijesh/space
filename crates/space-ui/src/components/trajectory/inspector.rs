//! The inspector: everything about the selected row or request, in tabs by kind.


use gpui::assets::IconName as Lucide;
use gpui::component::text::TextView;
use gpui::component::{Icon, IconName};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use space_core::protocol::trace::{
    ChunkRun, GuardVerdict, HeaderDetail, RecordKind, RecordStatus, RequestSummary, TrajectoryDetail, TrajectoryRow,
};

use super::json_tree::json_tree;
use super::ledger::kind_chip;
use super::{Handler, IdHandler, duration_label, full_time};
use crate::components::images::{OpenPicsHandler, Pic, message_images};
use crate::components::loaders::Spinner;
use crate::components::message_actions::usage_panel;
use crate::components::tip::TipExt as _;
use crate::components::widgets::icon_button;
use crate::format;
use crate::theme::{color, radius, size, text};

/// The inspector's width.
pub const INSPECTOR_W: Pixels = px(440.);
/// The note on every system prompt: what Space can't see.
pub const CLI_NOTE: &str = "The Claude CLI adds a small wrapper Space can't see.";

pub(super) struct InspectorData<'a> {
    pub selected: SharedString,
    pub row: Option<&'a TrajectoryRow>,
    pub request: Option<&'a RequestSummary>,
    pub detail: Option<&'a TrajectoryDetail>,
    pub now_ms: u64,
}

pub(super) struct InspectorHandlers {
    pub on_close: Handler,
    pub on_copy: IdHandler,
    pub on_select: IdHandler,
    pub on_open_image: OpenPicsHandler,
}

/// What the inspector shows: a request, or a row of some kind.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Subject {
    Request,
    Row(RecordKind),
}

fn tabs_for(subject: Subject, detail: Option<&TrajectoryDetail>) -> Vec<&'static str> {
    match subject {
        Subject::Request => vec!["Summary", "Options", "Usage", "Timing"],
        Subject::Row(RecordKind::System) => {
            let mut t = vec!["System Prompt", "Tools"];
            if detail.is_some_and(|d| d.previous_header.is_some()) {
                t.push("Diff");
            }
            t
        }
        Subject::Row(RecordKind::Assistant) => vec!["Summary", "Preview", "Raw"],
        Subject::Row(RecordKind::Tool) => vec!["Summary", "Input", "Output", "Timing", "Guard"],
        Subject::Row(RecordKind::User | RecordKind::Context) => vec!["Summary", "Raw"],
    }
}

pub(super) fn inspector(data: InspectorData<'_>, h: InspectorHandlers, window: &mut Window, cx: &mut App) -> Div {
    let subject = if data.selected.starts_with("request:") {
        Subject::Request
    } else {
        Subject::Row(data.row.map_or(RecordKind::Assistant, |r| r.kind))
    };
    let tabs = tabs_for(subject, data.detail);
    // The tab stays when moving between rows of the same kind.
    let tab_key = match subject {
        Subject::Request => "request".to_string(),
        Subject::Row(k) => format!("{k:?}"),
    };
    let tab_state = window.use_keyed_state(ElementId::Name(format!("inspector-tab-{tab_key}").into()), cx, |_, _| 0usize);
    let tab = (*tab_state.read(cx)).min(tabs.len().saturating_sub(1));

    let title = match (subject, data.row, data.request) {
        (Subject::Request, _, Some(r)) => format!("Request #{} · Turn {} · Step {}", r.number, r.turn, r.step),
        (_, Some(r), _) => match (r.turn, r.step) {
            (0, _) => r.label.clone().unwrap_or_else(|| "Before the first turn".into()),
            (t, Some(s)) => format!("Turn {t} · Step {s}"),
            (t, None) => format!("Turn {t}"),
        },
        _ => String::new(),
    };
    let chip = match subject {
        Subject::Request => div()
            .flex_none()
            .h(px(18.))
            .px_1p5()
            .flex()
            .items_center()
            .rounded(radius::XS)
            .bg(color::pressed())
            .text_size(px(10.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(color::text_2())
            .child("REQUEST"),
        Subject::Row(k) => kind_chip(k),
    };
    let on_close = h.on_close.clone();
    let header = div()
        .flex_none()
        .h(px(44.))
        .px_3()
        .flex()
        .items_center()
        .gap_2()
        .border_b_1()
        .border_color(color::border())
        .child(chip)
        .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().font_family(text::MONO_FONT).text_size(text::XS).text_color(color::text_2()).child(title))
        .child(icon_button("inspector-close", IconName::Close, true, false).on_click(move |_, w, cx| on_close(w, cx)));

    let tab_bar = div()
        .flex_none()
        .h(px(36.))
        .px_3()
        .flex()
        .items_end()
        .gap_4()
        .border_b_1()
        .border_color(color::border())
        .children(tabs.iter().enumerate().map(|(i, name)| {
            let st = tab_state.clone();
            div()
                .id(("inspector-tab", i))
                .h_full()
                .flex()
                .items_center()
                .text_size(text::SM)
                .cursor_pointer()
                .border_b_2()
                .map(|d| if i == tab { d.border_color(color::accent()).text_color(color::text()) } else { d.border_color(transparent_black()).text_color(color::text_2()).hover(|s| s.text_color(color::text())) })
                .child(*name)
                .on_click(move |_, window, cx| {
                    st.update(cx, |t, _| *t = i);
                    window.refresh();
                })
        }));

    let body: AnyElement = match data.detail {
        None => div().p_6().flex().items_center().justify_center().gap_2().text_color(color::text_3()).child(Spinner::new(size::SPINNER_SM)).child("Loading…").into_any_element(),
        Some(detail) => tab_body(subject, tabs[tab], &data, detail, &h, window, cx),
    };

    div()
        .w(INSPECTOR_W)
        .flex_none()
        .h_full()
        .flex()
        .flex_col()
        .bg(color::trajectory_bg())
        .border_l_1()
        .border_color(color::border())
        .child(header)
        .child(tab_bar)
        .child(div().id("inspector-body").flex_1().min_h_0().overflow_y_scroll().child(body))
}

/// A tab's content with its copy button.
fn tab_body(subject: Subject, tab: &str, data: &InspectorData<'_>, d: &TrajectoryDetail, h: &InspectorHandlers, window: &mut Window, cx: &mut App) -> AnyElement {
    let id = data.selected.clone();
    let row = data.row;
    let request = d.request.as_ref().map(|r| &r.summary).or(data.request);
    let raw_json = || serde_json::to_string_pretty(&d.raw).unwrap_or_default();
    let (content, copy): (AnyElement, Option<String>) = match (subject, tab) {
        // ----- system
        (_, "System Prompt") => {
            let prompt = d.header.as_ref().map(|h| h.system_prompt.clone()).unwrap_or_default();
            (
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(note(CLI_NOTE))
                    .child(markdown(format!("{id}-prompt"), &prompt))
                    .into_any_element(),
                Some(prompt),
            )
        }
        (_, "Tools") => {
            let header = d.header.clone().unwrap_or_default();
            let json = serde_json::to_string_pretty(&header.tools).unwrap_or_default();
            (tools_list(&id, &header, window, cx).into_any_element(), Some(json))
        }
        (_, "Diff") => {
            let text = diff_text(d.previous_header.as_ref(), d.header.as_ref());
            (diff_view(&text).into_any_element(), Some(text))
        }
        // ----- request
        (Subject::Request, "Summary") => (request_summary(request, d.request.as_ref().and_then(|r| r.stop_reason.clone())).into_any_element(), None),
        (Subject::Request, "Options") => {
            let opts = d.request.as_ref().map(|r| (r.header_id, r.options.clone()));
            let json = opts.as_ref().map(|(_, o)| serde_json::to_string_pretty(o).unwrap_or_default());
            (options_view(opts, h, window, cx).into_any_element(), json)
        }
        (Subject::Request, "Usage") => match request.and_then(|r| r.usage.clone()) {
            Some(u) => (div().flex().child(usage_panel(&u)).into_any_element(), None),
            None => (note("The agent didn't report usage for this request.").into_any_element(), None),
        },
        (_, "Timing") if subject == Subject::Request => (
            timing_view(&id, request, d.request.as_ref().map(|r| r.chunks.as_slice()).unwrap_or_default(), data.now_ms, window, cx).into_any_element(),
            None,
        ),
        // ----- assistant
        (Subject::Row(RecordKind::Assistant), "Summary") => (
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(source_fields(row, request, h))
                .child(section("Preview", markdown(format!("{id}-sum"), &format::truncate_chars(d.text.as_deref().unwrap_or(""), 600)).into_any_element()))
                .children(d.thinking.clone().filter(|t| !t.is_empty()).map(|t| thinking(&id, &t, window, cx)))
                .child(section("Request Timing", timing_view(&id, request, d.request.as_ref().map(|r| r.chunks.as_slice()).unwrap_or_default(), data.now_ms, window, cx).into_any_element()))
                .into_any_element(),
            d.text.clone(),
        ),
        (Subject::Row(RecordKind::Assistant), "Preview") => (
            div()
                .flex()
                .flex_col()
                .gap_4()
                .children(d.thinking.clone().filter(|t| !t.is_empty()).map(|t| thinking(&id, &t, window, cx)))
                .child(markdown(format!("{id}-preview"), d.text.as_deref().unwrap_or("")))
                .into_any_element(),
            d.text.clone(),
        ),
        // ----- tool
        (Subject::Row(RecordKind::Tool), "Summary") => {
            let r = row.cloned().unwrap_or_default();
            let mut f = fields();
            f = f.child(field("Tool", r.label.clone().unwrap_or_default()));
            f = f.child(field_el("Status", status_text(r.status)));
            if let Some(ms) = r.duration_ms {
                f = f.child(field("Duration", duration_label(ms)));
            }
            if let Some(g) = &d.guard {
                f = f.child(field("Guard", guard_line(g.verdict, g.reason.as_deref())));
            }
            (
                div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(source_fields(row, request, h))
                    .child(f)
                    .children(d.output.clone().map(|o| section("Output", mono(&format::truncate_chars(&o, 800)).into_any_element())))
                    .into_any_element(),
                d.output.clone(),
            )
        }
        (Subject::Row(RecordKind::Tool), "Input") => match &d.input {
            Some(v) => (json_tree(format!("{id}-input"), v, window, cx).into_any_element(), serde_json::to_string_pretty(v).ok()),
            None => (note("No input recorded.").into_any_element(), None),
        },
        (Subject::Row(RecordKind::Tool), "Output") => {
            let out = d.output.clone().unwrap_or_default();
            let parsed = serde_json::from_str::<serde_json::Value>(&out).ok().filter(|v| v.is_object() || v.is_array());
            let el = match &parsed {
                Some(v) => json_tree(format!("{id}-output"), v, window, cx).into_any_element(),
                None => mono(&out).into_any_element(),
            };
            (div().flex().flex_col().gap_3().when(d.truncated, |c| c.child(note("Cut at 256 KB; the tool's output was longer."))).child(el).into_any_element(), Some(out))
        }
        (Subject::Row(RecordKind::Tool), "Timing") => {
            let r = row.cloned().unwrap_or_default();
            let mut f = fields();
            if let Some(s) = r.started_ms {
                f = f.child(field("Started", full_time(s)));
            }
            f = f.child(field("Duration", r.duration_ms.map_or_else(|| unavailable("still running"), duration_label)));
            f = f.child(field("Held by the guard", if r.held_ms > 0 { duration_label(r.held_ms) } else { "—".into() }));
            (f.into_any_element(), None)
        }
        (Subject::Row(RecordKind::Tool), "Guard") => match &d.guard {
            Some(g) => {
                let f = fields()
                    .child(field("Mode", g.mode.clone()))
                    .child(field("Decided by", g.plugin.clone().unwrap_or_else(|| "every guard passed".into())))
                    .child(field("Verdict", guard_line(g.verdict, None)))
                    .child(field("Reason", g.reason.clone().unwrap_or_else(|| "—".into())))
                    .child(field("Held", if g.held_ms > 0 { duration_label(g.held_ms) } else { "—".into() }));
                (f.into_any_element(), g.reason.clone())
            }
            None => (note("No guard checked this call.").into_any_element(), None),
        },
        // ----- user, context
        (Subject::Row(kind @ (RecordKind::User | RecordKind::Context)), "Summary") => {
            let text_ = d.text.clone().unwrap_or_default();
            let pics: Vec<Pic> = d.images.iter().map(|p| Pic { src: std::path::PathBuf::from(p).into(), width: 1, height: 1 }).collect();
            let open = h.on_open_image.clone();
            (
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .when(kind == RecordKind::Context, |c| {
                        c.children(row.and_then(|r| r.label.clone()).map(|l| fields().child(field("From", l))))
                    })
                    .when(!pics.is_empty(), |c| c.child(div().flex().child(message_images(format!("{id}-pics"), pics, Some(open)))))
                    .child(mono(&text_))
                    .into_any_element(),
                Some(text_),
            )
        }
        (_, "Raw") => {
            let json = raw_json();
            (mono(&json).into_any_element(), Some(json))
        }
        _ => (note("Nothing to show.").into_any_element(), None),
    };
    let on_copy = h.on_copy.clone();
    div()
        .relative()
        .p_4()
        .child(content)
        .when_some(copy.filter(|c| !c.is_empty()), |d, text_| {
            d.child(
                div().absolute().top_2().right_2().child(
                    icon_button("inspector-copy", Lucide::Copy, true, false)
                        .tip("Copy")
                        .on_click(move |_, w, cx| on_copy(text_.clone().into(), w, cx)),
                ),
            )
        })
        .into_any_element()
}

// ----- pieces ---------------------------------------------------------------------------------

fn fields() -> Div {
    div().flex().flex_col().gap_1p5().text_size(text::SM)
}

fn field(label: &str, value: String) -> Div {
    field_el(label, div().child(value))
}

fn field_el(label: &str, value: impl IntoElement) -> Div {
    div()
        .flex()
        .gap_3()
        .child(div().w(px(118.)).flex_none().text_color(color::text_3()).child(label.to_string()))
        .child(div().flex_1().min_w_0().text_color(color::text()).child(value))
}

fn section(title: &str, body: AnyElement) -> Div {
    div().flex().flex_col().gap_2().child(div().text_size(text::SM).font_weight(FontWeight::MEDIUM).text_color(color::text()).child(title.to_string())).child(body)
}

fn note(s: &str) -> Div {
    div().text_size(text::XS).text_color(color::text_3()).child(s.to_string())
}

fn unavailable(why: &str) -> String {
    format!("unavailable ({why})")
}

fn mono(s: &str) -> Div {
    div()
        .w_full()
        .font_family(text::MONO_FONT)
        .text_size(text::CODE)
        .line_height(relative(1.5))
        .text_color(color::text())
        .whitespace_normal()
        .children(s.lines().map(|l| div().min_h(px(18.)).child(l.to_string())))
}

fn markdown(id: String, md: &str) -> Div {
    div().text_size(text::MD).line_height(relative(text::LEADING)).text_color(color::text()).child(TextView::markdown(ElementId::Name(id.into()), md.trim().to_string()).selectable(true))
}

fn status_text(status: RecordStatus) -> Div {
    let (label, c) = match status {
        RecordStatus::Done => ("Completed", color::green()),
        RecordStatus::Running => ("Running", color::blue()),
        RecordStatus::Failed => ("Failed", color::red()),
        RecordStatus::Cancelled => ("Stopped", color::text_2()),
    };
    div().flex().items_center().gap_1p5().child(div().size(px(7.)).rounded_full().bg(c)).child(label)
}

fn guard_line(verdict: Option<GuardVerdict>, reason: Option<&str>) -> String {
    let v = match verdict {
        Some(GuardVerdict::Allow) => "Allowed",
        Some(GuardVerdict::Deny) => "Denied",
        Some(GuardVerdict::Held) => "Held",
        None => "—",
    };
    match reason {
        Some(r) => format!("{v}: {r}"),
        None => v.to_string(),
    }
}

/// "Source → Request #N", status, tokens.
fn source_fields(row: Option<&TrajectoryRow>, request: Option<&RequestSummary>, h: &InspectorHandlers) -> Div {
    let mut f = fields();
    if let Some(n) = row.and_then(|r| r.request).or(request.map(|r| r.number)) {
        let on_select = h.on_select.clone();
        f = f.child(field_el(
            "Source",
            div()
                .id("inspector-source")
                .flex()
                .items_center()
                .gap_1()
                .text_color(color::accent_hover())
                .cursor_pointer()
                .child(format!("Request #{n}"))
                .child(Icon::new(IconName::ChevronRight).size(size::ICON_SM))
                .on_click(move |_, w, cx| on_select(format!("request:{n}").into(), w, cx)),
        ));
    }
    if let Some(r) = row {
        f = f.child(field_el("Status", status_text(r.status)));
        if let Some(t) = r.tokens {
            f = f.child(field("Tokens", format!("{} tok", format::thousands(t))));
        }
    }
    f
}

fn request_summary(r: Option<&RequestSummary>, stop: Option<String>) -> Div {
    let Some(r) = r else { return note("No request.") };
    let mut f = fields()
        .child(field_el("Status", status_text(r.status)))
        .child(field("Model", r.model.clone().unwrap_or_else(|| "—".into())))
        .child(field("Tool calls", r.tools.to_string()))
        .child(field("Retries", r.retries.to_string()));
    if let Some(s) = stop {
        f = f.child(field("Stop reason", s));
    }
    if let Some(u) = &r.usage {
        f = f.child(field("Tokens", format!("{} tok", format::thousands(u.total()))));
    }
    if let Some(e) = &r.error {
        f = f.child(field_el("Error", div().text_color(color::red()).child(e.clone())));
    }
    f.child(div().mt_2().child(note(CLI_NOTE)))
}

fn options_view(opts: Option<(u32, space_core::protocol::trace::RequestOptions)>, _h: &InspectorHandlers, window: &mut Window, cx: &mut App) -> Div {
    let Some((header_id, o)) = opts else { return note("No options recorded.") };
    let json = |v: &Option<serde_json::Value>| v.as_ref().map_or_else(|| "—".to_string(), |v| v.to_string());
    let _ = (window, cx);
    fields()
        .child(field("Provider", o.provider.clone()))
        .child(field("Model", o.model.clone()))
        .child(field("Max tokens", o.max_tokens.map_or_else(|| "—".into(), format::thousands)))
        .child(field("Thinking", json(&o.thinking)))
        .child(field("Effort", o.effort.clone().unwrap_or_else(|| "—".into())))
        .child(field("Tool choice", json(&o.tool_choice)))
        .child(field("Temperature", o.temperature.map_or_else(|| "—".into(), |t| format!("{t}"))))
        .child(field("Header", format!("#{header_id}")))
}

/// Started (click: Unix ms), total, TTFT, generation, throughput, and when chunks arrived.
fn timing_view(id: &SharedString, r: Option<&RequestSummary>, chunks: &[ChunkRun], now_ms: u64, window: &mut Window, cx: &mut App) -> Div {
    let Some(r) = r else { return note("No request.") };
    let unix = window.use_keyed_state(ElementId::Name(format!("{id}-unix").into()), cx, |_, _| false);
    let show_unix = *unix.read(cx);
    let end = r.ended_ms;
    let total = end.unwrap_or(now_ms).saturating_sub(r.started_ms);
    let ttft = r.first_token_ms.map(|f| f.saturating_sub(r.started_ms));
    let generation = match (r.first_token_ms, end) {
        (Some(f), Some(e)) => Some(e.saturating_sub(f)),
        _ => None,
    };
    let output = r.usage.as_ref().map(|u| u.output);
    let throughput = match (output, generation) {
        (Some(o), Some(g)) if g > 0 => Ok(format!("{:.1} tok/s", o as f64 * 1000. / g as f64)),
        (None, _) => Err("no usage reported"),
        (_, None) if r.first_token_ms.is_none() => Err("no tokens streamed"),
        (_, None) => Err("still running"),
        _ => Err("too fast to measure"),
    };
    let st = unix.clone();
    let started = div()
        .id(ElementId::Name(format!("{id}-started").into()))
        .cursor_pointer()
        .hover(|s| s.text_color(color::accent_hover()))
        .child(if show_unix { r.started_ms.to_string() } else { full_time(r.started_ms) })
        .on_click(move |_, window, cx| {
            st.update(cx, |v, _| *v = !*v);
            window.refresh();
        });
    let f = fields()
        .child(field_el("Started", started))
        .child(field("Total duration", if end.is_some() { duration_label(total) } else { format!("{} (running)", duration_label(total)) }))
        .child(field(
            "TTFT",
            ttft.map_or_else(|| unavailable(if r.status == RecordStatus::Running { "waiting" } else { "no tokens streamed" }), duration_label),
        ))
        .child(field("Generation", generation.map_or_else(|| unavailable(if end.is_none() { "still running" } else { "no tokens streamed" }), duration_label)))
        .child(field("Throughput", throughput.unwrap_or_else(unavailable)));
    div().flex().flex_col().gap_4().child(f).when(!chunks.is_empty(), |d| d.child(section("Chunks", sparkline(r.started_ms, end.unwrap_or(now_ms), chunks).into_any_element())))
}

/// When chunks arrived between `start` and `end`: one tick per chunk, colored by stream.
fn sparkline(start: u64, end: u64, chunks: &[ChunkRun]) -> Div {
    let span = end.saturating_sub(start).max(1) as f32;
    let mut ticks = Vec::new();
    for run in chunks {
        let c = match run.stream {
            space_core::protocol::trace::ChunkKind::Text => color::violet(),
            space_core::protocol::trace::ChunkKind::Thinking => color::text_3(),
            space_core::protocol::trace::ChunkKind::ToolInput => color::yellow(),
        };
        let mut t = run.t0;
        for (i, dt) in std::iter::once(&0).chain(run.dt.iter()).enumerate() {
            if i > 0 {
                t += *dt as u64;
            }
            ticks.push((t.saturating_sub(start) as f32 / span, c));
        }
    }
    let n = ticks.len();
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .relative()
                .w_full()
                .h(px(24.))
                .rounded(radius::SM)
                .bg(color::surface())
                .children(ticks.into_iter().map(|(f, c)| div().absolute().top(px(4.)).bottom(px(4.)).left(relative(f.clamp(0., 0.995))).w(px(1.)).bg(c))),
        )
        .child(note(&format!("{n} chunk{}", if n == 1 { "" } else { "s" })))
}

fn thinking(id: &SharedString, t: &str, window: &mut Window, cx: &mut App) -> AnyElement {
    let open = window.use_keyed_state(ElementId::Name(format!("{id}-thinking").into()), cx, |_, _| false);
    let is_open = *open.read(cx);
    let st = open.clone();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .id(ElementId::Name(format!("{id}-thinking-toggle").into()))
                .flex()
                .items_center()
                .gap_1()
                .text_size(text::SM)
                .text_color(color::text_2())
                .cursor_pointer()
                .hover(|s| s.text_color(color::text()))
                .child(Icon::new(if is_open { IconName::ChevronDown } else { IconName::ChevronRight }).size(size::ICON_SM))
                .child("Thinking")
                .on_click(move |_, window, cx| {
                    st.update(cx, |v, _| *v = !*v);
                    window.refresh();
                }),
        )
        .when(is_open, |d| d.child(div().text_color(color::text_2()).child(markdown(format!("{id}-thinking-md"), t))))
        .into_any_element()
}

/// Each tool: name and description, opening to its parameters.
fn tools_list(id: &SharedString, header: &HeaderDetail, window: &mut Window, cx: &mut App) -> Div {
    let open = window.use_keyed_state(ElementId::Name(format!("{id}-tools-open").into()), cx, |_, _| std::collections::HashSet::<usize>::new());
    let opened = open.read(cx).clone();
    let mut list = div().flex().flex_col().gap_1().child(note(&format!("{} tools", header.tools.len())));
    for (i, tool) in header.tools.iter().enumerate() {
        let st = open.clone();
        let is_open = opened.contains(&i);
        let mut item = div().flex().flex_col().rounded(radius::LG).border_1().border_color(color::border()).child(
            div()
                .id(("tool-def", i))
                .flex()
                .items_center()
                .gap_2()
                .px_2p5()
                .py_1p5()
                .cursor_pointer()
                .hover(|s| s.bg(color::trajectory_hover()))
                .child(Icon::new(if is_open { IconName::ChevronDown } else { IconName::ChevronRight }).size(size::ICON_SM).text_color(color::text_3()))
                .child(div().font_family(text::MONO_FONT).text_size(text::CODE).text_color(color::text()).child(tool.name.clone()))
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(text::XS).text_color(color::text_3()).child(format::one_line(&tool.description)))
                .on_click(move |_, window, cx| {
                    st.update(cx, |s, _| {
                        if !s.remove(&i) {
                            s.insert(i);
                        }
                    });
                    window.refresh();
                }),
        );
        if is_open {
            item = item.child(
                div()
                    .px_3()
                    .pb_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_size(text::SM).text_color(color::text_2()).child(tool.description.clone()))
                    .child(json_tree(format!("{id}-tool-{i}"), &tool.parameters, window, cx)),
            );
        }
        list = list.child(item);
    }
    list
}

/// A unified diff (3 lines of context) of the prompt, then of the tools as pretty JSON.
pub fn diff_text(before: Option<&HeaderDetail>, after: Option<&HeaderDetail>) -> String {
    let (Some(b), Some(a)) = (before, after) else { return String::new() };
    let tools = |h: &HeaderDetail| serde_json::to_string_pretty(&h.tools).unwrap_or_default() + "\n";
    let mut out = String::new();
    let prompt = similar::TextDiff::from_lines(&b.system_prompt, &a.system_prompt);
    let p = prompt.unified_diff().context_radius(3).header("system prompt (before)", "system prompt (after)").to_string();
    out.push_str(&p);
    let (tb, ta) = (tools(b), tools(a));
    let t = similar::TextDiff::from_lines(&tb, &ta).unified_diff().context_radius(3).header("tools (before)", "tools (after)").to_string();
    out.push_str(&t);
    if out.is_empty() {
        out.push_str("No differences in the prompt or tools (the options changed).\n");
    }
    out
}

fn diff_view(diff: &str) -> Div {
    div().w_full().flex().flex_col().font_family(text::MONO_FONT).text_size(text::CODE).line_height(relative(1.5)).children(diff.lines().map(|l| {
        let (c, bg) = match l.chars().next() {
            Some('+') if !l.starts_with("+++") => (color::green(), color::green().opacity(0.08)),
            Some('-') if !l.starts_with("---") => (color::red(), color::red().opacity(0.08)),
            Some('@') => (color::text_3(), transparent_black()),
            _ if l.starts_with("+++") || l.starts_with("---") => (color::text_2(), transparent_black()),
            _ => (color::text_2(), transparent_black()),
        };
        div().px_1().bg(bg).text_color(c).whitespace_normal().min_h(px(18.)).child(l.to_string())
    }))
}

#[cfg(test)]
mod tests {
    use space_core::protocol::trace::{HeaderDetail, ToolDef};

    use super::diff_text;

    #[test]
    fn diff_shows_prompt_and_tool_changes() {
        let before = HeaderDetail { system_prompt: "a\nb\nc\n".into(), ..Default::default() };
        let mut after = before.clone();
        after.system_prompt = "a\nB\nc\n".into();
        after.tools.push(ToolDef { name: "grep".into(), ..Default::default() });
        let d = diff_text(Some(&before), Some(&after));
        assert!(d.contains("-b\n") && d.contains("+B\n"), "{d}");
        assert!(d.contains("+    \"name\": \"grep\""), "{d}");
        assert!(diff_text(Some(&before), Some(&before)).starts_with("No differences"));

    }
}
