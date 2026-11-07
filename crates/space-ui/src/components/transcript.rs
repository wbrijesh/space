//! Transcript items (user bubble, markdown reply, thought, tool call, permission card, plan,
//! error, divider, working row) and [`transcript_items`], which lays out a session's items.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::assets::IconName as Lucide;
use gpui::component::text::TextView;
use gpui::component::{Icon, IconName};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use space_core::{PermissionDecision, PermissionRequest, PlanEntry, SessionState, ToolCallInfo, ToolStatus, TranscriptItem};

use super::loaders::Spinner;
use super::widgets::{pill_button, status_dot};
use crate::format;
use crate::theme::{color, radius, size, text};

pub type ToggleHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;
pub type AnswerHandler = Rc<dyn Fn(u64, PermissionDecision, &mut Window, &mut App)>;

/// Gap between transcript items; tool calls and thoughts stack tightly.
const GAP: Pixels = px(18.);
const GAP_TIGHT: Pixels = px(2.);
/// Max tool-output lines shown when a tool row is expanded.
const TOOL_MAX_LINES: usize = 40;

pub fn user_bubble(text_: &str, attachments: &[PathBuf]) -> Div {
    let mut bubble = div()
        .max_w(relative(0.82))
        .flex()
        .flex_col()
        .gap_2()
        .px_4()
        .py_2p5()
        .rounded(radius::CARD)
        .bg(color::surface_2())
        .text_size(text::BODY)
        .line_height(relative(text::LEADING));
    if !attachments.is_empty() {
        bubble = bubble.child(div().flex().flex_wrap().gap_1p5().children(attachments.iter().map(|p| attachment_chip(p))));
    }
    if !text_.is_empty() {
        bubble = bubble.child(text_.trim().to_string());
    }
    div().flex().justify_end().child(bubble)
}

pub fn attachment_chip(path: &Path) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .h(size::CHIP_H)
        .px_2()
        .rounded(radius::SM)
        .bg(color::pressed())
        .font_family(text::MONO_FONT)
        .text_size(text::XS)
        .text_color(color::text_2())
        .child(Icon::new(if path.is_dir() { IconName::Folder } else { IconName::File }).size(size::ICON_SM))
        .child(format::dir_name(path))
}

pub fn agent_message(id: impl Into<ElementId>, markdown: &str) -> Div {
    div()
        .text_size(text::BODY)
        .line_height(relative(text::LEADING_PROSE))
        .text_color(color::text())
        .child(TextView::markdown(id, markdown.trim().to_string()).selectable(true))
}

/// "Thinking…" while live (with a spinner), else "Thought for Ns"; expands to the text.
pub fn thought_row(ix: usize, text_: &str, live: bool, secs: Option<u64>, open: bool, on_toggle: ToggleHandler) -> Div {
    let label = if live {
        "Thinking…".to_string()
    } else {
        match secs {
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
                .h(size::ROW_H)
                .px_2()
                .rounded(radius::LG)
                .text_size(text::SM)
                .text_color(color::text_3())
                .cursor_pointer()
                .hover(|s| s.text_color(color::text_2()).bg(color::surface()))
                .map(|d| {
                    if live {
                        d.child(Spinner::new(size::SPINNER_MD))
                    } else {
                        d.child(Icon::new(Lucide::Brain).size(size::ICON))
                    }
                })
                .child(label)
                .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).size(size::ICON_SM))
                .on_click(move |_, w, cx| on_toggle(ix, w, cx)),
        )
        .when(open, |d| {
            d.child(
                div()
                    .ml(px(15.))
                    .mt_1()
                    .mb_1()
                    .pl_3()
                    .border_l_1()
                    .border_color(color::border_strong())
                    .text_size(text::SM)
                    .line_height(relative(text::LEADING))
                    .text_color(color::text_3())
                    .child(text_.trim().to_string()),
            )
        })
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

