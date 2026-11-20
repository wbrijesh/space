// The only module that imports the Claude Agent SDK. It decides how Claude Code runs for Space:
// isolated from the user's own Claude Code setup, with a fixed set of tools, on the signed-in
// subscription, with every tool call put to the host first (session.ts supplies the hooks).

import {
	type AccountInfo,
	type CanUseTool,
	getSessionMessages,
	type HookCallbackMatcher,
	type HookEvent,
	type HookJSONOutput,
	type ModelInfo,
	type Options,
	type PreToolUseHookInput,
	type Query,
	query,
	type SDKMessage,
	type SDKUserMessage,
	type SessionMessage,
	type SlashCommand,
} from "@anthropic-ai/claude-agent-sdk";
import { errorText, log } from "./env.js";
import type { UserContent } from "./translate.js";

export type { ModelInfo, SDKMessage, SessionMessage, SlashCommand };

/** The provider id Space knows the models by: the signed-in Claude Code CLI. */
export const PROVIDER = "claude-cli";

/**
 * The tools Claude may use. Claude Code has more (cron jobs, notifications, worktrees, remote
 * triggers, ...); Space's transcript and guards are built for these.
 */
export const TOOLS = ["Bash", "Read", "Edit", "Write", "Task", "WebFetch", "WebSearch"];

/** Effort levels, lowest first (Space shows them as thinking levels). */
export const EFFORTS = ["low", "medium", "high", "xhigh", "max"] as const;
export type Effort = (typeof EFFORTS)[number];
export const DEFAULT_EFFORT: Effort = "medium";

/**
 * How long a hook may take. A guard holds a call until it can decide (or the user answers its
 * card), which can take as long as the user is away; past this the call is blocked.
 */
const HOOK_TIMEOUT_S = 24 * 60 * 60;

/** Variables Claude Code must not see: any of them would take requests off the subscription. */
const CREDENTIAL_VARS = ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN", "CLAUDE_CODE_USE_BEDROCK", "CLAUDE_CODE_USE_VERTEX", "CLAUDE_CODE_USE_FOUNDRY"];

/** Claude Code's environment: ours, without credentials, with how Space needs it to behave. */
export function cliEnv(env: NodeJS.ProcessEnv = process.env): Record<string, string | undefined> {
	const out: Record<string, string | undefined> = { ...env };
	for (const name of CREDENTIAL_VARS) delete out[name];
	return {
		...out,
		// `session_state_changed`: a turn is over when the session is idle (see session.ts).
		CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS: "1",
		// Nothing runs on after a turn: no background commands or agents, so the session is
		// idle exactly when the turn's work is done, and Claude never starts a turn of its own.
		CLAUDE_CODE_DISABLE_BACKGROUND_TASKS: "1",
		// A dropped connection is waited out for about half an hour (the backoff between
		// retries tops out at about 30 s) before the turn fails.
		CLAUDE_CODE_MAX_RETRIES: "60",
		// claude.ai connectors aren't even fetched (strictMcpConfig keeps them out regardless).
		ENABLE_CLAUDEAI_MCP_SERVERS: "false",
	};
}

/** What every Claude Code process Space starts shares. */
function baseOptions(cli: string, cwd: string): Options {
	return {
		cwd,
		pathToClaudeCodeExecutable: cli,
		env: cliEnv(),
		// None of the user's or the project's Claude Code configuration: no CLAUDE.md or
		// AGENTS.md, skills, hooks, MCP servers, plugins or permission rules.
		settingSources: [],
		skills: [],
		settings: { autoMemoryEnabled: false },
		// No MCP servers: none of the user's, the project's, plugins' or claude.ai connectors.
		mcpServers: {},
		strictMcpConfig: true,
		stderr: (data) => log(`claude: ${data.trimEnd()}`),
	};
}

/** What a session's Claude Code asks of the session (session.ts). */
export interface SessionHooks {
	/** Before every tool call, the main conversation's and subagents' alike. */
	preToolUse(input: PreToolUseHookInput, toolUseId: string, signal: AbortSignal): Promise<HookJSONOutput>;
	/** Hidden context for the prompt being submitted. */
	promptContext(): string | undefined;
	/** Claude Code's own permission prompt (rare: a call its safety checks won't let through). */
	canUseTool: CanUseTool;
}

