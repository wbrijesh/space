// The host's tool pipeline, as pi-agent consumes it (see docs/architecture.md):
//
// * `_space/tools/preExecute` before every tool call: the host runs its hook chain (and may ask
//   the user) and answers allow or deny. A denied call is blocked with the reason; a failed
//   request blocks too (fail closed).
// * `_space/sandbox/confine` before bash spawns: the host's sandbox provider returns the argv
//   to run instead (wrapped, or unchanged). A failed request means the command doesn't run.
// * Hidden context sections arrive in each `session/prompt`'s `_meta["space/context"]` and go
//   into pi's system-prompt sections for that run, never into the user's message.

import type { BashOperations, ExtensionFactory, ToolDefinition } from "@earendil-works/pi-coding-agent";
import { createBashToolDefinition, createLocalBashOperations } from "@earendil-works/pi-coding-agent";
import { log } from "./env.js";
import {
	CONTEXT_META_KEY,
	type Confine,
	type ConfineResult,
	type ContextSection,
	type JsonValue,
	type PreExecute,
	type PreExecuteResult,
} from "./generated/protocol.js";
import { toolKind } from "./translate.js";

/** What pi-agent asks of the host. `main.ts` backs it with the ACP connection; tests fake it. */
export interface HostLink {
	preExecute(params: PreExecute): Promise<PreExecuteResult>;
	confine(params: Confine): Promise<ConfineResult>;
}

/** Context sections from a prompt's `_meta` (anything malformed is ignored). */
export function contextFromMeta(meta: unknown): ContextSection[] {
	const raw = meta && typeof meta === "object" ? (meta as Record<string, unknown>)[CONTEXT_META_KEY] : undefined;
	if (!Array.isArray(raw)) return [];
	return raw.filter(
		(s): s is ContextSection => !!s && typeof s === "object" && typeof s.id === "string" && typeof s.text === "string",
	);
}

/** Prompt-section name for a context section id. */
export function sectionName(id: string): string {
	return `space_${id.replace(/[^A-Za-z0-9_]/g, "_")}`;
}

/** Per-session wiring between pi and the host. */
export class HostBridge {
	/** Context sections for the next run (set by the prompt handler). */
	context: ContextSection[] = [];
	/** Section names added in the last run, removed before the next. */
	private added: string[] = [];

	private readonly host: HostLink;
	private readonly sessionId: string;

	constructor(host: HostLink, sessionId: string) {
		this.host = host;
		this.sessionId = sessionId;
	}

	/** Inline pi extension: ask the pipeline before each tool call; add the hidden sections. */
	extension(): ExtensionFactory {
		return (pi) => {
			pi.on("before_agent_start", (event) => {
				const sections = event.systemPromptOptions.sections;
				for (const name of this.added) delete sections[name];
				this.added = [];
				for (const s of this.context) {
					const name = sectionName(s.id);
					sections[name] = s.text;
					this.added.push(name);
				}
			});
			pi.on("tool_call", async (event) => {
				const params: PreExecute = {
					sessionId: this.sessionId,
					toolCallId: event.toolCallId,
					tool: event.toolName,
					kind: toolKind(event.toolName),
					args: event.input as JsonValue,
				};
				let result: PreExecuteResult;
				try {
					result = await this.host.preExecute(params);
				} catch (error) {
					log(`preExecute ${event.toolName} failed: ${errorText(error)}`);
					return { block: true, reason: `Space couldn't check this call, so it didn't run (${errorText(error)}).` };
				}
				if (result.decision === "allow") return undefined;
				return { block: true, reason: result.reason || "Space didn't allow this call." };
			});
		};
	}

	/** The bash tool, with its command confined by the host before it spawns. */
	bashTool(cwd: string): ToolDefinition {
		return createBashToolDefinition(cwd, { operations: this.confinedOperations() }) as unknown as ToolDefinition;
	}

	confinedOperations(): BashOperations {
		const local = createLocalBashOperations();
		return {
			exec: async (command, cwd, options) => {
				const argv = ["/bin/bash", "-c", command];
				const confined = await this.host.confine({ sessionId: this.sessionId, argv, cwd });
				if (!Array.isArray(confined.argv) || confined.argv.length === 0) throw new Error("the host returned no command to run");
				const same = confined.argv.length === argv.length && confined.argv.every((a, i) => a === argv[i]);
				// Unchanged: let pi run the command its usual way. Wrapped: run exactly that argv.
				return local.exec(same ? command : confined.argv.map(shellQuote).join(" "), cwd, options);
			},
		};
	}
}

/** Single-quote `s` for a POSIX shell. */
export function shellQuote(s: string): string {
	return /^[A-Za-z0-9_\/.,:=@%+-]+$/.test(s) ? s : `'${s.replaceAll("'", `'\\''`)}'`;
}

function errorText(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}