/// Compact tool-call row (icon, name, detail, status); expands to its output.
pub fn tool_row(ix: usize, call: &ToolCallInfo, open: bool, on_toggle: ToggleHandler) -> Div {
    let title = if call.title.is_empty() { call.id.clone() } else { call.title.clone() };
    let (name, detail) = format::tool_parts(&title);
    let has_body = !call.output.trim().is_empty() || !call.locations.is_empty();
    let status: AnyElement = match call.status {
        // Waiting for the guardrail (fail closed): paused, not failed.
        _ if call.held.is_some() => div()
            .flex()
            .items_center()
            .gap_1p5()
            .text_size(text::XS)
            .text_color(color::yellow())
            .child("Paused")
            .child(Icon::new(Lucide::CirclePause).size(size::ICON_SM))
            .into_any_element(),
        // Stopped by the tool pipeline: a calm shield, not an error.
        _ if call.blocked.is_some() => div()
            .flex()
            .items_center()
            .gap_1p5()
            .text_size(text::XS)
            .text_color(color::yellow())
            .child("Blocked")
            .child(Icon::new(Lucide::ShieldAlert).size(size::ICON_SM))
            .into_any_element(),
        ToolStatus::InProgress => Spinner::new(size::SPINNER_SM).into_any_element(),
        ToolStatus::Pending => status_dot(color::text_3()).into_any_element(),
        ToolStatus::Completed => status_dot(color::green()).into_any_element(),
        ToolStatus::Failed => div()
            .flex()
            .items_center()
            .gap_1p5()
            .text_size(text::XS)
            .text_color(color::red())
            .child("Failed")
            .child(status_dot(color::red()))
            .into_any_element(),
    };

    let header = div()
        .id(("tool", ix))
        .flex()
        .items_center()
        .gap_2()
        .h(size::ROW_H)
        .px_2()
        .rounded(radius::LG)
        .when(has_body, |d| d.cursor_pointer().hover(|s| s.bg(color::surface())))
        .child(tool_icon(&call.kind).size(size::ICON).text_color(color::text_3()))
        .child(div().flex_none().text_size(text::SM).font_weight(FontWeight::MEDIUM).text_color(color::text_2()).child(name))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family(text::MONO_FONT)
                .text_size(text::CODE)
                .text_color(color::text_3())
                .child(detail),
        )
        .child(status)
        .when(has_body, |d| {
            d.child(
                Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                    .size(size::ICON_SM)
                    .text_color(color::text_3()),
            )
            .on_click(move |_, w, cx| on_toggle(ix, w, cx))
        });

    let body = open.then(|| {
        let lines: Vec<&str> = call.output.lines().collect();
        let shown = lines.iter().take(TOOL_MAX_LINES).copied().collect::<Vec<_>>().join("\n");
        let more = lines.len().saturating_sub(TOOL_MAX_LINES);
        div()
            .ml(px(30.))
            .mt_1()
            .mb_1()
            .p_3()
            .rounded(radius::LG)
            .bg(color::bg_sidebar())
            .border_1()
            .border_color(color::border())
            .font_family(text::MONO_FONT)
            .text_size(text::CODE)
            .line_height(relative(text::LEADING))
            .text_color(color::text_2())
            .flex()
            .flex_col()
            .gap_1()
            .when(!call.locations.is_empty(), |d| d.child(div().text_color(color::text_3()).child(call.locations.join("\n"))))
            .when(!shown.is_empty(), |d| d.child(shown))
            .when(more > 0, |d| d.child(div().text_color(color::text_3()).child(format!("… {more} more lines"))))
    });

    // Why it waits or was stopped, without opening the row.
    let reason = match (&call.held, &call.blocked) {
        (Some(r), _) => Some(format!("Waiting for the guardrail: {r}")),
        (None, Some(r)) => Some(r.clone()),
        (None, None) => None,
    };
    let reason = reason.map(|r| div().ml(px(30.)).mb_1().text_size(text::SM).text_color(color::text_2()).line_clamp(2).child(r));
    div().flex().flex_col().child(header).children(reason).children(body)
}

