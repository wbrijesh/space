// Pure helpers of the trace recorder (see trace.ts): reading pi's Anthropic request payload,
// what changed since the previous request, compact references for tool results and images, and
// chunk timing runs. No pi or I/O here, so they're tested on their own (trace.test.ts).

import { createHash } from "node:crypto";
import type { ChunkKind, JsonValue, RequestOptions, ToolDef } from "./generated/protocol.js";

type Json = Record<string, unknown>;

function isObject(v: unknown): v is Json {
	return !!v && typeof v === "object" && !Array.isArray(v);
}

/** A deep copy without `cache_control` markers (they move every request, so they'd make every message look new). */
export function stripCacheControl(value: unknown): unknown {
	if (Array.isArray(value)) return value.map(stripCacheControl);
	if (!isObject(value)) return value;
	const out: Json = {};
	for (const [k, v] of Object.entries(value)) if (k !== "cache_control") out[k] = stripCacheControl(v);
	return out;
}

/** The system prompt as text (Anthropic sends a string or text blocks). */
export function systemText(system: unknown): string {
	if (typeof system === "string") return system;
	if (Array.isArray(system)) {
		return system
			.map((b) => (isObject(b) && typeof b.text === "string" ? b.text : ""))
			.filter(Boolean)
			.join("\n\n");
	}
	return "";
}

/** The tools as the model was told about them. */
export function toolDefs(tools: unknown): ToolDef[] {
	if (!Array.isArray(tools)) return [];
	return tools.filter(isObject).map((t) => ({
		name: typeof t.name === "string" ? t.name : "",
		description: typeof t.description === "string" ? t.description : "",
		parameters: (stripCacheControl(t.input_schema ?? {}) ?? {}) as JsonValue,
	}));
}

/** The request's options besides messages and tools. */
export function requestOptions(payload: Json, provider: string, model: string): RequestOptions {
	const options: RequestOptions = { provider, model: typeof payload.model === "string" ? payload.model : model };
	if (typeof payload.max_tokens === "number") options.maxTokens = payload.max_tokens;
	if (payload.thinking !== undefined) options.thinking = stripCacheControl(payload.thinking) as JsonValue;
	const effort = isObject(payload.output_config) ? payload.output_config.effort : undefined;
	if (typeof effort === "string") options.effort = effort;
	if (payload.tool_choice !== undefined) options.toolChoice = stripCacheControl(payload.tool_choice) as JsonValue;
	if (typeof payload.temperature === "number") options.temperature = payload.temperature;
	return options;
}

/** A stable hash of what a header holds, to send it only when it changes. */
export function headerHash(systemPrompt: string, tools: ToolDef[], options: RequestOptions): string {
	return createHash("sha256").update(JSON.stringify([systemPrompt, tools, options])).digest("hex");
}

/**
 * The messages added since the previous request: the tail after `previous` when the current
 * messages start with it (compared as JSON, cache markers ignored), else all of them with
 * `rewritten` (the history changed, e.g. compaction). `keys` is what to keep for the next call.
 */
export function newMessages(previous: readonly string[] | undefined, messages: readonly unknown[]): { added: unknown[]; rewritten: boolean; keys: string[] } {
	const keys = messages.map((m) => JSON.stringify(stripCacheControl(m)));
	if (!previous) return { added: [...messages], rewritten: false, keys };
	const prefix = previous.length <= keys.length && previous.every((k, i) => k === keys[i]);
	return prefix ? { added: messages.slice(previous.length), rewritten: false, keys } : { added: [...messages], rewritten: true, keys };
}

/** sha256 of base64 data's bytes. */
export function base64Sha(data: string): string {
	return createHash("sha256").update(Buffer.from(data, "base64")).digest("hex");
}

/**
 * A message for the trace: cache markers dropped, each tool result's content replaced by a
 * reference to its call (the output is in the call's ToolEnd), each image by its sha256, type
 * and (when known) file path.
 */
export function compactMessage(message: unknown, imagePaths: ReadonlyMap<string, string>): JsonValue {
	const walk = (value: unknown): unknown => {
		if (Array.isArray(value)) return value.map(walk);
		if (!isObject(value)) return value;
		if (value.type === "tool_result") {
			const { content: _content, cache_control: _cc, ...rest } = value;
			return { ...rest, content: { toolResultRef: value.tool_use_id ?? null } };
		}
		if (value.type === "image" && isObject(value.source) && typeof value.source.data === "string") {
			const sha = base64Sha(value.source.data);
			const path = imagePaths.get(sha);
			return { type: "image", image: sha, mime: value.source.media_type ?? null, ...(path ? { path } : {}) };
		}
		const out: Json = {};
		for (const [k, v] of Object.entries(value)) if (k !== "cache_control") out[k] = walk(v);
		return out;
	};
	return walk(message) as JsonValue;
}

/** The chunk stream a provider stream event belongs to (`undefined`: not a token). */
export function chunkKind(data: unknown): ChunkKind | undefined {
	if (!isObject(data) || data.type !== "content_block_delta" || !isObject(data.delta)) return undefined;
	switch (data.delta.type) {
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

/** Per-session numbering: turns, steps within a turn, requests across the session. */
export class Counters {
	turn = 0;
	step = 0;
	request = 0;

	startTurn(): number {
		this.turn++;
		this.step = 0;
		return this.turn;
	}

	nextRequest(): { request: number; step: number } {
		this.step++;
		this.request++;
		return { request: this.request, step: this.step };
	}
}
