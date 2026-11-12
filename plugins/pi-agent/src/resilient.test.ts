import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { type AssistantMessage, type AssistantMessageEvent, createAssistantMessageEventStream, type Model, type TranscriptContext } from "@earendil-works/pi-ai";
import { isRetryableAssistantError } from "@earendil-works/pi-ai/compat";
import { classify } from "./network.js";
import { GIVE_UP_MS, PI_RETRY_PREFIX, Resilience, type RetryState, STALL_MAX_MS, type StreamFn, UNKNOWN_LIMIT } from "./resilient.js";


/** `actual` has `expected`'s fields (and maybe more). */
function matches(actual: unknown, expected: Record<string, unknown>): void {
	for (const [k, v] of Object.entries(expected)) assert.deepEqual((actual as Record<string, unknown>)[k], v);
}

const SCREENSHOT = "API Error: Connection dropped (ECONNRESET). A VPN, firewall, or security software can cause this. Check those or switch networks, then try again.";
const CONTEXT = { messages: [] } as unknown as TranscriptContext;
const model = { api: "anthropic-messages", provider: "claude-cli", id: "m" } as Model<string>;

function message(stopReason: AssistantMessage["stopReason"], errorMessage?: string, text = ""): AssistantMessage {
	return {
		role: "assistant",
		content: text ? [{ type: "text", text }] : [],
		api: model.api,
		provider: model.provider,
		model: model.id,
		usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
		stopReason,
		errorMessage,
		timestamp: 0,
	};
}

/** One scripted response: a reply, a failure before or after some text, or a stream that never ends. */
type Step = { ok: string } | { fail: string; after?: string } | "hang";

/** A provider playing `steps` in order (the last repeats); it ends an aborted request as pi-ai does. */
function provider(steps: Step[]) {
	const calls: number[] = [];
	const fn: StreamFn = (_model, _context, options) => {
		const step = steps[Math.min(calls.length, steps.length - 1)]!;
		calls.push(calls.length);
		const out = createAssistantMessageEventStream();
		const partial = message("stop");
		const abort = () => {
			out.push({ type: "error", reason: "aborted", error: message("aborted", "Request was aborted") });
			out.end();
		};
		options?.signal?.addEventListener("abort", abort, { once: true });
		queueMicrotask(() => {
			out.push({ type: "start", partial });
			if (step === "hang") return;
			const text = "ok" in step ? step.ok : step.after;
			if (text) {
				out.push({ type: "text_start", contentIndex: 0, partial });
				out.push({ type: "text_delta", contentIndex: 0, delta: text, partial });
			}
			if ("ok" in step) out.push({ type: "done", reason: "stop", message: message("stop", undefined, text) });
			else out.push({ type: "error", reason: "error", error: message("error", step.fail) });
			out.end();
		});
		return out;
	};
	return { fn, calls };
}

/** Deps on a fake clock: sleeps return at once and move it; `probes` answer in order (the last repeats). */
function harness(probes: boolean[] = [true]) {
	let clock = 0;
	let probed = 0;
	const ticks: (() => void)[] = [];
	const h = {
		states: [] as (RetryState | null)[],
		piRetry: [] as boolean[],
		slept: [] as number[],
		advance(ms: number) {
			clock += ms;
			for (const tick of ticks) tick();
		},
		resilience: undefined as unknown as Resilience,
		onState: (_state: RetryState | null) => {},
	};
	h.resilience = new Resilience({
		probe: async () => probes[Math.min(probed++, probes.length - 1)]!,
		sleep: async (ms, signal) => {
			if (signal?.aborted) throw new Error("aborted");
			h.slept.push(ms);
			clock += ms;
		},
		now: () => clock,
		every: (_ms, fn) => {
			ticks.push(fn);
			return () => ticks.splice(ticks.indexOf(fn), 1);
		},
		random: () => 0.5,
		onRetry: (state) => {
			h.states.push(state);
			h.onState(state);
		},
		setPiRetry: (enabled) => h.piRetry.push(enabled),
		log: () => {},
	});
	return h;
}