/// Pending permission request (Allow/Reject buttons), or a one-line record once answered.
pub fn permission_card(request: &PermissionRequest, resolved: Option<&PermissionDecision>, on_answer: AnswerHandler) -> Div {
    let title = request.tool_call.title.clone().unwrap_or_else(|| request.tool_call.id.clone());
    let (name, detail) = format::tool_parts(&title);
    if let Some(decision) = resolved {
        let (icon, line) = match decision {
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
            .h(size::ROW_H)
            .px_2()
            .text_size(text::SM)
            .text_color(color::text_3())
            .child(Icon::new(icon).size(size::ICON))
            .child(div().min_w_0().truncate().child(line));
    }

    let mut buttons = div().flex().gap_2();
    for (n, opt) in request.options.iter().enumerate() {
        let (rid, oid, answer) = (request.request_id, opt.id.clone(), on_answer.clone());
        buttons = buttons.child(
            pill_button(SharedString::from(format!("perm-{rid}-{n}")), opt.name.clone(), opt.kind.is_allow())
                .on_click(move |_, w, cx| answer(rid, PermissionDecision::Selected(oid.clone()), w, cx)),
        );
    }
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .rounded(radius::XXL)
        .bg(color::surface())
        .border_1()
        .border_color(color::yellow_border())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(text::SM)
                .text_color(color::yellow())
                .child(if request.reason.is_some() {
                    Icon::new(Lucide::ShieldAlert).size(size::ICON)
                } else {
                    Icon::new(IconName::TriangleAlert).size(size::ICON)
                })
                .child(if request.reason.is_some() { "Check this call" } else { "Permission needed" }),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().text_size(text::MD).font_weight(FontWeight::MEDIUM).child(name))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_family(text::MONO_FONT)
                        .text_size(text::CODE)
                        .text_color(color::text_2())
                        .child(detail),
                ),
        )
        .when_some(request.reason.clone(), |d, reason| {
            d.child(div().text_size(text::SM).text_color(color::text_2()).child(reason))
        })
        .child(buttons)
}

pub fn plan_card(entries: &[PlanEntry]) -> Div {
    let mut card = div()
        .flex()
        .flex_col()
        .gap_1p5()
        .p_4()
        .rounded(radius::XXL)
        .bg(color::surface())
        .border_1()
        .border_color(color::border())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(text::SM)
                .text_color(color::text_2())
                .child(Icon::new(Lucide::ListChecks).size(size::ICON))
                .child("Plan"),
        );
    for e in entries {
        let (icon, icon_color, text_color): (Icon, Hsla, Hsla) = match e.status.as_str() {
            "completed" => (Icon::new(IconName::Check), color::green(), color::text_3()),
            "in_progress" => (Icon::new(IconName::ArrowRight), color::accent_hover(), color::text()),
            _ => (Icon::new(IconName::Dash), color::text_3(), color::text_2()),
        };
        card = card.child(
            div()
                .flex()
                .items_start()
                .gap_2()
                .text_size(text::MD)
                .child(icon.size(size::ICON).text_color(icon_color).mt(px(2.)))
                .child(div().flex_1().text_color(text_color).child(e.content.clone())),
        );
    }
    card
}

pub fn error_card(message: &str) -> Div {
    div()
        .flex()
        .items_start()
        .gap_2p5()
        .p_3()
        .rounded(radius::XL)
        .bg(color::red_bg())
        .border_1()
        .border_color(color::red_border())
        .child(Icon::new(IconName::CircleAlert).size(size::ICON_MD).text_color(color::red()).mt(px(1.)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(text::SM)
                .line_height(relative(text::LEADING))
                .text_color(color::text())
                .child(format::clean_error(message)),
        )
}

/// Centered hairline divider with a note (stopped turn, new session, …).
pub fn turn_divider(reason: &str) -> Div {
    let note = match reason {
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
        .text_size(text::XS)
        .text_color(color::text_3())
        .child(div().flex_1().h(px(1.)).bg(color::border()))
        .child(note)
        .child(div().flex_1().h(px(1.)).bg(color::border()))
}

/// Spinner + label at the end of a transcript while the agent works.
pub fn working_row(label: &str) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2p5()
        .h(size::ROW_H)
        .px_1p5()
        .text_size(text::SM)
        .text_color(color::text_3())
        .child(Spinner::new(size::SPINNER_LG))
        .child(label.to_string())
}

/// Everything [`transcript_items`] needs besides the session state.
pub struct TranscriptView<'a> {
    pub state: &'a SessionState,
    /// Unique per thread (element ids of markdown views).
    pub key: u64,
    pub expanded: &'a HashSet<usize>,
    pub thought_secs: &'a HashMap<usize, u64>,
    /// A message waiting for the session to start.
    pub queued: Option<(&'a str, &'a [PathBuf])>,
    pub on_toggle: ToggleHandler,
    pub on_answer: AnswerHandler,
}

