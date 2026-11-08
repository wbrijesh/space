//! A thread's trace events, turned into the rows, requests and details the Trajectory view
//! shows. Pure: events in, [`Projection`] out.
//!
//! A thread can have several sessions (a new one after the agent restarted, or after loading
//! failed); they're laid out one after the other, in the order they started. Turns and requests
//! are numbered across the thread; row ids (`<session>:<event index>`) stay the same as more
//! events arrive.

use std::collections::HashMap;

use space_plugin::protocol::trace::*;

/// Longest preview, in characters.
const PREVIEW: usize = 512;

/// One session's events, in the order they arrived.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionLog {
    pub id: String,
    pub events: Vec<TraceEvent>,
}

impl SessionLog {
    /// When the session started (its first timed event), for ordering sessions.
    pub fn started(&self) -> u64 {
        self.events.iter().find_map(event_at).unwrap_or(u64::MAX)
    }
}

/// What a row shows in full (looked up for `trajectory/detail`).
#[derive(Debug, Clone, PartialEq)]
enum RowData {
    System { header: usize },
    User { text: String, images: Vec<String> },
    Context { text: String },
    Assistant { request: usize },
    Tool { call: usize },
}

/// A request as the projection tracks it.
#[derive(Debug, Clone, Default)]
struct Request {
    summary: RequestSummary,
    header: Option<usize>,
    messages: Vec<serde_json::Value>,
    rewritten: bool,
    chunks: Vec<ChunkRun>,
    retries: Vec<RetryDetail>,
    response_id: Option<String>,
    stop_reason: Option<String>,
    text: String,
    thinking: String,
    /// (session, event) of the events behind it.
    raw: Vec<(usize, usize)>,
}

#[derive(Debug, Clone, Default)]
struct Call {
    input: serde_json::Value,
    output: Option<String>,
    truncated: bool,
    guard: Option<GuardDetail>,
    raw: Vec<(usize, usize)>,
}

/// A thread's trajectory.
#[derive(Debug, Clone, Default)]
pub struct Projection {
    pub rows: Vec<TrajectoryRow>,
    data: Vec<RowData>,
    /// Lowercased searchable text, one per row.
    haystacks: Vec<String>,
    raw: Vec<Vec<(usize, usize)>>,
    headers: Vec<HeaderDetail>,
    requests: Vec<Request>,
    calls: Vec<Call>,
    pub stats: TrajectoryStats,
}

