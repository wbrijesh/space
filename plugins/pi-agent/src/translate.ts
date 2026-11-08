// Mapping between pi tool executions and ACP tool-call updates.

import type { ContentBlock, ToolCallContent, ToolKind } from "@agentclientprotocol/sdk";
import type { TurnUsage } from "./generated/protocol.js";

const MAX_TITLE = 120;
const MAX_RESULT = 8000;

function oneLine(text: string, max = MAX_TITLE): string {
	const flat = text.replace(/\s+/g, " ").trim();
	return flat.length > max ? `${flat.slice(0, max - 1)}…` : flat;
}

function str(value: unknown): string | undefined {
	return typeof value === "string" && value.length > 0 ? value : undefined;
}

export function toolKind(toolName: string): ToolKind {
	switch (toolName) {
		case "read":
			return "read";
		case "edit":
		case "write":
			return "edit";
		case "bash":
		case "powershell":
			return "execute";
		case "grep":
		case "find":
		case "ls":
			return "search";
		default:
			return "other";
	}
}

/** Human-readable title such as `bash: ls -la` or `read: src/main.ts`. */
export function toolTitle(toolName: string, args: unknown): string {
	const a = (args && typeof args === "object" ? args : {}) as Record<string, unknown>;
	const detail =
		str(a.command) ?? str(a.path) ?? str(a.file_path) ?? str(a.pattern) ?? str(a.query) ?? str(a.url) ?? undefined;
	return detail ? `${toolName}: ${oneLine(detail)}` : toolName;
}

/** Text summary of a pi AgentToolResult (`{ content: (TextContent|ImageContent)[] }`) or error. */
export function toolResultText(result: unknown): string {
	if (result == null) return "";
	if (typeof result === "string") return result;
	if (result instanceof Error) return result.message;
	const content = (result as { content?: unknown }).content;
	if (Array.isArray(content)) {
		return content
			.map((c: { type?: string; text?: string; mimeType?: string }) =>
				c?.type === "text" ? (c.text ?? "") : c?.type === "image" ? `[image ${c.mimeType ?? ""}]` : "",
			)
			.filter(Boolean)
			.join("\n");
	}
	try {
		return JSON.stringify(result);
	} catch {
		return String(result);
	}
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

/** An image for pi (`ImageContent`). */
export interface PromptImage {
	type: "image";
	data: string;
	mimeType: string;
}

/** The local file an image block was saved to (`file://` uri), if any. */
function imagePath(uri: string | null | undefined): string | undefined {
	if (!uri?.startsWith("file://")) return undefined;
	try {
		return decodeURIComponent(new URL(uri).pathname);
	} catch {
		return undefined;
	}
}

/**
 * Flatten ACP prompt content blocks into the text and images pi takes. Each image gets a line
 * in the text where it was ("[Attached image 1: <path>]"), so the model can tell them apart and
 * re-read the file later. With `images: false` (a text-only model) images are left out and the
 * line says so; the path still lets the model read the file with a tool.
 */
export function promptInput(blocks: ContentBlock[], images = true): { text: string; images: PromptImage[] } {
	const parts: string[] = [];
	const out: PromptImage[] = [];
	let n = 0;
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
				if ("text" in r) parts.push(`[Resource ${r.uri}]\n${r.text}`);
				else parts.push(`[Binary resource ${r.uri} omitted]`);
				break;
			}
			case "image": {
				n += 1;
				const path = imagePath(block.uri);
				const at = path ? `: ${path}` : "";
				if (images) {
					parts.push(`[Attached image ${n}${at}]`);
					out.push({ type: "image", data: block.data, mimeType: block.mimeType });
				} else {
					parts.push(`[Attached image ${n} omitted because this model accepts text only${at}]`);
				}
				break;
			}
			case "audio":
				parts.push(`[Audio (${block.mimeType}) omitted: audio is not supported]`);
				break;
			default:
				parts.push(`[Unsupported content block omitted]`);
		}
	}
	return { text: parts.join("\n\n"), images: out };
}

/** An assistant message as pi keeps it (the fields usage needs). */
interface UsageMessage {
	role?: string;
	provider?: string;
	model?: string;
	usage?: { input?: number; output?: number; cacheRead?: number; cacheWrite?: number; reasoning?: number };
}

/**
 * The tokens a turn used: the usage of every assistant message it added (one per model request),
 * summed. `undefined` when none reported any.
 */
export function turnUsage(messages: readonly unknown[]): TurnUsage | undefined {
	let total: TurnUsage | undefined;
	let reasoning = true;
	for (const raw of messages) {
		const m = raw as UsageMessage;
		if (m?.role !== "assistant" || !m.usage) continue;
		const u = m.usage;
		total ??= { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, models: [] };
		total.input += u.input ?? 0;
		total.output += u.output ?? 0;
		total.cacheRead = (total.cacheRead ?? 0) + (u.cacheRead ?? 0);
		total.cacheWrite = (total.cacheWrite ?? 0) + (u.cacheWrite ?? 0);
		if (u.reasoning == null) reasoning = false;
		else total.reasoning = (total.reasoning ?? 0) + u.reasoning;
		const model = m.provider && m.model ? `${m.provider}/${m.model}` : undefined;
		if (model && !total.models?.includes(model)) total.models?.push(model);
	}
	if (total && !reasoning) delete total.reasoning;
	return total;
}