/// The transcript's elements, spaced, with a trailing working row while a turn runs.
pub fn transcript_items(view: TranscriptView<'_>) -> Vec<AnyElement> {
    let TranscriptView { state, key, expanded, thought_secs, queued, on_toggle, on_answer } = view;
    let items = &state.items;
    let compact = |i: &TranscriptItem| matches!(i, TranscriptItem::Tool(_) | TranscriptItem::Thought(_));
    let mut out = Vec::new();
    let mut prev: Option<&TranscriptItem> = None;
    for (ix, item) in items.iter().enumerate() {
        if matches!(item, TranscriptItem::TurnEnd(r) if r == "error" || r == "end_turn") {
            continue;
        }
        let gap = match prev {
            None => px(0.),
            Some(p) if compact(p) && compact(item) => GAP_TIGHT,
            _ => GAP,
        };
        let last = ix + 1 == items.len();
        let el = match item {
            TranscriptItem::User { text, attachments } => user_bubble(text, attachments),
            TranscriptItem::Agent(md) => agent_message(SharedString::from(format!("md-{key}-{ix}")), md),
            TranscriptItem::Thought(t) => thought_row(
                ix,
                t,
                last && state.running,
                thought_secs.get(&ix).copied(),
                expanded.contains(&ix),
                on_toggle.clone(),
            ),
            TranscriptItem::Tool(call) => tool_row(ix, call, expanded.contains(&ix), on_toggle.clone()),
            TranscriptItem::Permission { request, resolved } => permission_card(request, resolved.as_ref(), on_answer.clone()),
            TranscriptItem::Plan(entries) => plan_card(entries),
            TranscriptItem::Error(msg) => error_card(msg),
            TranscriptItem::TurnEnd(reason) => turn_divider(reason),
        };
        out.push(div().mt(gap).child(el).into_any_element());
        prev = Some(item);
    }
    if let Some((text_, attachments)) = queued {
        out.push(div().mt(GAP).child(user_bubble(text_, attachments)).into_any_element());
        out.push(div().mt(GAP).child(working_row("Starting agent…")).into_any_element());
    } else if state.running
        && state.pending_permission().is_none()
        // A live thought shows the spinner in its own "Thinking…" row.
        && !matches!(items.last(), Some(TranscriptItem::Thought(_)))
    {
        out.push(div().mt(px(14.)).child(working_row("Working…")).into_any_element());
    }
    out
}

/// Fixture data shared by previews (and scenarios' expectations).
#[cfg(feature = "dev")]
pub mod fixtures {
    use space_core::{PermissionKind, PermissionOption, ToolCallPatch};

    use super::*;

    pub const MARKDOWN: &str = "Plugin discovery scans three roots in order: the bundled `plugins/` directory, \
`~/.space/plugins` and the project's `.space/plugins`. Later roots win on duplicate ids.\n\n\
**What happens on start**\n\n\
- The manifest's `run.command` is resolved on `PATH` and spawned with piped stdio.\n\
- The harness performs the ACP `initialize` handshake.\n\
- stderr lines are kept in a ring buffer for the logs panel.\n\n\
```rust\nlet agent = host.start_agent(\"pi-agent\").await?;\nlet session = agent.new_session(dir).await?;\n```\n\n\
Invalid manifests are reported, not fatal.";

