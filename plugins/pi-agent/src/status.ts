// `_space/status` payloads and `_space/session/configure` handling for a pi session.

import type { AgentSession } from "@earendil-works/pi-coding-agent";
import { THINKING_LEVELS, type ThinkingLevelName } from "./env.js";
import type { Complete, ConfigureSession, ModelInfo, SpaceStatus } from "./generated/protocol.js";

function str(v: unknown): string | undefined {
	return typeof v === "string" && v.length > 0 ? v : undefined;
}

export function parseCompleteParams(raw: unknown): Complete {
	const p = (raw && typeof raw === "object" ? raw : {}) as Record<string, unknown>;
	const sessionId = str(p.sessionId);
	const prompt = str(p.prompt);
	if (!sessionId || !prompt) throw new Error("sessionId and prompt are required");
	return { sessionId, prompt, provider: str(p.provider), model: str(p.model) };
}

export function parseConfigureParams(raw: unknown): ConfigureSession {
	const p = (raw && typeof raw === "object" ? raw : {}) as Record<string, unknown>;
	const sessionId = str(p.sessionId);
	if (!sessionId) throw new Error("sessionId is required");
	return { sessionId, provider: str(p.provider), model: str(p.model), thinkingLevel: str(p.thinkingLevel) };
}

const FAMILY_ORDER = ["fable", "opus", "sonnet", "haiku"];

/** `claude-opus-5-5` -> `claude-opus`: the id without its version numbers. */
export function modelFamily(id: string): string {
	return id
		.split("-")
		.filter((part) => !/^\d+$/.test(part))
		.join("-");
}

/** Sort key: model family (Fable, Opus, Sonnet, Haiku, others), then newest version first. */
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
 * The newest model of each family (`claude-fable-5-1`, `claude-opus-5-5`, ...), ignoring dated
 * snapshots (`claude-opus-4-5-20251101`), sorted by family. `current` stays listed even when
 * it's an older one, so a thread on it still shows what it uses.
 */
export function latestModels<M extends { id: string }>(models: readonly M[], current?: string): M[] {
	const seen = new Set<string>();
	return models
		.filter((m) => !/-\d{8}$/.test(m.id))
		.sort(compareModels)
		.filter((m) => {
			const family = modelFamily(m.id);
			const newest = !seen.has(family);
			seen.add(family);
			return newest || m.id === current;
		});
}

export function availableModels(session: AgentSession): ModelInfo[] {
	const provider = session.model?.provider;
	if (!provider) return [];
	return latestModels(session.modelRuntime.getModels(provider), session.model?.id).map((m) => ({
		provider: m.provider,
		id: m.id,
		name: m.name,
	}));
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

/** Longest reply `_space/complete` asks for: it's for short answers (a thread's title). */
const COMPLETE_MAX_TOKENS = 256;

/**
 * `_space/complete`: one short, tool-free completion of `params.prompt` by the session's model,
 * or the requested one (its provider defaulting to the session's). The session's conversation
 * isn't touched. Throws a readable Error.
 */
export async function completeOnce(session: AgentSession, params: Complete): Promise<string> {
	const current = session.model;
	const provider = params.provider ?? current?.provider;
	const id = params.model ?? current?.id;
	const model = provider && id ? session.modelRuntime.getModel(provider, id) : undefined;
	if (!model) throw new Error(`Unknown model ${provider ?? "?"}/${id ?? "?"}`);
	const reply = await session.modelRuntime.completeSimple(
		model,
		{ messages: [{ role: "user", content: params.prompt, timestamp: Date.now() }] },
		{ maxTokens: COMPLETE_MAX_TOKENS },
	);
	if (reply.stopReason === "error" || reply.stopReason === "aborted") throw new Error(reply.errorMessage || "The model request failed");
	return reply.content.flatMap((c) => (c.type === "text" ? [c.text] : [])).join("");
}
