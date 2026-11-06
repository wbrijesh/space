// Mapping between pi tool executions and ACP tool-call updates.

import type { ContentBlock, ToolCallContent, ToolKind } from "@agentclientprotocol/sdk";

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

/** Flatten ACP prompt content blocks into the single text prompt pi takes. */
export function promptText(blocks: ContentBlock[]): string {
	const parts: string[] = [];
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
			case "image":
				parts.push(`[Image (${block.mimeType}) omitted: images are not supported yet]`);
				break;
			case "audio":
				parts.push(`[Audio (${block.mimeType}) omitted: audio is not supported]`);
				break;
			default:
				parts.push(`[Unsupported content block omitted]`);
		}
	}
	return parts.join("\n\n");
}
