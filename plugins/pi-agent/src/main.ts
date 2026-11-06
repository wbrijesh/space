// Space pi-agent plugin: an ACP agent over stdio backed by pi + the claude-cli provider.
//
// stdout carries newline-delimited JSON-RPC only; every log line goes to stderr.

import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
import { Readable, Writable } from "node:stream";
import * as acp from "@agentclientprotocol/sdk";
import type { AgentSessionEvent } from "@earendil-works/pi-coding-agent";
import { type AgentConfig, ensureClaudeOnPath, isolatePiConfig, loadConfig, log } from "./env.js";
import { isModeId, modeState } from "./mode.js";
import { createPiSession, type PiSession } from "./pi.js";
import { configureSession, parseConfigureParams, SPACE_CONFIGURE, SPACE_STATUS, sessionStatus } from "./status.js";
import { promptText, textToolContent, toolKind, toolResultText, toolTitle } from "./translate.js";

const VERSION: string = (() => {
	try {
		const pkg = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8")) as { version?: string };
		return pkg.version ?? "0.0.0";
	} catch {
		return "0.0.0";
	}
})();

interface SessionState {
	pi: PiSession;
	/** Set while a session/prompt is running. */
	turn?: { cancelled: boolean };
}

const sessions = new Map<string, SessionState>();

let config: AgentConfig;
try {
	config = loadConfig();
} catch (error) {
	log(error);
	process.exit(2);
}
isolatePiConfig(config);
const claudePath = ensureClaudeOnPath();
log(`v${VERSION} starting; model=${config.provider}/${config.modelId} thinking=${config.thinkingLevel} claude=${claudePath ?? "missing"}`);

/** Readable message for a failed pi turn, mapped to a JSON-RPC error. */
function turnError(message: string): acp.RequestError {
	return new acp.RequestError(-32603, message);
}

function lastAssistant(session: PiSession["session"]): { stopReason?: string; errorMessage?: string } | undefined {
	const messages = session.messages;
	for (let i = messages.length - 1; i >= 0; i--) {
		const m = messages[i] as { role?: string; stopReason?: string; errorMessage?: string };
		if (m?.role === "assistant") return m;
		if (m?.role === "user") return undefined;
	}
	return undefined;
}

