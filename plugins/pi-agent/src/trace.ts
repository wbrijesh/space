// The trace recorder: what happened around every model request of a session, sent to Space as
// `_space/trace` batches for its Trajectory view.
//
// A hidden pi extension (registered in pi.ts) reads pi's public hooks: `before_provider_request`
// (the payload as sent: system prompt, tools, options, messages; returned unchanged),
// `provider_stream_event` (first token and chunk timing), `message_end` (usage, timing, stop
// reason, text) and `tool_execution_start`/`_end`. The prompt handler marks turns and retries.
//
// Events queue and go out in order: every 100 ms while some are pending, and at once at request,
// response, tool and turn boundaries. Times are wall-clock ms, measured with performance.now()
// from an epoch fixed when the session starts (so gaps are exact). Recording never breaks a turn:
// a failure is logged and that event skipped.

import type { ExtensionFactory } from "@earendil-works/pi-coding-agent";
import { log } from "./env.js";
import type { ChunkKind, JsonValue, TraceBatch, TraceEvent } from "./generated/protocol.js";
import {
	base64Sha,
	ChunkRun,
	capText,
	chunkKind,
	Counters,
	compactMessage,
	headerHash,
	newMessages,
	requestOptions,
	systemText,
	toolDefs,
} from "./trace-core.js";
import { toolResultText, turnUsage } from "./translate.js";

/** How long events may wait before they're sent. */
const FLUSH_MS = 100;
/** A tool's output is kept up to this many characters. */
const MAX_OUTPUT = 256 * 1024;

/** Sends a batch to Space (`_space/trace`). */
export type TraceSink = (batch: TraceBatch) => Promise<void>;

interface AssistantLike {
	role?: string;
	content?: { type?: string; text?: string; thinking?: string }[];
	stopReason?: string;
	errorMessage?: string;
	responseId?: string;
	provider?: string;
	model?: string;
	responseModel?: string;
	durationMs?: number;
}

export class TraceRecorder {
	private readonly epoch = Date.now() - performance.now();
	private readonly counters = new Counters();
	private pending: TraceEvent[] = [];
	private timer: ReturnType<typeof setTimeout> | undefined;
	private sending: Promise<void> = Promise.resolve();
	/** Inside a prompt (requests outside one, e.g. title completions, aren't recorded). */
	private inTurn = false;
	private headerId = 0;
	private lastHeader: string | undefined;
	private previousMessages: string[] | undefined;
	/** The request being answered, and whether its first token came. */
	private current: { request: number; firstToken: boolean } | undefined;
	/** A retry is under way: the next payload is the same request again. */
	private retrying = false;
	private readonly runs = new Map<ChunkKind, ChunkRun>();
	private readonly imagePaths = new Map<string, string>();
	private readonly toolStarts = new Map<string, number>();

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

	/** The file an image with this base64 data was saved as (so its trace refers to the path). */
	noteImage(data: string, path: string): void {
		try {
			this.imagePaths.set(base64Sha(data), path);
		} catch (error) {
			log("trace: image", error);
		}
	}

	turnStart(): void {
		this.guard(() => {
			this.inTurn = true;
			this.retrying = false;
			this.push({ kind: "turnStart", turn: this.counters.startTurn(), at: this.now() });
		});
	}

	/** The turn ended (the ACP stop reason, or `error`); resolves once everything was sent. */
	async turnEnd(reason: string): Promise<void> {
		this.guard(() => {
			this.inTurn = false;
			this.current = undefined;
			this.push({ kind: "turnEnd", turn: this.counters.turn, at: this.now(), reason }, true);
		});
		await this.sending;
	}

	/** pi will send the request again after `delayMs`. */
	retry(attempt: number, max: number, delayMs: number, error: string): void {
		this.guard(() => {
			if (!this.current) return;
			this.retrying = true;
			this.push({ kind: "retry", request: this.current.request, at: this.now(), attempt, max, delayMs, error });
		});
	}

