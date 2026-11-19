// The trace recorder: what happened around every model request of a session, sent to Space as
// `_space/trace` batches for its Trajectory view.
//
// It reads Claude Code's message stream (session.ts hands it every message): `status:
// requesting` marks a request going out (with the messages added since the one before), stream
// events time its first token and chunks and carry its usage, `api_retry` its retries, tool_use
// and tool_result blocks the tool calls. Claude Code doesn't share its system prompt or tool
// schemas, so a header carries the tool names, model and effort.
//
// Events queue and go out in order: every 100 ms while some are pending, and at once at request,
// response, tool and turn boundaries. Recording never breaks a turn: a failure is logged and that
// event skipped.

import { createHash } from "node:crypto";
import { log } from "./env.js";
import type { ChunkKind, JsonValue, RequestOptions, TraceBatch, TraceEvent, TurnUsage } from "./generated/protocol.js";
import { type AnthropicUsage, addUsage, toolResultText, turnUsage } from "./translate.js";

/** How long events may wait before they're sent. */
const FLUSH_MS = 100;
/** A tool's output is kept up to this many characters. */
const MAX_OUTPUT = 256 * 1024;
/** What a header says in place of Claude Code's system prompt. */
export const SYSTEM_PROMPT_NOTE = "Claude Code's built-in system prompt. Claude Code doesn't share its text.";

/** Sends a batch to Space (`_space/trace`). */
export type TraceSink = (batch: TraceBatch) => Promise<void>;

type Json = Record<string, unknown>;

function isObject(v: unknown): v is Json {
	return !!v && typeof v === "object" && !Array.isArray(v);
}

/** sha256 of base64 data's bytes. */
export function base64Sha(data: string): string {
	return createHash("sha256").update(Buffer.from(data, "base64")).digest("hex");
}

/**
 * A message for the trace: each tool result's content replaced by a reference to its call (the
 * output is in the call's ToolEnd), each image by its sha256, type and (when known) file path.
 */
export function compactMessage(message: unknown, imagePaths: ReadonlyMap<string, string>): JsonValue {
	const walk = (value: unknown): unknown => {
		if (Array.isArray(value)) return value.map(walk);
		if (!isObject(value)) return value;
		if (value.type === "tool_result") {
			const { content: _content, ...rest } = value;
			return { ...rest, content: { toolResultRef: value.tool_use_id ?? null } };
		}
		if (value.type === "image" && isObject(value.source) && typeof value.source.data === "string") {
			const sha = base64Sha(value.source.data);
			const path = imagePaths.get(sha);
			return { type: "image", image: sha, mime: value.source.media_type ?? null, ...(path ? { path } : {}) };
		}
		const out: Json = {};
		for (const [k, v] of Object.entries(value)) if (k !== "signature") out[k] = walk(v);
		return out;
	};
	return walk(message) as JsonValue;
}

/** The chunk stream a stream event belongs to (`undefined`: not a token). */
export function chunkKind(event: unknown): ChunkKind | undefined {
	if (!isObject(event) || event.type !== "content_block_delta" || !isObject(event.delta)) return undefined;
	switch (event.delta.type) {
		case "text_delta":
			return "text";
		case "thinking_delta":
			return "thinking";
		case "input_json_delta":
			return "toolInput";
		default:
			return undefined;
	}
}

/** Arrival times of one stream's chunks: the first at `t0`, each next `dt` ms after the one before. */
export class ChunkRun {
	t0 = 0;
	dt: number[] = [];
	private last = 0;
	private count = 0;

	add(at: number): void {
		if (this.count === 0) this.t0 = at;
		else this.dt.push(Math.max(0, Math.round(at - this.last)));
		this.last = at;
		this.count++;
	}

	get empty(): boolean {
		return this.count === 0;
	}
}

/** Text of at most `max` UTF-16 units, and whether it was cut. */
export function capText(text: string, max: number): { text: string; truncated: boolean } {
	return text.length <= max ? { text, truncated: false } : { text: text.slice(0, max), truncated: true };
}

/** The model request being answered. */
interface Current {
	request: number;
	at: number;
	firstToken: boolean;
	/** The answer streaming in (between message_start and message_stop). */
	response?: { id?: string; model: string; usage: AnthropicUsage; stopReason: string; text: string; thinking: string };
}

/** What the next header would say. */
export interface HeaderFacts {
	tools: string[];
	options: RequestOptions;
}

export class TraceRecorder {
	private readonly epoch = Date.now() - performance.now();
	private turn = 0;
	private step = 0;
	private requests = 0;
	private pending: TraceEvent[] = [];
	private timer: ReturnType<typeof setTimeout> | undefined;
	private sending: Promise<void> = Promise.resolve();
	private inTurn = false;
	private headerId = 0;
	private lastHeader: string | undefined;
	private facts: HeaderFacts = { tools: [], options: { provider: "", model: "" } };
	private current: Current | undefined;
	/** Messages added to the conversation since the last request (sent with the next). */
	private added: unknown[] = [];
	/** The history was rewritten (compacted) since the last request. */
	private rewritten = false;
	private readonly runs = new Map<ChunkKind, ChunkRun>();
	private readonly imagePaths = new Map<string, string>();
	private readonly toolStarts = new Map<string, number>();
	/** What this turn's requests used. */
	private usage: TurnUsage | undefined;

