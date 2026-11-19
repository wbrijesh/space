// The session against recorded Claude Code streams (src/fixtures, recorded with
// SPACE_CLAUDE_AGENT_RECORD from the smoke test) played by a fake engine: no model calls.

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import type { SessionUpdate } from "@agentclientprotocol/sdk";
import type { HookJSONOutput, PreToolUseHookInput } from "@anthropic-ai/claude-agent-sdk";
import type { EngineOptions, ModelInfo, SDKMessage, SlashCommand, userMessage } from "./claude.js";
import type { ConfineResult, PreExecute, PreExecuteResult, TraceEvent } from "./generated/protocol.js";
import { availableModels, clampEffort, type EngineLike, menuCommands, Session } from "./session.js";
import { TraceRecorder } from "./trace.js";

type Sent = ReturnType<typeof userMessage>;

function fixture(name: string): SDKMessage[] {
	return readFileSync(join(import.meta.dir, "fixtures", `${name}.jsonl`), "utf8")
		.split("\n")
		.filter(Boolean)
		.map((line) => JSON.parse(line));
}

const MODELS = [
	{ value: "default", resolvedModel: "claude-opus-5-5", displayName: "Default (recommended)", supportsEffort: true, supportedEffortLevels: ["low", "medium", "high", "xhigh", "max"] },
	{ value: "opus", resolvedModel: "claude-opus-5-5", displayName: "Opus 5.5", supportsEffort: true, supportedEffortLevels: ["low", "medium", "high", "xhigh", "max"] },
	{ value: "claude-opus-4-8", resolvedModel: "claude-opus-4-8", displayName: "Opus 4.8", supportsEffort: true, supportedEffortLevels: ["low", "medium", "high"] },
	{ value: "haiku", resolvedModel: "claude-haiku-5-5", displayName: "Haiku 5.5", supportsEffort: true, supportedEffortLevels: ["low", "medium", "high", "xhigh", "max"] },
	{ value: "claude-haiku-4-5-20251001", resolvedModel: "claude-haiku-4-5-20251001", displayName: "Haiku 4.5", supportsEffort: false },
] as unknown as ModelInfo[];

const COMMANDS = [
	{ name: "compact", description: "Free up context by summarizing the conversation so far", argumentHint: "<optional custom summarization instructions>" },
	{ name: "context", description: "Show current context usage", argumentHint: "" },
	{ name: "goal", description: "Set a goal", argumentHint: "" },
	{ name: "model", description: "Set the AI model", argumentHint: "[model]" },
] as SlashCommand[];

/** A Claude Code process that plays recorded turns. */
class FakeEngine implements EngineLike {
	readonly sent: Sent[] = [];
	/** Turns to play, one per prompt sent. */
	readonly turns: SDKMessage[][] = [];
	/** Steered messages not delivered yet (a `requesting` status carries them). */
	readonly queued = new Set<string>();
	interrupts = 0;
	closed = false;
	effort: string | undefined;
	model: string | undefined;
	account: EngineLike["account"] = async () => ({ apiProvider: "firstParty", subscriptionType: "Claude Max" });
	/** Called before each played message (tests act mid-turn). */
	onPlay: ((m: SDKMessage) => Promise<void> | void) | undefined;

	readonly options: EngineOptions;
	readonly emit: (m: SDKMessage) => void;
	readonly exit: (error: Error | undefined) => void;

	constructor(options: EngineOptions, emit: (m: SDKMessage) => void, exit: (error: Error | undefined) => void) {
		this.options = options;
		this.emit = emit;
		this.exit = exit;
	}

	send(message: Sent): void {
		this.sent.push(message);
		if (message.priority === "next") {
			this.queued.add(message.uuid as string);
			return;
		}
		const turn = this.turns.shift();
		if (turn) void this.play(turn);
	}