impl Projection {
    /// Lay out `sessions` (already in order).
    pub fn build(sessions: &[SessionLog]) -> Self {
        let mut p = Projection::default();
        let mut turns_seen = 0u32;
        for (si, session) in sessions.iter().enumerate() {
            // Per session: its header ids and request numbers → ours; its calls → ours.
            let mut headers: HashMap<u32, usize> = HashMap::new();
            let mut requests: HashMap<u32, usize> = HashMap::new();
            let mut calls: HashMap<String, usize> = HashMap::new();
            let mut guards: HashMap<String, (GuardDetail, (usize, usize))> = HashMap::new();
            let mut turn = turns_seen;
            for (ei, event) in session.events.iter().enumerate() {
                let id = format!("{}:{ei}", session.id);
                let at = (si, ei);
                match event {
                    TraceEvent::TurnStart { .. } => {
                        turns_seen += 1;
                        turn = turns_seen;
                    }
                    TraceEvent::TurnEnd { reason, .. } => {
                        // What still runs when the turn ends was stopped or failed.
                        let status = if reason == "error" { RecordStatus::Failed } else { RecordStatus::Cancelled };
                        for (row, data) in p.rows.iter_mut().zip(&p.data) {
                            if row.turn == turn && row.status == RecordStatus::Running {
                                row.status = status;
                                if let RowData::Assistant { request } = data {
                                    p.requests[*request].summary.status = status;
                                }
                            }
                        }
                    }
                    TraceEvent::Header { header_id, reason, system_prompt, tools, options } => {
                        let previous = p.headers.last().cloned();
                        let detail = HeaderDetail {
                            header_id: *header_id,
                            reason: Some(*reason),
                            system_prompt: system_prompt.clone(),
                            tools: tools.clone(),
                            options: options.clone(),
                        };
                        let (preview, label) = header_preview(&detail, previous.as_ref());
                        p.headers.push(detail);
                        let hi = p.headers.len() - 1;
                        headers.insert(*header_id, hi);
                        let haystack = format!("{system_prompt}\n{}", serde_json::to_string(tools).unwrap_or_default());
                        let row = TrajectoryRow { id, kind: RecordKind::System, turn, preview, label, ..Default::default() };
                        p.push(row, RowData::System { header: hi }, haystack, vec![at]);
                    }
                    TraceEvent::Prompt { at: when, text, files, images } => {
                        let row = TrajectoryRow {
                            id,
                            kind: RecordKind::User,
                            turn: turns_seen + 1,
                            preview: preview(text),
                            started_ms: Some(*when),
                            images: images.len() as u32,
                            files: files.len() as u32,
                            ..Default::default()
                        };
                        let haystack = format!("{text}\n{}\n{}", files.join("\n"), images.join("\n"));
                        p.push(row, RowData::User { text: text.clone(), images: images.clone() }, haystack, vec![at]);
                    }
                    TraceEvent::Context { at: when, sections } => {
                        for (i, section) in sections.iter().enumerate() {
                            let row = TrajectoryRow {
                                id: format!("{id}.{i}"),
                                kind: RecordKind::Context,
                                turn: turns_seen + 1,
                                preview: preview(&section.text),
                                label: Some(section.plugin.clone()),
                                started_ms: Some(*when),
                                ..Default::default()
                            };
                            let haystack = format!("{} {}\n{}", section.plugin, section.id, section.text);
                            p.push(row, RowData::Context { text: section.text.clone() }, haystack, vec![at]);
                        }
                    }
                    TraceEvent::Request { request, step, at: when, header_id, messages, rewritten, .. } => {
                        let number = p.requests.len() as u32 + 1;
                        let summary = RequestSummary {
                            number,
                            turn,
                            step: *step,
                            started_ms: *when,
                            status: RecordStatus::Running,
                            model: headers.get(header_id).map(|h| p.headers[*h].options.model.clone()),
                            ..Default::default()
                        };
                        p.requests.push(Request {
                            summary,
                            header: headers.get(header_id).copied(),
                            messages: messages.clone(),
                            rewritten: *rewritten,
                            raw: vec![at],
                            ..Default::default()
                        });
                        let ri = p.requests.len() - 1;
                        requests.insert(*request, ri);
                        let row = TrajectoryRow {
                            id,
                            kind: RecordKind::Assistant,
                            turn,
                            step: Some(*step),
                            request: Some(number),
                            preview: String::new(),
                            started_ms: Some(*when),
                            status: RecordStatus::Running,
                            ..Default::default()
                        };
                        p.push(row, RowData::Assistant { request: ri }, String::new(), vec![at]);
                    }
                    TraceEvent::FirstToken { request, at: when } => {
                        if let Some(&ri) = requests.get(request) {
                            p.requests[ri].summary.first_token_ms = Some(*when);
                            p.requests[ri].raw.push(at);
                            p.with_assistant(ri, |row| row.first_token_ms = Some(*when));
                        }
                    }
                    TraceEvent::Chunks { request, stream, t0, dt } => {
                        if let Some(&ri) = requests.get(request) {
                            p.requests[ri].chunks.push(ChunkRun { stream: *stream, t0: *t0, dt: dt.clone() });
                        }
                    }
                    TraceEvent::Retry { request, at: when, attempt, max, delay_ms, error } => {
                        if let Some(&ri) = requests.get(request) {
                            let r = &mut p.requests[ri];
                            r.summary.retries += 1;
                            r.retries.push(RetryDetail { at: *when, attempt: *attempt, max: *max, delay_ms: *delay_ms, error: error.clone() });
                            r.raw.push(at);
                        }
                    }
                    TraceEvent::Response { request, at: when, duration_ms, usage, stop_reason, response_id, model, text, thinking, error } => {
                        let Some(&ri) = requests.get(request) else { continue };
                        let status = match (stop_reason.as_str(), error) {
                            ("aborted", _) => RecordStatus::Cancelled,
                            ("error", _) | (_, Some(_)) => RecordStatus::Failed,
                            _ => RecordStatus::Done,
                        };
                        let r = &mut p.requests[ri];
                        r.summary.ended_ms = Some(*when);
                        r.summary.status = status;
                        r.summary.usage.clone_from(usage);
                        r.summary.error.clone_from(error);
                        if model.is_some() {
                            r.summary.model.clone_from(model);
                        }
                        r.response_id.clone_from(response_id);
                        r.stop_reason = Some(stop_reason.clone());
                        r.text.clone_from(text);
                        r.thinking.clone_from(thinking);
                        r.raw.push(at);
                        let reply = if !text.trim().is_empty() {
                            preview(text)
                        } else if let Some(e) = error {
                            preview(e)
                        } else if !thinking.trim().is_empty() {
                            "(thinking only)".to_string()
                        } else {
                            "(no text)".to_string()
                        };
                        let tokens = usage.as_ref().map(|u| u.output);
                        let haystack = format!("{text}\n{thinking}\n{}", error.clone().unwrap_or_default()).to_lowercase();
                        p.with_assistant(ri, |row| {
                            row.preview = reply;
                            row.duration_ms = Some(*duration_ms);
                            row.status = status;
                            row.tokens = tokens;
                        });
                        if let Some(i) = p.assistant_row(ri) {
                            p.haystacks[i] = format!("{}\n{haystack}", p.rows[i].preview.to_lowercase());
                        }
                    }
                    TraceEvent::ToolStart { request, call_id, name, input, at: when } => {
                        let ri = requests.get(request).copied();
                        let (number, step) = ri.map_or((None, None), |ri| (Some(p.requests[ri].summary.number), Some(p.requests[ri].summary.step)));
                        if let Some(ri) = ri {
                            p.requests[ri].summary.tools += 1;
                        }
                        let mut call = Call { input: input.clone(), raw: vec![at], ..Default::default() };
                        let mut row = TrajectoryRow {
                            id,
                            kind: RecordKind::Tool,
                            turn,
                            step,
                            request: number,
                            preview: preview(&format!("{name} · {}", args_summary(input))),
                            label: Some(name.clone()),
                            started_ms: Some(*when),
                            status: RecordStatus::Running,
                            ..Default::default()
                        };
                        if let Some((guard, raw)) = guards.remove(call_id) {
                            row.guard = guard.verdict;
                            row.held_ms = guard.held_ms;
                            call.guard = Some(guard);
                            call.raw.push(raw);
                        }
                        p.calls.push(call);
                        let ci = p.calls.len() - 1;
                        calls.insert(call_id.clone(), ci);
                        let haystack = format!("{name} {call_id}\n{input}");
                        p.push(row, RowData::Tool { call: ci }, haystack, vec![at]);
                    }
                    TraceEvent::ToolEnd { call_id, at: when, duration_ms, output, is_error, truncated, .. } => {
                        let Some(&ci) = calls.get(call_id) else { continue };
                        let call = &mut p.calls[ci];
                        call.output = Some(output.clone());
                        call.truncated = *truncated;
                        call.raw.push(at);
                        if let Some(i) = p.tool_row(ci) {
                            let row = &mut p.rows[i];
                            row.duration_ms = Some(row.started_ms.map_or(*duration_ms, |s| when.saturating_sub(s).max(*duration_ms)));
                            row.status = if *is_error { RecordStatus::Failed } else { RecordStatus::Done };
                            let result = output.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
                            row.preview = preview(&format!("{} → {result}", row.preview));
                            p.haystacks[i].push('\n');
                            p.haystacks[i].push_str(&output.to_lowercase());
                        }
                    }
                    TraceEvent::Guard { call_id, plugin, mode, verdict, reason, held_ms, .. } => {
                        let guard = GuardDetail {
                            plugin: plugin.clone(),
                            mode: mode.clone(),
                            verdict: Some(*verdict),
                            reason: reason.clone(),
                            held_ms: *held_ms,
                        };
                        match calls.get(call_id) {
                            Some(&ci) => {
                                if let Some(i) = p.tool_row(ci) {
                                    p.rows[i].guard = Some(*verdict);
                                    p.rows[i].held_ms = *held_ms;
                                }
                                p.calls[ci].guard = Some(guard);
                                p.calls[ci].raw.push(at);
                            }
                            // The pipeline decides before the tool starts.
                            None => {
                                guards.insert(call_id.clone(), (guard, at));
                            }
                        }
                    }
                }
            }
        }
        p.stats = stats(turns_seen, &p.requests);
        p
    }