const app = acp
	.agent({ name: "pi-agent" })
	.onRequest(acp.methods.agent.initialize, async ({ params }) => {
		log(`initialize: client protocolVersion=${params.protocolVersion}`);
		return {
			protocolVersion: acp.PROTOCOL_VERSION,
			agentCapabilities: {
				loadSession: false,
				promptCapabilities: { image: false, audio: false, embeddedContext: true },
			},
			agentInfo: { name: "pi-agent", title: "Pi Agent", version: VERSION },
			authMethods: [],
		};
	})
	.onRequest(acp.methods.agent.session.new, async ({ params, client }) => {
		const sessionId = randomUUID();
		let pi: PiSession;
		try {
			pi = await createPiSession(params.cwd, config);
		} catch (error) {
			log("session/new failed:", error);
			throw turnError(`Failed to start pi session: ${error instanceof Error ? error.message : String(error)}`);
		}
		sessions.set(sessionId, { pi });
		log(`session/new ${sessionId} cwd=${params.cwd} model=${pi.provider}/${pi.model} thinking=${pi.thinkingLevel}`);
		// Sent after the response is written so the client already knows the sessionId.
		setImmediate(() => {
			client
				.notify(SPACE_STATUS, sessionStatus(sessionId, pi.session))
				.catch((error: unknown) => log("failed to send _space/status:", error));
		});
		return { sessionId, modes: modeState(pi.mode.mode) };
	})
	.onRequest(acp.methods.agent.session.prompt, async ({ params, client }) => {
		const state = sessions.get(params.sessionId);
		if (!state) throw acp.RequestError.sessionNotFound(params.sessionId);
		if (state.turn) throw acp.RequestError.invalidRequest(undefined, "A prompt is already running for this session");

		const { session } = state.pi;
		const sessionId = params.sessionId;
		const turn = { cancelled: false };
		state.turn = turn;

		// Keep notifications ordered and make sure all are flushed before the response.
		let outbox: Promise<void> = Promise.resolve();
		const send = (update: acp.SessionUpdate) => {
			outbox = outbox
				.then(() => client.notify(acp.methods.client.session.update, { sessionId, update }))
				.catch((error: unknown) => log("failed to send session/update:", error));
		};

		const unsubscribe = session.subscribe((event: AgentSessionEvent) => {
			switch (event.type) {
				case "message_update": {
					const e = event.assistantMessageEvent;
					if (e.type === "text_delta" && e.delta) {
						send({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: e.delta } });
					} else if (e.type === "thinking_delta" && e.delta) {
						send({ sessionUpdate: "agent_thought_chunk", content: { type: "text", text: e.delta } });
					}
					break;
				}
				case "tool_execution_start":
					send({
						sessionUpdate: "tool_call",
						toolCallId: event.toolCallId,
						title: toolTitle(event.toolName, event.args),
						kind: toolKind(event.toolName),
						status: "in_progress",
						rawInput: event.args,
					});
					break;
				case "tool_execution_end":
					send({
						sessionUpdate: "tool_call_update",
						toolCallId: event.toolCallId,
						status: event.isError ? "failed" : "completed",
						content: textToolContent(toolResultText(event.result)),
					});
					break;
				case "auto_retry_start":
					log(`retry ${event.attempt}/${event.maxAttempts} in ${event.delayMs}ms: ${event.errorMessage}`);
					break;
			}
		});

		const text = promptText(params.prompt);
		let failure: unknown;
		try {
			await session.prompt(text);
		} catch (error) {
			failure = error;
		} finally {
			unsubscribe();
			state.turn = undefined;
			// A mode switched mid-turn takes effect now, for the next prompt.
			state.pi.mode.applyTools();
		}
		await outbox;
		// Context usage changed; the model may have too (e.g. a retry fallback).
		await client
			.notify(SPACE_STATUS, sessionStatus(sessionId, session))
			.catch((error: unknown) => log("failed to send _space/status:", error));

		if (turn.cancelled) return { stopReason: "cancelled" };
		if (failure) {
			log("prompt failed:", failure);
			throw turnError(failure instanceof Error ? failure.message : String(failure));
		}
		const last = lastAssistant(session);
		switch (last?.stopReason) {
			case "aborted":
				return { stopReason: "cancelled" };
			case "error":
				log(`turn ended with error: ${last.errorMessage}`);
				throw turnError(last.errorMessage || "The model request failed");
			case "length":
				return { stopReason: "max_tokens" };
			default:
				return { stopReason: "end_turn" };
		}
	})
	.onRequest(SPACE_CONFIGURE, parseConfigureParams, async ({ params }) => {
		const state = sessions.get(params.sessionId);
		if (!state) throw acp.RequestError.sessionNotFound(params.sessionId);
		if (state.turn) throw acp.RequestError.invalidRequest(undefined, "Cannot reconfigure while a prompt is running");
		try {
			await configureSession(state.pi.session, params);
		} catch (error) {
			throw acp.RequestError.invalidParams(undefined, error instanceof Error ? error.message : String(error));
		}
		const status = sessionStatus(params.sessionId, state.pi.session);
		log(`configure ${params.sessionId}: model=${status.provider}/${status.model} thinking=${status.thinkingLevel}`);
		return status;
	})
	.onRequest(acp.methods.agent.session.setMode, async ({ params }) => {
		const state = sessions.get(params.sessionId);
		if (!state) throw acp.RequestError.sessionNotFound(params.sessionId);
		if (!isModeId(params.modeId)) throw acp.RequestError.invalidParams(undefined, `Unknown mode "${params.modeId}"`);
		const { mode } = state.pi;
		mode.mode = params.modeId;
		// Tools change between runs only; a running turn keeps its mode (applied when it ends).
		if (!state.turn) mode.applyTools();
		log(`set_mode ${params.sessionId}: ${mode.mode}${state.turn ? " (from the next prompt)" : ""}`);
		return {};
	})
	.onNotification(acp.methods.agent.session.cancel, async ({ params }) => {
		const state = sessions.get(params.sessionId);
		if (!state?.turn) return;
		log(`session/cancel ${params.sessionId}`);
		state.turn.cancelled = true;
		await state.pi.session.abort().catch((error: unknown) => log("abort failed:", error));
	});

const stream = acp.ndJsonStream(
	Writable.toWeb(process.stdout) as WritableStream<Uint8Array>,
	Readable.toWeb(process.stdin) as ReadableStream<Uint8Array>,
);
const connection = app.connect(stream);

let shuttingDown = false;
async function shutdown(reason: string, code = 0): Promise<void> {
	if (shuttingDown) return;
	shuttingDown = true;
	log(`shutting down (${reason}); disposing ${sessions.size} session(s)`);
	const all = [...sessions.values()];
	sessions.clear();
	await Promise.race([
		Promise.allSettled(all.map((s) => s.pi.runtime.dispose())),
		new Promise((resolve) => setTimeout(resolve, 3000).unref()),
	]);
	process.exit(code);
}

connection.closed.then(
	() => void shutdown("stdin closed"),
	(error: unknown) => {
		log("connection error:", error);
		void shutdown("connection error", 1);
	},
);
process.on("SIGINT", () => void shutdown("SIGINT"));
process.on("SIGTERM", () => void shutdown("SIGTERM"));
