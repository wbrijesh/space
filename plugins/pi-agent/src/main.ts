// Space pi-agent plugin: an ACP agent over stdio backed by pi + the claude-cli provider.
//
// stdout carries newline-delimited JSON-RPC only; every log line goes to stderr.

import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
import { Readable, Writable } from "node:stream";
import * as acp from "@agentclientprotocol/sdk";
import type { AgentSessionEvent } from "@earendil-works/pi-coding-agent";
import { type AgentConfig, ensureClaudeOnPath, isolatePiConfig, loadConfig, log } from "./env.js";
import {
	COMPLETE_METHOD,
	CONFIGURE_METHOD,
	CONFINE_METHOD,
	type ConfineResult,
	DATA_DIR_META_KEY,
	PRE_EXECUTE_METHOD,
	type PreExecuteResult,
	PROTOCOL_VERSION,
	PROTOCOL_VERSION_META_KEY,
	STATUS_METHOD,
} from "./generated/protocol.js";
import { contextFromMeta, type HostLink } from "./host.js";
import { createPiSession, type PiSession } from "./pi.js";
import { completeOnce, configureSession, parseCompleteParams, parseConfigureParams, sessionStatus } from "./status.js";
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

/** The host's pipeline over this ACP connection (requests to the client). */
const host: HostLink = {
	preExecute: (params) => connection.client.request<PreExecuteResult>(PRE_EXECUTE_METHOD, params),
	confine: (params) => connection.client.request<ConfineResult>(CONFINE_METHOD, params),
};

/** Readable message for a failed pi turn, mapped to a JSON-RPC error. */
function turnError(message: string): acp.RequestError {
	return new acp.RequestError(-32603, message);
}

/** The session's data folder from `session/new` or `session/load` `_meta`, if the host gave one. */
function dataDirFromMeta(meta: unknown): string | undefined {
	const dir = meta && typeof meta === "object" ? (meta as Record<string, unknown>)[DATA_DIR_META_KEY] : undefined;
	return typeof dir === "string" && dir ? dir : undefined;
}

/** Start (or load) a pi session for `sessionId` and announce its status once the reply is out. */
async function openSession(
	sessionId: string,
	cwd: string,
	meta: unknown,
	load: boolean,
	client: { notify: (method: string, params: unknown) => Promise<void> },
): Promise<void> {
	const dataDir = dataDirFromMeta(meta);
	if (load && !dataDir) throw acp.RequestError.invalidParams(undefined, "No saved session to load (no data folder)");
	let pi: PiSession;
	try {
		pi = await createPiSession(cwd, config, { sessionId, host, dataDir, load });
	} catch (error) {
		log(`session/${load ? "load" : "new"} failed:`, error);
		throw turnError(`Failed to start pi session: ${error instanceof Error ? error.message : String(error)}`);
	}
	sessions.set(sessionId, { pi });
	log(`session/${load ? "load" : "new"} ${sessionId} cwd=${cwd} model=${pi.provider}/${pi.model} thinking=${pi.thinkingLevel} ${dataDir ? `dir=${dataDir} messages=${pi.session.messages.length}` : "in memory"}`);
	// Sent after the response is written so the client already knows the sessionId.
	setImmediate(() => {
		client
			.notify(STATUS_METHOD, sessionStatus(sessionId, pi.session))
			.catch((error: unknown) => log("failed to send _space/status:", error));
	});
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
				// Sessions given a data folder are kept there and can be loaded again.
				loadSession: true,
				promptCapabilities: { image: false, audio: false, embeddedContext: true },
			},
			agentInfo: { name: "pi-agent", title: "Pi Agent", version: VERSION },
			authMethods: [],
			// The Space protocol this build speaks (the host refuses a mismatch).
			_meta: { [PROTOCOL_VERSION_META_KEY]: PROTOCOL_VERSION },
		};
	})
	.onRequest(acp.methods.agent.session.new, async ({ params, client }) => {
		const sessionId = randomUUID();
		await openSession(sessionId, params.cwd, params._meta, false, client);
		return { sessionId };
	})
	// Continues a session kept in its data folder. ACP has the agent replay the conversation
	// here; Space already shows it (from its own saved thread), so nothing is replayed.
	.onRequest(acp.methods.agent.session.load, async ({ params, client }) => {
		if (!sessions.has(params.sessionId)) await openSession(params.sessionId, params.cwd, params._meta, true, client);
		return {};
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

		// Hidden context from the host's plugins, for this run's system prompt.
		state.pi.host.context = contextFromMeta(params._meta);
		const text = promptText(params.prompt);
		let failure: unknown;
		try {
			await session.prompt(text);
		} catch (error) {
			failure = error;
		} finally {
			unsubscribe();
			state.turn = undefined;
		}
		await outbox;
		// Context usage changed; the model may have too (e.g. a retry fallback).
		await client
			.notify(STATUS_METHOD, sessionStatus(sessionId, session))
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
	.onRequest(CONFIGURE_METHOD, parseConfigureParams, async ({ params }) => {
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
	// A short answer from the session's model (or another), outside the conversation: Space's
	// title plugin names threads with it.
	.onRequest(COMPLETE_METHOD, parseCompleteParams, async ({ params }) => {
		const state = sessions.get(params.sessionId);
		if (!state) throw acp.RequestError.sessionNotFound(params.sessionId);
		try {
			return { text: await completeOnce(state.pi.session, params) };
		} catch (error) {
			log(`complete ${params.sessionId} failed:`, error);
			throw turnError(error instanceof Error ? error.message : String(error));
		}
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
