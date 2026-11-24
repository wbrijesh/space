// Parameters of Space's `_space/*` requests, checked (anything malformed is a readable error).

import type { ContentBlock } from "@agentclientprotocol/sdk";
import type { Complete, ConfigureSession, ForkOf, RewindSession, SteerSession, UnsteerSession } from "./generated/protocol.js";

function isObject(v: unknown): v is Record<string, unknown> {
	return !!v && typeof v === "object" && !Array.isArray(v);
}

function str(v: unknown): string | undefined {
	return typeof v === "string" && v.length > 0 ? v : undefined;
}

export function sessionRef(raw: unknown): UnsteerSession {
	const sessionId = str(isObject(raw) ? raw.sessionId : undefined);
	if (!sessionId) throw new Error("sessionId is required");
	return { sessionId };
}

export function parseConfigure(raw: unknown): ConfigureSession {
	const p = isObject(raw) ? raw : {};
	return { ...sessionRef(raw), provider: str(p.provider), model: str(p.model), thinkingLevel: str(p.thinkingLevel) };
}

export function parseComplete(raw: unknown): Complete {
	const p = isObject(raw) ? raw : {};
	const prompt = str(p.prompt);
	if (!prompt) throw new Error("prompt is required");
	return { ...sessionRef(raw), prompt, provider: str(p.provider), model: str(p.model) };
}

/** The session a new or loaded session is a side chat of (`_meta[key]`), if any. */
export function sideOf(meta: unknown, key: string): string | undefined {
	return str(isObject(meta) ? meta[key] : undefined);
}

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** `v` if it's a UUID (Claude Code takes only those as message ids). */
export function uuid(v: unknown): string | undefined {
	return typeof v === "string" && UUID.test(v) ? v : undefined;
}

/** The client's id for a prompt (`_meta[key]`), if it's a UUID. */
export function messageId(meta: unknown, key: string): string | undefined {
	return uuid(isObject(meta) ? meta[key] : undefined);
}

/** The conversation a new session is a copy of (`_meta[key]`), if any. */
export function forkOf(meta: unknown, key: string): ForkOf | undefined {
	const raw = isObject(meta) ? meta[key] : undefined;
	if (raw === undefined) return undefined;
	const p = isObject(raw) ? raw : {};
	const sessionId = str(p.sessionId);
	const messageId = uuid(p.messageId);
	if (!sessionId || !messageId) throw new Error("forkOf needs a sessionId and a messageId (a UUID)");
	return { sessionId, messageId };
}

export function parseRewind(raw: unknown): RewindSession {
	const p = isObject(raw) ? raw : {};
	const messageId = uuid(p.messageId);
	if (!messageId) throw new Error("messageId (a UUID) is required");
	return { ...sessionRef(raw), messageId, restoreFiles: p.restoreFiles === true };
}

export function parseSteer(raw: unknown): SteerSession {
	const p = isObject(raw) ? raw : {};
	const id = str(p.id);
	if (!id || typeof p.text !== "string") throw new Error("id and text are required");
	const records = (v: unknown) => (Array.isArray(v) ? v.filter(isObject) : []);
	const images = records(p.images).flatMap((r) => {
		const data = str(r.data);
		const mimeType = str(r.mimeType);
		return data && mimeType ? [{ data, mimeType, uri: str(r.uri) ?? "" }] : [];
	});
	const texts = records(p.texts).flatMap((r) => {
		const uri = str(r.uri);
		return typeof r.text === "string" && uri ? [{ text: r.text, mimeType: str(r.mimeType) ?? "text/plain", uri }] : [];
	});
	const attachments = (Array.isArray(p.attachments) ? p.attachments : []).filter((a): a is string => typeof a === "string");
	return { ...sessionRef(raw), id, text: p.text, images, texts, attachments };
}

/** A steered message as ACP prompt blocks (as a prompt would come): images, pasted texts, the text, files. */
export function steerBlocks(steer: SteerSession): ContentBlock[] {
	const blocks: ContentBlock[] = (steer.images ?? []).map((i) => ({ type: "image", data: i.data, mimeType: i.mimeType, uri: i.uri || undefined }));
	for (const t of steer.texts ?? []) blocks.push({ type: "resource", resource: { text: t.text, mimeType: t.mimeType, uri: t.uri } });
	blocks.push({ type: "text", text: steer.text });
	for (const uri of steer.attachments ?? []) blocks.push({ type: "resource_link", name: decodeURIComponent(uri.split("/").pop() ?? uri), uri });
	return blocks;
}

