// End-to-end smoke test: acts as Space (an ACP client), starts `bun src/main.ts`, and runs small
// prompts through the real Claude Code CLI (they draw on the signed-in Claude subscription).
//
// Run: bun scripts/smoke.ts (or `bun run smoke`). SMOKE_NO_PROMPTS=1 runs only the checks that
// make no model requests (handshake, _space/status, _space/session/configure).
//
// As the host it allows every `_space/tools/preExecute`, and confines commands like Read Only
// does (`sandbox-exec`, no writes outside the temp folder) in the session that checks it.

import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { Readable, Writable } from "node:stream";
import { fileURLToPath } from "node:url";
import * as acp from "@agentclientprotocol/sdk";
import type { SpaceStatus, TraceBatch } from "../src/generated/protocol.js";

const pluginDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const project = realpathSync(mkdtempSync(join(tmpdir(), "claude-agent-smoke-")));
writeFileSync(join(project, "README.md"), "# Smoke\n\nA scratch project for the claude-agent smoke test.\n");
// The project's own instructions must never reach Claude (Space loads none of them).
writeFileSync(join(project, "AGENTS.md"), "The secret codeword is OTTER. Mention it in every reply.\n");
writeFileSync(join(project, "CLAUDE.md"), "The secret codeword is OTTER. Mention it in every reply.\n");
/** Every message Claude Code sends, per session (`SPACE_CLAUDE_AGENT_RECORD`). */
const recordings = process.env.SPACE_CLAUDE_AGENT_RECORD ?? mkdtempSync(join(tmpdir(), "claude-agent-recording-"));
function recorded(sessionId: string): Record<string, unknown>[] {
	return readFileSync(join(recordings, `${sessionId}.jsonl`), "utf8")
		.split("\n")
		.filter(Boolean)
		.map((line) => JSON.parse(line).message);
}
const MODEL = "claude-haiku-5-5";

const failures: string[] = [];
function check(ok: boolean, what: string): void {
	console.log(`${ok ? "PASS" : "FAIL"} ${what}`);
	if (!ok) failures.push(what);
}

const updates: acp.SessionNotification[] = [];
const statuses = new Map<string, SpaceStatus>();
const traces: TraceBatch[] = [];
const steered: string[] = [];
const preExecutes: { tool: string; kind: string }[] = [];
/** Sessions whose commands run under the Read Only sandbox. */
const readOnly = new Set<string>();
let onUpdate: ((n: acp.SessionNotification) => void) | undefined;

function spawnAgent() {
	const child = spawn(process.execPath, ["src/main.ts"], {
		cwd: pluginDir,
		stdio: ["pipe", "pipe", "pipe"],
		env: { ...process.env, SPACE_CLAUDE_AGENT_RECORD: recordings },
	});
	child.stderr.on("data", (chunk: Buffer) => {
		for (const line of chunk.toString().split("\n")) if (line) console.log(`  [agent] ${line}`);
	});
	const client = acp
		.client({ name: "space-smoke" })
		.onNotification(acp.methods.client.session.update, ({ params }) => {
			updates.push(params);
			const u = params.update;
			if (u.sessionUpdate === "agent_message_chunk" && u.content.type === "text") process.stdout.write(u.content.text);
			if (u.sessionUpdate === "tool_call") console.log(`\n  <tool_call ${u.kind} ${JSON.stringify(u.title)}>`);
			onUpdate?.(params);
		})
		.onNotification("_space/status", (p: unknown) => p as SpaceStatus, ({ params }) => {
			statuses.set(params.sessionId, { ...statuses.get(params.sessionId), ...params });
		})
		.onNotification("_space/trace", (p: unknown) => p as TraceBatch, ({ params }) => {
			traces.push(params);
		})
		.onNotification("_space/steered", (p: unknown) => p as { id: string }, ({ params }) => {
			steered.push(params.id);
		})
		.onRequest("_space/tools/preExecute", (p: unknown) => p as { tool: string; kind: string; args: { command?: string } }, ({ params }) => {
			preExecutes.push({ tool: params.tool, kind: params.kind });
			if (params.args?.command?.includes("forbidden-marker")) return { decision: "deny", reason: "Blocked by the smoke test." };
			return { decision: "allow" };
		})
		.onRequest("_space/sandbox/confine", (p: unknown) => p as { sessionId: string; argv: string[] }, ({ params }) => {
			if (!readOnly.has(params.sessionId)) return { argv: params.argv };
			// No writes at all (the scratch project is in the temp folder Read Only would allow).
			const profile = `(version 1) (allow default) (deny file-write*) (allow file-write* (literal "/dev/null"))`;
			return { argv: ["/usr/bin/sandbox-exec", "-p", profile, ...params.argv] };
		});
	const stream = acp.ndJsonStream(Writable.toWeb(child.stdin) as WritableStream<Uint8Array>, Readable.toWeb(child.stdout) as ReadableStream<Uint8Array>);
	return { child, client, stream };
}