    fn push(&mut self, row: TrajectoryRow, data: RowData, haystack: String, raw: Vec<(usize, usize)>) {
        let haystack = format!("{} {}\n{haystack}", row.preview, row.label.clone().unwrap_or_default()).to_lowercase();
        self.rows.push(row);
        self.data.push(data);
        self.haystacks.push(haystack);
        self.raw.push(raw);
    }

    fn assistant_row(&self, request: usize) -> Option<usize> {
        self.data.iter().rposition(|d| *d == RowData::Assistant { request })
    }

    fn tool_row(&self, call: usize) -> Option<usize> {
        self.data.iter().rposition(|d| *d == RowData::Tool { call })
    }

    fn with_assistant(&mut self, request: usize, f: impl FnOnce(&mut TrajectoryRow)) {
        if let Some(i) = self.assistant_row(request) {
            f(&mut self.rows[i]);
        }
    }

    /// Indices of the rows matching every word of `query` (any case).
    pub fn search(&self, query: &str) -> Vec<usize> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        (0..self.rows.len()).filter(|&i| words.iter().all(|w| self.haystacks[i].contains(w.as_str()))).collect()
    }

    /// The requests the given rows belong to.
    pub fn requests_of(&self, rows: &[TrajectoryRow]) -> Vec<RequestSummary> {
        let mut numbers: Vec<u32> = rows.iter().filter_map(|r| r.request).collect();
        numbers.sort_unstable();
        numbers.dedup();
        numbers.into_iter().filter_map(|n| self.requests.get(n as usize - 1)).map(|r| r.summary.clone()).collect()
    }

    /// Everything the inspector shows for row `id` or `request:<n>`.
    pub fn detail(&self, id: &str, sessions: &[SessionLog]) -> Option<TrajectoryDetail> {
        let raw = |refs: &[(usize, usize)]| -> Vec<TraceEvent> {
            refs.iter().filter_map(|(s, e)| sessions.get(*s).and_then(|s| s.events.get(*e)).cloned()).collect()
        };
        if let Some(n) = id.strip_prefix("request:").and_then(|n| n.parse::<usize>().ok()) {
            let r = self.requests.get(n.checked_sub(1)?)?;
            let mut detail = self.request_detail(n - 1);
            detail.id = id.to_string();
            detail.raw = raw(&r.raw);
            return Some(detail);
        }
        let i = self.rows.iter().position(|r| r.id == id)?;
        let mut detail = match &self.data[i] {
            RowData::System { header } => TrajectoryDetail {
                header: self.headers.get(*header).cloned(),
                previous_header: header.checked_sub(1).and_then(|h| self.headers.get(h)).cloned(),
                ..Default::default()
            },
            RowData::User { text, images } => {
                TrajectoryDetail { text: Some(text.clone()), images: images.clone(), ..Default::default() }
            }
            RowData::Context { text } => TrajectoryDetail { text: Some(text.clone()), ..Default::default() },
            RowData::Assistant { request } => {
                let mut d = self.request_detail(*request);
                d.raw = raw(&self.requests[*request].raw);
                d
            }
            RowData::Tool { call } => {
                let c = &self.calls[*call];
                let request = self.rows[i].request.map(|n| self.request_detail(n as usize - 1).request).unwrap_or_default();
                TrajectoryDetail {
                    input: Some(c.input.clone()),
                    output: c.output.clone(),
                    truncated: c.truncated,
                    guard: c.guard.clone(),
                    request,
                    raw: raw(&c.raw),
                    ..Default::default()
                }
            }
        };
        detail.id = id.to_string();
        if detail.raw.is_empty() {
            detail.raw = raw(&self.raw[i]);
        }
        Some(detail)
    }

    fn request_detail(&self, ri: usize) -> TrajectoryDetail {
        let Some(r) = self.requests.get(ri) else { return TrajectoryDetail::default() };
        let header = r.header.and_then(|h| self.headers.get(h));
        TrajectoryDetail {
            text: Some(r.text.clone()).filter(|t| !t.is_empty()),
            thinking: Some(r.thinking.clone()).filter(|t| !t.is_empty()),
            request: Some(RequestDetail {
                summary: r.summary.clone(),
                header_id: header.map_or(0, |h| h.header_id),
                options: header.map(|h| h.options.clone()).unwrap_or_default(),
                messages: r.messages.clone(),
                rewritten: r.rewritten,
                chunks: r.chunks.clone(),
                retries: r.retries.clone(),
                response_id: r.response_id.clone(),
                stop_reason: r.stop_reason.clone(),
            }),
            ..Default::default()
        }
    }
}