export interface EngineOptions {
	cli: string;
	cwd: string;
	/** The session's id; Claude Code keeps its transcript under it. */
	sessionId: string;
	/** Continue the transcript kept under `sessionId` instead of starting it. */
	resume: boolean;
	/** Not resuming: start as a copy of this session's conversation (a side chat). */
	forkFrom?: string;
	/** The tools Claude may use. */
	tools: string[];
	/** Added to Claude Code's system prompt. */
	appendSystemPrompt?: string;
	model?: string;
	effort: Effort;
	hooks: SessionHooks;
}

/** A message for Claude Code, from the user. */
export function userMessage(content: string | UserContent[], uuid: string, how: { command?: boolean; priority?: "next" } = {}): SDKUserMessage {
	return {
		type: "user",
		message: { role: "user", content },
		parent_tool_use_id: null,
		uuid: uuid as SDKUserMessage["uuid"],
		origin: { kind: "human" },
		// Only a command Space recognized runs as one; any other text starting with `/` (or
		// containing `@path`) is the user's text, delivered as written.
		...(how.command ? {} : { client_composed: true as const }),
		...(how.priority ? { priority: how.priority } : {}),
	};
}

/** Claude Code's undocumented-in-types `cancelAsyncMessage` (see {@link Engine.cancelQueued}). */
interface QueryWithCancel extends Query {
	cancelAsyncMessage(uuid: string): Promise<boolean>;
}

/** Whether this SDK's queries can withdraw a queued message (a test pins it). */
export function canCancelQueued(q: Query): q is QueryWithCancel {
	return typeof (q as Partial<QueryWithCancel>).cancelAsyncMessage === "function";
}

/** One running Claude Code process for one session, in streaming-input mode. */
export class Engine {
	private readonly inbox = new Inbox<SDKUserMessage>();
	private readonly q: Query;
	private closing = false;

	/**
	 * Starts Claude Code. Every message it sends goes to `onMessage`, in order; `onExit` is
	 * called once when it stops (`undefined` after {@link close}, else why).
	 */
	constructor(options: EngineOptions, onMessage: (message: SDKMessage) => void, onExit: (error: Error | undefined) => void) {
		const { hooks } = options;
		const matcher = (hook: HookCallbackMatcher["hooks"][number]): HookCallbackMatcher[] => [{ hooks: [hook], timeout: HOOK_TIMEOUT_S, onFailure: "block" }];
		const hookSet: Partial<Record<HookEvent, HookCallbackMatcher[]>> = {
			PreToolUse: matcher(async (input, toolUseId, { signal }) => {
				if (input.hook_event_name !== "PreToolUse") return {};
				return hooks.preToolUse(input, toolUseId ?? input.tool_use_id, signal);
			}),
			UserPromptSubmit: matcher(async (input) => {
				const context = input.hook_event_name === "UserPromptSubmit" ? hooks.promptContext() : undefined;
				return context ? { hookSpecificOutput: { hookEventName: "UserPromptSubmit", additionalContext: context } } : {};
			}),
		};
		this.q = query({
			prompt: this.inbox,
			options: {
				...baseOptions(options.cli, options.cwd),
				systemPrompt: { type: "preset", preset: "claude_code", ...(options.appendSystemPrompt ? { append: options.appendSystemPrompt } : {}) },
				tools: options.tools,
				permissionMode: "default",
				hooks: hookSet,
				canUseTool: hooks.canUseTool,
				includePartialMessages: true,
				thinking: { type: "adaptive", display: "summarized" },
				effort: options.effort,
				...(options.model ? { model: options.model } : {}),
				...(options.resume
					? { resume: options.sessionId }
					: options.forkFrom
						? { resume: options.forkFrom, forkSession: true, sessionId: options.sessionId }
						: { sessionId: options.sessionId }),
			},
		});
		void this.pump(onMessage, onExit);
	}

