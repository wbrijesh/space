// Tests of the trace recorder (no model, no pi session): the pure helpers, and the recorder fed
// pi's hook events by hand. Run: pnpm test (node --test on the compiled dist/*.test.js).

import assert from "node:assert/strict";
import { test } from "node:test";
import type { TraceBatch, TraceEvent } from "./generated/protocol.js";
import { TraceRecorder } from "./trace.js";
import { ChunkRun, Counters, capText, chunkKind, compactMessage, headerHash, newMessages, requestOptions, systemText, toolDefs } from "./trace-core.js";

const user = (text: string) => ({ role: "user", content: [{ type: "text", text }] });

test("new messages: the tail after the previous request, or all of them when rewritten", () => {
	const a = user("a");
	const b = { role: "assistant", content: [{ type: "text", text: "b" }] };
	const first = newMessages(undefined, [a]);
	assert.deepEqual(first.added, [a]);
	assert.equal(first.rewritten, false);
	// The cache marker moved to the newest message: still a prefix.
	const marked = { role: "user", content: [{ type: "text", text: "a", cache_control: { type: "ephemeral" } }] };
	const second = newMessages(first.keys, [a, b, marked]);
	assert.deepEqual(second.added, [b, marked]);
	assert.equal(second.rewritten, false);
	const third = newMessages(second.keys, [user("summary"), user("c")]);
	assert.equal(third.rewritten, true);
	assert.equal(third.added.length, 2);
});

test("header: system text, tools, options and a hash that changes only with them", () => {
	const payload = {
		model: "claude-opus-5-5",
		system: [{ type: "text", text: "You are pi.", cache_control: { type: "ephemeral" } }, { type: "text", text: "Rules." }],
		tools: [{ name: "read", description: "Read a file", input_schema: { type: "object" } }],
		max_tokens: 32000,
		thinking: { type: "adaptive" },
		output_config: { effort: "medium" },
	};
	assert.equal(systemText(payload.system), "You are pi.\n\nRules.");
	assert.deepEqual(toolDefs(payload.tools), [{ name: "read", description: "Read a file", parameters: { type: "object" } }]);
	const options = requestOptions(payload, "claude-cli", "fallback");
	assert.deepEqual(options, { provider: "claude-cli", model: "claude-opus-5-5", maxTokens: 32000, thinking: { type: "adaptive" }, effort: "medium" });
	const h = headerHash("You are pi.", toolDefs(payload.tools), options);
	assert.equal(h, headerHash("You are pi.", toolDefs(payload.tools), { ...options }));
	assert.notEqual(h, headerHash("You are pi!", toolDefs(payload.tools), options));
	assert.notEqual(h, headerHash("You are pi.", [], options));
	assert.notEqual(h, headerHash("You are pi.", toolDefs(payload.tools), { ...options, effort: "high" }));
});

test("compact messages: tool results become references, images their sha and path", () => {
	const data = Buffer.from("png bytes").toString("base64");
	const message = {
		role: "user",
		content: [
			{ type: "tool_result", tool_use_id: "call-1", content: [{ type: "text", text: "huge output" }], cache_control: { type: "ephemeral" } },
			{ type: "image", source: { type: "base64", media_type: "image/png", data } },
			{ type: "text", text: "look" },
		],
	};
	const before = JSON.stringify(message);
	const compact = compactMessage(message, new Map()) as { content: Record<string, unknown>[] };
	assert.equal(JSON.stringify(message), before, "the payload isn't changed");
	assert.deepEqual(compact.content[0], { type: "tool_result", tool_use_id: "call-1", content: { toolResultRef: "call-1" } });
	assert.equal(compact.content[1]?.type, "image");
	assert.equal(compact.content[1]?.mime, "image/png");
	assert.equal(typeof compact.content[1]?.image, "string");
	assert.equal((compact.content[1]?.image as string).length, 64);
	assert.equal(compact.content[1]?.path, undefined);
	const withPath = compactMessage(message, new Map([[compact.content[1]?.image as string, "/tmp/a.png"]])) as { content: Record<string, unknown>[] };
	assert.equal(withPath.content[1]?.path, "/tmp/a.png");
	assert.deepEqual(compact.content[2], { type: "text", text: "look" });
});

