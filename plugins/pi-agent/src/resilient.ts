// Model requests that survive a bad network. Wraps the provider's stream function:
//
// - Before a request that follows a failure, wait: until Claude can be reached again (offline),
//   or a backoff (connection errors while online, Claude busy). Stop cancels the wait.
// - A request that fails before any content streamed is retried here; pi never sees it.
// - One that fails after content streamed goes to pi's own retry, which drops the partial reply
//   and calls this again (the host is told to drop it from the transcript too).
// - A silent stream is cut when Claude can't be reached, or after STALL_MAX_MS regardless.
// - Errors retrying can't fix (sign-in, usage limit, invalid request) pass through at once.
// - It gives up after GIVE_UP_MS of failing (offline or not), or UNKNOWN_LIMIT unknown errors.
//
// pi's retry is switched per failure (`setPiRetry`) so it retries exactly what this decides.

import type { AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, Model, SimpleStreamOptions, StreamFunction } from "@earendil-works/pi-ai";
import { createAssistantMessageEventStream } from "@earendil-works/pi-ai";
import type { RetryState } from "./generated/protocol.js";
import { classify, errorCode, type FailureKind } from "./network.js";

export type StreamFn = StreamFunction<string, SimpleStreamOptions>;

/** What the user sees while a request waits (`_space/retry`): offline (until Claude can be
 * reached), retrying (a connection error while online) or busy (Claude overloaded or limiting). */
export type { RetryState };

export interface Deps {
	/** Whether Claude's API can be reached now. */
	probe(): Promise<boolean>;
	/** Resolves after `ms`, rejects when `signal` aborts. */
	sleep(ms: number, signal?: AbortSignal): Promise<void>;
	now(): number;
	/** Call `fn` every `ms` until the returned function is called. */
	every(ms: number, fn: () => void): () => void;
	/** 0 to 1, for jitter. */
	random(): number;
	/** The waiting state changed (`null`: nothing waits). */
	onRetry(state: RetryState | null): void;
	/** Whether pi retries the error about to be returned. */
	setPiRetry(enabled: boolean): void;
	log(...args: unknown[]): void;
}

export const GIVE_UP_MS = 30 * 60_000;
export const UNKNOWN_LIMIT = 6;
/** Silence after which Claude is probed, and the most allowed whatever the probe says. */
export const STALL_PROBE_MS = 45_000;
export const STALL_MAX_MS = 150_000;
const WATCH_EVERY_MS = 5_000;
const BACKOFF_MS = [1_000, 2_000, 4_000, 8_000, 15_000];
const BUSY_BACKOFF_MS = [5_000, 10_000, 20_000, 40_000, 60_000];
/** Offline: how often to probe (growing to the last). */
const OFFLINE_PROBE_MS = [2_000, 3_000, 4_000, 5_000];
/** A failure this soon after coming back online means the network flaps: wait for it to settle. */
const FLAP_MS = 15_000;
const SETTLED_PROBES = 3;

/** Wordings pi's own classifier retries (`isRetryableAssistantError`). */
export const PI_RETRY_PREFIX = { network: "Connection lost: ", busy: "Rate limit: " } as const;

const CONTENT = new Set<AssistantMessageEvent["type"]>(["text_start", "text_delta", "thinking_start", "thinking_delta", "toolcall_start", "toolcall_delta"]);

class Aborted extends Error {}

/** Per session: the failures since the last reply that worked. */
export class Resilience {
	private failingSince?: number;
	private unknownFailures = 0;
	private attempts = 0;
	/** The next request waits first: why. */
	private pending?: { kind: RetryState["kind"]; reason: string };
	private backAt?: number;
	private flapping = false;
	private shown = false;

	private deps: Deps;

	constructor(deps: Deps) {
		this.deps = deps;
	}

	/** `base` with waits and retries around it. */
	wrap(base: StreamFn): StreamFn {
		return (model, context, options) => {
			const out = createAssistantMessageEventStream();
			this.run(base, model, context, options, out).catch((error: unknown) => {
				this.deps.log("resilient stream failed:", error);
				out.push({ type: "error", reason: "error", error: failure(model, error instanceof Error ? error.message : String(error)) });
				out.end();
			});
			return out;
		};
	}

