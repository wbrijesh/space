// `_space/status` payloads and `_space/session/configure` handling for a pi session.

import type { AgentSession } from "@earendil-works/pi-coding-agent";
import { THINKING_LEVELS, type ThinkingLevelName } from "./env.js";
import type { ConfigureSession, ModelInfo, SpaceStatus } from "./generated/protocol.js";

function str(v: unknown): string | undefined {
	return typeof v === "string" && v.length > 0 ? v : undefined;
}

export function parseConfigureParams(raw: unknown): ConfigureSession {
	const p = (raw && typeof raw === "object" ? raw : {}) as Record<string, unknown>;
	const sessionId = str(p.sessionId);
	if (!sessionId) throw new Error("sessionId is required");
	return { sessionId, provider: str(p.provider), model: str(p.model), thinkingLevel: str(p.thinkingLevel) };
}

const FAMILY_ORDER = ["opus", "sonnet", "haiku", "fable"];

/** Sort key: model family (Opus, Sonnet, Haiku, Fable, others), then newest version first. */
function modelSortKey(m: { id: string }): [number, number[]] {
	const family = FAMILY_ORDER.findIndex((f) => m.id.includes(f));
	const version = (m.id.match(/\d+/g) ?? []).map(Number);
	return [family === -1 ? FAMILY_ORDER.length : family, version];
}

function compareModels(a: { id: string }, b: { id: string }): number {
	const [fa, va] = modelSortKey(a);
	const [fb, vb] = modelSortKey(b);
	if (fa !== fb) return fa - fb;
	for (let i = 0; i < Math.max(va.length, vb.length); i++) {
		const d = (vb[i] ?? -1) - (va[i] ?? -1);
		if (d !== 0) return d;
	}
	return a.id.localeCompare(b.id);
}

/**
 * Models of the session's provider, minus dated snapshots (`claude-opus-4-5-20251101`) that
 * duplicate an alias, sorted by family then newest first.
 */
export function availableModels(session: AgentSession): ModelInfo[] {
	const provider = session.model?.provider;
	if (!provider) return [];
	return session.modelRuntime
		.getModels(provider)
		.filter((m) => !/-\d{8}$/.test(m.id))
		.sort(compareModels)
		.map((m) => ({ provider: m.provider, id: m.id, name: m.name }));
}

export function sessionStatus(sessionId: string, session: AgentSession): SpaceStatus {
	const model = session.model;
	const status: SpaceStatus = {
		sessionId,
		provider: model?.provider,
		model: model?.id,
		thinkingLevel: session.thinkingLevel,
		availableModels: availableModels(session),
		thinkingLevels: session.supportsThinking() ? session.getAvailableThinkingLevels() : [],
	};
	const usage = session.getContextUsage();
	if (usage) {
		status.contextWindow = usage.contextWindow;
		if (usage.tokens != null) status.contextTokens = usage.tokens;
	} else if (model?.contextWindow) {
		status.contextWindow = model.contextWindow;
	}
	return status;
}

/** Apply a configure request to `session`. Throws a readable Error on invalid input. */
export async function configureSession(session: AgentSession, params: ConfigureSession): Promise<void> {
	if (params.model) {
		const provider = params.provider ?? session.model?.provider;
		if (!provider) throw new Error("provider is required: the session has no current model");
		const model = session.modelRuntime.getModel(provider, params.model);
		if (!model) throw new Error(`Unknown model ${provider}/${params.model}`);
		await session.setModel(model);
	}
	if (params.thinkingLevel) {
		const level = params.thinkingLevel as ThinkingLevelName;
		if (!THINKING_LEVELS.includes(level)) {
			throw new Error(`thinkingLevel must be one of ${THINKING_LEVELS.join(", ")}`);
		}
		// pi clamps to what the current model supports.
		session.setThinkingLevel(level);
	}
}