	async play(messages: SDKMessage[]): Promise<void> {
		for (const m of messages) {
			await Bun.sleep(0);
			if (this.closed) return;
			await this.onPlay?.(m);
			const msg = m as unknown as Record<string, unknown>;
			// The recorded request carried the recorded steer; this one carries ours.
			if (msg.type === "system" && msg.subtype === "status" && msg.status === "requesting" && Array.isArray(msg.user_message_uuids)) {
				this.emit({ ...msg, user_message_uuids: [...this.queued] } as unknown as SDKMessage);
				this.queued.clear();
				continue;
			}
			this.emit(m);
		}
	}

	async interrupt(): Promise<void> {
		this.interrupts++;
	}

	async cancelQueued(uuid: string): Promise<boolean> {
		return this.queued.delete(uuid);
	}

	async setModel(model: string): Promise<void> {
		this.model = model;
	}

	async setEffort(effort: string): Promise<void> {
		this.effort = effort;
	}

	async models(): Promise<ModelInfo[]> {
		return MODELS;
	}

	async commands(): Promise<SlashCommand[]> {
		return COMMANDS;
	}

	close(): void {
		this.closed = true;
	}
}

interface Options {
	preExecute?: (p: PreExecute) => Promise<PreExecuteResult>;
	confine?: (p: { argv: string[] }) => Promise<ConfineResult>;
	account?: FakeEngine["account"];
	resume?: boolean;
}

async function harness(options: Options = {}) {
	const updates: SessionUpdate[] = [];
	const notes: { method: string; params: Record<string, unknown> }[] = [];
	const traces: TraceEvent[] = [];
	const engines: FakeEngine[] = [];
	const session = await Session.open({
		id: "0b9a2c1e-1111-4222-8333-444455556666",
		cwd: "/work/project",
		cli: "/usr/local/bin/claude",
		resume: options.resume ?? false,
		host: {
			preExecute: options.preExecute ?? (async () => ({ decision: "allow" })),
			confine: options.confine ?? (async (p) => ({ argv: p.argv })),
		},
		output: {
			update: (u) => updates.push(u),
			notify: (method, params) => notes.push({ method, params: params as Record<string, unknown> }),
			flushed: async () => {},
		},
		trace: new TraceRecorder("s", async (batch) => {
			traces.push(...batch.events);
		}),
		start: (opts, onMessage, onExit) => {
			const engine = new FakeEngine(opts, onMessage, onExit);
			if (options.account) engine.account = options.account;
			engines.push(engine);
			return engine;
		},
	});
	const engine = () => {
		const e = engines.at(-1);
		if (!e) throw new Error("no engine");
		return e;
	};
	const text = () => updates.map((u) => (u.sessionUpdate === "agent_message_chunk" && u.content.type === "text" ? u.content.text : "")).join("");
	const prompt = (t: string, ...turns: SDKMessage[][]) => {
		engine().turns.push(...turns);
		return session.prompt([{ type: "text", text: t }], []);
	};
	return { session, updates, notes, traces, engines, engine, text, prompt };
}

function hookInput(tool: string, input: Record<string, unknown>): PreToolUseHookInput {
	return { hook_event_name: "PreToolUse", tool_name: tool, tool_input: input, tool_use_id: "toolu_1", session_id: "s", transcript_path: "/t", cwd: "/work/project" } as PreToolUseHookInput;
}

function decision(out: HookJSONOutput): { permissionDecision?: string; permissionDecisionReason?: string; updatedInput?: Record<string, unknown> } {
	return (out as { hookSpecificOutput?: Record<string, never> }).hookSpecificOutput ?? {};
}

