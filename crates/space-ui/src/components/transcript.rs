//! Transcript items (user bubble, markdown reply, thought, tool call, permission card, plan,
//! error, divider, working row) and [`transcript_items`], which lays out a session's items.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::assets::IconName as Lucide;
use gpui::component::text::{TextView, TextViewMotion, TextViewState};
use gpui::component::{Icon, IconName};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use space_core::protocol::TurnUsage;
use space_core::{PermissionDecision, PermissionRequest, PlanEntry, SessionState, ToolCallInfo, ToolStatus, TranscriptItem};

use super::images::{OpenPicsHandler, Pic, message_images};
use super::message_actions::{ActionHandlers, MessageActions, message_actions};
use super::turns::{TurnFold, TurnToggleHandler, folds_away, foldable_turns, turn_summary_row};
use super::loaders::Spinner;
use super::widgets::{pill_button, status_dot};
use crate::format;
use crate::theme::{color, radius, size, text};

pub type ToggleHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;
pub type AnswerHandler = Rc<dyn Fn(u64, PermissionDecision, &mut Window, &mut App)>;
/// Scroll the transcript so this (window coordinates) is in view.
pub type RevealHandler = Rc<dyn Fn(Bounds<Pixels>, &mut Window, &mut App)>;

/// Find's matches in one message bubble: byte ranges of its trimmed text, the current one, and
/// (for the current match) the flag asking to scroll to it once it's laid out, with the scroll.
#[derive(Clone)]
pub struct BubbleMarks {
    pub ranges: Vec<Range<usize>>,
    pub current: Option<Range<usize>>,
    pub reveal: Option<(Rc<Cell<bool>>, RevealHandler)>,
}

/// Find's matches in your messages: by item index, ranges of the trimmed text; the current
/// match; and whether to scroll to it (done by its bubble once laid out). Matches in replies
/// are highlighted by their [`TextViewState`]s.
pub struct FindMarks<'a> {
    pub user: &'a HashMap<usize, Vec<Range<usize>>>,
    pub current: Option<(usize, Range<usize>)>,
    pub reveal: Rc<Cell<bool>>,
}

/// Gap between transcript items; tool calls and thoughts stack tightly.
const GAP: Pixels = px(18.);
const GAP_TIGHT: Pixels = px(2.);
/// Max tool-output lines shown when a tool row is expanded.
const TOOL_MAX_LINES: usize = 40;

/// A user message: its pictures (right-aligned, above), then the bubble with any file chips and
/// the text. `id` is unique in the thread (element ids of the pictures).
/// A message longer than this many lines is shown cut to [`FOLD_SHOWN`] lines, with Show more.
const FOLD_ABOVE: usize = 20;
const FOLD_SHOWN: usize = 12;
/// The bubble's side padding (`px_4`), taken off its width for the text.
const BUBBLE_PAD_X: Pixels = px(16.);

/// A long message's fold: its transcript index, whether it's open, and the toggle.
pub type UserFold = (usize, bool, ToggleHandler);

pub fn user_message(
    id: impl Into<SharedString>,
    text_: &str,
    attachments: &[PathBuf],
    pics: Vec<Pic>,
    on_open: Option<OpenPicsHandler>,
    fold: Option<UserFold>,
    marks: Option<BubbleMarks>,
) -> Div {
    let has_bubble = !text_.trim().is_empty() || !attachments.is_empty();
    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .when(!pics.is_empty(), |d| d.child(message_images(id, pics, on_open)))
        .when(has_bubble, |d| d.child(user_bubble(text_, attachments, fold, marks)))
}

/// Share of the column a bubble may take (and never more than [`size::USER_BUBBLE_MAX_W`]).
const BUBBLE_SHARE: f32 = 0.82;

thread_local! {
    /// The widest a bubble can be right now (82% of the column, as last laid out); until the
    /// first layout, the cap.
    static BUBBLE_ROOM: std::cell::Cell<Pixels> = const { std::cell::Cell::new(size::USER_BUBBLE_MAX_W) };
}

