// End-to-end smoke test: acts as an ACP client, spawns `node dist/main.js`, and runs a few tiny
// prompts against the real claude-cli provider (bills the signed-in Claude subscription).
//
// Run: pnpm smoke   (or: node scripts/smoke.ts after `pnpm build`)
// Set SMOKE_SKIP_CANCEL=1 to skip the cancellation check, SMOKE_NO_PROMPTS=1 to run only the
// checks that make no model requests (handshake, _space/status, _space/session/configure,
// session/set_mode).

import { spawn } from "node:child_process";
import { dirname, resolve } from "node:path";
import { Readable, Writable } from "node:stream";
import { fileURLToPath } from "node:url";
import * as acp from "@agentclientprotocol/sdk";

const pluginDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = resolve(pluginDir, "..", "..");

type Status = {
	sessionId: string;
	provider: string;
	model: string;
	thinkingLevel: string;
	availableModels?: { provider: string; id: string; name?: string }[];
	thinkingLevels?: string[];
	contextWindow?: number;
};

const failures: string[] = [];
function check(ok: boolean, what: string): void {
	console.log(`${ok ? "PASS" : "FAIL"} ${what}`);
	if (!ok) failures.push(what);
}

const child = spawn(process.execPath, ["dist/main.js"], { cwd: pluginDir, stdio: ["pipe", "pipe", "pipe"] });
child.stderr.on("data", (chunk: Buffer) => {
	for (const line of chunk.toString().split("\n")) if (line) console.log(`  [agent stderr] ${line}`);
});

const updates: acp.SessionNotification[] = [];
let onUpdate: ((n: acp.SessionNotification) => void) | undefined;
let status: Status | undefined;

const client = acp
	.client({ name: "space-smoke" })
	.onNotification(acp.methods.client.session.update, ({ params }) => {
		updates.push(params);
		const u = params.update;
		switch (u.sessionUpdate) {
			case "agent_message_chunk":
				if (u.content.type === "text") process.stdout.write(u.content.text);
				break;
			case "agent_thought_chunk":
				if (u.content.type === "text") process.stdout.write(`\x1b[2m${u.content.text}\x1b[0m`);
				break;
			case "tool_call":
				console.log(`\n  <tool_call id=${u.toolCallId} kind=${u.kind} status=${u.status} title=${JSON.stringify(u.title)}>`);
				break;
			case "tool_call_update": {
				const first = u.content?.[0];
				const text = first?.type === "content" && first.content.type === "text" ? first.content.text : "";
				console.log(`  <tool_call_update id=${u.toolCallId} status=${u.status} content=${JSON.stringify(text.slice(0, 200))}>`);
				break;
			}
		}
		onUpdate?.(params);
	})
	.onNotification("_space/status", (p: unknown) => p as Status, ({ params }) => {
		status = params;
		console.log(`  <_space/status ${JSON.stringify(params)}>`);
	});

const stream = acp.ndJsonStream(
	Writable.toWeb(child.stdin) as WritableStream<Uint8Array>,
	Readable.toWeb(child.stdout) as ReadableStream<Uint8Array>,
);

async function runPrompt(ctx: acp.ClientContext, sessionId: string, text: string): Promise<{ stopReason: string; from: number }> {
	console.log(`\n> ${text}`);
	const from = updates.length;
	const res = await ctx.request(acp.methods.agent.session.prompt, { sessionId, prompt: [{ type: "text", text }] });
	console.log(`\n  [stopReason=${res.stopReason}]`);
	return { stopReason: res.stopReason, from };
}