	private async run(base: StreamFn, model: Model<string>, context: Parameters<StreamFn>[1], options: SimpleStreamOptions | undefined, out: AssistantMessageEventStream): Promise<void> {
		const user = options?.signal;
		const finish = (event: AssistantMessageEvent) => {
			out.push(event);
			out.end();
		};
		for (;;) {
			if (this.pending) {
				let verdict: "go" | "gave-up";
				try {
					verdict = await this.wait(user);
				} catch (error) {
					if (!(error instanceof Aborted) && !user?.aborted) throw error;
					this.reset();
					return finish({ type: "error", reason: "aborted", error: failure(model, "Request was aborted", "aborted") });
				}
				if (verdict === "gave-up") return finish(this.giveUp(model));
			}

			if (user?.aborted) {
				this.reset();
				return finish({ type: "error", reason: "aborted", error: failure(model, "Request was aborted", "aborted") });
			}
			const attempt = new AbortController();
			const cancel = () => attempt.abort();
			user?.addEventListener("abort", cancel, { once: true });
			const held: AssistantMessageEvent[] = [];
			let content = false;
			let stalled: string | undefined;
			let last = this.deps.now();
			let probedAt = 0;
			let probing = false;
			const unwatch = this.deps.every(WATCH_EVERY_MS, () => {
				const silent = this.deps.now() - last;
				if (silent >= STALL_MAX_MS) {
					stalled = `No data from Claude for ${Math.round(silent / 1000)}s (connection timed out)`;
					attempt.abort();
				} else if (silent >= STALL_PROBE_MS && !probing && this.deps.now() - probedAt >= 15_000) {
					probing = true;
					probedAt = this.deps.now();
					void this.deps.probe().then((ok) => {
						probing = false;
						if (!ok && this.deps.now() - last >= STALL_PROBE_MS) {
							stalled = `No data from Claude for ${Math.round((this.deps.now() - last) / 1000)}s and the network is down (connection lost)`;
							attempt.abort();
						}
					});
				}
			});

			let end: AssistantMessageEvent | undefined;
			try {
				for await (const event of base(model, context, { ...options, signal: attempt.signal })) {
					last = this.deps.now();
					if (event.type === "done" || event.type === "error") {
						end = event;
						break;
					}
					if (!content) {
						if (!CONTENT.has(event.type)) {
							held.push(event);
							continue;
						}
						content = true;
						this.show(null);
						for (const e of held) out.push(e);
					}
					out.push(event);
				}
			} catch (error) {
				end = { type: "error", reason: "error", error: failure(model, error instanceof Error ? error.message : String(error)) };
			} finally {
				unwatch();
				user?.removeEventListener("abort", cancel);
			}

			if (end?.type === "done") {
				if (!content) for (const e of held) out.push(e);
				this.succeeded();
				return finish(end);
			}
			// Stopped by the user: as it came.
			if (user?.aborted) {
				this.reset();
				return finish(end ?? { type: "error", reason: "aborted", error: failure(model, "Request was aborted", "aborted") });
			}
			const error = end?.type === "error" ? end.error : failure(model, "");
			const message = stalled ?? (error.errorMessage || "The stream ended without a response (connection closed)");
			const retry = await this.failed(message);
			if (!retry) {
				this.deps.setPiRetry(false);
				error.stopReason = "error";
				error.errorMessage = message;
				return finish({ type: "error", reason: "error", error });
			}
			// Nothing reached the transcript: try again here.
			if (!content) continue;
			// Partial content went out: pi drops it and asks again.
			this.deps.setPiRetry(true);
			error.stopReason = "error";
			error.errorMessage = (this.pending?.kind === "busy" ? PI_RETRY_PREFIX.busy : PI_RETRY_PREFIX.network) + message;
			return finish({ type: "error", reason: "error", error });
		}
	}

	/** A request failed with `message`: whether to retry, and how the next one waits. */
	private async failed(message: string): Promise<boolean> {
		const kind: FailureKind = classify(message);
		this.deps.log(`request failed (${kind}): ${message}`);
		if (kind === "fatal") {
			this.reset();
			return false;
		}
		const now = this.deps.now();
		this.failingSince ??= now;
		if (kind === "unknown" && ++this.unknownFailures > UNKNOWN_LIMIT) {
			this.reset();
			return false;
		}
		const online = await this.deps.probe();
		if (!online && this.backAt !== undefined && now - this.backAt < FLAP_MS) this.flapping = true;
		this.pending = { kind: !online ? "offline" : kind === "busy" ? "busy" : "retrying", reason: message };
		return true;
	}

