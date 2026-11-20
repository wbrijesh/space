// Space claude-agent plugin: an ACP agent over stdio backed by Claude Code (the Agent SDK
// driving the user's signed-in `claude` CLI).
//
// stdout carries newline-delimited JSON-RPC only; every log line goes to stderr.

import { randomUUID } from "node:crypto";
import { appendFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import { Readable, Writable } from "node:stream";
import * as acp from "@agentclientprotocol/sdk";
import { complete, Engine, PROVIDER, type SDKMessage, savedMessages } from "./claude.js";
import { claudeCli, errorText, log } from "./env.js";
import {
	COMPLETE_METHOD,
	CONFIGURE_METHOD,
	CONFINE_METHOD,
	type ConfineResult,
	CONTEXT_META_KEY,
	PRE_EXECUTE_METHOD,
	type PreExecuteResult,
	PROTOCOL_VERSION,
	PROTOCOL_VERSION_META_KEY,
	SIDE_OF_META_KEY,
	STATUS_METHOD,
	STEER_META_KEY,
	STEER_METHOD,
	TRACE_METHOD,
	type TraceBatch,
	UNSTEER_METHOD,
	USAGE_META_KEY,
} from "./generated/protocol.js";
import { parseComplete, parseConfigure, parseSteer, sessionRef, sideOf, steerBlocks } from "./params.js";
import { contextFromMeta, type HostLink, type Output, Session } from "./session.js";
import { TraceRecorder } from "./trace.js";
// Built in (a single-file executable has no package.json next to it).
import pkg from "../package.json" with { type: "json" };

const VERSION: string = pkg.version;

const sessions = new Map<string, Session>();

let cli: { path: string; version: string } | undefined;
let cliError: string | undefined;
try {
	cli = claudeCli();
} catch (error) {
	cliError = errorText(error);
}
log(`v${VERSION} starting; claude=${cli ? `${cli.path} ${cli.version}` : `unavailable (${cliError})`}`);

/** The Claude Code CLI, or the reason it can't be used (a JSON-RPC error the app shows). */
function requireCli(): string {
	if (cli) return cli.path;
	throw new acp.RequestError(-32603, cliError ?? "Claude Code isn't available");
}

/** Every notification to the client, in the order it was produced. */
class Outbox implements Output {
	private chain: Promise<void> = Promise.resolve();
	private readonly sessionId: string;

	constructor(sessionId: string) {
		this.sessionId = sessionId;
	}

	update(update: acp.SessionUpdate): void {
		this.notify(acp.methods.client.session.update, { sessionId: this.sessionId, update });
	}

	notify(method: string, params: unknown): void {
		this.chain = this.chain.then(() => connection.client.notify(method, params)).catch((error: unknown) => log(`failed to send ${method}:`, errorText(error)));
	}

	flushed(): Promise<void> {
		return this.chain;
	}
}

/**
 * With `SPACE_CLAUDE_AGENT_RECORD=<dir>`, every message Claude Code sends is also appended to
 * `<dir>/<session>.jsonl` (the tests' fixtures are recorded this way).
 */
function record(sessionId: string, onMessage: (message: SDKMessage) => void): (message: SDKMessage) => void {
	const dir = process.env.SPACE_CLAUDE_AGENT_RECORD;
	if (!dir) return onMessage;
	mkdirSync(dir, { recursive: true });
	const file = join(dir, `${sessionId}.jsonl`);
	return (message) => {
		appendFileSync(file, `${JSON.stringify({ at: Date.now(), message })}\n`);
		onMessage(message);
	};
}

/** The host's pipeline over this ACP connection (requests to the client). */
const host: HostLink = {
	preExecute: (params) => connection.client.request<PreExecuteResult>(PRE_EXECUTE_METHOD, params),
	confine: (params) => connection.client.request<ConfineResult>(CONFINE_METHOD, params),
};

/**
 * Start (or continue) the session `sessionId` and announce its status once the reply is out.
 * `side`: it's a side chat of that session.
 */
async function openSession(sessionId: string, cwd: string, resume: boolean, side: string | undefined): Promise<void> {
	const output = new Outbox(sessionId);
	const trace = new TraceRecorder(sessionId, (batch: TraceBatch) => connection.client.notify(TRACE_METHOD, batch), resume);
	let session: Session;
	try {
		session = await Session.open({
			id: sessionId,
			cwd,
			cli: requireCli(),
			resume,
			host,
			output,
			trace,
			start: (options, onMessage, onExit) => new Engine(options, record(sessionId, onMessage), onExit),
			read: savedMessages,
			...(side ? { sideOf: side } : {}),
		});
	} catch (error) {
		log(`session/${resume ? "load" : "new"} failed:`, errorText(error));
		throw error instanceof acp.RequestError ? error : new acp.RequestError(-32603, `Couldn't start Claude Code: ${errorText(error)}`);
	}
	sessions.set(sessionId, session);
	log(`session/${resume ? "load" : "new"} ${sessionId} cwd=${cwd} model=${session.settings.model}${side ? ` side chat of ${side}` : ""}`);
	// Sent after the response is written, so the client already knows the session.
	setImmediate(() => output.notify(STATUS_METHOD, session.status()));
}

function sessionFor(sessionId: string): Session {
	const session = sessions.get(sessionId);
	if (!session) throw acp.RequestError.sessionNotFound(sessionId);
	return session;
}

/** A provider other than ours can't be served. */
function checkProvider(provider: string | undefined): void {
	if (provider && provider !== PROVIDER) throw new Error(`Unknown provider ${provider}`);
}

const app = acp
	.agent({ name: "claude-agent" })
	.onRequest(acp.methods.agent.initialize, async ({ params }) => {
		log(`initialize: client protocolVersion=${params.protocolVersion}`);
		return {
			protocolVersion: acp.PROTOCOL_VERSION,
			agentCapabilities: {
				// Claude Code keeps every conversation; a session continues it.
				loadSession: true,
				promptCapabilities: { image: true, audio: false, embeddedContext: true },
			},
			agentInfo: { name: "claude-agent", title: "Claude Agent", version: VERSION },
			authMethods: [],
			// The Space protocol this build speaks (the host refuses a mismatch).
			_meta: { [PROTOCOL_VERSION_META_KEY]: PROTOCOL_VERSION, [STEER_META_KEY]: true },
		};
	})
	.onRequest(acp.methods.agent.session.new, async ({ params }) => {
		// Claude Code takes the id as its own session id (a UUID).
		const sessionId = randomUUID();
		await openSession(sessionId, params.cwd, false, sideOf(params._meta, SIDE_OF_META_KEY));
		return { sessionId };
	})
	// Continues the conversation Claude Code kept. ACP has the agent replay it here; Space
	// already shows it (from its own saved thread), so nothing is replayed.
	.onRequest(acp.methods.agent.session.load, async ({ params }) => {
		if (!sessions.has(params.sessionId)) await openSession(params.sessionId, params.cwd, true, sideOf(params._meta, SIDE_OF_META_KEY));
		return {};
	})
	.onRequest(acp.methods.agent.session.prompt, async ({ params }) => {
		const session = sessionFor(params.sessionId);
		let result: Awaited<ReturnType<Session["prompt"]>>;
		try {
			result = await session.prompt(params.prompt, contextFromMeta(params._meta, CONTEXT_META_KEY));
		} catch (error) {
			throw new acp.RequestError(-32603, errorText(error));
		}
		if ("error" in result) {
			log(`turn ended with an error: ${result.error}`);
			throw new acp.RequestError(-32603, result.error);
		}
		return { stopReason: result.stopReason, ...(result.usage ? { _meta: { [USAGE_META_KEY]: result.usage } } : {}) };
	})
	.onRequest(CONFIGURE_METHOD, parseConfigure, async ({ params }) => {
		const session = sessionFor(params.sessionId);
		try {
			checkProvider(params.provider);
			await session.configure(params.model, params.thinkingLevel);
		} catch (error) {
			throw acp.RequestError.invalidParams(undefined, errorText(error));
		}
		const status = session.status();
		log(`configure ${params.sessionId}: model=${status.model} effort=${status.thinkingLevel}`);
		return status;
	})
	// A short answer outside any conversation: Space's titles plugin names threads with it.
	.onRequest(COMPLETE_METHOD, parseComplete, async ({ params }) => {
		const session = sessionFor(params.sessionId);
		try {
			checkProvider(params.provider);
			return { text: await complete(requireCli(), session.cwd, params.prompt, params.model ?? session.settings.model) };
		} catch (error) {
			log(`complete ${params.sessionId} failed:`, errorText(error));
			throw new acp.RequestError(-32603, errorText(error));
		}
	})
	// A message for the running turn, read after its current step.
	.onRequest(STEER_METHOD, parseSteer, async ({ params }) => {
		try {
			sessionFor(params.sessionId).steer(params.id, steerBlocks(params));
		} catch (error) {
			throw acp.RequestError.invalidRequest(undefined, errorText(error));
		}
		return {};
	})
	// Take back the steered messages Claude hasn't read.
	.onRequest(UNSTEER_METHOD, sessionRef, async ({ params }) => ({ ids: await sessionFor(params.sessionId).unsteer() }))
	.onNotification(acp.methods.agent.session.cancel, async ({ params }) => {
		const session = sessions.get(params.sessionId);
		if (!session?.running) return;
		log(`session/cancel ${params.sessionId}`);
		await session.cancel();
	});

const stream = acp.ndJsonStream(Writable.toWeb(process.stdout) as WritableStream<Uint8Array>, Readable.toWeb(process.stdin) as ReadableStream<Uint8Array>);
const connection = app.connect(stream);

let shuttingDown = false;
function shutdown(reason: string, code = 0): void {
	if (shuttingDown) return;
	shuttingDown = true;
	log(`shutting down (${reason}); closing ${sessions.size} session(s)`);
	for (const session of sessions.values()) session.close();
	sessions.clear();
	// Claude Code processes end with their stdin; give them a moment to flush their transcripts.
	setTimeout(() => process.exit(code), 500).unref();
}

connection.closed.then(
	() => shutdown("stdin closed"),
	(error: unknown) => {
		log("connection error:", errorText(error));
		shutdown("connection error", 1);
	},
);
process.on("SIGINT", () => shutdown("SIGINT"));
process.on("SIGTERM", () => shutdown("SIGTERM"));