describe("opening", () => {
	test("only on a Claude subscription", async () => {
		await expect(harness({ account: async () => ({ apiProvider: "bedrock" }) })).rejects.toThrow(/subscription/);
		await expect(harness({ account: async () => ({ apiProvider: "firstParty" }) })).rejects.toThrow(/subscription/);
		await expect(harness({ account: async () => ({ apiProvider: "firstParty", subscriptionType: "Max", apiKeySource: "ANTHROPIC_API_KEY" }) })).rejects.toThrow(/subscription/);
	});

	test("isolated, on Claude Code's default model, at medium effort", async () => {
		const h = await harness();
		const options = h.engine().options;
		expect(options.resume).toBe(false);
		expect(options.effort).toBe("medium");
		const status = h.session.status();
		expect(status.provider).toBe("claude-cli");
		expect(status.model).toBe("claude-opus-5-5");
		expect(status.thinkingLevel).toBe("medium");
		expect(status.availableModels?.map((m) => m.id)).toEqual(["claude-opus-5-5", "claude-haiku-5-5"]);
		expect(status.thinkingLevels).toEqual(["low", "medium", "high", "xhigh", "max"]);
		expect(status.commands?.map((c) => c.name)).toEqual(["btw", "compact", "context", "goal"]);
		expect(status.continueIn).toBeUndefined();
	});
});

describe("turns", () => {
	test("a turn streams its answer and ends when Claude Code is idle", async () => {
		const h = await harness();
		// An idle Claude Code reports before it takes the prompt doesn't end the turn.
		const stale = { type: "system", subtype: "session_state_changed", state: "idle", uuid: "x", session_id: "s" } as unknown as SDKMessage;
		const outcome = await h.prompt("Reply with the codeword", [stale, ...fixture("plain")]);
		expect(outcome).toMatchObject({ stopReason: "end_turn" });
		expect(h.text()).toBe("HERON");
		expect("usage" in outcome && outcome.usage?.models).toEqual(["claude-cli/claude-haiku-5-5"]);
		const sent = h.engine().sent[0];
		expect(sent?.client_composed).toBe(true);
		expect(sent?.message.content).toEqual([{ type: "text", text: "Reply with the codeword" }]);
		const status = h.notes.at(-1);
		expect(status?.method).toBe("_space/status");
		expect(status?.params.continueIn).toEqual({ app: "Claude Code", command: "cd /work/project && claude --resume 0b9a2c1e-1111-4222-8333-444455556666 --fork-session" });
		expect(status?.params.contextTokens).toBeGreaterThan(0);
	});

	test("tool calls are shown with their results", async () => {
		const h = await harness();
		await h.prompt("ls", fixture("tool"));
		const call = h.updates.find((u) => u.sessionUpdate === "tool_call");
		expect(call).toMatchObject({ kind: "execute", status: "in_progress", title: "bash: ls" });
		const result = h.updates.find((u) => u.sessionUpdate === "tool_call_update");
		expect(result).toMatchObject({ status: "completed" });
		expect(call?.sessionUpdate === "tool_call" && result?.sessionUpdate === "tool_call_update" && call.toolCallId === result.toolCallId).toBe(true);
	});

	test("hidden context is given for the prompt being submitted only", async () => {
		const h = await harness();
		h.engine().turns.push(fixture("plain"));
		const turn = h.session.prompt([{ type: "text", text: "hi" }], [{ id: "a", text: "Mode: Read Only." }, { id: "b", text: "More." }]);
		await Bun.sleep(0);
		expect(h.engine().options.hooks.promptContext()).toBe("Mode: Read Only.\n\nMore.");
		await turn;
		expect(h.engine().options.hooks.promptContext()).toBeUndefined();
	});

	test("Claude Code stopping fails the turn; the next prompt resumes the conversation", async () => {
		const h = await harness();
		const first = h.engine();
		const half = fixture("plain").slice(0, 6);
		first.onPlay = (m) => {
			if (m === half.at(-1)) first.exit(new Error("killed"));
		};
		const outcome = await h.prompt("hi", half);
		expect(outcome).toMatchObject({ error: "Claude Code stopped: killed" });
		const again = h.session.prompt([{ type: "text", text: "again" }], []);
		await Bun.sleep(0);
		expect(h.engines.length).toBe(2);
		expect(h.engine().options.resume).toBe(true);
		h.engine().turns.push(fixture("plain"));
		void h.engine().play(fixture("plain"));
		expect(await again).toMatchObject({ stopReason: "end_turn" });
	});

	test("an API key in use ends the turn and Claude Code", async () => {
		const h = await harness();
		const init = { type: "system", subtype: "init", apiKeySource: "ANTHROPIC_API_KEY", session_id: "s", uuid: "i" } as unknown as SDKMessage;
		const outcome = await h.prompt("hi", [init]);
		expect(outcome).toMatchObject({ error: expect.stringContaining("API key") });
		expect(h.engines[0]?.closed).toBe(true);
	});

	test("one prompt at a time", async () => {
		const h = await harness();
		const turn = h.prompt("hi", fixture("plain"));
		await expect(h.session.prompt([{ type: "text", text: "two" }], [])).rejects.toThrow(/already running/);
		await turn;
	});
});