	/** Wait before the next request: until Claude can be reached, or a backoff. */
	private async wait(signal?: AbortSignal): Promise<"go" | "gave-up"> {
		const pending = this.pending!;
		this.attempts++;
		const sleep = async (ms: number) => {
			try {
				await this.deps.sleep(ms, signal);
			} catch {
				throw new Aborted();
			}
		};
		const over = () => this.failingSince !== undefined && this.deps.now() - this.failingSince >= GIVE_UP_MS;
		if (pending.kind === "offline") {
			this.show({ kind: "offline", attempt: this.attempts, reason: pending.reason });
			// Back once a probe gets through; after a flap, once a few in a row do.
			const needed = this.flapping ? SETTLED_PROBES : 1;
			let good = 0;
			for (let i = 0; good < needed; i++) {
				if (over()) return "gave-up";
				if (signal?.aborted) throw new Aborted();
				if (await this.deps.probe()) good++;
				else good = 0;
				if (good < needed) await sleep(OFFLINE_PROBE_MS[Math.min(i, OFFLINE_PROBE_MS.length - 1)]!);
			}
			this.backAt = this.deps.now();
			this.flapping = false;
			this.deps.log("Claude can be reached again");
		} else {
			if (over()) return "gave-up";
			const steps = pending.kind === "busy" ? BUSY_BACKOFF_MS : BACKOFF_MS;
			const base = steps[Math.min(this.attempts - 1, steps.length - 1)]!;
			this.show({ kind: pending.kind, attempt: this.attempts, reason: pending.reason });
			await sleep(Math.round(base * (0.8 + 0.4 * this.deps.random())));
		}
		this.pending = undefined;
		return "go";
	}

	private giveUp(model: Model<string>): AssistantMessageEvent {
		const reason = this.pending?.reason ?? "";
		const busy = this.pending?.kind === "busy";
		const minutes = Math.round(GIVE_UP_MS / 60_000);
		const code = errorCode(reason);
		const message = busy
			? `Claude stayed busy for ${minutes} minutes: ${reason}`
			: `Couldn't reach Claude for ${minutes} minutes${code ? ` (${code})` : reason ? `: ${reason}` : ""}.`;
		this.reset();
		this.deps.setPiRetry(false);
		return { type: "error", reason: "error", error: failure(model, message) };
	}

	private succeeded(): void {
		this.reset();
	}

	private reset(): void {
		this.failingSince = undefined;
		this.unknownFailures = 0;
		this.attempts = 0;
		this.pending = undefined;
		this.flapping = false;
		this.show(null);
	}

	private show(state: RetryState | null): void {
		if (!state && !this.shown) return;
		this.shown = state !== null;
		this.deps.onRetry(state);
	}
}

/** An assistant message for a request that failed (or was stopped) before the provider made one. */
function failure(model: Model<string>, message: string, stopReason: "error" | "aborted" = "error"): AssistantMessage {
	return {
		role: "assistant",
		content: [],
		api: model.api,
		provider: model.provider,
		model: model.id,
		usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
		stopReason,
		errorMessage: message,
		timestamp: Date.now(),
	};
}

/** `setTimeout` that rejects when `signal` aborts. */
export function sleep(ms: number, signal?: AbortSignal): Promise<void> {
	return new Promise((resolve, reject) => {
		if (signal?.aborted) return reject(new Error("aborted"));
		const timer = setTimeout(() => {
			signal?.removeEventListener("abort", stop);
			resolve();
		}, ms);
		const stop = () => {
			clearTimeout(timer);
			reject(new Error("aborted"));
		};
		signal?.addEventListener("abort", stop, { once: true });
	});
}

/** Make an extension factory's `registerProvider` wrap each provider's stream function. */
export function wrapProviders<P extends { registerProvider: (...args: never[]) => unknown }>(pi: P, wrap: (fn: StreamFn) => StreamFn): P {
	return new Proxy(pi, {
		get(target, prop, receiver) {
			if (prop !== "registerProvider") return Reflect.get(target, prop, receiver);
			return (name: string, config: { streamSimple?: StreamFn }) => {
				const wrapped = config?.streamSimple ? { ...config, streamSimple: wrap(config.streamSimple) } : config;
				return (target.registerProvider as (n: string, c: unknown) => unknown)(name, wrapped);
			};
		},
	});
}