test("chunk runs, chunk kinds, capped text and counters", () => {
	const run = new ChunkRun();
	assert.equal(run.empty, true);
	run.add(1000);
	run.add(1012.4);
	run.add(1030);
	assert.deepEqual([run.t0, run.dt], [1000, [12, 18]]);
	assert.equal(chunkKind({ type: "content_block_delta", delta: { type: "text_delta", text: "x" } }), "text");
	assert.equal(chunkKind({ type: "content_block_delta", delta: { type: "thinking_delta" } }), "thinking");
	assert.equal(chunkKind({ type: "content_block_delta", delta: { type: "input_json_delta" } }), "toolInput");
	assert.equal(chunkKind({ type: "content_block_delta", delta: { type: "signature_delta" } }), undefined);
	assert.equal(chunkKind({ type: "message_start" }), undefined);
	assert.deepEqual(capText("abcdef", 4), { text: "abcd", truncated: true });
	assert.deepEqual(capText("ab", 4), { text: "ab", truncated: false });
	const c = new Counters();
	assert.equal(c.startTurn(), 1);
	assert.deepEqual(c.nextRequest(), { request: 1, step: 1 });
	assert.deepEqual(c.nextRequest(), { request: 2, step: 2 });
	assert.equal(c.startTurn(), 2);
	assert.deepEqual(c.nextRequest(), { request: 3, step: 1 });
});

/** The recorder's extension, driven by hand: handlers by event name. */
function harness(resumed = false) {
	const batches: TraceBatch[] = [];
	const recorder = new TraceRecorder("s1", async (b) => {
		batches.push(b);
	}, resumed);
	const handlers = new Map<string, (event: unknown, ctx: unknown) => unknown>();
	recorder.extension()({ on: (name: string, h: (event: unknown, ctx: unknown) => unknown) => handlers.set(name, h) } as never);
	const ctx = { model: { provider: "claude-cli", id: "claude-opus-5-5" } };
	const emit = (name: string, event: Record<string, unknown>) => handlers.get(name)?.({ type: name, ...event }, ctx);
	const events = () => batches.flatMap((b) => b.events);
	return { recorder, emit, events, batches };
}

const payload = (messages: unknown[], system = "You are pi.") => ({
	model: "claude-opus-5-5",
	system,
	tools: [{ name: "bash", description: "Run", input_schema: {} }],
	max_tokens: 1000,
	messages,
});

