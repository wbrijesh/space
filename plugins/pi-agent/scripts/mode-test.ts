// Read/write mode checks that make no model requests: the active tools per mode on a real pi
// session, the hidden prompt section from the inline extension, and the bash guard.
//
// Run: pnpm test-mode   (or: node scripts/mode-test.ts after `pnpm build`)

import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const pluginDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = resolve(pluginDir, "..", "..");
// A throwaway pi agent dir: never ~/.pi or ~/.space.
const agentDir = mkdtempSync(join(tmpdir(), "pi-agent-mode-test-"));
process.env.SPACE_PI_AGENT_DIR = agentDir;

const { isolatePiConfig, loadConfig } = await import(join(pluginDir, "dist/env.js"));
const { createPiSession } = await import(join(pluginDir, "dist/pi.js"));
const { mutatingCommand, READ_ONLY_PROMPT, READ_ONLY_SECTION } = await import(join(pluginDir, "dist/mode.js"));

const failures: string[] = [];
function check(ok: boolean, what: string): void {
	console.log(`${ok ? "PASS" : "FAIL"} ${what}`);
	if (!ok) failures.push(what);
}

// 1. Active tools per mode, on a real session (no prompt is sent).
const config = loadConfig();
isolatePiConfig(config);
const pi = await createPiSession(repoRoot, config);
const tools = () => (pi.session.getActiveToolNames() as string[]).slice().sort().join(",");
const write = tools();
console.log(`write tools: ${write}`);
check(write.includes("edit") && write.includes("write"), "write mode has edit and write");
pi.mode.mode = "read";
pi.mode.applyTools();
const read = tools();
console.log(`read tools:  ${read}`);
check(!read.split(",").includes("edit") && !read.split(",").includes("write"), "read mode drops edit and write");
check(["read", "bash", "grep", "find", "ls"].every((t) => read.split(",").includes(t)), "read mode keeps read, bash, grep, find, ls");
pi.mode.mode = "write";
pi.mode.applyTools();
check(tools() === write, "write mode restores the original tools");
await pi.runtime.dispose();

// 2. The inline extension: prompt section and tool guard, driven with fake events.
type Handler = (event: Record<string, unknown>) => unknown;
const handlers = new Map<string, Handler>();
pi.mode.extension()({ on: (name: string, h: Handler) => void handlers.set(name, h) } as never);
const start = (mode: "read" | "write") => {
	pi.mode.mode = mode;
	const sections: Record<string, string> = { other: "x" };
	handlers.get("before_agent_start")?.({ type: "before_agent_start", systemPromptOptions: { sections } });
	return sections;
};
check(start("read")[READ_ONLY_SECTION] === READ_ONLY_PROMPT, "read turn adds the hidden prompt section");
const call = (toolName: string, input: Record<string, unknown>) =>
	handlers.get("tool_call")?.({ type: "tool_call", toolCallId: "t", toolName, input }) as { block?: boolean } | undefined;
check(call("write", { path: "x" })?.block === true, "read turn blocks the write tool");
check(call("bash", { command: "git commit -am x" })?.block === true, "read turn blocks git commit");
check(call("bash", { command: "git status" }) === undefined, "read turn allows git status");
const w = start("write");
check(!(READ_ONLY_SECTION in w) && w.other === "x", "write turn adds no prompt text");
check(call("bash", { command: "rm -rf target" }) === undefined, "write turn blocks nothing");

// 3. Bash guard table.
const mutating = [
	"rm -rf build",
	"mv a b",
	"echo hi > notes.txt",
	"cat x >> y.log",
	"ls && touch x",
	"git commit -m 'msg'",
	"git -C repo checkout main",
	"git stash",
	"git reset --hard",
	"git push origin main",
	"npm install left-pad",
	"pnpm add zod",
	"sed -i '' 's/a/b/' f.txt",
	"find . -name '*.tmp' -delete",
	"sudo rm x",
	"cargo fmt",
	"xargs rm < list",
	"echo a | tee out.txt",
];
const readOnly = [
	"ls -la",
	"cat README.md",
	"rg 'a > b' src",
	"git status",
	"git log --oneline -5",
	"git diff HEAD~1",
	"git stash list",
	"cargo test 2>&1 | tail -20",
	"cargo build >/dev/null 2>&1",
	"echo 'git commit' ",
	"grep -rn \"rm -rf\" .",
	"find . -name '*.rs' | wc -l",
	"sed -n 1,20p file.rs",
];
for (const c of mutating) check(mutatingCommand(c) !== undefined, `blocks: ${c}  (${mutatingCommand(c)})`);
for (const c of readOnly) check(mutatingCommand(c) === undefined, `allows: ${c}`);

rmSync(agentDir, { recursive: true, force: true });
console.log(failures.length ? `\nMODE TEST FAILED (${failures.length})` : "\nMODE TEST OK");
process.exit(failures.length ? 1 : 0);