/// A header row's preview and label: what changed since the previous header.
fn header_preview(header: &HeaderDetail, previous: Option<&HeaderDetail>) -> (String, Option<String>) {
    let Some(previous) = previous else { return ("Initial system prompt".into(), None) };
    let prompt = header.system_prompt != previous.system_prompt;
    let tools = header.tools != previous.tools;
    let options = header.options != previous.options;
    let label = match (prompt, tools) {
        (true, true) => Some("prompt+tools"),
        (true, false) => Some("prompt"),
        (false, true) => Some("tools"),
        (false, false) if options => Some("options"),
        _ => None,
    };
    let preview = match (header.reason, label) {
        (Some(HeaderReason::Resume), _) => "System prompt (session loaded again)".to_string(),
        (_, Some(what)) => format!("System prompt changed ({})", what.replace('+', " and ")),
        _ => "System prompt".to_string(),
    };
    (preview, label.map(str::to_string))
}

/// One line of `text`, at most [`PREVIEW`] characters.
fn preview(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= PREVIEW { line } else { format!("{}…", line.chars().take(PREVIEW - 1).collect::<String>()) }
}

/// The interesting part of a tool's input: its command, path, pattern or URL, else its JSON.
fn args_summary(input: &serde_json::Value) -> String {
    ["command", "path", "file_path", "pattern", "query", "url"]
        .iter()
        .find_map(|k| input.get(*k).and_then(|v| v.as_str()))
        .map_or_else(|| input.to_string(), str::to_string)
}