	private async pump(onMessage: (message: SDKMessage) => void, onExit: (error: Error | undefined) => void): Promise<void> {
		let error: Error | undefined;
		try {
			for await (const message of this.q) onMessage(message);
			if (!this.closing) error = new Error("Claude Code stopped");
		} catch (e) {
			// Closing ends the CLI, which may exit non-zero after an interrupted turn.
			if (!this.closing) error = e instanceof Error ? e : new Error(String(e));
		}
		onExit(error);
	}

	send(message: SDKUserMessage): void {
		this.inbox.push(message);
	}

	/** Stop the running turn. */
	async interrupt(): Promise<void> {
		await this.q.interrupt();
	}

	/**
	 * Withdraw a message sent with `priority: "next"` that Claude hasn't read yet. Claude Code
	 * decides atomically: `true` means it never reaches Claude; `false`, that it already did
	 * (or is about to, as part of the next request).
	 */
	async cancelQueued(uuid: string): Promise<boolean> {
		if (!canCancelQueued(this.q)) throw new Error("this Claude Agent SDK can't withdraw queued messages");
		return this.q.cancelAsyncMessage(uuid);
	}

	setModel(model: string): Promise<void> {
		return this.q.setModel(model);
	}

	setEffort(effort: Effort): Promise<void> {
		return this.q.applyFlagSettings({ effortLevel: effort });
	}

	/** Who Claude Code is signed in as, and how it would pay for requests. */
	account(): Promise<AccountInfo> {
		return this.q.accountInfo();
	}

	models(): Promise<ModelInfo[]> {
		return this.q.supportedModels();
	}

	commands(): Promise<SlashCommand[]> {
		return this.q.supportedCommands();
	}

	/** End the process (its transcript stays, to resume). */
	close(): void {
		if (this.closing) return;
		this.closing = true;
		this.inbox.close();
		try {
			this.q.close();
		} catch (error) {
			log("close:", errorText(error));
		}
	}
}

/** Run a one-turn, tool-free query and return its answer. Throws a readable error. */
async function answer(prompt: string, options: Options): Promise<string> {
	let text: string | undefined;
	let failure: string | undefined;
	for await (const message of query({ prompt, options })) {
		if (message.type !== "result") continue;
		if (message.subtype === "success" && !message.is_error) text = message.result;
		else failure = ("errors" in message && message.errors?.join("; ")) || ("result" in message && typeof message.result === "string" ? message.result : "") || message.subtype;
	}
	if (text !== undefined) return text;
	throw new Error(failure || "Claude Code gave no answer");
}

/** `_space/complete`: one short completion outside any conversation (thread titles). */
export function complete(cli: string, cwd: string, prompt: string, model: string | undefined): Promise<string> {
	return answer(prompt, { ...baseOptions(cli, cwd), tools: [], maxTurns: 1, persistSession: false, ...(model ? { model } : {}) });
}

/**
 * Session `sessionId`'s saved messages in order (its transcript's conversation chain), read
 * from where Claude Code keeps sessions run in `cwd`. Empty when there's none.
 */
export async function savedMessages(sessionId: string, cwd: string): Promise<SessionMessage[]> {
	return getSessionMessages(sessionId, { dir: cwd });
}

/** An async queue: the messages Claude Code reads, in order, until it's closed. */
export class Inbox<T> implements AsyncIterable<T> {
	private readonly items: T[] = [];
	private wake: (() => void) | undefined;
	private closed = false;

	push(item: T): void {
		if (this.closed) throw new Error("Claude Code has stopped");
		this.items.push(item);
		this.wake?.();
	}

	close(): void {
		this.closed = true;
		this.wake?.();
	}

	async *[Symbol.asyncIterator](): AsyncIterator<T> {
		while (true) {
			const item = this.items.shift();
			if (item !== undefined) {
				yield item;
				continue;
			}
			if (this.closed) return;
			await new Promise<void>((resolve) => {
				this.wake = resolve;
			});
			this.wake = undefined;
		}
	}
}