	private readonly sessionId: string;
	private readonly sink: TraceSink;
	/** The session was loaded again (its first header says `resume`). */
	private readonly resumed: boolean;

	constructor(sessionId: string, sink: TraceSink, resumed = false) {
		this.sessionId = sessionId;
		this.sink = sink;
		this.resumed = resumed;
	}

	/** Wall-clock ms now. */
	now(): number {
		return Math.round(this.epoch + performance.now());
	}

	/** The tools, model and effort the next requests go out with. */
	setHeader(facts: HeaderFacts): void {
		this.facts = facts;
	}

	/** The file an image with this base64 data was saved as (so its trace refers to the path). */
	noteImage(data: string, path: string): void {
		this.guard(() => this.imagePaths.set(base64Sha(data), path));
	}

	/** A user message joined the conversation (a prompt, or a steered message as it's read). */
	userMessage(content: unknown): void {
		this.guard(() => this.added.push({ role: "user", content }));
	}

	turnStart(): void {
		this.guard(() => {
			this.inTurn = true;
			this.turn++;
			this.step = 0;
			this.usage = undefined;
			this.push({ kind: "turnStart", turn: this.turn, at: this.now() });
		});
	}

	/** The turn ended (the ACP stop reason, or `error`); resolves once everything was sent. */
	async turnEnd(reason: string): Promise<void> {
		this.guard(() => {
			this.inTurn = false;
			this.current = undefined;
			this.push({ kind: "turnEnd", turn: this.turn, at: this.now(), reason }, true);
		});
		await this.sending;
	}

	/** What this turn's requests used, so far. */
	turnUsage(): TurnUsage | undefined {
		return this.usage;
	}

	/** One message from Claude Code (session.ts passes every one). */
	observe(message: unknown): void {
		this.guard(() => this.handle(message));
	}

	private handle(m: unknown): void {
		if (!this.inTurn || !isObject(m)) return;
		const main = (m.parent_tool_use_id ?? null) === null;
		switch (m.type) {
			case "system":
				if (m.subtype === "status" && m.status === "requesting") this.request();
				else if (m.subtype === "api_retry") this.retry(m);
				else if (m.subtype === "compact_boundary") {
					this.rewritten = true;
					this.added = [];
				}
				break;
			case "stream_event":
				if (main && isObject(m.event)) this.stream(m.event);
				break;
			case "assistant":
				if (isObject(m.message)) this.assistant(m.message, main);
				break;
			case "user":
				if (isObject(m.message) && m.isReplay !== true) this.user(m.message, main);
				break;
		}
	}

	private request(): void {
		const at = this.now();
		this.header();
		this.flushRuns();
		this.step++;
		this.requests++;
		this.current = { request: this.requests, at, firstToken: false };
		this.push(
			{
				kind: "request",
				request: this.requests,
				turn: this.turn,
				step: this.step,
				at,
				headerId: this.headerId,
				messages: this.added.map((m) => compactMessage(m, this.imagePaths)),
				rewritten: this.rewritten,
			},
			true,
		);
		this.added = [];
		this.rewritten = false;
	}

	/** A header, when the tools, model or effort changed since the last one. */
	private header(): void {
		const hash = JSON.stringify(this.facts);
		if (hash === this.lastHeader) return;
		const reason = this.lastHeader === undefined ? (this.resumed ? "resume" : "initial") : "change";
		this.lastHeader = hash;
		this.headerId++;
		this.push({
			kind: "header",
			headerId: this.headerId,
			reason,
			systemPrompt: SYSTEM_PROMPT_NOTE,
			tools: this.facts.tools.map((name) => ({ name, description: "", parameters: {} })),
			options: this.facts.options,
		});
	}

	private retry(m: Json): void {
		const request = this.current?.request ?? this.requests;
		const error = typeof m.error === "string" ? m.error : JSON.stringify(m.error ?? "error");
		const status = typeof m.error_status === "number" ? ` (HTTP ${m.error_status})` : "";
		this.push({
			kind: "retry",
			request,
			at: this.now(),
			attempt: Number(m.attempt ?? 0),
			max: Number(m.max_retries ?? 0),
			delayMs: Math.round(Number(m.retry_delay_ms ?? 0)),
			error: `${error}${status}`,
		});
		// The retried request streams its answer from the start.
		if (this.current) {
			this.current.firstToken = false;
			this.current.response = undefined;
		}
		this.runs.clear();
	}

