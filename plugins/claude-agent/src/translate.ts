// Pure mappings between ACP (what Space speaks) and Claude Code (what the SDK speaks): tool
// kinds and titles, prompt content, tool results, slash commands, token usage.

import type { ContentBlock, ToolCallContent, ToolKind } from "@agentclientprotocol/sdk";
import type { TurnUsage } from "./generated/protocol.js";

const MAX_TITLE = 120;
const MAX_RESULT = 8000;

type Json = Record<string, unknown>;

function isObject(v: unknown): v is Json {
	return !!v && typeof v === "object" && !Array.isArray(v);
}

function str(value: unknown): string | undefined {
	return typeof value === "string" && value.length > 0 ? value : undefined;
}

function oneLine(text: string, max = MAX_TITLE): string {
	const flat = text.replace(/\s+/g, " ").trim();
	return flat.length > max ? `${flat.slice(0, max - 1)}…` : flat;
}

/**
 * The ACP kind of a Claude Code tool. The host's guards rely on it: `edit` calls have their
 * contents withheld from the decision model, `execute` calls are checked as shell commands.
 */
export function toolKind(name: string): ToolKind {
	switch (name) {
		case "Read":
			return "read";
		case "Edit":
		case "Write":
		case "NotebookEdit":
			return "edit";
		case "Bash":
			return "execute";
		case "Glob":
		case "Grep":
			return "search";
		case "WebFetch":
		case "WebSearch":
			return "fetch";
		default:
			return "other";
	}
}

/** Lowercase, readable names for the transcript (`bash: ls -la`, `read: src/main.rs`). */
const TITLE_NAMES: Record<string, string> = { Task: "agent", Agent: "agent", WebFetch: "fetch", WebSearch: "web search", NotebookEdit: "notebook" };

/** A tool call's title: its name and the one argument that says what it does. */
export function toolTitle(name: string, input: unknown): string {
	const a = isObject(input) ? input : {};
	const detail = str(a.command) ?? str(a.file_path) ?? str(a.notebook_path) ?? str(a.path) ?? str(a.pattern) ?? str(a.url) ?? str(a.query) ?? str(a.description);
	const label = TITLE_NAMES[name] ?? name.toLowerCase();
	return detail ? `${label}: ${oneLine(detail)}` : label;
}

/** A `tool_result` block's content as text (a string, or text and image blocks). */
export function toolResultText(content: unknown): string {
	if (typeof content === "string") return content;
	if (!Array.isArray(content)) return content == null ? "" : JSON.stringify(content);
	return content
		.map((c) => (isObject(c) && c.type === "text" ? String(c.text ?? "") : isObject(c) && c.type === "image" ? "[image]" : ""))
		.filter(Boolean)
		.join("\n");
}

export function truncate(text: string, max = MAX_RESULT): string {
	if (text.length <= max) return text;
	const head = text.slice(0, Math.floor(max * 0.75));
	const tail = text.slice(text.length - Math.floor(max * 0.2));
	return `${head}\n… [${text.length - head.length - tail.length} characters truncated] …\n${tail}`;
}

export function textToolContent(text: string): ToolCallContent[] {
	if (!text) return [];
	return [{ type: "content", content: { type: "text", text: truncate(text) } }];
}

/** An Anthropic user-message content block, as Claude Code takes it. */
export type UserContent = { type: "text"; text: string } | { type: "image"; source: { type: "base64"; media_type: ImageType; data: string } };

const IMAGE_TYPES = ["image/jpeg", "image/png", "image/gif", "image/webp"] as const;
type ImageType = (typeof IMAGE_TYPES)[number];

function imageType(mime: string): ImageType | undefined {
	return IMAGE_TYPES.find((t) => t === mime);
}

/** The local file an image block was saved to (`file://` uri), if any. */
export function filePath(uri: string | null | undefined): string | undefined {
	if (!uri?.startsWith("file://")) return undefined;
	try {
		return decodeURIComponent(new URL(uri).pathname);
	} catch {
		return undefined;
	}
}

