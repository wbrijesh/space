//! Realistic trajectory data for the gallery and the app's `trajectory` scenario: two turns,
//! a retried request, a tool call held by the guard, a prompt change and a running request.

use std::rc::Rc;

use gpui::*;
use serde_json::json;
use space_core::protocol::TurnUsage;
use space_core::protocol::trace::*;

use super::*;
use crate::preview::{Area, ComponentPreview, Example};

/// 2026-11-07 11:23:02.000 UTC.
pub const T0: u64 = 1_794_050_582_000;

fn usage(input: u64, read: u64, write: u64, output: u64) -> TurnUsage {
    TurnUsage { input, output, cache_read: Some(read), cache_write: Some(write), reasoning: None, models: vec!["claude-cli/claude-opus-5-5".into()] }
}

fn row(id: &str, kind: RecordKind, turn: u32, step: Option<u32>, preview: &str, start: u64, dur: Option<u64>) -> TrajectoryRow {
    TrajectoryRow { id: id.into(), kind, turn, step, request: step, preview: preview.into(), started_ms: Some(T0 + start), duration_ms: dur, ..Default::default() }
}

/// The page: oldest first.
pub fn page() -> TrajectoryPage {
    let mut rows = vec![
        TrajectoryRow { label: Some("Initial system prompt".into()), ..row("sys-1", RecordKind::System, 0, None, "Initial system prompt · 4 tools", 0, None) },
        TrajectoryRow { images: 1, ..row("u-1", RecordKind::User, 1, None, "The login test fails since the redirect change. Can you fix it?", 0, None) },
        TrajectoryRow {
            label: Some("permissions".into()),
            ..row("c-1", RecordKind::Context, 1, None, "Permission mode: Workspace Write. Every tool call is checked by a decision model first.", 0, None)
        },
        TrajectoryRow { first_token_ms: Some(T0 + 1330), tokens: Some(41), ..row("a-1", RecordKind::Assistant, 1, Some(1), "Let me run the tests first.", 20, Some(1460)) },
        TrajectoryRow { label: Some("bash".into()), guard: Some(GuardVerdict::Allow), ..row("t-1", RecordKind::Tool, 1, Some(1), "cargo test -p space-core → 1 failed", 1500, Some(8200)) },
        TrajectoryRow {
            first_token_ms: Some(T0 + 12_900),
            tokens: Some(212),
            ..row("a-2", RecordKind::Assistant, 1, Some(2), "The test expects the old redirect target; it should follow `/login?next=`.", 9800, Some(4300))
        },
        TrajectoryRow { label: Some("read".into()), guard: Some(GuardVerdict::Allow), ..row("t-2", RecordKind::Tool, 1, Some(2), "crates/space-core/tests/login.rs → 84 lines", 14_200, Some(40)) },
        TrajectoryRow {
            label: Some("edit".into()),
            guard: Some(GuardVerdict::Allow),
            held_ms: 2600,
            ..row("t-3", RecordKind::Tool, 1, Some(2), "crates/space-core/tests/login.rs → ok", 14_300, Some(3100))
        },
        TrajectoryRow { first_token_ms: Some(T0 + 19_100), tokens: Some(96), ..row("a-3", RecordKind::Assistant, 1, Some(3), "Fixed: the test now expects `/login?next=/home`. All 6 tests pass.", 17_500, Some(2200)) },
        TrajectoryRow { label: Some("tools".into()), ..row("sys-2", RecordKind::System, 2, None, "Tools changed: + grep", 600_000, None) },
        row("u-2", RecordKind::User, 2, None, "Now check the other redirect tests.", 600_000, None),
        TrajectoryRow { label: Some("grep".into()), guard: Some(GuardVerdict::Deny), status: RecordStatus::Failed, ..row("t-4", RecordKind::Tool, 2, Some(4), "redirect_to ~/ → denied: outside the project", 602_400, Some(900)) },
        TrajectoryRow { status: RecordStatus::Running, ..row("a-4", RecordKind::Assistant, 2, Some(5), "Searching inside the project instead…", 603_500, None) },
    ];
    rows[3].request = Some(1);
    let req = |number, turn, step, start: u64, ft: Option<u64>, end: Option<u64>, status, retries, tools, u: Option<TurnUsage>| RequestSummary {
        number,
        turn,
        step,
        started_ms: T0 + start,
        first_token_ms: ft.map(|f| T0 + f),
        ended_ms: end.map(|e| T0 + e),
        status,
        retries,
        model: Some("claude-cli/claude-opus-5-5".into()),
        usage: u,
        error: None,
        tools,
    };
    let requests = vec![
        req(1, 1, 1, 20, Some(1330), Some(1480), RecordStatus::Done, 0, 1, Some(usage(4, 0, 2692, 41))),
        req(2, 1, 2, 9800, Some(12_900), Some(14_100), RecordStatus::Done, 1, 2, Some(usage(310, 2692, 1830, 212))),
        req(3, 1, 3, 17_500, Some(19_100), Some(19_700), RecordStatus::Done, 0, 0, Some(usage(96, 4522, 410, 96))),
        req(4, 2, 4, 600_100, Some(601_900), Some(602_300), RecordStatus::Done, 0, 1, Some(usage(40, 4932, 120, 58))),
        req(5, 2, 5, 603_500, None, None, RecordStatus::Running, 0, 0, None),
    ];
    let stats = TrajectoryStats {
        turns: 2,
        steps: 5,
        usage: TurnUsage { input: 450, output: 407, cache_read: Some(12_146), cache_write: Some(5052), reasoning: None, models: vec![] },
        generated_tokens: 407,
        generation_ms: 2_850,
    };
    TrajectoryPage { rows, requests, older: Some("before:sys-1".into()), version: 7, stats }
}