test("recorder: a turn with a tool call, in order, with headers only on change", async () => {
	const { recorder, emit, events } = harness();
	// Outside a turn (e.g. a title completion): nothing is recorded.
	assert.equal(emit("before_provider_request", { payload: payload([user("title?")]) }), undefined);
	recorder.turnStart();
	const m1 = [user("fix it")];
	assert.equal(emit("before_provider_request", { payload: payload(m1) }), undefined, "the payload goes out unchanged");
	emit("provider_stream_event", { data: { type: "message_start" } });
	emit("provider_stream_event", { data: { type: "content_block_delta", delta: { type: "text_delta", text: "ok" } } });
	emit("provider_stream_event", { data: { type: "content_block_delta", delta: { type: "input_json_delta" } } });
	emit("message_end", {
		message: {
			role: "assistant",
			content: [{ type: "text", text: "Running it." }],
			usage: { input: 4, output: 9, cacheRead: 0, cacheWrite: 2600 },
			provider: "claude-cli",
			model: "claude-opus-5-5",
			stopReason: "toolUse",
			responseId: "msg_1",
			durationMs: 812,
		},
	});
	emit("tool_execution_start", { toolCallId: "c1", toolName: "bash", args: { command: "ls" } });
	emit("tool_execution_end", { toolCallId: "c1", toolName: "bash", result: { content: [{ type: "text", text: "a\nb" }] }, isError: false, durationMs: 15 });
	const m2 = [...m1, { role: "assistant", content: [{ type: "tool_use", id: "c1" }] }, { role: "user", content: [{ type: "tool_result", tool_use_id: "c1", content: "a\nb" }] }];
	emit("before_provider_request", { payload: payload(m2) });
	emit("message_end", { message: { role: "assistant", content: [{ type: "text", text: "Done." }], stopReason: "stop", durationMs: 300 } });
	await recorder.turnEnd("end_turn");

	const all = events();
	const kinds = all.map((e) => e.kind);
	assert.deepEqual(kinds, ["turnStart", "header", "request", "firstToken", "chunks", "chunks", "response", "toolStart", "toolEnd", "request", "response", "turnEnd"]);
	const header = all.find((e) => e.kind === "header") as Extract<TraceEvent, { kind: "header" }>;
	assert.equal(header.reason, "initial");
	assert.equal(header.headerId, 1);
	const [r1, r2] = all.filter((e) => e.kind === "request") as Extract<TraceEvent, { kind: "request" }>[];
	assert.deepEqual([r1?.request, r1?.step, r1?.turn, r1?.messages.length], [1, 1, 1, 1]);
	assert.deepEqual([r2?.request, r2?.step, r2?.messages.length, r2?.rewritten], [2, 2, 2, false]);
	assert.deepEqual((r2?.messages[1] as { content: unknown[] }).content[0], { type: "tool_result", tool_use_id: "c1", content: { toolResultRef: "c1" } });
	const response = all.find((e) => e.kind === "response") as Extract<TraceEvent, { kind: "response" }>;
	assert.equal(response.request, 1);
	assert.deepEqual(response.usage, { input: 4, output: 9, cacheRead: 0, cacheWrite: 2600, models: ["claude-cli/claude-opus-5-5"] });
	assert.equal(response.model, "claude-cli/claude-opus-5-5");
	assert.equal(response.text, "Running it.");
	const toolEnd = all.find((e) => e.kind === "toolEnd") as Extract<TraceEvent, { kind: "toolEnd" }>;
	assert.deepEqual([toolEnd.request, toolEnd.output, toolEnd.durationMs, toolEnd.truncated], [1, "a\nb", 15, false]);
	const times = all.map((e) => ("at" in e ? e.at : "t0" in e ? e.t0 : undefined)).filter((t): t is number => t !== undefined);
	assert.deepEqual([...times].sort((a, b) => a - b), times, "times never go backwards");

	// Next turn: the same header isn't sent again; a changed prompt is.
	recorder.turnStart();
	emit("before_provider_request", { payload: payload([...m2, user("again")]) });
	emit("before_provider_request", { payload: payload([...m2, user("again")], "You are pi, now in Read Only.") });
	await recorder.turnEnd("end_turn");
	const later = events().slice(all.length);
	assert.deepEqual(later.map((e) => e.kind), ["turnStart", "request", "header", "request", "turnEnd"]);
	assert.equal((later[2] as Extract<TraceEvent, { kind: "header" }>).reason, "change");
	assert.equal((later[3] as Extract<TraceEvent, { kind: "request" }>).headerId, 2);
	assert.equal((later[3] as Extract<TraceEvent, { kind: "request" }>).step, 2);
});

test("recorder: a retry stays the same request; a loaded session starts with a resume header", async () => {
	const { recorder, emit, events } = harness(true);
	recorder.turnStart();
	emit("before_provider_request", { payload: payload([user("hi")]) });
	emit("message_end", { message: { role: "assistant", content: [], stopReason: "error", errorMessage: "overloaded" } });
	recorder.retry(1, 2, 2000, "overloaded");
	emit("before_provider_request", { payload: payload([user("hi")]) });
	emit("message_end", { message: { role: "assistant", content: [{ type: "text", text: "Hello" }], stopReason: "stop" } });
	await recorder.turnEnd("end_turn");
	const all = events();
	assert.deepEqual(all.map((e) => e.kind), ["turnStart", "header", "request", "response", "retry", "response", "turnEnd"]);
	assert.equal((all[1] as Extract<TraceEvent, { kind: "header" }>).reason, "resume");
	assert.deepEqual(all.filter((e) => e.kind === "response").map((e) => (e as { request: number }).request), [1, 1]);
});

test("recorder: a failing sink or bad event never throws into the turn", async () => {
	const recorder = new TraceRecorder("s", async () => {
		throw new Error("connection gone");
	});
	const handlers = new Map<string, (event: unknown, ctx: unknown) => unknown>();
	recorder.extension()({ on: (name: string, h: (event: unknown, ctx: unknown) => unknown) => handlers.set(name, h) } as never);
	recorder.turnStart();
	assert.doesNotThrow(() => handlers.get("before_provider_request")?.({ type: "before_provider_request", payload: { messages: "not a list", tools: 7 } }, {}));
	assert.doesNotThrow(() => handlers.get("message_end")?.({ type: "message_end", message: null }, {}));
	await recorder.turnEnd("end_turn");
});