describe("recorded turns", () => {
	test("a call the host denied: its row fails with the host's reason, and the turn goes on", async () => {
		const h = await harness();
		const outcome = await h.prompt("echo forbidden-marker", fixture("denied"));
		const failed = h.updates.find((u) => u.sessionUpdate === "tool_call_update");
		expect(failed).toMatchObject({ status: "failed" });
		expect(JSON.stringify(failed)).toContain("Blocked by the smoke test");
		expect(outcome).toMatchObject({ stopReason: "end_turn" });
		expect(h.text().length).toBeGreaterThan(0);
	});

	test("a subagent: its own call shows beside the agent's, and both finish", async () => {
		const h = await harness();
		await h.prompt("Use a subagent", fixture("subagent"));
		const calls = h.updates.filter((u) => u.sessionUpdate === "tool_call");
		expect(calls.map((c) => c.sessionUpdate === "tool_call" && [c.kind, c.title])).toEqual([
			["other", expect.stringMatching(/^agent: /)],
			["execute", "bash: echo from-subagent"],
		]);
		const done = h.updates.filter((u) => u.sessionUpdate === "tool_call_update" && u.status === "completed");
		expect(done).toHaveLength(2);
		expect(h.text()).toContain("from-subagent");
	});

	test("stopping a /goal clears the goal, quietly, before the turn is reported stopped", async () => {
		const h = await harness();
		const engine = h.engine();
		const turn = fixture("goal-stopped");
		const stopAt = turn.findIndex((m) => m.type === "assistant" && (m as unknown as { message: { model: string } }).message.model !== "<synthetic>");
		engine.onPlay = async (m) => {
			if (m === turn[stopAt]) await h.session.cancel();
		};
		const outcome = await h.prompt("/goal You have written six words", turn, fixture("goal-cleared"));
		expect(outcome).toMatchObject({ stopReason: "cancelled" });
		expect(engine.sent.map((m) => m.message.content)).toEqual(["/goal You have written six words", "/goal clear"]);
		expect(engine.sent[1]?.client_composed).toBeUndefined();
		expect(h.text()).toContain("Goal set:");
		expect(h.text()).not.toContain("Goal cleared");
		// Once cleared, stopping again sends nothing more.
		await h.prompt("hi", fixture("plain"));
		expect(engine.sent).toHaveLength(3);
	});

	test("a goal that was cleared, or only asked about, isn't cleared again on stop", async () => {
		const h = await harness();
		const engine = h.engine();
		for (const text of ["/goal", "/goal clear"]) {
			const turn = fixture("cancel");
			engine.onPlay = async (m) => {
				if (m === turn[2]) await h.session.cancel();
			};
			await h.prompt(text, turn);
		}
		expect(engine.sent.map((m) => m.message.content)).toEqual(["/goal", "/goal clear"]);
	});
});