pub fn header(id: u32, with_grep: bool) -> HeaderDetail {
    let tool = |name: &str, desc: &str, params: serde_json::Value| ToolDef { name: name.into(), description: desc.into(), parameters: params };
    let mut tools = vec![
        tool("read", "Read a file's contents. Use offset and limit for large files.", json!({"type": "object", "properties": {"path": {"type": "string"}, "offset": {"type": "number"}, "limit": {"type": "number"}}, "required": ["path"]})),
        tool("bash", "Run a shell command in the project folder.", json!({"type": "object", "properties": {"command": {"type": "string"}, "timeout": {"type": "number"}}, "required": ["command"]})),
        tool("edit", "Replace exact text in a file.", json!({"type": "object", "properties": {"path": {"type": "string"}, "oldText": {"type": "string"}, "newText": {"type": "string"}}})),
        tool("write", "Write a file, replacing it.", json!({"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}})),
    ];
    if with_grep {
        tools.push(tool("grep", "Search file contents with a regex.", json!({"type": "object", "properties": {"pattern": {"type": "string"}, "path": {"type": "string"}}})));
    }
    HeaderDetail {
        header_id: id,
        reason: Some(if id == 1 { HeaderReason::Initial } else { HeaderReason::Change }),
        system_prompt: "You are an expert coding assistant operating inside pi, a coding agent harness.\n\n\
Available tools:\n- read: Read file contents\n- bash: Execute bash commands\n- edit: Make surgical edits to files\n- write: Create or overwrite files\n\n\
Guidelines:\n- Use read to examine files before editing.\n- Use edit for precise changes (old text must match exactly).\n- Be concise in your responses.\n\n\
Current working directory: /Users/brijesh/projects/space"
            .into(),
        tools,
        options: RequestOptions {
            provider: "claude-cli".into(),
            model: "claude-opus-5-5".into(),
            max_tokens: Some(32_000),
            thinking: Some(json!({"type": "adaptive"})),
            effort: Some("medium".into()),
            tool_choice: None,
            temperature: None,
        },
    }
}

/// The detail the inspector shows for `id` in [`page`].
pub fn detail(id: &str) -> TrajectoryDetail {
    let p = page();
    let request_detail = |n: u32| {
        let summary = p.requests.iter().find(|r| r.number == n).cloned().unwrap_or_default();
        let t = summary.first_token_ms.unwrap_or(summary.started_ms);
        RequestDetail {
            header_id: if n >= 4 { 2 } else { 1 },
            options: header(1, false).options,
            messages: vec![json!({"role": "user", "content": [{"type": "text", "text": "The login test fails since the redirect change. Can you fix it?"}]})],
            rewritten: false,
            chunks: vec![
                ChunkRun { stream: ChunkKind::Thinking, t0: t, dt: vec![30, 28, 40, 35] },
                ChunkRun { stream: ChunkKind::Text, t0: t + 140, dt: vec![12, 15, 9, 14, 11, 18, 10, 13] },
            ],
            retries: if n == 2 { vec![RetryDetail { at: summary.started_ms + 900, attempt: 1, max: 2, delay_ms: 2000, error: "overloaded (529)".into() }] } else { vec![] },
            response_id: Some(format!("msg_01ABC{n}")),
            stop_reason: Some(if summary.tools > 0 { "toolUse".into() } else { "stop".into() }),
            summary,
        }
    };
    let raw = vec![TraceEvent::TurnStart { turn: 1, at: T0 }];
    match id {
        "sys-1" => TrajectoryDetail { id: id.into(), header: Some(header(1, false)), raw, ..Default::default() },
        "sys-2" => TrajectoryDetail { id: id.into(), header: Some(header(2, true)), previous_header: Some(header(1, false)), raw, ..Default::default() },
        "u-1" => TrajectoryDetail {
            id: id.into(),
            text: Some("The login test fails since the redirect change. Can you fix it?".into()),
            images: vec!["/nonexistent/screenshot.png".into()],
            raw,
            ..Default::default()
        },
        "c-1" => TrajectoryDetail { id: id.into(), text: Some("Permission mode: Workspace Write. Every tool call is checked by a decision model first.".into()), raw, ..Default::default() },
        "t-1" | "t-3" | "t-4" => TrajectoryDetail {
            id: id.into(),
            input: Some(if id == "t-1" { json!({"command": "cargo test -p space-core"}) } else { json!({"path": "crates/space-core/tests/login.rs", "oldText": "/login", "newText": "/login?next=/home"}) }),
            output: Some(if id == "t-1" {
                "running 6 tests\ntest login_redirects ... FAILED\ntest login_keeps_session ... ok\n\nfailures:\n  expected \"/login\", got \"/login?next=/home\"\n\ntest result: FAILED. 5 passed; 1 failed".into()
            } else {
                "{\"ok\": true, \"replacements\": 1}".into()
            }),
            guard: Some(GuardDetail {
                plugin: Some("permissions".into()),
                mode: "workspace-write".into(),
                verdict: Some(if id == "t-4" { GuardVerdict::Deny } else { GuardVerdict::Allow }),
                reason: Some(if id == "t-4" { "Searches outside the project folder.".into() } else { "Edits a test inside the project.".into() }),
                held_ms: if id == "t-3" { 2600 } else { 0 },
            }),
            request: Some(request_detail(if id == "t-4" { 4 } else if id == "t-1" { 1 } else { 2 })),
            raw,
            ..Default::default()
        },
        _ => {
            let n: u32 = id.strip_prefix("request:").and_then(|n| n.parse().ok()).or_else(|| p.rows.iter().find(|r| r.id == id).and_then(|r| r.request)).unwrap_or(1);
            TrajectoryDetail {
                id: id.into(),
                text: p.rows.iter().find(|r| r.id == id).map(|r| r.preview.clone()),
                thinking: Some("The failure says the target changed. Check the test's expected value against the new redirect.".into()),
                request: Some(request_detail(n)),
                raw,
                ..Default::default()
            }
        }
    }
}

fn view(id: &'static str, selected: Option<&str>, mode: DurationMode, range: Option<TimeRange>, window: &mut Window, cx: &mut App) -> AnyElement {
    let scroll = window.use_keyed_state(ElementId::Name(format!("{id}-scroll").into()), cx, |_, _| LedgerScroll::new()).read(cx).clone();
    let input = window.use_keyed_state(ElementId::Name(format!("{id}-search").into()), cx, |w, cx| InputState::new(w, cx).placeholder("Search"));
    let sel = selected.map(|s| SharedString::from(s.to_string()));
    let det = selected.map(|s| Rc::new(detail(s)));
    div()
        .h(px(560.))
        .child(
            TrajectoryView::new(Rc::new(page()), &scroll)
                .selected(sel, det)
                .search(&input, None)
                .mode(mode)
                .range(range)
                .now(T0 + 606_000),
        )
        .into_any_element()
}

fn inspector_example(name: &'static str, id: &'static str) -> Example {
    Example::new(name, move |window, cx| {
        let p = page();
        let row = p.rows.iter().find(|r| r.id == id).cloned();
        let request = id.strip_prefix("request:").and_then(|n| n.parse::<u32>().ok()).and_then(|n| p.requests.iter().find(|r| r.number == n).cloned());
        let d = detail(id);
        div()
            .h(px(520.))
            .flex()
            .child(inspector::inspector(
                inspector::InspectorData { selected: id.into(), row: row.as_ref(), request: request.as_ref(), detail: Some(&d), now_ms: T0 + 606_000 },
                inspector::InspectorHandlers {
                    on_close: Rc::new(|_, _| {}),
                    on_copy: Rc::new(|_, _, _| {}),
                    on_select: Rc::new(|_, _, _| {}),
                    on_open_image: Rc::new(|_, _, _, _| {}),
                },
                window,
                cx,
            ))
            .into_any_element()
    })
}

pub fn previews() -> Vec<ComponentPreview> {
    vec![
        ComponentPreview::new(Area::Trajectory, "Trajectory view", "The whole tab: toolbar, timing overview, ledger, inspector.").stacked().examples([
            Example::new("ledger", |w, cx| view("tv-plain", None, DurationMode::Sequence, None, w, cx)),
            Example::new("assistant selected (duration mode)", |w, cx| view("tv-sel", Some("a-2"), DurationMode::Duration, None, w, cx)),
            Example::new("time filter", |w, cx| {
                view("tv-range", None, DurationMode::Duration, Some(TimeRange { from_ms: T0 + 9_000, to_ms: T0 + 15_000 }), w, cx)
            }),
        ]),
        ComponentPreview::new(Area::Trajectory, "Trajectory inspector", "Tabs by kind; the tab stays when moving between rows of one kind.")
            .examples([
                inspector_example("system", "sys-1"),
                inspector_example("system (changed)", "sys-2"),
                inspector_example("request", "request:2"),
                inspector_example("assistant", "a-2"),
                inspector_example("tool (held)", "t-3"),
                inspector_example("tool (denied)", "t-4"),
                inspector_example("user", "u-1"),
                inspector_example("context", "c-1"),
            ]),
        ComponentPreview::new(Area::Trajectory, "Trajectory tabs and context panel", "Chat | Trajectory under the title bar; the context ring's panel with the thread's totals.").stacked().examples([
            Example::new("tabs", |_, _| view_tabs(ViewTab::Trajectory, Rc::new(|_, _, _| {})).into_any_element()).width(px(480.)),
            Example::new("context panel", |_, _| {
                crate::components::composer::context_panel(182_340, Some(1_000_000), Some(&page().stats)).into_any_element()
            }),
            Example::new("context panel (no trajectory)", |_, _| {
                crate::components::composer::context_panel(182_340, Some(1_000_000), None).into_any_element()
            }),
        ]),
    ]
}
