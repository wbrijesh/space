import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import type { TraceEvent } from "./generated/protocol.js";
import { base64Sha, ChunkRun, compactMessage, SYSTEM_PROMPT_NOTE, TraceRecorder } from "./trace.js";

function fixture(name: string): unknown[] {
	return readFileSync(join(import.meta.dir, "fixtures", `${name}.jsonl`), "utf8")
		.split("\n")
		.filter(Boolean)
		.map((line) => JSON.parse(line));
}

/** Record turns of recorded messages (each after its prompt) and return the events. */
async function record(turns: { prompt?: unknown; messages: unknown[] }[], options: { model?: string; resumed?: boolean } = {}): Promise<TraceEvent[]> {
	const events: TraceEvent[] = [];
	const trace = new TraceRecorder("s", async (batch) => {
		events.push(...batch.events);
	}, options.resumed);
	trace.setHeader({ tools: ["Bash", "Read"], options: { provider: "claude-cli", model: options.model ?? "claude-haiku-5-5", effort: "low" } });
	for (const turn of turns) {
		trace.turnStart();
		if (turn.prompt) trace.userMessage(turn.prompt);
		for (const m of turn.messages) trace.observe(m);
		await trace.turnEnd("end_turn");
	}
	return events;
}

test("a tool turn: header, requests with what they added, responses with usage, the tool call", async () => {
	const events = await record([{ prompt: [{ type: "text", text: "ls" }], messages: fixture("tool") }]);
	const kinds = events.map((e) => e.kind);
	expect(kinds[0]).toBe("turnStart");
	expect(kinds.at(-1)).toBe("turnEnd");
	const header = events.find((e) => e.kind === "header");
	expect(header).toMatchObject({ headerId: 1, reason: "initial", systemPrompt: SYSTEM_PROMPT_NOTE, options: { model: "claude-haiku-5-5", effort: "low" } });
	expect(header?.kind === "header" && header.tools.map((t) => t.name)).toEqual(["Bash", "Read"]);

	const requests = events.filter((e) => e.kind === "request");
	expect(requests.map((r) => r.kind === "request" && [r.request, r.step])).toEqual([[1, 1], [2, 2]]);
	// The first carries the prompt; the second Claude's tool call and its result.
	const [first, second] = requests;
	expect(first?.kind === "request" && first.messages).toEqual([{ role: "user", content: [{ type: "text", text: "ls" }] }]);
	const added = second?.kind === "request" ? second.messages : [];
	expect(added.map((m) => (m as { role: string }).role)).toEqual(["assistant", "user"]);
	expect(JSON.stringify(added[1])).toContain("toolResultRef");

	const responses = events.filter((e) => e.kind === "response");
	expect(responses).toHaveLength(2);
	for (const r of responses) {
		expect(r.kind === "response" && r.usage?.output).toBeGreaterThan(0);
		expect(r.kind === "response" && r.model).toBe("claude-cli/claude-haiku-5-5");
	}
	expect(responses[0]?.kind === "response" && responses[0].stopReason).toBe("tool_use");
	expect(responses[1]?.kind === "response" && responses[1].text.length).toBeGreaterThan(0);
	expect(events.filter((e) => e.kind === "firstToken")).toHaveLength(2);

	const start = events.find((e) => e.kind === "toolStart");
	const end = events.find((e) => e.kind === "toolEnd");
	expect(start).toMatchObject({ name: "Bash", input: { command: "ls" } });
	expect(end?.kind === "toolEnd" && start?.kind === "toolStart" && end.callId === start.callId && !end.isError).toBe(true);
});

test("the header is sent again only when the model or effort changes; a loaded session's first says resume", async () => {
	const events = await record([{ messages: fixture("plain") }, { messages: fixture("plain") }], { resumed: true });
	const headers = events.filter((e) => e.kind === "header");
	expect(headers).toHaveLength(1);
	expect(headers[0]).toMatchObject({ reason: "resume" });
});

test("after a compaction the next request is rewritten: it holds the summary", async () => {
	const events = await record([{ messages: fixture("compact") }, { prompt: "next", messages: fixture("plain") }]);
	const request = events.find((e) => e.kind === "request");
	expect(request?.kind === "request" && request.rewritten).toBe(true);
	expect(JSON.stringify(request?.kind === "request" && request.messages)).toContain("continued from a previous conversation");
});

test("command output Claude Code writes itself is no model request", async () => {
	const events = await record([{ messages: fixture("context") }]);
	expect(events.filter((e) => e.kind === "request" || e.kind === "response")).toEqual([]);
});

test("retries are recorded against their request, which starts streaming again", async () => {
	const messages = fixture("plain");
	const at = messages.findIndex((m) => (m as { type: string }).type === "stream_event");
	const retry = { type: "system", subtype: "api_retry", attempt: 1, max_retries: 60, retry_delay_ms: 512, error_status: 529, error: "overloaded" };
	const events = await record([{ messages: [...messages.slice(0, at), retry, ...messages.slice(at)] }]);
	expect(events.find((e) => e.kind === "retry")).toMatchObject({ request: 1, attempt: 1, max: 60, delayMs: 512, error: "overloaded (HTTP 529)" });
	expect(events.filter((e) => e.kind === "response")).toHaveLength(1);
});

test("tool results become references and images their hash and path", () => {
	const data = Buffer.from("png").toString("base64");
	const paths = new Map([[base64Sha(data), "/t/a.png"]]);
	const message = {
		role: "user",
		content: [
			{ type: "tool_result", tool_use_id: "t1", content: "big output" },
			{ type: "image", source: { type: "base64", media_type: "image/png", data } },
		],
	};
	expect(compactMessage(message, paths)).toEqual({
		role: "user",
		content: [
			{ type: "tool_result", tool_use_id: "t1", content: { toolResultRef: "t1" } },
			{ type: "image", image: base64Sha(data), mime: "image/png", path: "/t/a.png" },
		],
	});
});

test("chunk runs: the first time, then gaps", () => {
	const run = new ChunkRun();
	expect(run.empty).toBe(true);
	for (const at of [100, 130, 131]) run.add(at);
	expect([run.t0, run.dt]).toEqual([100, [30, 1]]);
});
