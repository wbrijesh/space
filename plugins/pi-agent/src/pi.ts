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
import { ModeControl } from "./mode.js";

export interface PiSession {
	runtime: AgentSessionRuntime;
	session: AgentSession;
	provider: string;
	model: string;
	thinkingLevel: string;
	/** ACP session mode (read/write) and its pi hooks. */
	mode: ModeControl;
}

export async function createPiSession(cwd: string, config: AgentConfig): Promise<PiSession> {
	const mode = new ModeControl();
	const createRuntime: CreateAgentSessionRuntimeFactory = async ({ cwd, agentDir, sessionManager, sessionStartEvent }) => {
		// No API keys: in-memory credentials, no models.json, in-memory settings. The only
		// provider we use is claude-cli, which signs requests through the `claude` CLI.
		const modelRuntime = await ModelRuntime.create({
			credentials: new InMemoryCredentialStore(),
			modelsPath: null,
		});
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
					// Read-only mode: hidden prompt section and bash guard (see mode.ts).
					{ name: "space-mode", factory: mode.extension(), hidden: true },
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
		});
		return { ...result, services, diagnostics: services.diagnostics };
	};

	const runtime = await createAgentSessionRuntime(createRuntime, {
		cwd,
		agentDir: config.agentDir,
		sessionManager: SessionManager.inMemory(cwd),
	});
	const session = runtime.session;
	// Emits session_start to extensions (claude-cli manages its CLI process per session).
	await session.bindExtensions({});
	mode.attach(session);
	const model = session.model;
	return {
		runtime,
		session,
		provider: model?.provider ?? config.provider,
		model: model?.id ?? config.modelId,
		thinkingLevel: session.thinkingLevel,
		mode,
	};
}