describe("tool calls go to the host first", () => {
	test("allowed, denied, or blocked when the host can't answer", async () => {
		const verdicts: PreExecuteResult[] = [{ decision: "allow" }, { decision: "deny", reason: "Blocked by guardrail: no." }];
		const seen: PreExecute[] = [];
		const h = await harness({
			preExecute: async (p) => {
				seen.push(p);
				const v = verdicts.shift();
				if (!v) throw new Error("host gone");
				return v;
			},
		});
		const { hooks } = h.engine().options;
		const signal = new AbortController().signal;
		expect(decision(await hooks.preToolUse(hookInput("Edit", { file_path: "a", old_string: "x", new_string: "y" }), "c1", signal))).toEqual({ hookEventName: "PreToolUse", permissionDecision: "allow" } as never);
		expect(seen[0]).toMatchObject({ toolCallId: "c1", tool: "Edit", kind: "edit" });
		expect(decision(await hooks.preToolUse(hookInput("Read", { file_path: "a" }), "c2", signal)).permissionDecisionReason).toBe("Blocked by guardrail: no.");
		const failed = decision(await hooks.preToolUse(hookInput("Read", { file_path: "a" }), "c3", signal));
		expect(failed.permissionDecision).toBe("deny");
		expect(failed.permissionDecisionReason).toContain("host gone");
		// Claude Code's own prompt: only for calls the host allowed.
		const opts = { signal, toolUseID: "c1" } as Parameters<typeof hooks.canUseTool>[2];
		expect((await hooks.canUseTool("Edit", {}, opts))?.behavior).toBe("allow");
		expect((await hooks.canUseTool("Edit", {}, { ...opts, toolUseID: "c2" }))?.behavior).toBe("deny");
	});

	test("a stopped turn ends a held call", async () => {
		const h = await harness({ preExecute: () => new Promise(() => {}) });
		const stop = new AbortController();
		const held = h.engine().options.hooks.preToolUse(hookInput("Read", { file_path: "a" }), "c1", stop.signal);
		stop.abort();
		expect(decision(await held).permissionDecision).toBe("deny");
	});

	test("shell commands run as the host confines them", async () => {
		const wrap = (argv: string[]) => (argv[2] === "touch it's" ? ["/usr/bin/sandbox-exec", "-p", "(version 1)", ...argv] : argv);
		const h = await harness({ confine: async (p) => ({ argv: wrap(p.argv) }) });
		const { hooks } = h.engine().options;
		const signal = new AbortController().signal;
		const wrapped = decision(await hooks.preToolUse(hookInput("Bash", { command: "touch it's", description: "d" }), "c1", signal));
		expect(wrapped.updatedInput).toEqual({ command: "/usr/bin/sandbox-exec -p '(version 1)' /bin/bash -c 'touch it'\\''s'", description: "d" });
		const plain = decision(await hooks.preToolUse(hookInput("Bash", { command: "ls" }), "c2", signal));
		expect(plain).toEqual({ hookEventName: "PreToolUse", permissionDecision: "allow" } as never);
	});
});

describe("slash commands", () => {
	test("an offered command runs as one; any other `/text` is the user's text", async () => {
		const h = await harness();
		await h.prompt("/compact", fixture("compact"));
		expect(h.engine().sent[0]?.message.content).toBe("/compact");
		expect(h.engine().sent[0]?.client_composed).toBeUndefined();
		await h.prompt("/model haiku", fixture("plain"));
		expect(h.engine().sent[1]?.client_composed).toBe(true);
	});

	test("/compact says what it did, once (the messages it keeps come again and aren't shown twice)", async () => {
		const h = await harness();
		await h.prompt("/context", fixture("context"));
		expect(h.text()).toStartWith("## Context Usage");
		const before = h.text().length;
		await h.prompt("/compact", fixture("compact"));
		expect(h.text().slice(before)).toMatch(/^Compacted the conversation \(9\.7k → 1\.4k tokens\)\.$/);
		const boundary = fixture("compact").find((m) => m.type === "system" && m.subtype === "compact_boundary") as unknown as { compact_metadata: { post_tokens: number } };
		expect(h.session.status().contextTokens).toBe(boundary.compact_metadata.post_tokens);
	});

	test("/goal runs to its condition; a check that isn't met yet is shown", async () => {
		const h = await harness();
		const check = { type: "user", message: { role: "user", content: [{ type: "text", text: "Stop hook feedback:\n[Write DONE]: Not yet." }] }, parent_tool_use_id: null, uuid: "g1", session_id: "s" } as unknown as SDKMessage;
		const goal = fixture("goal");
		const at = goal.findIndex((m) => m.type === "result");
		await h.prompt("/goal You have replied DONE", [...goal.slice(0, at), check, ...goal.slice(at)]);
		expect(h.text()).toContain("Goal set:");
		expect(h.updates.find((u) => u.sessionUpdate === "tool_call" && u.kind === "think")).toMatchObject({ title: "Goal check: not met yet", status: "completed" });
	});

	test("the menu offers Claude Code's commands Space supports, and /btw", () => {
		const menu = menuCommands(COMMANDS);
		expect(menu.map((c) => c.name)).toEqual(["btw", "compact", "context", "goal"]);
		expect(menu.find((c) => c.name === "btw")?.side).toBe(true);
		expect(menu.find((c) => c.name === "context")?.hint).toBeUndefined();
		expect(menuCommands([]).map((c) => c.name)).toEqual(["btw"]);
	});
});

