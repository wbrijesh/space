// One Space session: a Claude Code process (claude.ts) and what Space sees of it. Prompts,
// steering and configuration go in; ACP updates, `_space/*` notifications and
// trace batches come out, in the order Claude Code produced them.
//
// A turn is one `session/prompt`. It ends when Claude Code reports the session idle after
// having run it: that covers every model request of the turn, a /goal's continuations, and a
// steered message that arrived as the turn ended (Claude reads it as one more step).
//
// A side chat (side.ts) is a session too: a copy of another session's conversation that can only
// read, told before each prompt what the other one added since.

import { randomUUID } from "node:crypto";
import type { ContentBlock, SessionUpdate, StopReason } from "@agentclientprotocol/sdk";
import type { HookJSONOutput, PermissionResult, PreToolUseHookInput } from "@anthropic-ai/claude-agent-sdk";
import { DEFAULT_EFFORT, EFFORTS, type Effort, type EngineOptions, type ModelInfo, PROVIDER, type SDKMessage, type SessionHooks, type SlashCommand, TOOLS, userMessage } from "./claude.js";
import { errorText, log } from "./env.js";
import {
	type CommandInfo,
	type Confine,
	type ConfineResult,
	type ContextSection,
	type JsonValue,
	type ModelInfo as SpaceModel,
	type PreExecute,
	type PreExecuteResult,
	RETRY_METHOD,
	type RetryState,
	type SpaceStatus,
	STEERED_METHOD,
	type TurnUsage,
} from "./generated/protocol.js";
import { leftOff, mainUpdate, type SavedMessage, SIDE_NOTE, SIDE_TOOLS } from "./side.js";
import { type TraceRecorder } from "./trace.js";
import {
	type AnthropicUsage,
	commandOf,
	contextTokens,
	filePath,
	promptContent,
	shortTokens,
	textToolContent,
	toolKind,
	toolResultText,
	toolTitle,
	type UserContent,
} from "./translate.js";

/** The commands Space offers from Claude Code's own (the rest need its terminal UI or overlap with Space's). */
export const COMMANDS = ["compact", "context", "goal"];

/** `/btw`: Space opens the side chat with it (a session made with `sideOf`); it's never sent as a prompt. */
export const SIDE_COMMAND: CommandInfo = {
	name: "btw",
	description: "Talk about this thread in a side chat, without adding to it",
	hint: "<question>",
	side: true,
};

/** What a session asks of the host (backed by the ACP connection; tests fake it). */
export interface HostLink {
	preExecute(params: PreExecute): Promise<PreExecuteResult>;
	confine(params: Confine): Promise<ConfineResult>;
}

/** Where a session's output goes, in order. */
export interface Output {
	update(update: SessionUpdate): void;
	notify(method: string, params: unknown): void;
	/** Resolves once everything sent so far is out. */
	flushed(): Promise<void>;
}

/** A Claude Code process, as a session drives it (claude.ts's `Engine`; tests fake it). */
export interface EngineLike {
	send(message: ReturnType<typeof userMessage>): void;
	interrupt(): Promise<void>;
	cancelQueued(uuid: string): Promise<boolean>;
	setModel(model: string): Promise<void>;
	setEffort(effort: Effort): Promise<void>;
	models(): Promise<ModelInfo[]>;
	commands(): Promise<SlashCommand[]>;
	account(): Promise<{ apiProvider?: string; subscriptionType?: string; apiKeySource?: string }>;
	close(): void;
}

export type StartEngine = (options: EngineOptions, onMessage: (message: SDKMessage) => void, onExit: (error: Error | undefined) => void) => EngineLike;

export interface SessionOptions {
	id: string;
	cwd: string;
	cli: string;
	/** Continue the conversation Claude Code kept under `id` (ACP `session/load`). */
	resume: boolean;
	host: HostLink;
	output: Output;
	trace: TraceRecorder;
	start: StartEngine;
	/** A side chat of this session (side.ts). */
	sideOf?: string;
	/** A session's saved messages, in order (claude.ts's `savedMessages`; tests fake it). */
	read: (sessionId: string, cwd: string) => Promise<SavedMessage[]>;
}

/** How a turn ended. */
export type Outcome = { stopReason: StopReason; usage?: TurnUsage } | { error: string; usage?: TurnUsage };

