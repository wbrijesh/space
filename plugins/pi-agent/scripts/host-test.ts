// The host bridge on a real pi session, with no model requests: pi-ai's faux provider plays
// the model (tool calls included) and a fake HostLink stands in for Space.
//
// Checks: every tool call is put to `_space/tools/preExecute` (tool, kind, args, call id) and a
// denial blocks it with the reason; a failing request blocks too; bash asks
// `_space/sandbox/confine` and runs the argv it gets (here: wrapped in sandbox-exec, so a write
// outside the allowed folder fails); context sections reach the system prompt and leave it again;
// a session given a data folder is kept there and loads again with its messages.
//
// Run: pnpm test-host   (or: node scripts/host-test.ts after `pnpm build`)

import { existsSync, mkdtempSync, readFileSync, realpathSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { fauxAssistantMessage, fauxProvider, fauxToolCall } from "@earendil-works/pi-ai";

const pluginDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
// A throwaway pi agent dir: never ~/.pi or ~/.space.
const agentDir = mkdtempSync(join(tmpdir(), "pi-agent-host-test-"));
process.env.SPACE_PI_AGENT_DIR = agentDir;
process.env.SPACE_PI_MODEL = "faux/m";

const { isolatePiConfig, loadConfig } = await import(join(pluginDir, "dist/env.js"));
const { createPiSession } = await import(join(pluginDir, "dist/pi.js"));
const { contextFromMeta, sectionName, shellQuote } = await import(join(pluginDir, "dist/host.js"));
const { completeOnce } = await import(join(pluginDir, "dist/status.js"));

const failures: string[] = [];
function check(ok: boolean, what: string): void {
	console.log(`${ok ? "PASS" : "FAIL"} ${what}`);
	if (!ok) failures.push(what);
}

const work = realpathSync(mkdtempSync(join(tmpdir(), "pi-agent-host-work-")));
const outside = realpathSync(mkdtempSync(join(tmpdir(), "pi-agent-host-outside-")));
const faux = fauxProvider({ provider: "faux", models: [{ id: "m" }] });
const calls: { tool: string; kind: string; args: unknown; toolCallId: string }[] = [];
const confines: string[][] = [];
// Writes are allowed under `work` only.
const profile = `(version 1)(allow default)(deny file-write*)(allow file-write* (literal "/dev/null") (subpath "${work}"))`;
const host = {
	async preExecute(p: { tool: string; kind: string; args: { command?: string }; toolCallId: string }) {
		calls.push(p);
		if (p.tool === "write") return { decision: "deny" as const, reason: "Blocked by guardrail: test denial" };
		if (p.args?.command === "explode") throw new Error("host gone");
		return { decision: "allow" as const };
	},
	async confine(p: { argv: string[] }) {
		confines.push(p.argv);
		return { argv: ["/usr/bin/sandbox-exec", "-p", profile, ...p.argv] };
	},
};

const config = loadConfig();
isolatePiConfig(config);
const pi = await createPiSession(work, config, {
	sessionId: "s1",
	host,
	registerProviders: (runtime: { registerNativeProvider: (p: unknown) => void }) => runtime.registerNativeProvider(faux.provider),
});
const results = new Map<string, { isError: boolean; text: string }>();
pi.session.subscribe((e: { type: string; toolCallId?: string; isError?: boolean; result?: { content?: { text?: string }[] } }) => {
	if (e.type === "tool_execution_end" && e.toolCallId) {
		results.set(e.toolCallId, { isError: !!e.isError, text: e.result?.content?.map((c) => c.text ?? "").join("") ?? "" });
	}
});

// What the model is sent for each request (the system prompt or its patch is in there).
const seen: string[] = [];
const capture = (message: ReturnType<typeof fauxAssistantMessage>) => (context: unknown) => {
	seen.push(JSON.stringify(context));
	return message;
};
faux.setResponses([
	capture(fauxAssistantMessage(
		[
			fauxToolCall("bash", { command: `echo in > ${work}/a.txt; echo out > ${outside}/b.txt; echo done` }, { id: "c-bash" }),
			fauxToolCall("write", { path: join(work, "w.txt"), content: "secret contents" }, { id: "c-write" }),
			fauxToolCall("bash", { command: "explode" }, { id: "c-fail" }),
		],
		{ stopReason: "toolUse" },
	)),
	fauxAssistantMessage("finished"),
]);
pi.host.context = contextFromMeta({ "space/context": [{ id: "permissions", text: "Mode: Read Only (test)." }, { bad: 1 }] });
await pi.session.prompt("go");

check(calls.length === 3, `every tool call went to preExecute (${calls.length})`);
const bashCall = calls.find((c) => c.toolCallId === "c-bash");
check(bashCall?.kind === "execute" && typeof (bashCall.args as { command?: string }).command === "string", "bash call carries kind and args");
check(calls.find((c) => c.toolCallId === "c-write")?.kind === "edit", "write call has kind edit");
const write = results.get("c-write");
check(!!write?.isError && write.text.includes("Blocked by guardrail: test denial"), `denied write is blocked with the reason (${write?.text})`);
check(!existsSync(join(work, "w.txt")), "denied write did not run");
const failed = results.get("c-fail");
check(!!failed?.isError && failed.text.includes("couldn't check this call"), `a failing preExecute blocks the call (${failed?.text})`);
check(confines.length === 1 && confines[0]?.[0] === "/bin/bash" && confines[0]?.[1] === "-c", "bash asked confine with /bin/bash -c");
check(existsSync(join(work, "a.txt")) && readFileSync(join(work, "a.txt"), "utf8").trim() === "in", "confined bash wrote inside the allowed folder");
check(!existsSync(join(outside, "b.txt")), "confined bash could not write outside it");
check(results.get("c-bash")?.text.includes("Operation not permitted") ?? false, "the sandbox denial shows in the output");
check(seen[0]?.includes("Mode: Read Only (test).") ?? false, "context section reached the model's system prompt");
check(contextFromMeta({ "space/context": "nope" }).length === 0 && contextFromMeta(undefined).length === 0, "malformed context is ignored");
check(sectionName("a-b.c") === "space_a_b_c", "section names are sanitized");
check(shellQuote("it's") === `'it'\\''s'` && shellQuote("/bin/bash") === "/bin/bash", "shell quoting");

// The next run without context drops the section again.
faux.setResponses([capture(fauxAssistantMessage("ok"))]);
pi.host.context = [{ id: "permissions", text: "Mode: Full access (test)." }];
await pi.session.prompt("again");
const last = seen.at(-1) ?? "";
check(last.includes("Mode: Full access (test)."), "the next run carries the new section");
check(!(pi.session.systemPrompt as string).includes("Mode: Read Only (test)."), "the old section is gone from the system prompt");

await pi.runtime.dispose();

// Kept in a data folder, a session loads again with its conversation (ACP session/load).
const dataDir = join(work, "thread-data");
const providers = (runtime: { registerNativeProvider: (p: unknown) => void }) => runtime.registerNativeProvider(faux.provider);
const kept = await createPiSession(work, config, { sessionId: "s2", host, dataDir, registerProviders: providers });
faux.setResponses([fauxAssistantMessage("first answer")]);
await kept.session.prompt("remember this");
const before = kept.session.messages.length;
await kept.runtime.dispose();
const loaded = await createPiSession(work, config, { sessionId: "s2", host, dataDir, load: true, registerProviders: providers });
check(before >= 2 && loaded.session.messages.length === before, `a loaded session has its messages (${loaded.session.messages.length}/${before})`);
check(JSON.stringify(loaded.session.messages).includes("remember this"), "the loaded session has the earlier prompt");

// `_space/complete`: a short answer from the session's model, outside its conversation.
const asked: unknown[] = [];
faux.setResponses([(context: unknown) => (asked.push(context), fauxAssistantMessage("\"A Title\""))]);
const keptMessages = loaded.session.messages.length;
const answer = await completeOnce(loaded.session, { sessionId: "s2", prompt: "name this thread" });
check(answer === "\"A Title\"", `complete answers with the model's text (${answer})`);
check(JSON.stringify(asked).includes("name this thread") && !JSON.stringify(asked).includes("remember this"), "complete sends only its prompt");
check(loaded.session.messages.length === keptMessages, "complete leaves the conversation alone");
let unknown = "";
await completeOnce(loaded.session, { sessionId: "s2", prompt: "x", model: "nope" }).catch((e: Error) => (unknown = e.message));
check(unknown.includes("faux/nope"), `an unknown model is refused (${unknown})`);
await loaded.runtime.dispose();

for (const d of [agentDir, work, outside]) rmSync(d, { recursive: true, force: true });
if (failures.length) {
	console.log(`\n${failures.length} check(s) failed`);
	process.exit(1);
}
console.log("\nall host checks passed");