	/** The hidden pi extension that reads the hooks. */
	extension(): ExtensionFactory {
		return (pi) => {
			pi.on("before_provider_request", (event, ctx) => {
				this.guard(() => this.request(event.payload, ctx.model?.provider ?? "", ctx.model?.id ?? ""));
				return undefined;
			});
			pi.on("provider_stream_event", (event) => {
				this.guard(() => this.stream(event.data));
			});
			pi.on("message_end", (event) => {
				this.guard(() => this.response(event.message as AssistantLike));
			});
			pi.on("tool_execution_start", (event) => {
				this.guard(() => {
					if (!this.inTurn) return;
					const at = this.now();
					this.toolStarts.set(event.toolCallId, at);
					this.push(
						{
							kind: "toolStart",
							request: this.current?.request ?? this.counters.request,
							callId: event.toolCallId,
							name: event.toolName,
							input: (event.args ?? null) as JsonValue,
							at,
						},
						true,
					);
				});
			});
			pi.on("tool_execution_end", (event) => {
				this.guard(() => {
					if (!this.inTurn) return;
					const at = this.now();
					const started = this.toolStarts.get(event.toolCallId);
					this.toolStarts.delete(event.toolCallId);
					const output = capText(toolResultText(event.result), MAX_OUTPUT);
					this.push(
						{
							kind: "toolEnd",
							request: this.counters.request,
							callId: event.toolCallId,
							at,
							durationMs: Math.round(event.durationMs ?? (started !== undefined ? at - started : 0)),
							output: output.text,
							isError: event.isError,
							truncated: output.truncated,
						},
						true,
					);
				});
			});
		};
	}

	private request(payload: unknown, provider: string, model: string): void {
		if (!this.inTurn || !payload || typeof payload !== "object") return;
		const p = payload as Record<string, unknown>;
		const systemPrompt = systemText(p.system);
		const tools = toolDefs(p.tools);
		const options = requestOptions(p, provider, model);
		const hash = headerHash(systemPrompt, tools, options);
		const at = this.now();
		if (hash !== this.lastHeader) {
			const reason = this.lastHeader === undefined ? (this.resumed ? "resume" : "initial") : "change";
			this.lastHeader = hash;
			this.headerId++;
			this.push({ kind: "header", headerId: this.headerId, reason, systemPrompt, tools, options });
		}
		const messages = Array.isArray(p.messages) ? p.messages : [];
		const diff = newMessages(this.previousMessages, messages);
		this.previousMessages = diff.keys;
		// A retry sends the same request again: same number, no new Request.
		if (this.retrying && this.current) {
			this.retrying = false;
			this.current.firstToken = false;
			this.flush();
			return;
		}
		this.flushRuns();
		const { request, step } = this.counters.nextRequest();
		this.current = { request, firstToken: false };
		this.push(
			{
				kind: "request",
				request,
				turn: this.counters.turn,
				step,
				at,
				headerId: this.headerId,
				messages: diff.added.map((m) => compactMessage(m, this.imagePaths)),
				rewritten: diff.rewritten,
			},
			true,
		);
	}

	private stream(data: unknown): void {
		if (!this.current) return;
		const kind = chunkKind(data);
		if (!kind) return;
		const at = this.now();
		if (!this.current.firstToken) {
			this.current.firstToken = true;
			this.push({ kind: "firstToken", request: this.current.request, at });
		}
		let run = this.runs.get(kind);
		if (!run) {
			run = new ChunkRun();
			this.runs.set(kind, run);
		}
		run.add(at);
		this.schedule();
	}

	private response(message: AssistantLike): void {
		if (!this.inTurn || message?.role !== "assistant" || !this.current) return;
		this.flushRuns();
		const text = (message.content ?? []).flatMap((c) => (c.type === "text" && c.text ? [c.text] : [])).join("");
		const thinking = (message.content ?? []).flatMap((c) => (c.type === "thinking" && c.thinking ? [c.thinking] : [])).join("");
		const usage = turnUsage([message]);
		const model = message.provider && (message.responseModel || message.model) ? `${message.provider}/${message.responseModel || message.model}` : undefined;
		this.push(
			{
				kind: "response",
				request: this.current.request,
				at: this.now(),
				durationMs: Math.round(message.durationMs ?? 0),
				...(usage ? { usage } : {}),
				stopReason: message.stopReason ?? "",
				...(message.responseId ? { responseId: message.responseId } : {}),
				...(model ? { model } : {}),
				text,
				thinking,
				...(message.errorMessage ? { error: message.errorMessage } : {}),
			},
			true,
		);
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