fn event_at(e: &TraceEvent) -> Option<u64> {
    match e {
        TraceEvent::TurnStart { at, .. }
        | TraceEvent::TurnEnd { at, .. }
        | TraceEvent::Request { at, .. }
        | TraceEvent::FirstToken { at, .. }
        | TraceEvent::Response { at, .. }
        | TraceEvent::Retry { at, .. }
        | TraceEvent::ToolStart { at, .. }
        | TraceEvent::ToolEnd { at, .. }
        | TraceEvent::Prompt { at, .. }
        | TraceEvent::Context { at, .. }
        | TraceEvent::Guard { at, .. } => Some(*at),
        TraceEvent::Chunks { t0, .. } => Some(*t0),
        TraceEvent::Header { .. } => None,
    }
}

/// The thread's totals.
fn stats(turns: u32, requests: &[Request]) -> TrajectoryStats {
    let mut s = TrajectoryStats { turns, steps: requests.len() as u32, ..Default::default() };
    let (mut read, mut write) = (None::<u64>, None::<u64>);
    for r in requests {
        let Some(u) = &r.summary.usage else { continue };
        s.usage.input += u.input;
        s.usage.output += u.output;
        if let Some(c) = u.cache_read {
            *read.get_or_insert(0) += c;
        }
        if let Some(c) = u.cache_write {
            *write.get_or_insert(0) += c;
        }
        if let Some(r) = u.reasoning {
            *s.usage.reasoning.get_or_insert(0) += r;
        }
        for m in &u.models {
            if !s.usage.models.contains(m) {
                s.usage.models.push(m.clone());
            }
        }
        if let (Some(first), Some(end)) = (r.summary.first_token_ms, r.summary.ended_ms) {
            s.generated_tokens += u.output;
            s.generation_ms += end.saturating_sub(first);
        }
    }
    s.usage.cache_read = read;
    s.usage.cache_write = write;
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use space_plugin::protocol::TurnUsage;

    fn usage(input: u64, output: u64, read: u64, write: u64) -> Option<TurnUsage> {
        Some(TurnUsage { input, output, cache_read: Some(read), cache_write: Some(write), reasoning: None, models: vec!["claude-cli/m".into()] })
    }

    fn header(id: u32, reason: HeaderReason, prompt: &str, tools: &[&str]) -> TraceEvent {
        TraceEvent::Header {
            header_id: id,
            reason,
            system_prompt: prompt.into(),
            tools: tools.iter().map(|t| ToolDef { name: (*t).into(), ..Default::default() }).collect(),
            options: RequestOptions { provider: "claude-cli".into(), model: "m".into(), ..Default::default() },
        }
    }

    fn response(request: u32, at: u64, text: &str, stop: &str, out: u64) -> TraceEvent {
        TraceEvent::Response {
            request,
            at,
            duration_ms: at - 1000,
            usage: usage(10, out, 100, 5),
            stop_reason: stop.into(),
            response_id: Some(format!("resp-{request}")),
            model: Some("claude-cli/m".into()),
            text: text.into(),
            thinking: String::new(),
            error: None,
        }
    }

    /// A turn with a tool call (guarded before it starts) and a final answer.
    pub(crate) fn tool_turn() -> SessionLog {
        let events = vec![
            TraceEvent::Prompt { at: 1000, text: "fix the test".into(), files: vec![], images: vec!["/a.png".into()] },
            TraceEvent::Context {
                at: 1001,
                sections: vec![ContextFact { plugin: "permissions".into(), id: "mode".into(), text: "Workspace Write".into() }],
            },
            TraceEvent::TurnStart { turn: 1, at: 1002 },
            header(1, HeaderReason::Initial, "You are pi.", &["bash", "read"]),
            TraceEvent::Request { request: 1, turn: 1, step: 1, at: 1100, header_id: 1, messages: vec![json!({"role":"user"})], rewritten: false },
            TraceEvent::FirstToken { request: 1, at: 1500 },
            TraceEvent::Chunks { request: 1, stream: ChunkKind::Text, t0: 1500, dt: vec![10, 10] },
            response(1, 2000, "Let me run the tests.", "toolUse", 20),
            TraceEvent::Guard {
                call_id: "c1".into(),
                at: 2001,
                plugin: None,
                mode: "workspace-write".into(),
                verdict: GuardVerdict::Allow,
                reason: None,
                held_ms: 40,
            },
            TraceEvent::ToolStart { request: 1, call_id: "c1".into(), name: "bash".into(), input: json!({"command":"cargo test"}), at: 2050 },
            TraceEvent::ToolEnd {
                request: 1,
                call_id: "c1".into(),
                at: 2550,
                duration_ms: 500,
                output: "1 failed\nmore".into(),
                is_error: false,
                truncated: false,
            },
            TraceEvent::Request { request: 2, turn: 1, step: 2, at: 2600, header_id: 1, messages: vec![], rewritten: false },
            TraceEvent::FirstToken { request: 2, at: 3000 },
            response(2, 3200, "Fixed it.", "stop", 30),
            TraceEvent::TurnEnd { turn: 1, at: 3201, reason: "end_turn".into() },
        ];
        SessionLog { id: "s1".into(), events }
    }

    #[test]
    fn lays_out_a_turn_with_a_tool() {
        let p = Projection::build(&[tool_turn()]);
        let kinds: Vec<_> = p.rows.iter().map(|r| (r.kind, r.turn, r.request, r.status)).collect();
        use RecordKind::*;
        let done = RecordStatus::Done;
        assert_eq!(
            kinds,
            [
                (User, 1, None, done),
                (Context, 1, None, done),
                (System, 1, None, done),
                (Assistant, 1, Some(1), done),
                (Tool, 1, Some(1), done),
                (Assistant, 1, Some(2), done),
            ]
        );
        assert_eq!((p.rows[0].images, p.rows[1].label.as_deref(), p.rows[2].preview.as_str()), (1, Some("permissions"), "Initial system prompt"));
        assert_eq!(p.rows[3].preview, "Let me run the tests.");
        assert_eq!((p.rows[3].first_token_ms, p.rows[3].tokens), (Some(1500), Some(20)));
        let tool = &p.rows[4];
        assert_eq!(tool.preview, "bash · cargo test → 1 failed");
        assert_eq!((tool.guard, tool.held_ms, tool.step, tool.duration_ms), (Some(GuardVerdict::Allow), 40, Some(1), Some(500)));
        // Totals: two requests, usage summed, generation from first token to end.
        let s = &p.stats;
        assert_eq!((s.turns, s.steps, s.usage.input, s.usage.output, s.usage.cache_read), (1, 2, 20, 50, Some(200)));
        assert_eq!((s.generated_tokens, s.generation_ms), (50, 500 + 200));
        let reqs = p.requests_of(&p.rows);
        assert_eq!((reqs.len(), reqs[0].tools, reqs[1].step), (2, 1, 2));
    }

    #[test]
    fn details_carry_the_request_header_and_raw_events() {
        let sessions = [tool_turn()];
        let p = Projection::build(&sessions);
        let assistant = p.detail("s1:4", &sessions).unwrap();
        let req = assistant.request.unwrap();
        assert_eq!((req.summary.number, req.options.model.as_str(), req.chunks.len()), (1, "m", 1));
        assert_eq!(assistant.text.as_deref(), Some("Let me run the tests."));
        assert_eq!(assistant.raw.len(), 3, "request, first token, response");
        let system = p.detail("s1:3", &sessions).unwrap();
        assert_eq!(system.header.unwrap().tools.len(), 2);
        assert!(system.previous_header.is_none());
        let tool = p.detail("s1:9", &sessions).unwrap();
        assert_eq!((tool.output.as_deref(), tool.guard.unwrap().mode.as_str()), (Some("1 failed\nmore"), "workspace-write"));
        assert_eq!(tool.raw.len(), 3, "start, end and the guard");
        let by_number = p.detail("request:2", &sessions).unwrap();
        assert_eq!(by_number.request.unwrap().summary.number, 2);
        assert!(p.detail("request:9", &sessions).is_none() && p.detail("nope", &sessions).is_none());
    }

    #[test]
    fn retries_failures_and_cancelled_turns() {
        let events = vec![
            TraceEvent::TurnStart { turn: 1, at: 1 },
            header(1, HeaderReason::Initial, "p", &[]),
            TraceEvent::Request { request: 1, turn: 1, step: 1, at: 10, header_id: 1, messages: vec![], rewritten: false },
            TraceEvent::Retry { request: 1, at: 20, attempt: 1, max: 2, delay_ms: 1000, error: "overloaded".into() },
            TraceEvent::Response {
                request: 1,
                at: 30,
                duration_ms: 20,
                usage: None,
                stop_reason: "error".into(),
                response_id: None,
                model: None,
                text: String::new(),
                thinking: String::new(),
                error: Some("overloaded again".into()),
            },
            TraceEvent::TurnEnd { turn: 1, at: 31, reason: "error".into() },
            TraceEvent::TurnStart { turn: 2, at: 40 },
            TraceEvent::Request { request: 2, turn: 2, step: 1, at: 41, header_id: 1, messages: vec![], rewritten: false },
            TraceEvent::TurnEnd { turn: 2, at: 50, reason: "cancelled".into() },
        ];
        let p = Projection::build(&[SessionLog { id: "s".into(), events }]);
        let a: Vec<_> = p.rows.iter().filter(|r| r.kind == RecordKind::Assistant).map(|r| (r.status, r.preview.clone())).collect();
        assert_eq!(a, [(RecordStatus::Failed, "overloaded again".to_string()), (RecordStatus::Cancelled, String::new())]);
        let reqs = p.requests_of(&p.rows);
        assert_eq!((reqs[0].retries, reqs[0].status, reqs[1].status), (1, RecordStatus::Failed, RecordStatus::Cancelled));
        assert_eq!(p.stats.turns, 2);
    }

    #[test]
    fn header_changes_say_what_changed_and_keep_the_previous_one() {
        let events = vec![
            header(1, HeaderReason::Initial, "p", &["bash"]),
            header(2, HeaderReason::Change, "p2", &["bash"]),
            header(3, HeaderReason::Change, "p2", &["bash", "read"]),
        ];
        let sessions = [SessionLog { id: "s".into(), events }];
        let p = Projection::build(&sessions);
        let labels: Vec<_> = p.rows.iter().map(|r| (r.preview.as_str(), r.label.as_deref())).collect();
        assert_eq!(
            labels,
            [("Initial system prompt", None), ("System prompt changed (prompt)", Some("prompt")), ("System prompt changed (tools)", Some("tools"))]
        );
        let d = p.detail("s:1", &sessions).unwrap();
        assert_eq!((d.header.unwrap().system_prompt.as_str(), d.previous_header.unwrap().system_prompt.as_str()), ("p2", "p"));
    }

    #[test]
    fn sessions_of_a_thread_number_turns_and_requests_across_them() {
        let mut second = tool_turn();
        second.id = "s2".into();
        let p = Projection::build(&[tool_turn(), second]);
        let last = p.rows.last().unwrap();
        assert_eq!((last.turn, last.request, last.id.as_str()), (2, Some(4), "s2:11"));
        assert_eq!(p.rows.iter().filter(|r| r.kind == RecordKind::User).map(|r| r.turn).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(p.stats.steps, 4);
    }

    #[test]
    fn search_matches_every_word_in_any_part() {
        let p = Projection::build(&[tool_turn()]);
        let ids = |q: &str| p.search(q).into_iter().map(|i| p.rows[i].id.clone()).collect::<Vec<_>>();
        assert_eq!(ids("cargo FAILED"), ["s1:9"]);
        assert_eq!(ids("more"), ["s1:9"], "the full output, not just the preview");
        assert_eq!(ids("workspace"), ["s1:1.0"]);
        assert_eq!(ids("you are pi"), ["s1:3"]);
        assert!(ids("nothing-like-this").is_empty());
    }
}