    pub fn tool(status: ToolStatus, output: &str) -> ToolCallInfo {
        ToolCallInfo {
            id: "t1".into(),
            title: "bash: cargo test --workspace".into(),
            kind: "execute".into(),
            status,
            output: output.into(),
            locations: vec![],
            blocked: None,
            held: None,
        }
    }

    /// A call the guardrail stopped.
    pub fn blocked_tool() -> ToolCallInfo {
        ToolCallInfo {
            id: "t2".into(),
            title: "bash: rm -rf ~/Library/Caches/space-old".into(),
            kind: "execute".into(),
            status: ToolStatus::Failed,
            output: "Blocked by guardrail: it changes files outside the project folder.".into(),
            locations: vec![],
            blocked: Some("Blocked by guardrail: it changes files outside the project folder.".into()),
            held: None,
        }
    }

    /// A call held while the guardrail can't be used.
    pub fn held_tool() -> ToolCallInfo {
        ToolCallInfo {
            id: "t3".into(),
            title: "bash: cargo build --release".into(),
            kind: "execute".into(),
            status: ToolStatus::Pending,
            output: String::new(),
            locations: vec![],
            blocked: None,
            held: Some("OpenAI rejected your API key, so actions are paused.".into()),
        }
    }

    /// The pipeline asking the user, with the guardrail's reason.
    pub fn guardrail_ask() -> PermissionRequest {
        PermissionRequest {
            reason: Some(
                "The guardrail isn't sure about this call (allow, 64% sure): it changes the system outside the project."
                    .into(),
            ),
            tool_call: space_core::ToolCallPatch {
                id: "t3".into(),
                title: Some("bash: brew upgrade node".into()),
                ..Default::default()
            },
            options: vec![
                PermissionOption { id: "allow".into(), name: "Allow once".into(), kind: PermissionKind::AllowOnce },
                PermissionOption { id: "reject".into(), name: "Reject".into(), kind: PermissionKind::RejectOnce },
            ],
            request_id: 2,
        }
    }

    pub fn permission() -> PermissionRequest {
        PermissionRequest {
            request_id: 1,
            tool_call: ToolCallPatch { id: "t2".into(), title: Some("bash: rm -rf target/debug/incremental".into()), ..Default::default() },
            options: vec![
                PermissionOption { id: "allow".into(), name: "Allow".into(), kind: PermissionKind::AllowOnce },
                PermissionOption { id: "reject".into(), name: "Reject".into(), kind: PermissionKind::RejectOnce },
            ],
            reason: None,
        }
    }

