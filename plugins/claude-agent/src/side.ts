// Side chats: a session beside another (the main thread). It starts as a copy of the main
// conversation (Claude Code forks it), can only read, and before each of its prompts it's told
// what was added to the main conversation since it last looked: the main session's saved
// messages after the last one it saw, as a note at the top of the prompt. The note names that
// message (`through`), so where it left off is in its own transcript and survives restarts.

import { toolResultText, toolTitle } from "./translate.js";

/** What a side chat may use: reading the project and the web, nothing that changes anything. */
export const SIDE_TOOLS = ["Read", "Glob", "Grep", "WebFetch", "WebSearch"];

/** Added to the side chat's system prompt, and at the top of each of its prompts. */
export const SIDE_NOTE =
	"This is a side chat beside the main thread of this conversation. You can read files and search, but you can't change anything, and the main thread's work goes on without you: answer the user here, and don't continue the main thread's task. Updates on the main thread come in <main-thread-update> blocks.";

/** A saved message, as the Agent SDK reads it from a session's transcript. */
export interface SavedMessage {
	type: string;
	uuid: string;
	message: unknown;
	parent_tool_use_id: string | null;
}

/** Longest the note gets; older steps are left out past it. */
const MAX_NOTE = 60_000;
const MAX_TEXT = 4000;
const MAX_RESULT = 600;

type Json = Record<string, unknown>;

function isObject(v: unknown): v is Json {
	return !!v && typeof v === "object" && !Array.isArray(v);
}

function short(text: string, max: number): string {
	const t = text.trim();
	return t.length > max ? `${t.slice(0, max)}… [${t.length - max} more characters]` : t;
}

/** The lines a saved main-thread message adds to the note (none for thinking or subagents' work). */
function lines(m: SavedMessage): string[] {
	if (m.parent_tool_use_id !== null || !isObject(m.message)) return [];
	const content = m.message.content;
	if (m.type === "user") {
		if (typeof content === "string") return [`User: ${short(content, MAX_TEXT)}`];
		if (!Array.isArray(content)) return [];
		return content.flatMap((b): string[] => {
			if (!isObject(b)) return [];
			if (b.type === "text" && typeof b.text === "string") return [`User: ${short(b.text, MAX_TEXT)}`];
			if (b.type === "image") return ["User: [image]"];
			if (b.type === "tool_result") return [`  ${b.is_error === true ? "failed:" : "→"} ${short(toolResultText(b.content), MAX_RESULT)}`];
			return [];
		});
	}
	if (m.type === "assistant" && Array.isArray(content)) {
		return content.flatMap((b): string[] => {
			if (!isObject(b)) return [];
			if (b.type === "text" && typeof b.text === "string" && b.text.trim()) return [`Claude: ${short(b.text, MAX_TEXT)}`];
			if (b.type === "tool_use") return [`Claude used ${toolTitle(String(b.name ?? ""), b.input)}`];
			return [];
		});
	}
	return [];
}

/** A note on what's new in the main thread: its text, the last message it covers, and how many steps. */
export interface Update {
	text: string;
	through: string;
	steps: number;
}

/**
 * What the main thread (`main`, its saved messages in order) has after message `through`;
 * `undefined` when there's nothing new. When `through` isn't there (the main thread was
 * compacted since, or started over), all of it is new.
 */
export function mainUpdate(main: readonly SavedMessage[], through: string | undefined): Update | undefined {
	const at = through === undefined ? -1 : main.findIndex((m) => m.uuid === through);
	const after = main.slice(at + 1);
	const last = after.at(-1);
	if (!last) return undefined;
	let steps = after.flatMap(lines);
	if (steps.length === 0) return undefined;
	const total = steps.length;
	let size = steps.reduce((n, l) => n + l.length + 1, 0);
	while (size > MAX_NOTE && steps.length > 1) {
		size -= (steps[0]?.length ?? 0) + 1;
		steps = steps.slice(1);
	}
	const left = total - steps.length;
	const head = [...(at === -1 && through !== undefined ? ["(The main thread was compacted or started over since the last update; this is what it holds now.)"] : []), ...(left > 0 ? [`(${left} earlier steps left out.)`] : [])];
	const body = [...head, ...steps].join("\n");
	return { text: `<main-thread-update through="${last.uuid}">\n${body}\n</main-thread-update>`, through: last.uuid, steps: total };
}

/**
 * Where a side chat left off in the main thread, from its own saved messages: the last update
 * it was given, or else the last message it shares with the main thread (where it was copied).
 */
export function leftOff(side: readonly SavedMessage[], main: readonly SavedMessage[]): string | undefined {
	const known = new Set(main.map((m) => m.uuid));
	for (let i = side.length - 1; i >= 0; i--) {
		const m = side[i];
		if (!m) continue;
		if (m.type === "user" && isObject(m.message)) {
			const content = m.message.content;
			const text = typeof content === "string" ? content : Array.isArray(content) ? content.map((b) => (isObject(b) && b.type === "text" ? String(b.text) : "")).join("\n") : "";
			const through = text.match(/<main-thread-update through="([^"]+)">/)?.[1];
			if (through) return through;
		}
		if (known.has(m.uuid)) return m.uuid;
	}
	return undefined;
}