async function collect(stream: ReturnType<StreamFn>): Promise<AssistantMessageEvent[]> {
	const events: AssistantMessageEvent[] = [];
	for await (const e of stream) events.push(e);
	return events;
}

const last = (events: AssistantMessageEvent[]) => events[events.length - 1]!;
const errorOf = (events: AssistantMessageEvent[]) => {
	const e = last(events);
	if (e.type !== "error") throw new Error(`ended with ${e.type}`);
	return e.error;
};

describe("classify", () => {
	test("sorts real errors", () => {
		assert.equal(classify(SCREENSHOT), "network");
		assert.equal(classify("fetch failed"), "network");
		assert.equal(classify("getaddrinfo ENOTFOUND api.anthropic.com"), "network");
		assert.equal(classify("socket hang up"), "network");
		assert.equal(classify("Overloaded"), "busy");
		assert.equal(classify("429 Too Many Requests"), "busy");
		assert.equal(classify("You've hit your usage limit · resets 3pm"), "fatal");
		assert.equal(classify("OAuth token has expired. Please run /login"), "fatal");
		assert.equal(classify("prompt is too long: 210000 tokens"), "fatal");
		assert.equal(classify("Claude Code exited with code 1"), "unknown");
	});

	test("pi retries what is handed back as retryable, and didn't retry the screenshot's error", () => {
		assert.equal(isRetryableAssistantError(message("error", SCREENSHOT)), false);
		for (const prefix of Object.values(PI_RETRY_PREFIX)) {
			for (const reason of [SCREENSHOT, "Overloaded", "Claude Code exited with code 1"]) {
				assert.equal(isRetryableAssistantError(message("error", prefix + reason)), true);
			}
		}
	});
});