/// The width a full bubble's text wraps at.
fn bubble_text_width() -> Pixels {
    BUBBLE_ROOM.with(|r| r.get()).min(size::USER_BUBBLE_MAX_W) - BUBBLE_PAD_X * 2.
}

/// Whether a message is long enough to fold: more than [`FOLD_ABOVE`] lines as the bubble wraps
/// it at the width it has now.
pub fn is_long_message(text_: &str, window: &Window) -> bool {
    let text_ = text_.trim();
    let width = bubble_text_width();
    // Too short to wrap that far (the widest glyphs are well over 4px).
    let per_line = (width / px(4.)).max(1.) as usize;
    if text_.lines().map(|l| l.len() / per_line + 1).sum::<usize>() <= FOLD_ABOVE {
        return false;
    }
    let run = TextRun {
        len: text_.len(),
        font: window.text_style().font(),
        color: color::text(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    match window.text_system().shape_text(text_.to_string().into(), text::BODY, &[run], Some(width), None) {
        Ok(lines) => lines.iter().map(|l| l.wrap_boundaries().len() + 1).sum::<usize>() > FOLD_ABOVE,
        Err(_) => text_.lines().count() > FOLD_ABOVE,
    }
}

/// A bubble's text, with find's matches highlighted (and the current one scrolled to when asked).
fn bubble_text(body: String, marks: Option<BubbleMarks>) -> AnyElement {
    let Some(marks) = marks.filter(|m| !m.ranges.is_empty()) else { return body.into_any_element() };
    let style = |bg: Hsla| HighlightStyle { background_color: Some(bg), ..Default::default() };
    let highlights = marks
        .ranges
        .iter()
        .map(|r| (r.clone(), style(if marks.current.as_ref() == Some(r) { color::find_current() } else { color::find_match() })))
        .collect::<Vec<_>>();
    let styled = StyledText::new(body).with_highlights(highlights);
    let (Some(current), Some((pending, on_reveal))) = (marks.current, marks.reveal) else { return styled.into_any_element() };
    // After the text is laid out: scroll to the current match's line.
    let layout = styled.layout().clone();
    let reveal = canvas(
        move |_, window, cx| {
            if !pending.get() {
                return;
            }
            if let Some(at) = layout.position_for_index(current.start) {
                pending.set(false);
                let line = layout.line_height();
                on_reveal(Bounds::new(at, size(px(1.), line)), window, cx);
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_0();
    div().relative().child(styled).child(reveal).into_any_element()
}

pub fn user_bubble(text_: &str, attachments: &[PathBuf], fold: Option<UserFold>, marks: Option<BubbleMarks>) -> Div {
    let mut bubble = div()
        .min_w_0()
        .max_w(size::USER_BUBBLE_MAX_W)
        .flex()
        .flex_col()
        .gap_2()
        .px(BUBBLE_PAD_X)
        .py_2p5()
        .rounded(radius::CARD)
        .bg(color::surface_2())
        .text_size(text::BODY)
        .line_height(relative(text::LEADING));
    if !attachments.is_empty() {
        bubble = bubble.child(div().flex().flex_wrap().gap_1p5().children(attachments.iter().map(|p| attachment_chip(p))));
    }
    if !text_.is_empty() {
        let body = text_.trim().to_string();
        bubble = match fold {
            None => bubble.child(bubble_text(body, marks)),
            Some((ix, open, on_toggle)) => {
                let line = text::BODY * text::LEADING;
                let body = bubble_text(body, marks);
                let text_el = if open {
                    div().child(body)
                } else {
                    // The first lines, fading out at the bottom.
                    let bg = color::surface_2();
                    div().relative().max_h(line * FOLD_SHOWN as f32).overflow_hidden().child(body).child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom_0()
                            .h(line * 1.5)
                            .bg(linear_gradient(180., linear_color_stop(bg.opacity(0.), 0.), linear_color_stop(bg, 1.))),
                    )
                };
                let toggle = div()
                    .id(("user-fold", ix))
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(text::SM)
                    .text_color(color::text_3())
                    .cursor_pointer()
                    .hover(|s| s.text_color(color::text_2()))
                    .child(if open { "Show less" } else { "Show more" })
                    .child(Icon::new(if open { IconName::ChevronUp } else { IconName::ChevronDown }).size(size::ICON_SM))
                    .on_click(move |_, w, cx| on_toggle(ix, w, cx));
                bubble.child(div().flex().flex_col().gap_1().child(text_el).child(toggle))
            }
        };
    }
    // 82% of the column, right-aligned; the bubble fits its text within that and the cap. The
    // canvas keeps the room up to date for measuring folds (redrawn when it changes).
    let room = canvas(
        |bounds, window, _| {
            let width = bounds.size.width;
            if width > px(0.) && BUBBLE_ROOM.with(|r| r.replace(width)) != width {
                window.refresh();
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_full();
    div().flex().justify_end().child(
        div().relative().w(relative(BUBBLE_SHARE)).flex().justify_end().child(room).child(bubble),
    )
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
    agent_message_streaming(id, markdown, false)
}

/// An agent reply; `fade`: it's streaming, so text it gains fades in (word by word).
pub fn agent_message_streaming(id: impl Into<ElementId>, markdown: &str, fade: bool) -> Div {
    let view = TextView::markdown(id, markdown.trim().to_string()).selectable(true).code_block_actions(code_block_header);
    let view = if fade {
        view.motion(
            TextViewMotion::default()
                .with_stream_fade(std::time::Duration::from_millis(180))
                .with_stream_fade_stagger(std::time::Duration::from_millis(12)),
        )
    } else {
        view
    };
    div().text_size(text::BODY).line_height(relative(text::LEADING_PROSE)).text_color(color::text()).child(view)
}

/// How a code block's language is written (`typescript` → TypeScript, `rs` → Rust). Unknown
/// ones keep what the fence says, with a capital first letter.
pub fn language_name(lang: &str) -> String {
    let known = match lang.to_ascii_lowercase().as_str() {
        "rust" | "rs" => "Rust",
        "typescript" | "ts" => "TypeScript",
        "tsx" => "TSX",
        "javascript" | "js" | "mjs" | "cjs" => "JavaScript",
        "jsx" => "JSX",
        "python" | "py" => "Python",
        "go" | "golang" => "Go",
        "c" => "C",
        "cpp" | "c++" | "cc" | "cxx" | "hpp" => "C++",
        "csharp" | "c#" | "cs" => "C#",
        "objective-c" | "objc" => "Objective-C",
        "java" => "Java",
        "kotlin" | "kt" => "Kotlin",
        "swift" => "Swift",
        "ruby" | "rb" => "Ruby",
        "php" => "PHP",
        "lua" => "Lua",
        "zig" => "Zig",
        "elixir" | "ex" | "exs" => "Elixir",
        "haskell" | "hs" => "Haskell",
        "scala" => "Scala",
        "dart" => "Dart",
        "r" => "R",
        "bash" => "Bash",
        "sh" | "shell" => "Shell",
        "zsh" => "Zsh",
        "fish" => "Fish",
        "powershell" | "ps1" => "PowerShell",
        "console" | "terminal" => "Terminal",
        "html" => "HTML",
        "xml" => "XML",
        "svg" => "SVG",
        "css" => "CSS",
        "scss" => "SCSS",
        "json" => "JSON",
        "jsonc" => "JSONC",
        "json5" => "JSON5",
        "yaml" | "yml" => "YAML",
        "toml" => "TOML",
        "ini" => "INI",
        "sql" => "SQL",
        "graphql" | "gql" => "GraphQL",
        "markdown" | "md" => "Markdown",
        "diff" | "patch" => "Diff",
        "dockerfile" | "docker" => "Dockerfile",
        "makefile" | "make" => "Makefile",
        "nix" => "Nix",
        "proto" | "protobuf" => "Protocol Buffers",
        "text" | "txt" | "plaintext" => "Text",
        _ => "",
    };
    if !known.is_empty() {
        return known.to_string();
    }
    let mut chars = lang.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// A code block's top bar: its language on the left, Copy on the right (a check and "Copied"
/// for a moment after a click), over a rule. Drawn by the text view above the code.
pub fn code_block_header(block: &gpui::base::text::CodeBlock, window: &mut Window, cx: &mut App) -> AnyElement {
    const COPIED_FOR: std::time::Duration = std::time::Duration::from_millis(1500);
    let code = block.code();
    let lang = block.lang().filter(|l| !l.is_empty());
    // One "copied" moment per block, known by its text.
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (lang.as_deref(), code.as_ref()).hash(&mut h);
        h.finish()
    };
    let copied_at = window.use_keyed_state(SharedString::from(format!("code-copied-{key}")), cx, |_, _| None::<std::time::Instant>);
    let copied = copied_at.read(cx).is_some_and(|at| at.elapsed() < COPIED_FOR);
    let copy = div()
        .id("copy")
        .flex()
        .items_center()
        .gap_1()
        .h(px(24.))
        .px_2()
        .rounded(radius::SM)
        .text_size(text::XS)
        .text_color(color::text_2())
        .cursor_pointer()
        .hover(|s| s.bg(color::hover()).text_color(color::text()))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(Icon::new(if copied { Lucide::Check } else { Lucide::Copy }).size(size::ICON_SM))
        .child(if copied { "Copied" } else { "Copy" })
        .on_click(move |_, window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(code.trim_end_matches('\n').to_string()));
            copied_at.update(cx, |at, cx| {
                *at = Some(std::time::Instant::now());
                cx.notify();
            });
            window
                .spawn(cx, async move |cx| {
                    cx.background_executor().timer(COPIED_FOR).await;
                    let _ = cx.update(|window, _| window.refresh());
                })
                .detach();
        });
    div()
        .flex()
        .items_center()
        .justify_between()
        .h(px(34.))
        .pl_4()
        .pr_1p5()
        .border_b_1()
        // Darker than the block: the thread's background shows through as a groove.
        .border_color(color::bg())
        .font_family(text::UI_FONT)
        .child(div().text_size(text::XS).text_color(color::text_3()).child(lang.map(|l| language_name(&l)).unwrap_or_default()))
        .child(copy)
        .into_any_element()
}

/// An agent reply drawn from its own [`TextViewState`] (kept by the app, so find can read its
/// text and highlight it); `fade`: it's streaming. `on_reveal` scrolls the transcript to a
/// line the state was asked to reveal.
pub fn agent_reply(state: &Entity<TextViewState>, fade: bool, on_reveal: Option<RevealHandler>) -> Div {
    let mut view = TextView::new(state).selectable(true).code_block_actions(code_block_header);
    if fade {
        view = view.motion(
            TextViewMotion::default()
                .with_stream_fade(std::time::Duration::from_millis(180))
                .with_stream_fade_stagger(std::time::Duration::from_millis(12)),
        );
    }
    if let Some(on_reveal) = on_reveal {
        view = view.on_reveal(move |bounds, window, cx| on_reveal(bounds, window, cx));
    }
    div().text_size(text::BODY).line_height(relative(text::LEADING_PROSE)).text_color(color::text()).child(view)
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
        "new-session" => "Continuing in a new session, without the earlier context".to_string(),
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
/// Laid out exactly like a live [`thought_row`] (height, padding, spinner, gap, text), so
/// "Working…" turning into "Thinking…" moves nothing.
pub fn working_row(label: &str) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .h(size::ROW_H)
        .px_2()
        .text_size(text::SM)
        .text_color(color::text_3())
        .child(Spinner::new(size::SPINNER_MD))
        .child(label.to_string())
}

/// Everything [`transcript_items`] needs besides the session state.
pub struct TranscriptView<'a> {
    pub state: &'a SessionState,
    /// Unique per thread (element ids of markdown views).
    pub key: u64,
    pub expanded: &'a HashSet<usize>,
    pub thought_secs: &'a HashMap<usize, u64>,
    /// A message waiting for the session to start (its text, files and pictures).
    pub queued: Option<(&'a str, &'a [PathBuf], Vec<Pic>)>,
    pub on_toggle: ToggleHandler,
    pub on_answer: AnswerHandler,
    /// A message's picture was clicked.
    pub on_open_image: Option<OpenPicsHandler>,
    /// For the rows under messages: now (Unix ms), the row showing "Copied", the row whose
    /// usage panel is open, and what their buttons do.
    pub now_ms: u64,
    pub copied: Option<&'a str>,
    pub usage_open: Option<&'a str>,
    pub actions: ActionHandlers,
    /// Finished turns the user opened (by their user message's index), and what toggles one.
    pub open_turns: &'a HashSet<usize>,
    pub on_toggle_turn: TurnToggleHandler,
    /// The streaming reply: its index and how many bytes of it to show (the rest is still to
    /// be revealed); it fades in as it grows.
    pub streaming: Option<(usize, usize)>,
    /// The replies' text views, by item index (a reply without one renders on its own).
    pub replies: &'a HashMap<usize, Entity<TextViewState>>,
    /// Find's matches while the find bar is open.
    pub find: Option<FindMarks<'a>>,
    /// Scrolls the transcript (to show a match).
    pub on_reveal: Option<RevealHandler>,
}

/// The transcript's elements, spaced, with a trailing working row while a turn runs. Each
/// message of yours, and each agent turn (from after your message to its end), is one hover
/// area with its row of actions under it. A finished turn's thinking and tool calls fold under
/// a "Completed in …" row (see [`super::turns`]) unless the user opened it.
pub fn transcript_items(view: TranscriptView<'_>, window: &mut Window, cx: &mut App) -> Vec<AnyElement> {
    let TranscriptView {
        state,
        key,
        expanded,
        thought_secs,
        queued,
        on_toggle,
        on_answer,
        on_open_image,
        now_ms,
        copied,
        usage_open,
        actions,
        open_turns,
        on_toggle_turn,
        streaming,
        replies,
        find,
        on_reveal,
    } = view;
    let items = &state.items;
    let folds = foldable_turns(items);
    // The folded turn being laid out (its work is hidden until its end).
    let mut folded: Option<TurnFold> = None;
    // Stands for the summary row when spacing what follows it (spaced like a thought row).
    let summary_marker = TranscriptItem::Thought(String::new());
    let compact = |i: &TranscriptItem| matches!(i, TranscriptItem::Tool(_) | TranscriptItem::Thought(_));
    let row = |id: String, group: &SharedString, copy: Option<&str>, at_ms: Option<u64>, usage: Option<&TurnUsage>, end: bool| {
        message_actions(
            MessageActions {
                copied: copied == Some(id.as_str()),
                usage_open: usage_open == Some(id.as_str()),
                id: id.into(),
                group: group.clone(),
                copy_text: copy.map(str::trim).filter(|t| !t.is_empty()).map(|t| SharedString::from(t.to_string())),
                at_ms,
                now_ms,
                usage: usage.cloned(),
                end,
                shown: false,
            },
            &actions,
        )
    };
    let mut out = Vec::new();
    // The agent turn being laid out: its elements and its last reply's text.
    let mut turn: Vec<AnyElement> = Vec::new();
    let mut reply: Option<&str> = None;
    let mut prev: Option<&TranscriptItem> = None;
    // The previous element ends with a row of actions (its height already spaces them).
    let mut after_row = false;
    for (ix, item) in items.iter().enumerate() {
        let gap = match prev {
            None => px(0.),
            _ if after_row => px(6.),
            Some(p) if compact(p) && compact(item) => GAP_TIGHT,
            _ => GAP,
        };
        let last = ix + 1 == items.len();
        if folded.is_some_and(|f| ix < f.end) && folds_away(item) {
            continue;
        }
        let el = match item {
            TranscriptItem::User { text, attachments, images, at_ms } => {
                // An unfinished turn before it (no end): as it is, without a row.
                if !turn.is_empty() {
                    out.push(div().flex().flex_col().children(std::mem::take(&mut turn)).into_any_element());
                }
                let group = SharedString::from(format!("user-{key}-{ix}"));
                let fold = is_long_message(text, window).then(|| (ix, expanded.contains(&ix), on_toggle.clone()));
                let marks = find.as_ref().and_then(|f| {
                    let ranges = f.user.get(&ix)?.clone();
                    let current = f.current.as_ref().filter(|(at, _)| *at == ix).map(|(_, r)| r.clone());
                    let reveal = current.is_some().then(|| on_reveal.clone()).flatten().map(|h| (f.reveal.clone(), h));
                    Some(BubbleMarks { ranges, current, reveal })
                });
                let message = user_message(
                    format!("pics-{key}-{ix}"),
                    text,
                    attachments,
                    images.iter().map(Pic::file).collect(),
                    on_open_image.clone(),
                    fold,
                    marks,
                );
                out.push(
                    div()
                        .group(group.clone())
                        .mt(gap)
                        .child(message)
                        .child(row(format!("user-{key}-{ix}"), &group, Some(text), *at_ms, None, true))
                        .into_any_element(),
                );
                (prev, after_row, reply) = (Some(item), true, None);
                // A finished turn: its summary row comes first; its work is hidden unless opened.
                if let Some(fold) = folds.get(&ix).copied() {
                    let open = open_turns.contains(&ix);
                    folded = (!open).then_some(fold);
                    let summary = turn_summary_row(ix, fold.duration_ms, open, window, cx, on_toggle_turn.clone());
                    turn.push(div().mt(px(6.)).child(summary).into_any_element());
                    (prev, after_row) = (Some(&summary_marker), false);
                } else {
                    folded = None;
                }
                continue;
            }
            TranscriptItem::TurnEnd { reason, at_ms, usage } => {
                let divider = !matches!(reason.as_str(), "end_turn" | "error");
                if divider {
                    turn.push(div().mt(gap).child(turn_divider(reason)).into_any_element());
                }
                let group = SharedString::from(format!("turn-{key}-{ix}"));
                let has_row = reply.is_some() || at_ms.is_some() || usage.is_some();
                if !turn.is_empty() {
                    out.push(
                        div()
                            .group(group.clone())
                            .flex()
                            .flex_col()
                            .children(std::mem::take(&mut turn))
                            .when(has_row, |d| d.child(row(format!("turn-{key}-{ix}"), &group, reply, *at_ms, usage.as_ref(), false)))
                            .into_any_element(),
                    );
                }
                after_row = has_row || (after_row && !divider);
                prev = Some(item);
                reply = None;
                continue;
            }
            TranscriptItem::Agent(md) => {
                reply = Some(md);
                let id = SharedString::from(format!("md-{key}-{ix}"));
                let live = streaming.is_some_and(|(at, _)| at == ix);
                match (replies.get(&ix), streaming) {
                    (Some(state), _) => agent_reply(state, live && !cx.reduce_motion(), on_reveal.clone()),
                    (None, Some((at, shown))) if at == ix => agent_message_streaming(id, &md[..shown.min(md.len())], !cx.reduce_motion()),
                    _ => agent_message(id, md),
                }
            }
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
        };
        turn.push(div().mt(gap).child(el).into_any_element());
        (prev, after_row) = (Some(item), false);
    }
    // The running turn: no row until it ends.
    if !turn.is_empty() {
        out.push(div().flex().flex_col().children(turn).into_any_element());
    }
    if let Some((text_, attachments, pics)) = queued {
        out.push(div().mt(GAP).child(user_message(format!("pics-{key}-queued"), text_, attachments, pics, None, None, None)).into_any_element());
        out.push(div().mt(GAP).child(working_row("Starting agent…")).into_any_element());
    } else if state.running
        && state.pending_permission().is_none()
        // A live thought shows the spinner in its own "Thinking…" row.
        && !matches!(items.last(), Some(TranscriptItem::Thought(_)))
    {
        // Spaced as a thought in its place would be.
        let gap = match prev {
            None => px(0.),
            _ if after_row => px(6.),
            Some(p) if compact(p) => GAP_TIGHT,
            _ => GAP,
        };
        out.push(div().mt(gap).child(working_row("Working…")).into_any_element());
    }
    out
}

/// Fixture data shared by previews (and scenarios' expectations).
#[cfg(feature = "dev")]
pub mod fixtures {
    use space_core::{PermissionKind, PermissionOption, ToolCallPatch};

    use super::*;

    pub const LONG_MESSAGE: &str = "Here's the plan for the release, step by step:\n\n\
1. Bump the version in Cargo.toml.\n2. Write the release notes.\n3. Build the release app and check every plugin starts.\n\
4. Tag the commit.\n5. Upload the zip to a GitHub release.\n6. Update the cask in the tap.\n7. Install it on the other Mac.\n\
8. Check the update command works.\n9. Check Spotlight finds it.\n10. Tell the team.\n\
11. Watch the crash reports for a day.\n12. Answer questions in the channel.\n13. Note what went wrong.\n\
14. Fix the release script where it was slow.\n15. Plan the next release.\n\n\
Before any of that, make sure the tests pass and lint is clean, and that the terminal plugin is in the bundle.";

    pub const MARKDOWN: &str = "Plugin discovery scans three roots in order: the bundled `plugins/` directory, \
`~/.space/plugins` and the project's `.space/plugins`. Later roots win on duplicate ids.\n\n\
**What happens on start**\n\n\
- The manifest's `run.command` is resolved on `PATH` and spawned with piped stdio.\n\
- The harness performs the ACP `initialize` handshake.\n\
- stderr lines are kept in a ring buffer for error messages.\n\n\
```rust\nlet agent = host.start_agent(\"pi-agent\").await?;\nlet session = agent.new_session(dir).await?;\n```\n\n\
Invalid manifests are reported, not fatal.";

    pub const CODE_BLOCKS: &str = "Here are three:\n\n\
```rust\nfn main() {\n    // Greet the world.\n    println!(\"Hello, world! {}\", 42);\n}\n```\n\n\
```typescript\nconst greet = (name: string): string => `Hello, ${name}!`;\nconsole.log(greet(\"world\"));\n```\n\n\
```python\ndef main() -> None:\n    print(\"Hello, world!\")\n\nif __name__ == \"__main__\":\n    main()\n```\n\n\
```\nno language: plain text\n```";

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

    // A turn laid out by transcript_items from session events; `open`: the user opened it.
    let turn = {
        let (toggle, answer) = (toggle.clone(), answer.clone());
        move |name: &'static str, events: Vec<SessionEvent>, open: bool, streaming: Option<(usize, usize)>| {
            let (toggle, answer) = (toggle.clone(), answer.clone());
            Example::new(name, move |window, cx| {
                let mut s = SessionState::new("preview");
                for e in &events {
                    s.apply(e);
                }
                let open_turns: HashSet<usize> = if open { HashSet::from([0]) } else { HashSet::new() };
                let view = TranscriptView {
                    state: &s,
                    key: 0,
                    expanded: &HashSet::new(),
                    thought_secs: &HashMap::from([(1, 3)]),
                    queued: None,
                    on_toggle: toggle.clone(),
                    on_answer: answer.clone(),
                    on_open_image: None,
                    now_ms: 0,
                    copied: None,
                    usage_open: None,
                    actions: ActionHandlers::default(),
                    open_turns: &open_turns,
                    on_toggle_turn: Rc::new(|_, _, _| {}),
                    streaming,
                    replies: &HashMap::new(),
                    find: None,
                    on_reveal: None,
                };
                div().flex().flex_col().children(transcript_items(view, window, cx)).into_any_element()
            })
            .width(w)
        }
    };
    let work = |at: Option<u64>| {
        vec![
            SessionEvent::UserMessage { text: "Run the tests and fix what fails".into(), attachments: vec![], images: vec![], at_ms: at },
            SessionEvent::TurnStarted,
            SessionEvent::AgentThought { text: "Run the suite first.".into() },
            SessionEvent::ToolCall(tool(ToolStatus::Completed, TEST_OUTPUT)),
            SessionEvent::AgentText { text: "All 6 tests pass. Now checking clippy…".into() },
        ]
    };
    let finished = |at: Option<u64>, reason: &str| {
        let mut events = work(at);
        events.push(SessionEvent::AgentText { text: "Clippy is clean too; nothing to fix.".into() });
        events.push(SessionEvent::TurnEnded { stop_reason: reason.into(), at_ms: at.map(|t| t + 123_000), usage: None });
        events
    };
    let full = turn("tool turn, working", work(None), false, None);
    let collapsed = turn("finished: work folded", finished(Some(1_000), "end_turn"), false, None);
    let expanded = turn("finished: opened", finished(Some(1_000), "end_turn"), true, None);
    let failed = turn("stopped: stays open", finished(Some(1_000), "cancelled"), false, None);
    let no_times = turn("finished, saved before times were kept", finished(None, "end_turn"), false, None);
    let streaming = turn("streaming: the reply's first part shown so far", work(None), false, Some((3, 14)));

    let (t1, t2, t3, t4, t5, t6, t7, t8, t9) =
        (t.clone(), t.clone(), t.clone(), t.clone(), t.clone(), t.clone(), t.clone(), t.clone(), t);
    let (a1, a2, a3) = (answer.clone(), answer.clone(), answer);
    let (t10, t11): (ToggleHandler, ToggleHandler) = (Rc::new(|_, _, _| {}), Rc::new(|_, _, _| {}));
    vec![
        ComponentPreview::new(Area::Transcript, "Messages", "User bubble and markdown reply.").stacked().examples([
            ex("user", Rc::new(|| user_bubble("Explain how plugin discovery works", &[], None, None))),
            ex(
                "user with attachments",
                Rc::new(|| user_bubble("Review these", &[PathBuf::from("Cargo.toml"), PathBuf::from("crates")], None, None)),
            ),
            ex(
                "user with images",
                Rc::new(|| {
                    let pics = vec![super::images::sample(1600, 900), super::images::sample(1024, 1024)];
                    user_message("preview-pics", "Why does the sidebar look like this?", &[], pics, None, None, None)
                }),
            ),
            ex("long, folded", Rc::new(move || user_bubble(LONG_MESSAGE, &[], Some((0, false, t10.clone())), None))),
            ex("long, shown in full", Rc::new(move || user_bubble(LONG_MESSAGE, &[], Some((1, true, t11.clone())), None))),
            ex(
                "user, with find matches",
                Rc::new(|| {
                    let text_ = "Explain how plugin discovery works, and which plugin wins";
                    let ranges = super::find::find_matches(text_, "plugin");
                    let current = ranges.get(1).cloned();
                    user_bubble(text_, &[], None, Some(BubbleMarks { ranges, current, reveal: None }))
                }),
            ),
            ex("assistant markdown", Rc::new(|| agent_message("preview-md", MARKDOWN))),
            ex("code blocks", Rc::new(|| agent_message("preview-code", CODE_BLOCKS))),
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
            .examples([full, streaming]),
        ComponentPreview::new(
            Area::Transcript,
            "Finished turns",
            "A finished turn folds its thinking and tool calls under \"Completed in …\"; replies stay.",
        )
        .stacked()
        .examples([collapsed, expanded, failed, no_times]),
    ]
}

#[cfg(test)]
mod tests {
    use super::language_name;

    #[test]
    fn language_names() {
        assert_eq!(language_name("typescript"), "TypeScript");
        assert_eq!(language_name("rust"), "Rust");
        assert_eq!(language_name("RS"), "Rust");
        assert_eq!(language_name("json"), "JSON");
        assert_eq!(language_name("elm"), "Elm");
        assert_eq!(language_name(""), "");
    }
}