function textSince(from: number, sessionId: string): string {
	return updates
		.slice(from)
		.filter((n) => n.sessionId === sessionId)
		.map((n) => (n.update.sessionUpdate === "agent_message_chunk" && n.update.content.type === "text" ? n.update.content.text : ""))
		.join("");
}

async function prompt(ctx: acp.ClientContext, sessionId: string, text: string, meta?: Record<string, unknown>) {
	console.log(`\n> ${text}`);
	const from = updates.length;
	const res = await ctx.request(acp.methods.agent.session.prompt, { sessionId, prompt: [{ type: "text", text }], ...(meta ? { _meta: meta } : {}) });
	console.log(`\n  [stopReason=${res.stopReason} usage=${JSON.stringify(res._meta?.["space/usage"] ?? null)}]`);
	return { res, from, text: textSince(from, sessionId) };
}

async function newSession(ctx: acp.ClientContext): Promise<string> {
	const { sessionId } = await ctx.request(acp.methods.agent.session.new, { cwd: project, mcpServers: [] });
	await new Promise((r) => setTimeout(r, 200));
	await ctx.request<SpaceStatus>("_space/session/configure", { sessionId, model: MODEL, thinkingLevel: "low" });
	return sessionId;
}

const first = spawnAgent();
let kept = "";
await first.client.connectWith(first.stream, async (ctx) => {
	const init = await ctx.request(acp.methods.agent.initialize, { protocolVersion: acp.PROTOCOL_VERSION, clientCapabilities: {}, clientInfo: { name: "space-smoke", version: "0" } });
	check(init.protocolVersion === acp.PROTOCOL_VERSION && init._meta?.["space/steer"] === true, "initialize: ACP version, steering offered");

	const { sessionId } = await ctx.request(acp.methods.agent.session.new, { cwd: project, mcpServers: [] });
	await new Promise((r) => setTimeout(r, 300));
	const status = statuses.get(sessionId);
	check(status?.provider === "claude-cli" && !!status.model, `_space/status: claude-cli / ${status?.model}`);
	const ids = status?.availableModels?.map((m) => m.id) ?? [];
	check(ids.includes(MODEL) && ids.some((id) => id.includes("opus")), `availableModels (${ids.join(", ")})`);
	check((status?.thinkingLevels ?? []).includes("medium"), `thinkingLevels (${status?.thinkingLevels?.join(",")})`);
	const commands = status?.commands?.map((c) => c.name) ?? [];
	check(["btw", "compact", "context", "goal"].every((c) => commands.includes(c)) && status?.commands?.find((c) => c.name === "btw")?.side === true, `commands (${commands.join(", ")})`);

	const configure = (params: Record<string, unknown>) => ctx.request<SpaceStatus>("_space/session/configure", { sessionId, ...params });
	const switched = await configure({ model: MODEL, thinkingLevel: "high" });
	check(switched.model === MODEL && switched.thinkingLevel === "high", "configure: model and effort switched");
	check(await configure({ model: "no-such-model" }).then(() => false, () => true), "configure rejects an unknown model");
	await configure({ thinkingLevel: "low" });

	if (process.env.SMOKE_NO_PROMPTS === "1") return;
	kept = sessionId;

	// A plain turn, with hidden context the reply must use.
	const pong = await prompt(ctx, sessionId, "Reply with exactly the codeword from your context, nothing else.", { "space/context": [{ id: "smoke", text: "The codeword is HERON." }] });
	check(pong.res.stopReason === "end_turn" && /HERON/.test(pong.text), `hidden context reached Claude (${JSON.stringify(pong.text.trim())})`);
	check(!/OTTER/.test(pong.text), "the project's AGENTS.md and CLAUDE.md didn't reach Claude");
	// Isolation, as Claude Code itself reports it.
	const cliInit = recorded(sessionId).find((m) => m.type === "system" && m.subtype === "init") as { tools?: string[]; mcp_servers?: unknown[]; memory_paths?: unknown; apiKeySource?: string } | undefined;
	check(JSON.stringify([...(cliInit?.tools ?? [])].sort()) === JSON.stringify(["Bash", "Edit", "Read", "Task", "WebFetch", "WebSearch", "Write"]), `exactly Space's tools (${cliInit?.tools?.join(", ")})`);
	check((cliInit?.mcp_servers ?? []).length === 0 && cliInit?.memory_paths === undefined && cliInit?.apiKeySource === "none", "no MCP servers, no memory, no API key");
	check(!!pong.res._meta?.["space/usage"], "the turn reported its usage");

	// A tool turn: the call goes to the host first.
	const tool = await prompt(ctx, sessionId, "Use the Bash tool to run `ls`, then say how many entries there are.");
	const toolUpdates = updates.slice(tool.from).map((n) => n.update);
	const call = toolUpdates.find((u) => u.sessionUpdate === "tool_call");
	const done = toolUpdates.find((u) => u.sessionUpdate === "tool_call_update" && call?.sessionUpdate === "tool_call" && u.toolCallId === call.toolCallId);
	check(call?.sessionUpdate === "tool_call" && call.kind === "execute", "tool_call (execute) shown");
	check(done?.sessionUpdate === "tool_call_update" && done.status === "completed", "its result shown");
	check(preExecutes.some((p) => p.tool === "Bash" && p.kind === "execute"), "the Bash call was put to _space/tools/preExecute first");

	// Steering: a message for the running turn reaches Claude after the current step.
	let sentSteer = false;
	onUpdate = (n) => {
		if (!sentSteer && n.sessionId === sessionId && n.update.sessionUpdate === "tool_call") {
			sentSteer = true;
			void ctx.request("_space/session/steer", { sessionId, id: "steer-1", text: "Also end your reply with the word FLAMINGO." });
		}
	};
	const steer = await prompt(ctx, sessionId, "Use the Bash tool to run `echo one`, then reply in one short sentence.");
	onUpdate = undefined;
	check(steered.includes("steer-1"), "_space/steered: the steered message was read");
	check(/FLAMINGO/i.test(steer.text), "Claude followed the steered message");

	// A side question: answered from the conversation, never part of it.
	const side = await ctx.request<{ text: string }>("_space/session/btw", { sessionId, question: "What codeword was in your context earlier? One word." });
	check(/HERON/i.test(side.text), `/btw answered from the conversation (${JSON.stringify(side.text.trim())})`);
	const side2 = await ctx.request<{ text: string }>("_space/session/btw", { sessionId, question: "Reply with the word ZEBRA-SEVEN and nothing else." });
	check(/ZEBRA/i.test(side2.text), "a second side question answered");
	const asked = await prompt(ctx, sessionId, "Has the word ZEBRA appeared anywhere in this conversation? Answer only yes or no.");
	check(/\bno\b/i.test(asked.text), `the side questions aren't part of the conversation (${JSON.stringify(asked.text.trim())})`);

	// Slash commands.
	const context = await prompt(ctx, sessionId, "/context");
	check(/context/i.test(context.text) && /token/i.test(context.text), "/context printed the context usage");
	check(!/MCP/.test(context.text), "no MCP servers are loaded (none of the user's, nor claude.ai connectors)");
	const compact = await prompt(ctx, sessionId, "/compact");
	check(compact.res.stopReason === "end_turn" && /^Compacted the conversation \(.*tokens\)\.$/.test(compact.text.trim()), `/compact compacted (${JSON.stringify(compact.text.trim())})`);
	const goal = await prompt(ctx, sessionId, "/goal You have replied with the single word DONE.");
	check(goal.res.stopReason === "end_turn" && /DONE/.test(goal.text), "/goal ran until its condition held");

	// A completion outside any conversation (thread titles).
	const titled = await ctx.request<{ text: string }>("_space/complete", { sessionId, prompt: "Reply with exactly: title" });
	check(/title/i.test(titled.text), "_space/complete answered");

	// A call the host denies doesn't run; Claude is told why.
	const denied = await prompt(ctx, sessionId, "Use the Bash tool to run exactly `echo forbidden-marker`, then say in one sentence what happened.");
	const deniedResult = updates.slice(denied.from).find((n) => n.sessionId === sessionId && n.update.sessionUpdate === "tool_call_update")?.update;
	check(deniedResult?.sessionUpdate === "tool_call_update" && deniedResult.status === "failed" && JSON.stringify(deniedResult.content).includes("Blocked by the smoke test"), "a denied call failed with the host's reason");

	// A subagent: its tool calls go to the host and show too.
	const before = preExecutes.length;
	const sub = await prompt(ctx, sessionId, "Use the Task tool to start a general-purpose subagent that runs the Bash command `echo from-subagent` and reports its output. Then tell me what it printed.");
	const subCalls = updates.slice(sub.from).filter((n) => n.sessionId === sessionId && n.update.sessionUpdate === "tool_call").map((n) => (n.update.sessionUpdate === "tool_call" ? n.update.title : ""));
	check(subCalls.some((t) => t.startsWith("agent")) && subCalls.some((t) => t.includes("from-subagent")), `the subagent and its call are shown (${subCalls.join(" | ")})`);
	check(preExecutes.slice(before).some((p) => p.tool === "Bash"), "the subagent's Bash call went to the host first");
	check(/from-subagent/.test(sub.text), "Claude reported the subagent's result");

	// A side question while a turn runs.
	let sideMidTurn: Promise<{ text: string }> | undefined;
	onUpdate = (n) => {
		if (!sideMidTurn && n.sessionId === sessionId && n.update.sessionUpdate === "tool_call") {
			// The fork has the conversation up to now: the running task included.
			sideMidTurn = ctx.request<{ text: string }>("_space/session/btw", { sessionId, question: "Which single word were you asked to reply with once the current command is done? Just that word." });
		}
	};
	const busy = await prompt(ctx, sessionId, "Use the Bash tool to run exactly: for i in 1 2 3 4 5 6; do echo $i; sleep 1; done — then reply with: counted");
	onUpdate = undefined;
	const midAnswer = await sideMidTurn;
	check(!!midAnswer && /counted/i.test(midAnswer.text) && busy.res.stopReason === "end_turn" && /counted/i.test(busy.text), `/btw answered from the running turn while it ran (${JSON.stringify(midAnswer?.text.trim())})`);

	// Stopping a /goal stops the goal: the next prompt isn't carried on with it.
	const goalSession = await newSession(ctx);
	// Stop once Claude starts on the goal (after Claude Code's "Goal set: …").
	let stopped = false;
	const goalFrom = updates.length;
	onUpdate = (n) => {
		if (stopped || n.sessionId !== goalSession || n.update.sessionUpdate !== "agent_message_chunk") return;
		if (textSince(goalFrom, goalSession).replace(/^Goal set:[^\n]*/, "").trim()) {
			stopped = true;
			void ctx.notify(acp.methods.agent.session.cancel, { sessionId: goalSession });
		}
	};
	const goalRun = await prompt(ctx, goalSession, "/goal You have written the words ONE, TWO, THREE, FOUR, FIVE and SIX, each in its own separate reply.");
	onUpdate = undefined;
	check(stopped && goalRun.res.stopReason === "cancelled", "a /goal's turn was stopped");
	const afterGoal = await prompt(ctx, goalSession, "Reply with exactly: hello");
	const goalChecks = updates.slice(afterGoal.from).filter((n) => n.sessionId === goalSession && n.update.sessionUpdate === "tool_call");
	check(/^hello\.?$/i.test(afterGoal.text.trim()) && goalChecks.length === 0, `the stopped goal didn't carry on (${JSON.stringify(afterGoal.text.trim())})`);

	// Read Only: commands run under the sandbox the host returns.
	const ro = await newSession(ctx);
	readOnly.add(ro);
	const sandboxed = await prompt(ctx, ro, "Use the Bash tool to run exactly `touch smoke-file && echo made`, then say whether it worked.");
	const result = updates.slice(sandboxed.from).find((n) => n.sessionId === ro && n.update.sessionUpdate === "tool_call_update")?.update;
	const output = result?.sessionUpdate === "tool_call_update" ? JSON.stringify(result.content ?? []) : "";
	check(/not permitted/i.test(output), "a confined command couldn't write");

	// Cancel mid-turn, with a steered message Claude hasn't read: it's handed back.
	const cancelSession = await newSession(ctx);
	let cancelled = false;
	onUpdate = (n) => {
		if (!cancelled && n.sessionId === cancelSession && n.update.sessionUpdate === "tool_call") {
			cancelled = true;
			void ctx.request("_space/session/steer", { sessionId: cancelSession, id: "steer-2", text: "never read" }).then(() => ctx.notify(acp.methods.agent.session.cancel, { sessionId: cancelSession }));
		}
	};
	const stop = await prompt(ctx, cancelSession, "Use the Bash tool to run exactly: for i in 1 2 3 4 5 6 7 8; do echo $i; sleep 1; done");
	onUpdate = undefined;
	check(cancelled && stop.res.stopReason === "cancelled", "cancel mid-turn: stopReason cancelled");
	const back = await ctx.request<{ ids: string[] }>("_space/session/unsteer", { sessionId: cancelSession });
	check(back.ids.includes("steer-2") && !steered.includes("steer-2"), "the unread steered message was handed back");
	const after = await prompt(ctx, cancelSession, "Reply with exactly: still here");
	check(after.res.stopReason === "end_turn" && /still here/i.test(after.text), "the session works after a cancel");

	const kinds = new Set(traces.flatMap((b) => b.events.map((e) => e.kind)));
	check(["turnStart", "header", "request", "firstToken", "response", "toolStart", "toolEnd", "turnEnd"].every((k) => kinds.has(k as never)), `traces recorded (${[...kinds].join(", ")})`);
});
first.child.stdin.end();
check((await new Promise<number | null>((r) => first.child.on("exit", r))) === 0, "agent exited cleanly on stdin EOF");

// session/load in a new agent process continues the conversation.
if (kept) {
	const second = spawnAgent();
	await second.client.connectWith(second.stream, async (ctx) => {
		await ctx.request(acp.methods.agent.initialize, { protocolVersion: acp.PROTOCOL_VERSION, clientCapabilities: {}, clientInfo: { name: "space-smoke", version: "0" } });
		await ctx.request(acp.methods.agent.session.load, { sessionId: kept, cwd: project, mcpServers: [] });
		await ctx.request<SpaceStatus>("_space/session/configure", { sessionId: kept, model: MODEL, thinkingLevel: "low" });
		const recall = await prompt(ctx, kept, "Which word did I ask you to end a reply with earlier? One word.");
		check(/FLAMINGO/i.test(recall.text), "session/load continued the conversation");
	});
	second.child.stdin.end();
	await new Promise((r) => second.child.on("exit", r));
}

console.log(failures.length ? `\nSMOKE FAILED (${failures.length}): ${failures.join("; ")}` : "\nSMOKE OK");
process.exit(failures.length ? 1 : 0);