    pub const TEST_OUTPUT: &str = "running 6 tests\ntest configure_switches_model_and_thinking ... ok\n\
test crash_is_reported ... ok\ntest full_turn_streams_and_ends ... ok\ntest result: ok. 6 passed; 0 failed";
}

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use space_core::SessionEvent;

    use crate::preview::{Area, ComponentPreview, Example};
    use fixtures::*;

    let w = size::CONTENT_MAX_W;
    let toggle: ToggleHandler = Rc::new(|_, _, _| {});
    let answer: AnswerHandler = Rc::new(|_, _, _, _| {});
    let t = toggle.clone();
    let ex = |name: &'static str, f: Rc<dyn Fn() -> Div>| Example::new(name, move |_, _| f().into_any_element()).width(w);

    let full = {
        let (toggle, answer) = (toggle.clone(), answer.clone());
        Example::new("tool turn, working", move |_, _| {
            let mut s = SessionState::new("preview");
            for e in [
                SessionEvent::UserMessage { text: "Run the tests and fix what fails".into(), attachments: vec![] },
                SessionEvent::TurnStarted,
                SessionEvent::AgentThought { text: "Run the suite first.".into() },
                SessionEvent::ToolCall(tool(ToolStatus::Completed, TEST_OUTPUT)),
                SessionEvent::AgentText { text: "All 6 tests pass. Now checking clippy…".into() },
            ] {
                s.apply(&e);
            }
            let view = TranscriptView {
                state: &s,
                key: 0,
                expanded: &HashSet::new(),
                thought_secs: &HashMap::from([(1, 3)]),
                queued: None,
                on_toggle: toggle.clone(),
                on_answer: answer.clone(),
            };
            div().flex().flex_col().children(transcript_items(view)).into_any_element()
        })
        .width(w)
    };

    let (t1, t2, t3, t4, t5, t6, t7, t8, t9) =
        (t.clone(), t.clone(), t.clone(), t.clone(), t.clone(), t.clone(), t.clone(), t.clone(), t);
    let (a1, a2, a3) = (answer.clone(), answer.clone(), answer);
    vec![
        ComponentPreview::new(Area::Transcript, "Messages", "User bubble and markdown reply.").stacked().examples([
            ex("user", Rc::new(|| user_bubble("Explain how plugin discovery works", &[]))),
            ex(
                "user with attachments",
                Rc::new(|| user_bubble("Review these", &[PathBuf::from("Cargo.toml"), PathBuf::from("crates")])),
            ),
            ex("assistant markdown", Rc::new(|| agent_message("preview-md", MARKDOWN))),
        ]),
        ComponentPreview::new(Area::Transcript, "Thought", "Collapsed by default; expands to the reasoning.")
            .stacked()
            .examples([
                ex("collapsed", Rc::new(move || thought_row(0, "Check the manifest first.", false, Some(4), false, t1.clone()))),
                ex("expanded", Rc::new(move || thought_row(1, "Check the manifest first, then the stub.", false, Some(4), true, t2.clone()))),
                ex("thinking (live)", Rc::new(move || thought_row(2, "", true, None, false, t3.clone()))),
            ]),
        ComponentPreview::new(Area::Transcript, "Tool call", "Compact row; expands to the output.").stacked().examples([
            ex("running", Rc::new(move || tool_row(0, &tool(ToolStatus::InProgress, ""), false, t4.clone()))),
            ex("done", Rc::new(move || tool_row(1, &tool(ToolStatus::Completed, TEST_OUTPUT), false, t5.clone()))),
            ex("failed", Rc::new(move || tool_row(2, &tool(ToolStatus::Failed, "error: could not compile"), false, t6.clone()))),
            ex("expanded", Rc::new(move || tool_row(3, &tool(ToolStatus::Completed, TEST_OUTPUT), true, t7.clone()))),
            ex("blocked by the guardrail", Rc::new(move || tool_row(4, &blocked_tool(), false, t8.clone()))),
            ex("paused for the guardrail", Rc::new(move || tool_row(5, &held_tool(), false, t9.clone()))),
        ]),
        ComponentPreview::new(Area::Transcript, "Permission", "Allow/Reject; a one-line record once answered.")
            .stacked()
            .examples([
                ex("pending", Rc::new(move || permission_card(&permission(), None, a1.clone()))),
                ex(
                    "allowed",
                    Rc::new(move || permission_card(&permission(), Some(&PermissionDecision::Selected("allow".into())), a2.clone())),
                ),
                ex("guardrail asks (with its reason)", Rc::new(move || permission_card(&guardrail_ask(), None, a3.clone()))),
            ]),
        ComponentPreview::new(Area::Transcript, "Status items", "Plan, error, divider, working row.").stacked().examples([
            ex(
                "plan",
                Rc::new(|| {
                    plan_card(&[
                        PlanEntry { content: "Reproduce the failure".into(), status: "completed".into() },
                        PlanEntry { content: "Fix the retry logic".into(), status: "in_progress".into() },
                        PlanEntry { content: "Add a regression test".into(), status: "pending".into() },
                    ])
                }),
            ),
            ex("error", Rc::new(|| error_card("session/prompt failed: The model request failed: overloaded"))),
            ex("stopped divider", Rc::new(|| turn_divider("cancelled"))),
            ex("working", Rc::new(|| working_row("Working…"))),
        ]),
        ComponentPreview::new(Area::Transcript, "Transcript", "A whole turn laid out by transcript_items.")
            .stacked()
            .example(full),
    ]
}