/**
 * ACP prompt blocks as a Claude Code user message: the images first, then one text block.
 * Each image gets a line in the text where it was (`[Attached image 1: <path>]`), so Claude
 * can tell them apart and read the file again later.
 */
export function promptContent(blocks: readonly ContentBlock[]): UserContent[] {
	const parts: string[] = [];
	const images: UserContent[] = [];
	for (const block of blocks) {
		switch (block.type) {
			case "text":
				parts.push(block.text);
				break;
			case "resource_link":
				parts.push(`[Referenced resource: ${block.name} <${block.uri}>]`);
				break;
			case "resource": {
				const r = block.resource;
				parts.push("text" in r ? `[Resource ${r.uri}]\n${r.text}` : `[Binary resource ${r.uri} omitted]`);
				break;
			}
			case "image": {
				const path = filePath(block.uri);
				const at = path ? `: ${path}` : "";
				const mediaType = imageType(block.mimeType);
				if (mediaType) {
					images.push({ type: "image", source: { type: "base64", media_type: mediaType, data: block.data } });
					parts.push(`[Attached image ${images.length}${at}]`);
				} else {
					parts.push(`[Image (${block.mimeType}) omitted: unsupported type${at}]`);
				}
				break;
			}
			case "audio":
				parts.push(`[Audio (${block.mimeType}) omitted: audio is not supported]`);
				break;
			default:
				parts.push("[Unsupported content block omitted]");
		}
	}
	return [...images, { type: "text", text: parts.join("\n\n") }];
}

/**
 * The slash command a prompt runs: the prompt is a single text block `/<name>` or
 * `/<name> <arguments>` and `name` is one of `names`. Anything else is text for the model.
 */
export function commandOf(blocks: readonly ContentBlock[], names: readonly string[]): { name: string; text: string } | undefined {
	if (blocks.length !== 1 || blocks[0]?.type !== "text") return undefined;
	const text = blocks[0].text.trim();
	const name = text.match(/^\/([A-Za-z][\w-]*)(?:\s|$)/)?.[1];
	return name && names.includes(name) ? { name, text } : undefined;
}

/** An Anthropic usage object (message_start / message_delta / result). */
export interface AnthropicUsage {
	input_tokens?: number | null;
	output_tokens?: number | null;
	cache_read_input_tokens?: number | null;
	cache_creation_input_tokens?: number | null;
}

/** Usage as Space reports it, for one model request. */
export function turnUsage(u: AnthropicUsage, model: string): TurnUsage {
	return {
		input: u.input_tokens ?? 0,
		output: u.output_tokens ?? 0,
		cacheRead: u.cache_read_input_tokens ?? 0,
		cacheWrite: u.cache_creation_input_tokens ?? 0,
		models: model ? [model] : [],
	};
}

/** `a + b`, the models of both in order without repeats. */
export function addUsage(a: TurnUsage | undefined, b: TurnUsage): TurnUsage {
	if (!a) return { ...b, models: [...(b.models ?? [])] };
	const models = [...(a.models ?? [])];
	for (const m of b.models ?? []) if (!models.includes(m)) models.push(m);
	return {
		input: a.input + b.input,
		output: a.output + b.output,
		cacheRead: (a.cacheRead ?? 0) + (b.cacheRead ?? 0),
		cacheWrite: (a.cacheWrite ?? 0) + (b.cacheWrite ?? 0),
		models,
	};
}

/** Tokens in the model's context after a request: its whole prompt and its answer. */
export function contextTokens(u: AnthropicUsage): number {
	return (u.input_tokens ?? 0) + (u.cache_read_input_tokens ?? 0) + (u.cache_creation_input_tokens ?? 0) + (u.output_tokens ?? 0);
}

/** `9758` → `9.8k`. */
export function shortTokens(n: number): string {
	return n < 1000 ? String(n) : n < 1_000_000 ? `${(n / 1000).toFixed(n < 10_000 ? 1 : 0)}k` : `${(n / 1_000_000).toFixed(1)}M`;
}
