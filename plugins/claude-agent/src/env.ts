// The process around the agent: logging, and the Claude Code CLI it drives.
//
// stdout carries ACP records only; every log line goes to stderr.

import { execFileSync } from "node:child_process";
import { accessSync, constants } from "node:fs";
import { homedir } from "node:os";
import { delimiter, join } from "node:path";

/**
 * The Claude Code release the pinned Agent SDK was built against (its package.json's
 * `claudeCodeVersion`; a test keeps the two equal). An older CLI may not speak everything the
 * SDK sends, so it isn't used.
 */
export const MIN_CLAUDE_VERSION = "2.1.328";

export function log(...parts: unknown[]): void {
	const line = parts.map((p) => (typeof p === "string" ? p : p instanceof Error ? (p.stack ?? p.message) : JSON.stringify(p))).join(" ");
	process.stderr.write(`[claude-agent] ${line}\n`);
}

export function errorText(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

function isExecutable(path: string): boolean {
	try {
		accessSync(path, constants.X_OK);
		return true;
	} catch {
		return false;
	}
}

/**
 * The `claude` executable: on `PATH`, else where its installers put it (an app started from
 * the Dock gets a minimal `PATH`). `undefined` when it isn't installed.
 */
export function findClaude(path = process.env.PATH, home = homedir()): string | undefined {
	const dirs = [...(path ?? "").split(delimiter), join(home, ".local", "bin"), join(home, ".claude", "local"), "/opt/homebrew/bin", "/usr/local/bin"];
	return dirs.filter(Boolean).map((dir) => join(dir, "claude")).find(isExecutable);
}

/** `2.1.328` from `claude --version`'s `2.1.328 (Claude Code)`. */
export function parseVersion(output: string): string | undefined {
	return output.match(/\d+\.\d+\.\d+/)?.[0];
}

/** Whether version `a` is at least `b` (both `x.y.z`). */
export function atLeast(a: string, b: string): boolean {
	const pa = a.split(".").map(Number);
	const pb = b.split(".").map(Number);
	for (let i = 0; i < 3; i++) {
		const d = (pa[i] ?? 0) - (pb[i] ?? 0);
		if (d !== 0) return d > 0;
	}
	return true;
}

/**
 * The Claude Code CLI to drive. Throws a message for the user when it's missing or older than
 * {@link MIN_CLAUDE_VERSION}.
 */
export function claudeCli(): { path: string; version: string } {
	const path = findClaude();
	if (!path) throw new Error("Space needs the Claude Code CLI: install it (brew install --cask claude-code) and run `claude` once to sign in.");
	let output: string;
	try {
		output = execFileSync(path, ["--version"], { encoding: "utf8", timeout: 15_000, stdio: ["ignore", "pipe", "pipe"] });
	} catch (error) {
		throw new Error(`Couldn't run ${path} --version: ${errorText(error)}`);
	}
	const version = parseVersion(output);
	if (!version) throw new Error(`Couldn't read the Claude Code version from "${output.trim()}"`);
	if (!atLeast(version, MIN_CLAUDE_VERSION)) {
		throw new Error(`Claude Code ${version} is too old for Space; it needs ${MIN_CLAUDE_VERSION} or later. Update it with \`claude update\`.`);
	}
	return { path, version };
}
