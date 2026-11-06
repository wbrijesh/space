// Process environment setup and configuration for the pi-agent plugin.
//
// Everything here runs before any pi session is created. Logs go to stderr only:
// stdout carries ACP protocol records exclusively.

import { accessSync, constants } from "node:fs";
import { homedir } from "node:os";
import { delimiter, join } from "node:path";

export const DEFAULT_MODEL = "claude-cli/claude-opus-5-5";
export const DEFAULT_THINKING = "medium";

export const THINKING_LEVELS = ["off", "minimal", "low", "medium", "high", "xhigh", "max"] as const;
export type ThinkingLevelName = (typeof THINKING_LEVELS)[number];

export interface AgentConfig {
	provider: string;
	modelId: string;
	thinkingLevel: ThinkingLevelName;
	/** Private pi agent dir; never the user's ~/.pi/agent. */
	agentDir: string;
}

export function log(...parts: unknown[]): void {
	const line = parts.map((p) => (typeof p === "string" ? p : p instanceof Error ? (p.stack ?? p.message) : JSON.stringify(p))).join(" ");
	process.stderr.write(`[pi-agent] ${line}\n`);
}

function isExecutable(path: string): boolean {
	try {
		accessSync(path, constants.X_OK);
		return true;
	} catch {
		return false;
	}
}

function findOnPath(command: string, pathValue: string | undefined): string | undefined {
	for (const dir of (pathValue ?? "").split(delimiter)) {
		if (!dir) continue;
		const candidate = join(dir, command);
		if (isExecutable(candidate)) return candidate;
	}
	return undefined;
}

/**
 * The claude-cli provider spawns `claude` from PATH. Hosts (like a GUI app) may launch
 * us with a minimal PATH, so add the usual install locations when it is not resolvable.
 */
export function ensureClaudeOnPath(): string | undefined {
	const found = findOnPath("claude", process.env.PATH);
	if (found) return found;
	const home = homedir();
	const fallbacks = [join(home, ".local", "bin"), join(home, ".claude", "local"), "/opt/homebrew/bin", "/usr/local/bin"];
	for (const dir of fallbacks) {
		const candidate = join(dir, "claude");
		if (isExecutable(candidate)) {
			process.env.PATH = [dir, process.env.PATH].filter(Boolean).join(delimiter);
			log(`claude not on PATH; added ${dir}`);
			return candidate;
		}
	}
	log("warning: `claude` CLI not found on PATH or in ~/.local/bin; requests will fail");
	return undefined;
}

export function loadConfig(): AgentConfig {
	const modelSpec = process.env.SPACE_PI_MODEL?.trim() || DEFAULT_MODEL;
	const slash = modelSpec.indexOf("/");
	if (slash <= 0 || slash === modelSpec.length - 1) {
		throw new Error(`SPACE_PI_MODEL must look like "provider/model-id", got "${modelSpec}"`);
	}
	const thinking = (process.env.SPACE_PI_THINKING?.trim() || DEFAULT_THINKING) as ThinkingLevelName;
	if (!THINKING_LEVELS.includes(thinking)) {
		throw new Error(`SPACE_PI_THINKING must be one of ${THINKING_LEVELS.join(", ")}, got "${thinking}"`);
	}
	const agentDir = process.env.SPACE_PI_AGENT_DIR?.trim() || join(homedir(), ".space", "pi-agent");
	return {
		provider: modelSpec.slice(0, slash),
		modelId: modelSpec.slice(slash + 1),
		thinkingLevel: thinking,
		agentDir,
	};
}

/**
 * pi resolves its global config dir from PI_CODING_AGENT_DIR (default ~/.pi/agent). Point it
 * at a private directory so nothing pi does by default (crash logs, settings, auth) touches
 * the user's own pi setup.
 */
export function isolatePiConfig(config: AgentConfig): void {
	process.env.PI_CODING_AGENT_DIR = config.agentDir;
}
