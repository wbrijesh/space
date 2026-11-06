// ACP session modes for pi sessions: "write" (default, all tools) and "read" (investigate only).
//
// Read mode, per turn:
// 1. edit/write are deactivated (`session.setActiveToolsByName`); read, bash, grep, find and ls stay.
// 2. A hidden system-prompt section is added in `before_agent_start` (pi diffs prompt sections
//    and sends them as a system patch, so it never becomes a user message or reaches the client).
// 3. Best effort: obviously mutating bash commands are blocked in `tool_call`.
// A mode change applies from the next prompt; a running turn keeps the mode it started with.

import type { SessionMode, SessionModeState } from "@agentclientprotocol/sdk";
import type { AgentSession, ExtensionFactory } from "@earendil-works/pi-coding-agent";

export type ModeId = "write" | "read";
export const DEFAULT_MODE: ModeId = "write";

export const AVAILABLE_MODES: SessionMode[] = [
	{ id: "write", name: "Write", description: "The agent can edit files and run any command." },
	{ id: "read", name: "Read-only", description: "The agent investigates without changing anything." },
];

export function isModeId(id: string): id is ModeId {
	return id === "write" || id === "read";
}

export function modeState(current: ModeId): SessionModeState {
	return { currentModeId: current, availableModes: AVAILABLE_MODES };
}

/** System prompt section (tag name) carrying the read-only instructions. */
export const READ_ONLY_SECTION = "read_only_mode";

export const READ_ONLY_PROMPT = `Read-only mode is on for this message. Investigate freely: read files and run commands that only inspect (ls, cat, rg, git status/log/diff/show, read-only queries, builds or tests only if they don't modify tracked files). Do not create, edit, move or delete files, change git state (commit, checkout, reset, stash, push), install packages, or run anything that changes content or system state. If a change is needed, describe it instead of making it.`;

/** Tools that change files; inactive in read mode. */
const MUTATING_TOOLS = new Set(["edit", "write"]);
/** Inspection tools added in read mode when the session has them. */
const INSPECT_TOOLS = ["grep", "find", "ls"];

/** The read-mode tool set derived from the write-mode set. */
export function readOnlyTools(writeTools: string[], registered: string[]): string[] {
	const tools = writeTools.filter((t) => !MUTATING_TOOLS.has(t));
	for (const t of INSPECT_TOOLS) if (registered.includes(t) && !tools.includes(t)) tools.push(t);
	return tools;
}

/** Per-session mode state shared with the session's inline extension. */
export class ModeControl {
	/** The mode the client chose (applies from the next prompt). */
	mode: ModeId = DEFAULT_MODE;
	/** The mode of the running (or last) turn, fixed when the turn starts. */
	turnMode: ModeId = DEFAULT_MODE;
	/** Active tools in write mode, captured when the session is created. */
	private writeTools: string[] = [];
	private session?: AgentSession;

	attach(session: AgentSession): void {
		this.session = session;
		this.writeTools = session.getActiveToolNames();
	}

	/** Point the session's active tools at the chosen mode (takes effect on the next run). */
	applyTools(): void {
		const session = this.session;
		if (!session) return;
		const registered = session.getAllTools().map((t) => t.name);
		session.setActiveToolsByName(this.mode === "read" ? readOnlyTools(this.writeTools, registered) : this.writeTools);
	}

	/** Inline pi extension: the hidden prompt section and the bash guard. */
	extension(): ExtensionFactory {
		return (pi) => {
			pi.on("before_agent_start", (event) => {
				this.turnMode = this.mode;
				if (this.turnMode === "read") event.systemPromptOptions.sections[READ_ONLY_SECTION] = READ_ONLY_PROMPT;
				else delete event.systemPromptOptions.sections[READ_ONLY_SECTION];
			});
			pi.on("tool_call", (event) => {
				if (this.turnMode !== "read") return;
				if (MUTATING_TOOLS.has(event.toolName)) {
					return { block: true, reason: "Read-only mode: file edits are disabled. Describe the change instead." };
				}
				if (event.toolName === "bash" || event.toolName === "powershell") {
					const command = (event.input as { command?: unknown }).command;
					const why = typeof command === "string" ? mutatingCommand(command) : undefined;
					if (why) return { block: true, reason: `Read-only mode: ${why}. Describe the change instead of making it.` };
				}
				return undefined;
			});
		};
	}
}