interface Turn {
	/** Set by `session/cancel`. */
	cancelled: boolean;
	/** Claude Code reported `running` since the prompt went out (an `idle` before it is stale). */
	running: boolean;
	/** The last `result` of the turn. */
	result?: ResultLike;
	/** Claude Code stopped during the turn. */
	exit?: string;
	/** Space's own housekeeping (clearing a goal): nothing of it is shown. */
	silent?: boolean;
	end: (turn: Turn) => void;
}

interface ResultLike {
	subtype: string;
	is_error?: boolean;
	stop_reason?: string | null;
	result?: string;
	errors?: string[];
	modelUsage?: Record<string, { contextWindow?: number }>;
}

/** A steered message on its way to Claude. */
interface Steer {
	/** Space's id for it. */
	id: string;
	content: UserContent[];
}

type Json = Record<string, unknown>;

function isObject(v: unknown): v is Json {
	return !!v && typeof v === "object" && !Array.isArray(v);
}

/** Single-quote `s` for a POSIX shell. */
export function shellQuote(s: string): string {
	return /^[A-Za-z0-9_\/.,:=@%+-]+$/.test(s) ? s : `'${s.replaceAll("'", `'\\''`)}'`;
}

function deny(reason: string): HookJSONOutput {
	return { hookSpecificOutput: { hookEventName: "PreToolUse", permissionDecision: "deny", permissionDecisionReason: reason } };
}

function allow(updatedInput?: Record<string, unknown>): HookJSONOutput {
	return { hookSpecificOutput: { hookEventName: "PreToolUse", permissionDecision: "allow", ...(updatedInput ? { updatedInput } : {}) } };
}

/** `promise`, or a rejection once `signal` aborts. */
function abortable<T>(promise: Promise<T>, signal: AbortSignal): Promise<T> {
	if (signal.aborted) return Promise.reject(new Error("the turn was stopped"));
	return new Promise((resolve, reject) => {
		const onAbort = () => reject(new Error("the turn was stopped"));
		signal.addEventListener("abort", onAbort, { once: true });
		promise.then(resolve, reject).finally(() => signal.removeEventListener("abort", onAbort));
	});
}

/** Context sections from a prompt's `_meta` (anything malformed is ignored). */
export function contextFromMeta(meta: unknown, key: string): ContextSection[] {
	const raw = isObject(meta) ? meta[key] : undefined;
	if (!Array.isArray(raw)) return [];
	return raw.filter((s): s is ContextSection => isObject(s) && typeof s.id === "string" && typeof s.text === "string");
}

const FAMILY_ORDER = ["fable", "opus", "sonnet", "haiku"];

/** `claude-opus-5-5` → `claude-opus`: the id without its version numbers. */
export function modelFamily(id: string): string {
	return id
		.split("-")
		.filter((part) => !/^\d+$/.test(part))
		.join("-");
}

/** Sort key: model family (Fable, Opus, Sonnet, Haiku, others), then newest version first. */
function compareModels(a: string, b: string): number {
	const family = (id: string) => {
		const at = FAMILY_ORDER.findIndex((f) => id.includes(f));
		return at === -1 ? FAMILY_ORDER.length : at;
	};
	if (family(a) !== family(b)) return family(a) - family(b);
	const va = (a.match(/\d+/g) ?? []).map(Number);
	const vb = (b.match(/\d+/g) ?? []).map(Number);
	for (let i = 0; i < Math.max(va.length, vb.length); i++) {
		const d = (vb[i] ?? -1) - (va[i] ?? -1);
		if (d !== 0) return d;
	}
	return a.localeCompare(b);
}

/**
 * The models to offer: the newest of each family Claude Code lists (`claude-fable-5-2`,
 * `claude-opus-5-5`, ...), without dated snapshots, by family. `current` stays listed even
 * when it's an older one, so a thread on it still shows what it uses.
 */
export function availableModels(models: readonly ModelInfo[], current: string | undefined): SpaceModel[] {
	const names = new Map<string, string>();
	for (const m of models) {
		const id = m.resolvedModel ?? m.value;
		if (m.value !== "default" && !names.has(id)) names.set(id, m.displayName);
	}
	const seen = new Set<string>();
	const ids = [...names.keys()]
		.filter((id) => !/-\d{8}$/.test(id))
		.sort(compareModels)
		.filter((id) => {
			const newest = !seen.has(modelFamily(id));
			seen.add(modelFamily(id));
			return newest || id === current;
		});
	if (current && !ids.includes(current)) ids.push(current);
	return ids.map((id) => ({ provider: PROVIDER, id, name: names.get(id) ?? id }));
}

/** Whether Claude Code can run model `id` (any model it lists, not only the newest). */
function knownModel(models: readonly ModelInfo[], id: string): boolean {
	return models.some((m) => m.value !== "default" && (m.resolvedModel ?? m.value) === id);
}

/** The effort levels model `id` supports (none: it has no effort setting). */
export function effortsOf(models: readonly ModelInfo[], id: string | undefined): Effort[] {
	const m = models.find((m) => (m.resolvedModel ?? m.value) === id && m.value !== "default") ?? models.find((m) => m.value === "default");
	if (!m?.supportsEffort) return [];
	return EFFORTS.filter((e) => m.supportedEffortLevels?.includes(e) ?? true);
}

/** `want`, or the nearest level below it the model supports (else its lowest). */
export function clampEffort(want: Effort, supported: readonly Effort[]): Effort {
	if (supported.length === 0 || supported.includes(want)) return want;
	const below = EFFORTS.slice(0, EFFORTS.indexOf(want)).filter((e) => supported.includes(e));
	return below.at(-1) ?? supported[0] ?? want;
}

/** The commands for the composer's menu: those of {@link COMMANDS} Claude Code has, and `/btw`. */
export function menuCommands(commands: readonly SlashCommand[]): CommandInfo[] {
	const out: CommandInfo[] = [];
	for (const name of COMMANDS) {
		const c = commands.find((c) => c.name === name);
		if (c) out.push({ name, description: c.description, ...(c.argumentHint ? { hint: c.argumentHint } : {}) });
	}
	return [SIDE_COMMAND, ...out].sort((a, b) => a.name.localeCompare(b.name));
}

/** What a `result` message means for the turn. */
export function outcome(turn: { cancelled: boolean; result?: ResultLike; exit?: string }): { stopReason: StopReason } | { error: string } {
	if (turn.cancelled) return { stopReason: "cancelled" };
	if (turn.exit) return { error: turn.exit };
	const r = turn.result;
	if (!r) return { stopReason: "end_turn" };
	if (r.subtype === "success" && !r.is_error) return { stopReason: r.stop_reason === "max_tokens" ? "max_tokens" : "end_turn" };
	const detail = r.errors?.filter(Boolean).join("; ") || (typeof r.result === "string" ? r.result : "");
	return { error: detail || `Claude Code ended the turn (${r.subtype})` };
}

/** The text of a `<local-command-stdout>` replay, if `content` is one. */
function localCommandOutput(content: unknown): string | undefined {
	const text = typeof content === "string" ? content : Array.isArray(content) ? content.map((c) => (isObject(c) && c.type === "text" ? String(c.text) : "")).join("") : "";
	return text.match(/^<local-command-stdout>([\s\S]*)<\/local-command-stdout>$/)?.[1]?.trim();
}

/** The retry state Space shows for an `api_retry`. */
export function retryState(m: Json): RetryState {
	const status = typeof m.error_status === "number" ? m.error_status : undefined;
	const error = typeof m.error === "string" ? m.error : "unknown";
	const busy = status === 429 || status === 529 || error === "rate_limit" || error === "overloaded";
	const reason = status ? `${error.replaceAll("_", " ")} (HTTP ${status})` : error === "unknown" ? "connection error" : error.replaceAll("_", " ");
	return { kind: busy ? "busy" : "retrying", attempt: Number(m.attempt ?? 1), reason };
}

export class Session {
	readonly id: string;
	readonly cwd: string;
	private readonly options: SessionOptions;
	private readonly output: Output;
	private readonly trace: TraceRecorder;
	private engine: EngineLike | undefined;
	/** Claude Code is starting (one start at a time). */
	private starting: Promise<void> | undefined;
	/** Claude Code keeps a transcript for this session (it can be resumed). */
	private conversation: boolean;
	private model: string | undefined;
	private effort: Effort = DEFAULT_EFFORT;
	private models: ModelInfo[] = [];
	private commands: CommandInfo[] = [];
	private turn: Turn | undefined;
	/** Steered messages Claude hasn't read yet, by the uuid they were sent with. */
	private readonly steers = new Map<string, Steer>();
	/** Steered messages withdrawn when the turn was stopped (the next unsteer reports them). */
	private withdrawn: string[] = [];
	/** Hidden context for the prompt being submitted. */
	private context: string | undefined;
	/** Tool calls the host allowed (Claude Code's own prompt may ask about them again). */
	private readonly approved = new Set<string>();
	/** Tool calls already shown. */
	private readonly shown = new Set<string>();
	/**
	 * Messages already handled. A compaction sends again the messages it keeps (same uuids):
	 * they're in the transcript already.
	 */
	private readonly handled = new Set<string>();
	/** The current answer's usage, for the context gauge. */
	private usage: AnthropicUsage = {};
	private contextTokens: number | undefined;
	private contextWindow: number | undefined;
	/** A request is being retried (the next content clears the notice). */
	private retrying = false;
	/** Text or thinking of the current answer was shown (a retry takes it back). */
	private streamed = false;
	/** The "Compacted" line Claude Code prints after a compaction (Space says more). */
	private compacted = false;
	/** A `/goal` may be set: stopping a turn clears it, or the next prompt would carry on with it. */
	private goal = false;
	/** The tools Claude may use. */
	private readonly tools: string[];
	/** A side chat: the last main-thread message it was told about (or copied with). */
	private through: string | undefined;
	/** A side chat before its first prompt: the session it starts as a copy of. */
	private forkFrom: string | undefined;

	private constructor(options: SessionOptions) {
		this.options = options;
		this.id = options.id;
		this.cwd = options.cwd;
		this.output = options.output;
		this.trace = options.trace;
		this.conversation = options.resume;
		this.tools = options.sideOf ? SIDE_TOOLS : TOOLS;
	}

	/** Start (or resume) the session's Claude Code. Throws a readable error. */
	static async open(options: SessionOptions): Promise<Session> {
		const session = new Session(options);
		await session.ensureEngine();
		return session;
	}

	/** The session's status, as `_space/status` carries it. */
	status(): SpaceStatus {
		return {
			sessionId: this.id,
			provider: PROVIDER,
			...(this.model ? { model: this.model } : {}),
			thinkingLevel: this.effort,
			availableModels: availableModels(this.models, this.model),
			thinkingLevels: effortsOf(this.models, this.model),
			...(this.contextTokens !== undefined ? { contextTokens: this.contextTokens } : {}),
			...(this.contextWindow !== undefined ? { contextWindow: this.contextWindow } : {}),
			...(this.conversation ? { continueIn: { app: "Claude Code", command: `cd ${shellQuote(this.cwd)} && claude --resume ${this.id} --fork-session` } } : {}),
			commands: this.commands,
		};
	}

	get running(): boolean {
		return this.turn !== undefined;
	}

	/** Start Claude Code unless it runs (after it stopped, it resumes the conversation). */
	private async ensureEngine(): Promise<EngineLike> {
		while (!this.engine) {
			this.starting ??= this.startEngine().finally(() => {
				this.starting = undefined;
			});
			await this.starting;
		}
		return this.engine;
	}

	private async startEngine(): Promise<void> {
		if (this.options.sideOf) await this.prepareSide(this.options.sideOf);
		const engine = this.options.start(
			{
				cli: this.options.cli,
				cwd: this.cwd,
				sessionId: this.id,
				resume: this.conversation,
				...(this.forkFrom && !this.conversation ? { forkFrom: this.forkFrom } : {}),
				tools: this.tools,
				...(this.options.sideOf ? { appendSystemPrompt: SIDE_NOTE } : {}),
				...(this.model ? { model: this.model } : {}),
				effort: this.effort,
				hooks: this.hooks(),
			},
			(message) => {
				if (this.engine === engine) this.onMessage(message);
			},
			(error) => {
				if (this.engine === engine) this.onExit(error);
			},
		);
		this.engine = engine;
		try {
			const [account, models, commands] = await Promise.all([engine.account(), engine.models(), engine.commands()]);
			// Requests must draw on the subscription the CLI is signed in to, never an API key.
			if (account.apiProvider !== "firstParty" || !account.subscriptionType || (account.apiKeySource && account.apiKeySource !== "none")) {
				throw new Error("Claude Code isn't signed in to a Claude subscription: run `claude` in a terminal and sign in with your Claude account.");
			}
			this.models = models;
			this.commands = menuCommands(commands);
			this.model ??= models.find((m) => m.value === "default")?.resolvedModel ?? models[0]?.resolvedModel;
			this.effort = clampEffort(this.effort, effortsOf(models, this.model));
			this.setHeader();
		} catch (error) {
			this.engine = undefined;
			engine.close();
			throw error;
		}
	}

	/**
	 * A side chat starting Claude Code. Before its first prompt it's a fresh copy of the main
	 * conversation as it is now (none while that has no messages yet); after, it continues its
	 * own, from where its transcript says it left off in the main thread.
	 */
	private async prepareSide(main: string): Promise<void> {
		if (!this.conversation) {
			const messages = await this.readSafely(main);
			this.forkFrom = messages.length > 0 ? main : undefined;
			this.through = messages.at(-1)?.uuid;
		} else if (this.through === undefined) {
			const [side, mainMessages] = await Promise.all([this.readSafely(this.id), this.readSafely(main)]);
			this.through = leftOff(side, mainMessages);
		}
	}

	/** A session's saved messages; none when they can't be read (the side chat goes on without). */
	private async readSafely(sessionId: string): Promise<SavedMessage[]> {
		try {
			return await this.options.read(sessionId, this.cwd);
		} catch (error) {
			log(`${this.id}: couldn't read session ${sessionId}: ${errorText(error)}`);
			return [];
		}
	}

	private setHeader(): void {
		this.trace.setHeader({ tools: this.tools, options: { provider: PROVIDER, model: this.model ?? "", effort: this.effort, thinking: { type: "adaptive" } as JsonValue } });
	}

	private hooks(): SessionHooks {
		return {
			preToolUse: (input, toolUseId, signal) => this.preToolUse(input, toolUseId, signal),
			promptContext: () => this.context,
			canUseTool: async (name, input, { toolUseID }): Promise<PermissionResult> => {
				if (this.approved.has(toolUseID)) return { behavior: "allow", updatedInput: input };
				log(`${this.id}: Claude Code asked about ${name} ${toolUseID}, which Space hadn't approved`);
				return { behavior: "deny", message: "Space didn't approve this call." };
			},
		};
	}

	/** Every tool call goes to the host's pipeline first; a shell command is confined too. */
	private async preToolUse(input: PreToolUseHookInput, toolUseId: string, signal: AbortSignal): Promise<HookJSONOutput> {
		const tool = input.tool_name;
		const args = isObject(input.tool_input) ? input.tool_input : {};
		let verdict: PreExecuteResult;
		try {
			verdict = await abortable(this.options.host.preExecute({ sessionId: this.id, toolCallId: toolUseId, tool, kind: toolKind(tool), args: args as JsonValue }), signal);
		} catch (error) {
			return deny(`Space couldn't check this call, so it didn't run (${errorText(error)}).`);
		}
		if (verdict.decision !== "allow") return deny(verdict.reason || "Space didn't allow this call.");
		if (tool !== "Bash") {
			this.approved.add(toolUseId);
			return allow();
		}
		if (typeof args.command !== "string") return deny("The command is missing.");
		const argv = ["/bin/bash", "-c", args.command];
		let confined: ConfineResult;
		try {
			confined = await abortable(this.options.host.confine({ sessionId: this.id, argv, cwd: input.cwd }), signal);
		} catch (error) {
			return deny(`Space couldn't prepare this command, so it didn't run (${errorText(error)}).`);
		}
		if (!Array.isArray(confined.argv) || confined.argv.length === 0) return deny("Space returned no command to run.");
		this.approved.add(toolUseId);
		const same = confined.argv.length === argv.length && confined.argv.every((a, i) => a === argv[i]);
		return same ? allow() : allow({ ...args, command: confined.argv.map(shellQuote).join(" ") });
	}

	/** `session/prompt`. Resolves when the turn is over. */
	async prompt(blocks: readonly ContentBlock[], context: readonly ContextSection[]): Promise<Outcome> {
		if (this.turn) throw new Error("A prompt is already running for this session");
		const engine = await this.ensureEngine();
		if (this.turn) throw new Error("A prompt is already running for this session");
		const command = commandOf(blocks, COMMANDS);
		let content: string | UserContent[] = command ? command.text : promptContent(blocks);
		// A side chat is told what the main thread added since it last looked.
		const sideOf = this.options.sideOf;
		const update = sideOf && !command ? mainUpdate(await this.readSafely(sideOf), this.through) : undefined;
		if (this.turn) throw new Error("A prompt is already running for this session");
		if (sideOf && !command && Array.isArray(content)) {
			const note = [`<side-chat>${SIDE_NOTE}</side-chat>`, ...(update ? [update.text] : [])].join("\n\n");
			content = [{ type: "text", text: note }, ...content];
		}
		if (command?.name === "goal") {
			const condition = command.text.slice("/goal".length).trim();
			if (condition === "clear") this.goal = false;
			else if (condition) this.goal = true;
		}
		for (const block of blocks) {
			const path = block.type === "image" ? filePath(block.uri) : undefined;
			if (block.type === "image" && path) this.trace.noteImage(block.data, path);
		}
		this.context = context.length ? context.map((s) => s.text).join("\n\n") : undefined;
		let end: (turn: Turn) => void = () => {};
		const done = new Promise<Turn>((resolve) => {
			end = resolve;
		});
		const turn: Turn = { cancelled: false, running: false, end };
		this.turn = turn;
		this.trace.turnStart();
		if (!command) this.trace.userMessage(content);
		if (update) {
			const steps = `${update.steps} new ${update.steps === 1 ? "step" : "steps"}`;
			this.output.update({ sessionUpdate: "tool_call", toolCallId: `catch-up-${randomUUID()}`, title: `Caught up with the main thread: ${steps}`, kind: "read", status: "completed", content: textToolContent(update.text) });
		}
		this.conversation = true;
		try {
			engine.send(userMessage(content, randomUUID(), { command: !!command }));
			if (update) this.through = update.through;
		} catch (error) {
			turn.exit = errorText(error);
			this.finish();
		}
		const ended = await done;
		const result = outcome(ended);
		const usage = this.trace.turnUsage();
		await this.trace.turnEnd("stopReason" in result ? result.stopReason : "error");
		// Stopped: a goal stops too.
		if (ended.cancelled && this.goal) await this.clearGoal();
		await this.output.flushed();
		this.output.notify("_space/status", this.status());
		return { ...result, ...(usage ? { usage } : {}) };
	}

	/** `/goal clear`, quietly, between turns. */
	private async clearGoal(): Promise<void> {
		this.goal = false;
		const engine = this.engine;
		if (!engine) return;
		let end: (turn: Turn) => void = () => {};
		const done = new Promise<Turn>((resolve) => {
			end = resolve;
		});
		this.turn = { cancelled: false, running: false, silent: true, end };
		const timer = setTimeout(() => {
			log(`${this.id}: clearing the goal took too long`);
			this.finish();
		}, 30_000);
		try {
			engine.send(userMessage("/goal clear", randomUUID(), { command: true }));
			await done;
		} catch (error) {
			log(`${this.id}: couldn't clear the goal: ${errorText(error)}`);
			this.finish();
		} finally {
			clearTimeout(timer);
		}
	}

	private finish(): void {
		const turn = this.turn;
		if (!turn) return;
		this.turn = undefined;
		this.context = undefined;
		this.retrying = false;
		this.streamed = false;
		turn.end(turn);
	}

	/** `session/cancel`: withdraw what's steered and not read yet, then stop the turn. */
	async cancel(): Promise<void> {
		const turn = this.turn;
		if (!turn || turn.cancelled) return;
		turn.cancelled = true;
		this.withdrawn.push(...(await this.withdrawSteers()));
		await this.engine?.interrupt().catch((error: unknown) => log("interrupt failed:", errorText(error)));
	}

	/** `_space/session/steer`: a message for the running turn, read after its current step. */
	steer(id: string, blocks: readonly ContentBlock[]): void {
		if (!this.turn || !this.engine) throw new Error("No turn is running");
		const content = promptContent(blocks);
		for (const block of blocks) {
			const path = block.type === "image" ? filePath(block.uri) : undefined;
			if (block.type === "image" && path) this.trace.noteImage(block.data, path);
		}
		const uuid = randomUUID();
		this.steers.set(uuid, { id, content });
		this.engine.send(userMessage(content, uuid, { priority: "next" }));
	}

	/** `_space/session/unsteer`: the steered messages Claude will never read (withdrawn now, or when the turn was stopped). */
	async unsteer(): Promise<string[]> {
		const ids = [...this.withdrawn, ...(await this.withdrawSteers())];
		this.withdrawn = [];
		return ids;
	}

	private async withdrawSteers(): Promise<string[]> {
		const engine = this.engine;
		const dropped: string[] = [];
		for (const [uuid, steer] of [...this.steers]) {
			let cancelled = !engine;
			if (engine) {
				try {
					cancelled = await engine.cancelQueued(uuid);
				} catch (error) {
					log(`couldn't withdraw a steered message: ${errorText(error)}`);
				}
			}
			// Not cancelled: Claude has it (or is about to read it), and `steered` follows.
			if (cancelled && this.steers.delete(uuid)) dropped.push(steer.id);
		}
		return dropped;
	}

	/** `_space/session/configure`. Throws a readable error. */
	async configure(model: string | undefined, level: string | undefined): Promise<void> {
		if (this.turn) throw new Error("Cannot reconfigure while a prompt is running");
		const engine = await this.ensureEngine();
		if (model && model !== this.model) {
			if (!knownModel(this.models, model)) throw new Error(`Unknown model ${PROVIDER}/${model}`);
			await engine.setModel(model);
			this.model = model;
		}
		const want = (level ?? this.effort) as Effort;
		if (!EFFORTS.includes(want)) throw new Error(`thinkingLevel must be one of ${EFFORTS.join(", ")}`);
		const effort = clampEffort(want, effortsOf(this.models, this.model));
		if (effort !== this.effort) {
			await engine.setEffort(effort);
			this.effort = effort;
		}
		this.setHeader();
	}

	/** The model and effort a completion uses. */
	get settings(): { model: string | undefined; effort: Effort; conversation: boolean } {
		return { model: this.model, effort: this.effort, conversation: this.conversation };
	}

	close(): void {
		const engine = this.engine;
		this.engine = undefined;
		engine?.close();
		if (this.turn) {
			this.turn.exit ??= "Claude Code was stopped";
			this.finish();
		}
	}

	private onExit(error: Error | undefined): void {
		this.engine = undefined;
		this.steers.clear();
		log(`${this.id}: Claude Code stopped${error ? `: ${error.message}` : ""}`);
		if (this.turn) {
			this.turn.exit ??= `Claude Code stopped${error ? `: ${error.message}` : ""}`;
			this.finish();
		}
	}

	private onMessage(m: SDKMessage): void {
		const msg = m as unknown as Json;
		// Housekeeping shows nothing; only its end matters.
		if (this.turn?.silent) {
			if (msg.type === "system" && msg.subtype === "session_state_changed") this.onSystem(msg);
			return;
		}
		this.trace.observe(m);
		if ((msg.type === "assistant" || msg.type === "user") && typeof msg.uuid === "string") {
			if (this.handled.has(msg.uuid)) return;
			this.handled.add(msg.uuid);
		}
		const main = (msg.parent_tool_use_id ?? null) === null;
		switch (msg.type) {
			case "system":
				this.onSystem(msg);
				break;
			case "stream_event":
				if (main && isObject(msg.event)) this.onStream(msg.event);
				break;
			case "assistant":
				// An API error's message is reported by the turn's result instead.
				if (isObject(msg.message) && !msg.error) this.onAssistant(msg.message, main);
				break;
			case "user":
				if (isObject(msg.message)) this.onUser(msg.message, main, msg.isReplay === true);
				break;
			case "result":
				if (this.turn) this.turn.result = msg as unknown as ResultLike;
				this.onResult(msg as unknown as ResultLike);
				break;
		}
	}

	private onSystem(m: Json): void {
		switch (m.subtype) {
			case "init":
				if (m.apiKeySource !== "none") {
					log(`${this.id}: Claude Code would use an API key (${String(m.apiKeySource)}); stopping it`);
					if (this.turn) this.turn.exit = "Claude Code is set up to use an API key; Space only uses your Claude subscription.";
					const engine = this.engine;
					this.onExit(undefined);
					engine?.close();
				}
				break;
			case "session_state_changed": {
				const turn = this.turn;
				if (!turn) break;
				if (m.state === "running") turn.running = true;
				else if (m.state === "idle" && turn.running) this.finish();
				break;
			}
			case "status":
				if (m.status === "requesting") this.onRequest(m);
				break;
			case "api_retry": {
				// A request whose answer had started streaming is sent again from the start.
				if (this.streamed) this.output.notify(RETRY_METHOD, { sessionId: this.id, discard: true });
				this.streamed = false;
				this.retrying = true;
				this.output.notify(RETRY_METHOD, { sessionId: this.id, state: retryState(m) });
				break;
			}
			case "compact_boundary": {
				const meta = isObject(m.compact_metadata) ? m.compact_metadata : {};
				const before = Number(meta.pre_tokens ?? 0);
				const after = Number(meta.post_tokens ?? 0);
				if (after > 0) this.contextTokens = after;
				this.compacted = true;
				const sizes = before > 0 && after > 0 ? ` (${shortTokens(before)} → ${shortTokens(after)} tokens)` : "";
				this.text(`Compacted the conversation${sizes}.`);
				break;
			}
		}
	}

	/** A model request goes out: steered messages it carries were read. */
	private onRequest(m: Json): void {
		this.streamed = false;
		if (this.steers.size === 0) return;
		const carried = Array.isArray(m.user_message_uuids) ? new Set(m.user_message_uuids) : undefined;
		for (const [uuid, steer] of [...this.steers]) {
			if (carried && !carried.has(uuid)) continue;
			this.steers.delete(uuid);
			this.trace.userMessage(steer.content);
			this.output.notify(STEERED_METHOD, { sessionId: this.id, id: steer.id });
		}
	}

	private onStream(event: Json): void {
		switch (event.type) {
			case "message_start": {
				const msg = isObject(event.message) ? event.message : {};
				this.usage = isObject(msg.usage) ? { ...(msg.usage as AnthropicUsage) } : {};
				break;
			}
			case "message_delta":
				if (isObject(event.usage)) Object.assign(this.usage, event.usage);
				this.contextTokens = contextTokens(this.usage);
				break;
			case "content_block_delta": {
				const delta = isObject(event.delta) ? event.delta : {};
				if (delta.type === "text_delta" && typeof delta.text === "string" && delta.text) {
					this.content();
					this.output.update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: delta.text } });
				} else if (delta.type === "thinking_delta" && typeof delta.thinking === "string" && delta.thinking) {
					this.content();
					this.output.update({ sessionUpdate: "agent_thought_chunk", content: { type: "text", text: delta.thinking } });
				}
				break;
			}
		}
	}

	/** Content arrived: a retry notice is over. */
	private content(): void {
		this.streamed = true;
		if (this.retrying) {
			this.retrying = false;
			this.output.notify(RETRY_METHOD, { sessionId: this.id });
		}
	}

	private text(text: string): void {
		this.output.update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text } });
	}

	private onAssistant(message: Json, main: boolean): void {
		const content = Array.isArray(message.content) ? message.content : [];
		// Output of a command (`/context`, `/goal`) is a message Claude Code writes itself.
		if (message.model === "<synthetic>") {
			if (main) {
				const text = content.map((c) => (isObject(c) && c.type === "text" ? String(c.text) : "")).join("");
				if (text) this.text(text);
			}
			return;
		}
		for (const block of content) {
			if (!isObject(block) || block.type !== "tool_use" || typeof block.id !== "string" || this.shown.has(block.id)) continue;
			this.shown.add(block.id);
			this.streamed = false;
			const name = String(block.name ?? "");
			this.output.update({
				sessionUpdate: "tool_call",
				toolCallId: block.id,
				title: toolTitle(name, block.input),
				kind: toolKind(name),
				status: "in_progress",
				rawInput: block.input ?? {},
			});
		}
	}

	private onUser(message: Json, main: boolean, replay: boolean): void {
		const content = message.content;
		if (replay) {
			const output = localCommandOutput(content);
			// Space already said what a compaction did.
			if (output && !(this.compacted && output.startsWith("Compacted"))) this.text(output);
			this.compacted = false;
			return;
		}
		if (!Array.isArray(content)) {
			// Claude Code's own messages to Claude (a /goal's check, a compaction's summary):
			// a goal check is shown, the rest is the model's context, not the transcript.
			if (main && typeof content === "string") this.harnessMessage(content);
			return;
		}
		for (const block of content) {
			if (isObject(block) && block.type === "tool_result" && typeof block.tool_use_id === "string") {
				this.output.update({
					sessionUpdate: "tool_call_update",
					toolCallId: block.tool_use_id,
					status: block.is_error === true ? "failed" : "completed",
					content: textToolContent(toolResultText(block.content)),
				});
			} else if (main && isObject(block) && block.type === "text" && typeof block.text === "string") {
				this.harnessMessage(block.text);
			}
		}
	}

	/** A /goal's check: shown as a step of the turn. */
	private harnessMessage(text: string): void {
		const check = text.match(/^Stop hook feedback:\s*([\s\S]*)$/);
		if (!check?.[1]) return;
		const id = `goal-${randomUUID()}`;
		this.output.update({ sessionUpdate: "tool_call", toolCallId: id, title: "Goal check: not met yet", kind: "think", status: "completed", content: textToolContent(check[1].trim()) });
	}

	private onResult(r: ResultLike): void {
		const window = Object.values(r.modelUsage ?? {}).find((u) => typeof u.contextWindow === "number")?.contextWindow;
		if (window) this.contextWindow = window;
	}
}
