//! ACP wire types → Space session model.

use space_protocol::acp_schema as acp;

use crate::session::{
    PermissionKind, PermissionOption, PlanEntry, SessionEvent, ToolCallInfo, ToolCallPatch, ToolStatus,
};

/// Max characters of tool output kept per tool call.
const MAX_TOOL_OUTPUT: usize = 4000;

pub(crate) fn session_update(update: acp::SessionUpdate) -> Option<SessionEvent> {
    use acp::SessionUpdate as U;
    Some(match update {
        U::AgentMessageChunk(chunk) => SessionEvent::AgentText { text: content_text(&chunk.content) },
        U::AgentThoughtChunk(chunk) => SessionEvent::AgentThought { text: content_text(&chunk.content) },
        U::ToolCall(call) => SessionEvent::ToolCall(tool_call(call)),
        U::ToolCallUpdate(update) => SessionEvent::ToolCallUpdate(tool_call_patch(update)),
        U::Plan(plan) => SessionEvent::Plan(
            plan.entries
                .into_iter()
                .map(|e| PlanEntry { content: e.content, status: wire_str(&e.status) })
                .collect(),
        ),
        // User message replays, mode, command and usage updates are not shown in v0.
        _ => return None,
    })
}

pub(crate) fn tool_call(call: acp::ToolCall) -> ToolCallInfo {
    ToolCallInfo {
        id: call.tool_call_id.0.to_string(),
        title: call.title,
        kind: wire_str(&call.kind),
        status: tool_status(call.status),
        output: tool_output(&call.content),
        locations: call.locations.iter().map(|l| l.path.display().to_string()).collect(),
        blocked: None,
        held: None,
    }
}

pub(crate) fn tool_call_patch(update: acp::ToolCallUpdate) -> ToolCallPatch {
    let f = update.fields;
    ToolCallPatch {
        id: update.tool_call_id.0.to_string(),
        title: f.title,
        kind: f.kind.map(|k| wire_str(&k)),
        status: f.status.map(tool_status),
        output: f.content.as_deref().map(tool_output),
        locations: f
            .locations
            .map(|ls| ls.iter().map(|l| l.path.display().to_string()).collect()),
    }
}

pub(crate) fn permission_options(options: &[acp::PermissionOption]) -> Vec<PermissionOption> {
    options
        .iter()
        .map(|o| PermissionOption {
            id: o.option_id.0.to_string(),
            name: o.name.clone(),
            kind: match o.kind {
                acp::PermissionOptionKind::AllowOnce => PermissionKind::AllowOnce,
                acp::PermissionOptionKind::AllowAlways => PermissionKind::AllowAlways,
                acp::PermissionOptionKind::RejectOnce => PermissionKind::RejectOnce,
                _ => PermissionKind::RejectAlways,
            },
        })
        .collect()
}

fn tool_status(s: acp::ToolCallStatus) -> ToolStatus {
    match s {
        acp::ToolCallStatus::Pending => ToolStatus::Pending,
        acp::ToolCallStatus::InProgress => ToolStatus::InProgress,
        acp::ToolCallStatus::Completed => ToolStatus::Completed,
        _ => ToolStatus::Failed,
    }
}

fn tool_output(content: &[acp::ToolCallContent]) -> String {
    let mut out = content
        .iter()
        .map(|c| match c {
            acp::ToolCallContent::Content(c) => content_text(&c.content),
            acp::ToolCallContent::Diff(d) => {
                let added = d.new_text.lines().count();
                let removed = d.old_text.as_deref().map_or(0, |t| t.lines().count());
                format!("edit {} (+{added} -{removed})", d.path.display())
            }
            acp::ToolCallContent::Terminal(t) => format!("[terminal {}]", t.terminal_id.0),
            _ => "[content]".to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    if out.len() > MAX_TOOL_OUTPUT {
        let mut cut = MAX_TOOL_OUTPUT;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push_str("\n…");
    }
    out
}

/// Plain-text rendering of a content block.
pub(crate) fn content_text(block: &acp::ContentBlock) -> String {
    match block {
        acp::ContentBlock::Text(t) => t.text.clone(),
        acp::ContentBlock::ResourceLink(l) => format!("[{}]({})", l.name, l.uri),
        acp::ContentBlock::Image(_) => "[image]".into(),
        acp::ContentBlock::Audio(_) => "[audio]".into(),
        _ => "[resource]".into(),
    }
}

/// The serde wire name of a unit enum (e.g. `ToolKind::Execute` → `"execute"`).
pub(crate) fn wire_str<T: serde::Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => "unknown".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_wire_json() {
        let update: acp::SessionUpdate = serde_json::from_value(serde_json::json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "t1",
            "title": "Run ls",
            "kind": "execute",
            "status": "in_progress",
            "content": [{"type": "content", "content": {"type": "text", "text": "out"}}],
            "locations": [{"path": "/tmp/x"}]
        }))
        .unwrap();
        let SessionEvent::ToolCall(call) = session_update(update).unwrap() else { panic!() };
        assert_eq!(call.kind, "execute");
        assert_eq!(call.status, ToolStatus::InProgress);
        assert_eq!(call.output, "out");
        assert_eq!(call.locations, ["/tmp/x"]);

        let update: acp::SessionUpdate = serde_json::from_value(serde_json::json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": "hi"}
        }))
        .unwrap();
        assert_eq!(session_update(update), Some(SessionEvent::AgentText { text: "hi".into() }));

        // Mode updates are not shown (permission modes are the host's, not the agent's).
        let update: acp::SessionUpdate = serde_json::from_value(serde_json::json!({
            "sessionUpdate": "current_mode_update",
            "currentModeId": "read"
        }))
        .unwrap();
        assert_eq!(session_update(update), None);
    }
}
