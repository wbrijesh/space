// resilient.ts in a real pi session: pi's own retry drops a reply that broke off partway and
// asks again, and an error retrying can't fix ends the turn. A scripted provider plays the model.

import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { type AssistantMessage, createAssistantMessageEventStream, type SimpleStreamOptions, type TranscriptContext } from "@earendil-works/pi-ai";

// A throwaway pi agent dir: never ~/.pi or ~/.space.
process.env.SPACE_PI_AGENT_DIR = mkdtempSync(join(tmpdir(), "pi-agent-resilient-test-"));
process.env.SPACE_PI_MODEL = "scripted/m";
const { isolatePiConfig, loadConfig } = await import("./env.js");
const { createPiSession } = await import("./pi.js");
const config = loadConfig();
isolatePiConfig(config);


/** `actual` has `expected`'s fields (and maybe more). */
function matches(actual: unknown, expected: Record<string, unknown>): void {
	for (const [k, v] of Object.entries(expected)) assert.deepEqual((actual as Record<string, unknown>)[k], v);
}

type Step = { text: string; fail?: string };

function assistant(text: string, stopReason: AssistantMessage["stopReason"], errorMessage?: string): AssistantMessage {
	return {
		role: "assistant",
		content: [{ type: "text", text }],
		api: "scripted",
		provider: "scripted",
		model: "m",
		usage: { input: 1, output: 1, cacheRead: 0, cacheWrite: 0, totalTokens: 2, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
		stopReason,
		errorMessage,
		timestamp: Date.now(),
	};
}

/** A session on a provider playing `steps`; what each request was sent, and what was shown. */
async function session(steps: Step[]) {
	const sent: TranscriptContext[] = [];
	const states: unknown[] = [];
	const streamSimple = (_model: unknown, context: TranscriptContext, _options?: SimpleStreamOptions) => {
		const step = steps[Math.min(sent.length, steps.length - 1)]!;
		sent.push(structuredClone(context));
		const out = createAssistantMessageEventStream();
		queueMicrotask(() => {
			const partial = assistant("", "stop");
			out.push({ type: "start", partial });
			out.push({ type: "text_start", contentIndex: 0, partial });
			out.push({ type: "text_delta", contentIndex: 0, delta: step.text, partial });
			if (step.fail) out.push({ type: "error", reason: "error", error: assistant(step.text, "error", step.fail) });
			else out.push({ type: "done", reason: "stop", message: assistant(step.text, "stop") });
			out.end();
		});
		return out;
	};
	const pi = await createPiSession(process.env.SPACE_PI_AGENT_DIR!, config, {
		sessionId: "s",
		host: { preExecute: async () => ({ decision: "allow" as const }), confine: async (p) => ({ argv: p.argv }) },
		probe: async () => true,
		onRetry: (state) => states.push(state),
		providerExtension: ((pi: { registerProvider: (name: string, config: unknown) => void }) => {
			pi.registerProvider("scripted", {
				name: "Scripted",
				baseUrl: "https://example.invalid",
				apiKey: "scripted",
				api: "scripted",
				streamSimple,
				models: [{ id: "m", name: "M", reasoning: false, input: ["text"], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 100_000, maxTokens: 1000 }],
			});
		}) as never,
	});
	const retries: string[] = [];
	pi.session.subscribe((e) => {
		if (e.type === "auto_retry_start") retries.push(e.errorMessage);
	});
	return { pi, sent, states, retries };
}

const lastAssistant = (messages: unknown[]) => messages.filter((m) => (m as { role: string }).role === "assistant").pop() as AssistantMessage;

test("a reply that broke off is dropped and asked for again", async () => {
	const { pi, sent, states, retries } = await session([{ text: "BROKEN-OFF", fail: "API Error: Connection dropped (ECONNRESET)." }, { text: "whole reply" }]);
	await pi.session.prompt("hello");
	assert.equal(retries.length, 1);
	assert.equal(sent.length, 2);
	// The broken attempt isn't sent back to the model.
	assert.ok(!JSON.stringify(sent[1]!.messages).includes("BROKEN-OFF"));
	const last = lastAssistant(pi.session.messages);
	assert.equal(last.stopReason, "stop");
	assert.deepEqual(last.content, [{ type: "text", text: "whole reply" }]);
	matches(states[0], { kind: "retrying", attempt: 1 });
	assert.equal(states[states.length - 1], null);
});

test("an error retrying can't fix ends the turn", async () => {
	const { pi, sent, retries } = await session([{ text: "par", fail: "You've hit your usage limit" }]);
	await pi.session.prompt("hello");
	assert.equal(retries.length, 0);
	assert.equal(sent.length, 1);
	const last = lastAssistant(pi.session.messages);
	assert.equal(last.stopReason, "error");
	assert.equal(last.errorMessage, "You've hit your usage limit");
});
