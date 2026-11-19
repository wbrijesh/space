import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { query } from "@anthropic-ai/claude-agent-sdk";
import { canCancelQueued, cliEnv, Inbox, userMessage } from "./claude.js";
import { atLeast, MIN_CLAUDE_VERSION, parseVersion } from "./env.js";

describe("Claude Code's environment", () => {
	test("no credential reaches it, so requests use the subscription", () => {
		const env = cliEnv({ PATH: "/bin", ANTHROPIC_API_KEY: "sk", ANTHROPIC_AUTH_TOKEN: "t", CLAUDE_CODE_OAUTH_TOKEN: "o", CLAUDE_CODE_USE_BEDROCK: "1" });
		for (const name of ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN", "CLAUDE_CODE_USE_BEDROCK"]) expect(env).not.toHaveProperty(name);
		expect(env.PATH).toBe("/bin");
	});

	test("turns end when it's idle, nothing runs in the background, no claude.ai connectors", () => {
		const env = cliEnv({});
		expect(env.CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS).toBe("1");
		expect(env.CLAUDE_CODE_DISABLE_BACKGROUND_TASKS).toBe("1");
		expect(env.ENABLE_CLAUDEAI_MCP_SERVERS).toBe("false");
	});
});

describe("the pinned Agent SDK", () => {
	test("MIN_CLAUDE_VERSION is the Claude Code release the SDK was built against", () => {
		const pkg = JSON.parse(readFileSync(join(import.meta.dir, "../node_modules/@anthropic-ai/claude-agent-sdk/package.json"), "utf8"));
		expect(pkg.claudeCodeVersion).toBe(MIN_CLAUDE_VERSION);
	});

	test("its queries can withdraw a queued message (steering relies on it)", () => {
		const q = query({ prompt: new Inbox(), options: { pathToClaudeCodeExecutable: "/nonexistent/claude" } });
		try {
			expect(canCancelQueued(q)).toBe(true);
		} finally {
			q.close();
		}
	});
});

test("versions", () => {
	expect(parseVersion("2.1.328 (Claude Code)\n")).toBe("2.1.328");
	expect(parseVersion("claude")).toBeUndefined();
	expect(atLeast("2.1.328", "2.1.328")).toBe(true);
	expect(atLeast("2.2.0", "2.1.328")).toBe(true);
	expect(atLeast("2.1.99", "2.1.328")).toBe(false);
	expect(atLeast("10.0.0", "9.9.9")).toBe(true);
});

test("user messages: text as written unless it's a recognized command; steering is `next`", () => {
	const m = userMessage([{ type: "text", text: "/model x" }], "u1");
	expect(m.client_composed).toBe(true);
	expect(m.origin).toEqual({ kind: "human" });
	expect(m.priority).toBeUndefined();
	expect(userMessage("/compact", "u2", { command: true }).client_composed).toBeUndefined();
	expect(userMessage("hi", "u3", { priority: "next" }).priority).toBe("next");
});

test("the inbox yields in order and ends once closed", async () => {
	const inbox = new Inbox<number>();
	inbox.push(1);
	const seen: number[] = [];
	const done = (async () => {
		for await (const n of inbox) seen.push(n);
	})();
	inbox.push(2);
	await Promise.resolve();
	inbox.push(3);
	inbox.close();
	await done;
	expect(seen).toEqual([1, 2, 3]);
	expect(() => inbox.push(4)).toThrow();
});