// ----- best-effort detection of mutating shell commands ------------------------------------

/** Quoted strings blanked out, so `rg "a > b"` or `echo 'git commit'` don't trip the checks. */
function stripQuotes(command: string): string {
	return command.replace(/'[^']*'|"(?:[^"\\]|\\.)*"/g, "''");
}

/** Starts of simple commands: line start, after ; & | ( or `$(`, optionally behind sudo/env/xargs. */
const CMD = String.raw`(?:^|[;&|(\n]|\$\()\s*(?:(?:sudo|env|command|xargs|nohup|time)(?:\s+-\S+)*\s+)*`;

const RULES: [RegExp, string][] = [
	[new RegExp(`${CMD}(rm|rmdir|mv|cp|touch|mkdir|ln|chmod|chown|truncate|dd|tee|install|unlink|shred)\\b`), "`$1` changes files"],
	[new RegExp(`${CMD}(?:sed|perl|ruby)\\b[^;&|\\n]*\\s(?:-i\\S*|--in-place)`), "in-place edits change files"],
	[/\s-delete\b|-exec\s+(?:rm|mv)\b/, "`find -delete`/`-exec rm` removes files"],
	[
		new RegExp(
			`${CMD}git(?:\\s+-[cC]\\s+\\S+)*\\s+(commit|checkout|switch|reset|push|pull|merge|rebase|clean|restore|rm|mv|add|apply|am|cherry-pick|revert|init|clone|worktree)\\b`,
		),
		"`git $1` changes the repository",
	],
	[new RegExp(`${CMD}git(?:\\s+-[cC]\\s+\\S+)*\\s+stash(?!\\s+(?:list|show)\\b)`), "`git stash` changes the working tree"],
	[new RegExp(`${CMD}git(?:\\s+-[cC]\\s+\\S+)*\\s+branch\\s+(?:.*\\s)?-[dDmMcC]\\b`), "`git branch` with -d/-m/-c changes branches"],
	[
		new RegExp(`${CMD}(npm|pnpm|yarn|bun)\\s+(install|i|add|remove|rm|uninstall|update|up|upgrade|link|ci)\\b`),
		"`$1 $2` changes installed packages",
	],
	[new RegExp(`${CMD}(pip3?|uv\\s+pip|gem|brew|apt|apt-get)\\s+(install|uninstall|remove|upgrade|update)\\b`), "installing packages changes the system"],
	[new RegExp(`${CMD}cargo\\s+(install|add|remove|update|fmt|fix)\\b`), "`cargo $1` changes files"],
];

/** Output redirection to a file (not /dev/null or another descriptor). */
function redirectsToFile(command: string): boolean {
	for (const m of command.matchAll(/(?:\d|&)?>>?\|?\s*([^\s;&|)]*)/g)) {
		const target = m[1] ?? "";
		if (target.startsWith("&") || /^\/dev\/(?:null|stdout|stderr|tty)$/.test(target)) continue;
		// `2>&1` style matched above; `>` followed by nothing is a syntax error, not a write.
		if (target === "") continue;
		return true;
	}
	return false;
}

/** Why `command` looks like it changes content or state, or undefined if it looks read-only. */
export function mutatingCommand(command: string): string | undefined {
	const c = stripQuotes(command);
	for (const [re, why] of RULES) {
		const m = c.match(re);
		if (m) return why.replace(/\$(\d)/g, (_, i: string) => m[Number(i)] ?? "");
	}
	if (redirectsToFile(c)) return "redirecting output into a file changes it";
	return undefined;
}
