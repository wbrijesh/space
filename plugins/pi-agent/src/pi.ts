// Creates isolated pi agent sessions backed by the claude-cli provider.

import { InMemoryCredentialStore } from "@earendil-works/pi-ai";
import {
	type AgentSession,
	type AgentSessionRuntime,
	type CreateAgentSessionRuntimeFactory,
	createAgentSessionFromServices,
	createAgentSessionRuntime,
	createAgentSessionServices,
	ModelRuntime,
	SessionManager,
	SettingsManager,
} from "@earendil-works/pi-coding-agent";
import claudeCli from "@pi-plugins/claude-cli";
import type { AgentConfig } from "./env.js";
import { log } from "./env.js";
import { HostBridge, type HostLink } from "./host.js";
import { TraceRecorder, type TraceSink } from "./trace.js";

export interface PiSession {
	runtime: AgentSessionRuntime;
	session: AgentSession;
	provider: string;
	model: string;
	thinkingLevel: string;
	/** The host's tool pipeline, sandbox and context for this session. */
	host: HostBridge;
	/** Records what happens around each model request (`_space/trace`). */
	trace: TraceRecorder;
}

export interface PiSessionOptions {
	/** The ACP session id (the host knows the session by it). */
	sessionId: string;
	host: HostLink;
	/** A folder to keep the session in (the host's `_meta["space/dataDir"]`); else it lives in memory. */
	dataDir?: string;
	/** Continue the session kept in `dataDir` (ACP `session/load`) instead of starting one. */
	load?: boolean;
	/** Tests: register extra providers (e.g. pi-ai's faux provider) before the model is looked up. */
	registerProviders?: (runtime: ModelRuntime) => void;
	/** Where trace batches go (none: they're dropped). */
	traceSink?: TraceSink;
}

export async function createPiSession(cwd: string, config: AgentConfig, options: PiSessionOptions): Promise<PiSession> {
	const bridge = new HostBridge(options.host, options.sessionId);
	const trace = new TraceRecorder(options.sessionId, options.traceSink ?? (async () => {}), options.load ?? false);
	const createRuntime: CreateAgentSessionRuntimeFactory = async ({ cwd, agentDir, sessionManager, sessionStartEvent }) => {
		// No API keys: in-memory credentials, no models.json, in-memory settings. The only
		// provider we use is claude-cli, which signs requests through the `claude` CLI.
		const modelRuntime = await ModelRuntime.create({
			credentials: new InMemoryCredentialStore(),
			modelsPath: null,
		});
		options.registerProviders?.(modelRuntime);
		const settingsManager = SettingsManager.inMemory({
			retry: { enabled: true, maxRetries: 2 },
		});
		const services = await createAgentSessionServices({
			cwd,
			agentDir,
			modelRuntime,
			settingsManager,
			resourceLoaderOptions: {
				// Do not load the user's configured pi packages/extensions (their settings may
				// install @pi-plugins/claude-cli too); register it inline exactly once instead.
				noExtensions: true,
				noSkills: true,
				noPromptTemplates: true,
				noThemes: true,
				extensionFactories: [
					{ name: "claude-cli", factory: claudeCli },
					// The host's tool pipeline and hidden context sections (see host.ts).
					{ name: "space-host", factory: bridge.extension(), hidden: true },
					// What each request sent and got back, for Space's Trajectory view (see trace.ts).
					{ name: "space-trace", factory: trace.extension(), hidden: true },
				],
				// Project context files (AGENTS.md / CLAUDE.md walking up from cwd) still load.
			},
		});
		for (const d of services.diagnostics) log(`pi ${d.type}: ${d.message}`);

		const model = services.modelRuntime.getModel(config.provider, config.modelId);
		if (!model) {
			const available = services.modelRuntime
				.getModels(config.provider)
				.map((m) => `${m.provider}/${m.id}`)
				.join(", ");
			throw new Error(
				`Model ${config.provider}/${config.modelId} is not available. ${available ? `Known ${config.provider} models: ${available}` : `Provider "${config.provider}" is not registered.`}`,
			);
		}

		const result = await createAgentSessionFromServices({
			services,
			sessionManager,
			sessionStartEvent,
			model,
			thinkingLevel: config.thinkingLevel,
			// Same name as the built-in bash tool, so it replaces it: commands are confined first.
			customTools: [bridge.bashTool(cwd)],
		});
		return { ...result, services, diagnostics: services.diagnostics };
	};

	// One session per data folder: loading continues the one kept there.
	const { dataDir, load, sessionId } = options;
	const sessionManager = !dataDir
		? SessionManager.inMemory(cwd)
		: load
			? SessionManager.continueRecent(cwd, dataDir)
			: SessionManager.create(cwd, dataDir, { id: sessionId });
	const runtime = await createAgentSessionRuntime(createRuntime, {
		cwd,
		agentDir: config.agentDir,
		sessionManager,
	});
	const session = runtime.session;
	// Emits session_start to extensions (claude-cli manages its CLI process per session).
	await session.bindExtensions({});
	const model = session.model;
	return {
		runtime,
		session,
		provider: model?.provider ?? config.provider,
		model: model?.id ?? config.modelId,
		thinkingLevel: session.thinkingLevel,
		host: bridge,
		trace,
	};
}