	private stream(event: Json): void {
		const current = this.current;
		if (!current) return;
		switch (event.type) {
			case "message_start": {
				const msg = isObject(event.message) ? event.message : {};
				current.response = {
					...(typeof msg.id === "string" ? { id: msg.id } : {}),
					model: typeof msg.model === "string" ? msg.model : this.facts.options.model,
					usage: isObject(msg.usage) ? { ...(msg.usage as AnthropicUsage) } : {},
					stopReason: "",
					text: "",
					thinking: "",
				};
				return;
			}
			case "content_block_delta": {
				const kind = chunkKind(event);
				if (!kind) return;
				const at = this.now();
				if (!current.firstToken) {
					current.firstToken = true;
					this.push({ kind: "firstToken", request: current.request, at });
				}
				let run = this.runs.get(kind);
				if (!run) {
					run = new ChunkRun();
					this.runs.set(kind, run);
				}
				run.add(at);
				const delta = event.delta as Json;
				if (current.response && kind === "text") current.response.text += String(delta.text ?? "");
				if (current.response && kind === "thinking") current.response.thinking += String(delta.thinking ?? "");
				this.schedule();
				return;
			}
			case "message_delta": {
				if (!current.response) return;
				const delta = isObject(event.delta) ? event.delta : {};
				if (typeof delta.stop_reason === "string") current.response.stopReason = delta.stop_reason;
				if (isObject(event.usage)) Object.assign(current.response.usage, event.usage);
				return;
			}
			case "message_stop":
				this.response(current);
				return;
		}
	}

	private response(current: Current): void {
		const r = current.response;
		if (!r) return;
		current.response = undefined;
		this.flushRuns();
		const at = this.now();
		const model = `${this.facts.options.provider}/${r.model}`;
		const usage = turnUsage(r.usage, model);
		this.usage = addUsage(this.usage, usage);
		this.push(
			{
				kind: "response",
				request: current.request,
				at,
				durationMs: Math.max(0, at - current.at),
				usage,
				stopReason: r.stopReason,
				...(r.id ? { responseId: r.id } : {}),
				model,
				text: r.text,
				thinking: r.thinking,
			},
			true,
		);
	}

	/** Claude's message (one content block at a time, as the SDK sends them). */
	private assistant(message: Json, main: boolean): void {
		if (message.model === "<synthetic>") return;
		const content = Array.isArray(message.content) ? message.content : [];
		if (main) this.addAssistant(message.id, content);
		for (const block of content) {
			if (!isObject(block) || block.type !== "tool_use" || typeof block.id !== "string") continue;
			const at = this.now();
			this.toolStarts.set(block.id, at);
			this.push(
				{ kind: "toolStart", request: this.current?.request ?? this.requests, callId: block.id, name: String(block.name ?? ""), input: (block.input ?? null) as JsonValue, at },
				true,
			);
		}
	}

	/** Claude's answer joins the conversation: blocks of one message are kept together. */
	private addAssistant(id: unknown, content: unknown[]): void {
		const last = this.added.at(-1);
		if (isObject(last) && last.role === "assistant" && last.id === id && Array.isArray(last.content)) last.content.push(...content);
		else this.added.push({ role: "assistant", id, content: [...content] });
	}

	/** Tool results (and any other message Claude Code adds to the conversation). */
	private user(message: Json, main: boolean): void {
		const content = Array.isArray(message.content) ? message.content : [];
		for (const block of content) {
			if (!isObject(block) || block.type !== "tool_result" || typeof block.tool_use_id !== "string") continue;
			const at = this.now();
			const started = this.toolStarts.get(block.tool_use_id);
			this.toolStarts.delete(block.tool_use_id);
			const output = capText(toolResultText(block.content), MAX_OUTPUT);
			this.push(
				{
					kind: "toolEnd",
					request: this.requests,
					callId: block.tool_use_id,
					at,
					durationMs: started === undefined ? 0 : Math.max(0, at - started),
					output: output.text,
					isError: block.is_error === true,
					truncated: output.truncated,
				},
				true,
			);
		}
		if (main) this.added.push({ role: "user", content: message.content });
	}

	/** Queue an event; `now` sends the queue at once (a boundary). */
	private push(event: TraceEvent, now = false): void {
		this.pending.push(event);
		if (now) this.flush();
		else this.schedule();
	}

	private schedule(): void {
		if (this.timer) return;
		this.timer = setTimeout(() => {
			this.timer = undefined;
			this.guard(() => this.flush());
		}, FLUSH_MS);
		this.timer.unref?.();
	}

	/** Move the chunk runs into the queue. */
	private flushRuns(): void {
		if (!this.current) return;
		for (const [stream, run] of this.runs) {
			if (run.empty) continue;
			this.pending.push({ kind: "chunks", request: this.current.request, stream, t0: Math.round(run.t0), dt: run.dt });
		}
		this.runs.clear();
	}

	private flush(): void {
		if (this.timer) {
			clearTimeout(this.timer);
			this.timer = undefined;
		}
		this.flushRuns();
		if (this.pending.length === 0) return;
		const batch: TraceBatch = { sessionId: this.sessionId, events: this.pending };
		this.pending = [];
		this.sending = this.sending.then(() => this.sink(batch)).catch((error: unknown) => log("trace: sending failed:", error));
	}

	private guard(f: () => void): void {
		try {
			f();
		} catch (error) {
			log("trace: skipped an event:", error);
		}
	}
}
