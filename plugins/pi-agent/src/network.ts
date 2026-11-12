// What a failed model request means, and whether Claude can be reached: the parts of
// resilient.ts that touch the outside world, kept small so they can be replaced in tests.

import { connect as tcpConnect } from "node:net";
import { connect as tlsConnect } from "node:tls";

/** fatal: retrying can't help; busy: Claude is overloaded or limiting us; network: the connection
 * failed; unknown: anything else. */
export type FailureKind = "fatal" | "busy" | "network" | "unknown";

const FATAL =
	/usage limit|extra usage|usage credits|quota|billing|insufficient|unauthori[sz]ed|authentication|oauth|log ?in|sign ?in|api key|forbidden|\b40[0134]\b|invalid[_ ]request|prompt is too long|too many tokens|context (?:length|window)|maximum context|not installed|permission denied/i;
const BUSY = /overloaded|\b529\b|\b429\b|rate.?limit|too many requests|capacity|server_busy/i;
const NETWORK =
	/\bE(?:CONN[A-Z]+|TIMEDOUT|NETUNREACH|NETDOWN|HOSTUNREACH|HOSTDOWN|PIPE|NOTFOUND|AI_AGAIN|ADDRNOTAVAIL)\b|connection (?:dropped|reset|closed|aborted|lost|error|refused|failed)|fetch failed|socket|network|timed? ?out|timeout|stream ended|ended without|other side closed|premature close|terminated|\b50[0234]\b|server error|internal error/i;

export function classify(message: string): FailureKind {
	if (FATAL.test(message)) return "fatal";
	if (BUSY.test(message)) return "busy";
	if (NETWORK.test(message)) return "network";
	return "unknown";
}

/** The `ECONNRESET`-style code in an error message, for a short reason. */
export function errorCode(message: string): string | undefined {
	return /\bE[A-Z]{3,}\b/.exec(message)?.[0];
}

const API_HOST = "api.anthropic.com";
const PROBE_TIMEOUT_MS = 5000;

/** The proxy requests go through, if one is set (`https_proxy` and friends). */
function proxy(): { host: string; port: number } | undefined {
	const url = process.env.HTTPS_PROXY || process.env.https_proxy || process.env.ALL_PROXY || process.env.all_proxy;
	if (!url) return undefined;
	try {
		const u = new URL(url);
		return { host: u.hostname, port: Number(u.port) || (u.protocol === "https:" ? 443 : 80) };
	} catch {
		return undefined;
	}
}

/** Whether Claude's API can be reached now: a TLS handshake with it (so a captive portal or a
 * broken DNS counts as unreachable), or a TCP connection to the proxy when one is set. */
export function probe(): Promise<boolean> {
	return new Promise((resolve) => {
		const via = proxy();
		const socket = via
			? tcpConnect({ host: via.host, port: via.port })
			: tlsConnect({ host: API_HOST, port: 443, servername: API_HOST });
		let settled = false;
		const done = (ok: boolean) => {
			if (settled) return;
			settled = true;
			clearTimeout(timer);
			socket.destroy();
			resolve(ok);
		};
		const timer = setTimeout(() => done(false), PROBE_TIMEOUT_MS);
		socket.once(via ? "connect" : "secureConnect", () => done(true));
		socket.once("error", () => done(false));
	});
}