describe("Resilience", () => {
	test("a failure before any content is retried here, after a backoff", async () => {
		const h = harness([true]);
		const p = provider([{ fail: SCREENSHOT }, { ok: "hi" }]);
		const events = await collect(h.resilience.wrap(p.fn)(model, CONTEXT));
		assert.equal(last(events).type, "done");
		assert.equal(events.filter((e) => e.type === "start").length, 1);
		assert.equal(p.calls.length, 2);
		assert.deepEqual(h.slept, [1000]);
		assert.deepEqual(h.states, [{ kind: "retrying", attempt: 1, reason: SCREENSHOT }, null]);
	});

	test("offline, it waits until Claude can be reached", async () => {
		const h = harness([false, false, false, true]);
		const p = provider([{ fail: SCREENSHOT }, { ok: "hi" }]);
		const events = await collect(h.resilience.wrap(p.fn)(model, CONTEXT));
		assert.equal(last(events).type, "done");
		assert.deepEqual(h.slept, [2000, 3000]);
		assert.deepEqual(h.states[0], { kind: "offline", attempt: 1, reason: SCREENSHOT });
		assert.equal(h.states[h.states.length - 1], null);
	});

	test("Claude busy waits longer", async () => {
		const h = harness([true]);
		const p = provider([{ fail: "Overloaded" }, { ok: "hi" }]);
		await collect(h.resilience.wrap(p.fn)(model, CONTEXT));
		assert.deepEqual(h.slept, [5000]);
		assert.equal(h.states[0]?.kind, "busy");
	});

	test("a failure after content goes to pi's retry, which waits first", async () => {
		const h = harness([true]);
		const p = provider([{ fail: SCREENSHOT, after: "par" }, { ok: "hi" }]);
		const stream = h.resilience.wrap(p.fn);
		const first = await collect(stream(model, CONTEXT));
		assert.equal(first.some((e) => e.type === "text_delta"), true);
		const error = errorOf(first);
		assert.equal(isRetryableAssistantError(error), true);
		assert.deepEqual(h.piRetry, [true]);
		// pi asks again: the backoff comes first.
		const second = await collect(stream(model, CONTEXT));
		assert.equal(last(second).type, "done");
		assert.deepEqual(h.slept, [1000]);
	});

	test("an error retrying can't fix passes through at once", async () => {
		const h = harness([true]);
		const p = provider([{ fail: "You've hit your usage limit" }]);
		const error = errorOf(await collect(h.resilience.wrap(p.fn)(model, CONTEXT)));
		assert.equal(error.errorMessage, "You've hit your usage limit");
		assert.deepEqual(h.piRetry, [false]);
		assert.equal(p.calls.length, 1);
		assert.deepEqual(h.states, []);
	});

	test("it gives up after GIVE_UP_MS without Claude", async () => {
		const h = harness([false]);
		const p = provider([{ fail: SCREENSHOT }]);
		const error = errorOf(await collect(h.resilience.wrap(p.fn)(model, CONTEXT)));
		assert.equal(error.errorMessage, "Couldn't reach Claude for 30 minutes (ECONNRESET).");
		assert.ok(h.slept.reduce((a, b) => a + b, 0) >= GIVE_UP_MS);
		assert.deepEqual(h.piRetry, [false]);
		assert.equal(h.states[h.states.length - 1], null);
	});

	test("unknown errors are retried a few times only", async () => {
		const h = harness([true]);
		const p = provider([{ fail: "Claude Code exited with code 1" }]);
		const error = errorOf(await collect(h.resilience.wrap(p.fn)(model, CONTEXT)));
		assert.equal(error.errorMessage, "Claude Code exited with code 1");
		assert.equal(p.calls.length, UNKNOWN_LIMIT + 1);
	});

	test("Stop ends a wait at once, and the next request starts fresh", async () => {
		const h = harness([false]);
		const stop = new AbortController();
		h.onState = (state) => state?.kind === "offline" && stop.abort();
		const p = provider([{ fail: SCREENSHOT }, { ok: "hi" }]);
		const wrapped = h.resilience.wrap(p.fn);
		const events = await collect(wrapped(model, CONTEXT, { signal: stop.signal }));
		matches(last(events), { type: "error", reason: "aborted" });
		assert.deepEqual(h.states, [{ kind: "offline", attempt: 1, reason: SCREENSHOT }, null]);
		assert.ok(h.slept.length <= 1);
		// No wait carried over to the next turn.
		h.onState = () => {};
		assert.equal(last(await collect(wrapped(model, CONTEXT))).type, "done");
		assert.equal(p.calls.length, 2);
	});

	test("Stop before a request sends nothing", async () => {
		const h = harness();
		const stop = new AbortController();
		stop.abort();
		const p = provider([{ ok: "hi" }]);
		const events = await collect(h.resilience.wrap(p.fn)(model, CONTEXT, { signal: stop.signal }));
		matches(last(events), { type: "error", reason: "aborted" });
		assert.equal(p.calls.length, 0);
	});

	test("a silent stream is cut when the network is down", async () => {
		const h = harness([false, true]);
		const p = provider(["hang", { ok: "hi" }]);
		const stream = h.resilience.wrap(p.fn)(model, CONTEXT);
		const done = collect(stream);
		await new Promise((r) => setTimeout(r, 1));
		h.advance(50_000);
		const events = await done;
		assert.equal(last(events).type, "done");
		assert.equal(p.calls.length, 2);
	});

	test("a silent stream is kept while Claude can be reached, up to STALL_MAX_MS", async () => {
		const h = harness([true]);
		const p = provider(["hang", { ok: "hi" }]);
		const done = collect(h.resilience.wrap(p.fn)(model, CONTEXT));
		await new Promise((r) => setTimeout(r, 1));
		h.advance(50_000);
		await new Promise((r) => setTimeout(r, 1));
		assert.equal(p.calls.length, 1);
		h.advance(STALL_MAX_MS);
		assert.equal(last(await done).type, "done");
		assert.equal(p.calls.length, 2);
	});
});