describe("steering", () => {
	test("a steered message is reported when the request that carries it goes out", async () => {
		const h = await harness();
		const engine = h.engine();
		engine.onPlay = (m) => {
			if (m.type === "assistant" && engine.sent.length === 1) h.session.steer("steer-1", [{ type: "text", text: "Also say FLAMINGO" }]);
		};
		await h.prompt("echo one", fixture("steer"));
		expect(engine.sent[1]).toMatchObject({ priority: "next", message: { content: [{ type: "text", text: "Also say FLAMINGO" }] } });
		expect(h.notes.filter((n) => n.method === "_space/steered").map((n) => n.params.id)).toEqual(["steer-1"]);
		expect(await h.session.unsteer()).toEqual([]);
	});

	test("stopping withdraws what Claude hasn't read, then interrupts", async () => {
		const h = await harness();
		const engine = h.engine();
		const turn = fixture("cancel");
		const result = turn.findIndex((m) => m.type === "result");
		engine.onPlay = async (m) => {
			if (m === turn[result]) {
				h.session.steer("steer-2", [{ type: "text", text: "never read" }]);
				await h.session.cancel();
			}
		};
		const outcome = await h.prompt("count", turn);
		expect(outcome).toMatchObject({ stopReason: "cancelled" });
		expect(engine.interrupts).toBe(1);
		expect(await h.session.unsteer()).toEqual(["steer-2"]);
		expect(h.notes.some((n) => n.method === "_space/steered")).toBe(false);
	});

	test("only while a turn runs", async () => {
		const h = await harness();
		expect(() => h.session.steer("s", [{ type: "text", text: "x" }])).toThrow(/No turn/);
	});
});

describe("configure", () => {
	test("switches model and effort; an effort the model lacks is clamped", async () => {
		const h = await harness();
		await h.session.configure("claude-opus-4-8", "max");
		expect(h.engine().model).toBe("claude-opus-4-8");
		expect(h.engine().effort).toBe("high");
		expect(h.session.status()).toMatchObject({ model: "claude-opus-4-8", thinkingLevel: "high", thinkingLevels: ["low", "medium", "high"] });
		await expect(h.session.configure("gpt-9", undefined)).rejects.toThrow(/Unknown model/);
		await expect(h.session.configure(undefined, "ultra")).rejects.toThrow(/thinkingLevel/);
	});

	test("an older model a thread uses stays listed", () => {
		expect(availableModels(MODELS, "claude-opus-4-8").map((m) => m.id)).toEqual(["claude-opus-5-5", "claude-opus-4-8", "claude-haiku-5-5"]);
		expect(clampEffort("xhigh", ["low", "medium"])).toBe("medium");
		expect(clampEffort("low", ["medium", "high"])).toBe("medium");
		expect(clampEffort("max", [])).toBe("max");
	});
});

test("a loaded session resumes Claude Code's conversation", async () => {
	const h = await harness({ resume: true });
	expect(h.engine().options.resume).toBe(true);
	expect(h.session.status().continueIn).toBeDefined();
});