await client.connectWith(stream, async (ctx) => {
	const init = await ctx.request(acp.methods.agent.initialize, {
		protocolVersion: acp.PROTOCOL_VERSION,
		clientCapabilities: {},
		clientInfo: { name: "space-smoke", version: "0.0.0" },
	});
	console.log(`initialize -> protocolVersion=${init.protocolVersion} agentInfo=${JSON.stringify(init.agentInfo)}`);
	check(init.protocolVersion === acp.PROTOCOL_VERSION, "protocol version negotiated");

	const { sessionId, modes } = await ctx.request(acp.methods.agent.session.new, { cwd: repoRoot, mcpServers: [] });
	console.log(`session/new -> ${sessionId} modes=${JSON.stringify(modes)}`);
	const modeIds = modes?.availableModes.map((m) => m.id) ?? [];
	check(modes?.currentModeId === "write" && modeIds.includes("read") && modeIds.includes("write"), "session/new advertises write (current) and read modes");
	await new Promise((r) => setTimeout(r, 200));
	check(
		status?.sessionId === sessionId && status.provider === "claude-cli" && status.model === "claude-opus-5-5",
		"_space/status arrived with claude-cli / claude-opus-5-5",
	);

	const ids = status?.availableModels?.map((m) => m.id) ?? [];
	check(ids.includes("claude-opus-5-5") && ids.includes("claude-sonnet-5-5"), `availableModels lists Opus/Sonnet 5.5 (${ids.length} models)`);
	check((status?.thinkingLevels ?? []).includes("medium"), `thinkingLevels reported (${status?.thinkingLevels?.join(",")})`);

	// _space/session/configure (no model request).
	const configure = (params: Record<string, unknown>) =>
		ctx.request<Status>("_space/session/configure", { sessionId, ...params });
	const switched = await configure({ model: "claude-sonnet-5-5", thinkingLevel: "high" });
	check(switched.model === "claude-sonnet-5-5" && switched.thinkingLevel === "high", "configure switched to sonnet-5-5 / high");
	const back = await configure({ model: "claude-opus-5-5", thinkingLevel: "medium" });
	check(back.model === "claude-opus-5-5" && back.thinkingLevel === "medium", "configure switched back to opus-5-5 / medium");
	const rejected = await configure({ model: "no-such-model" }).then(
		() => false,
		() => true,
	);
	check(rejected, "configure rejects an unknown model");

	// session/set_mode (no model request; the active tools per mode are checked by scripts/mode-test.ts).
	const setMode = (modeId: string) => ctx.request(acp.methods.agent.session.setMode, { sessionId, modeId });
	check(await setMode("read").then(() => true, () => false), "set_mode read accepted");
	check(await setMode("write").then(() => true, () => false), "set_mode write accepted");
	check(await setMode("nope").then(() => false, () => true), "set_mode rejects an unknown mode");

	if (process.env.SMOKE_NO_PROMPTS === "1") return;

	// 1. Plain text turn.
	const pong = await runPrompt(ctx, sessionId, "Reply with exactly: pong");
	const pongText = updates
		.slice(pong.from)
		.map((n) => (n.update.sessionUpdate === "agent_message_chunk" && n.update.content.type === "text" ? n.update.content.text : ""))
		.join("");
	check(pong.stopReason === "end_turn", "pong turn ended with end_turn");
	check(/pong/i.test(pongText), `pong reply received (${JSON.stringify(pongText.trim())})`);

	// 2. Tool turn.
	const tool = await runPrompt(
		ctx,
		sessionId,
		"Use the bash tool to run `ls` in the current directory and tell me how many entries there are.",
	);
	const toolUpdates = updates.slice(tool.from).map((n) => n.update);
	const call = toolUpdates.find((u) => u.sessionUpdate === "tool_call");
	const done = toolUpdates.find(
		(u) => u.sessionUpdate === "tool_call_update" && call?.sessionUpdate === "tool_call" && u.toolCallId === call.toolCallId,
	);
	check(call?.sessionUpdate === "tool_call" && call.kind === "execute", "tool_call (kind=execute) received");
	check(done?.sessionUpdate === "tool_call_update" && done.status === "completed", "tool_call_update (completed) received");
	check(tool.stopReason === "end_turn", "tool turn ended with end_turn");

	// 3. Cancellation.
	if (process.env.SMOKE_SKIP_CANCEL !== "1") {
		const cancelSession = (await ctx.request(acp.methods.agent.session.new, { cwd: repoRoot, mcpServers: [] })).sessionId;
		let sentCancel = false;
		onUpdate = (n) => {
			if (!sentCancel && n.sessionId === cancelSession && (n.update.sessionUpdate === "agent_message_chunk" || n.update.sessionUpdate === "agent_thought_chunk")) {
				sentCancel = true;
				console.log("\n  [first chunk received -> sending session/cancel]");
				void ctx.notify(acp.methods.agent.session.cancel, { sessionId: cancelSession });
			}
		};
		const cancelled = await runPrompt(ctx, cancelSession, "Count from 1 to 300, one number per line, no other text.");
		onUpdate = undefined;
		check(sentCancel && cancelled.stopReason === "cancelled", "cancel mid-turn resolved with stopReason cancelled");
	}
});

child.stdin.end();
const exitCode = await new Promise<number | null>((r) => child.on("exit", (code) => r(code)));
check(exitCode === 0, `agent exited cleanly on stdin EOF (code ${exitCode})`);

console.log(failures.length ? `\nSMOKE FAILED (${failures.length})` : "\nSMOKE OK");
process.exit(failures.length ? 1 : 0);
