// Going back to an earlier message. Claude Code keeps a session's conversation as a JSONL
// transcript of entries chained by `parentUuid`; going back to just before a message is a copy of
// that chain up to the entry before it (the CLI forks a session with `resumeSessionAt`). The
// files Claude's edits changed since come back with Claude Code's own file checkpoints
// (`rewindFiles`, keyed by the message's uuid), which the session does.
//
// The message is the one Space sent with that uuid: a prompt's entry has it as its `uuid`; a
// message that went to a running turn is an `attachment` (`queued_command`) holding it in
// `source_uuid`; and in a copy made with the SDK's `forkSession()` (for a point before a
// compaction) every entry has a new uuid and the old one in `forkedFrom.messageUuid`.

import { existsSync, readdirSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

type Json = Record<string, unknown>;

function isObject(v: unknown): v is Json {
	return !!v && typeof v === "object" && !Array.isArray(v);
}

/** A transcript entry: what going back needs of it. */
export interface Entry {
	uuid: string;
	parentUuid: string | null;
	type: string;
	isSidechain?: boolean;
	attachment?: { source_uuid?: string };
	forkedFrom?: { messageUuid?: string };
}

/** Where Claude Code keeps its sessions (it reads the same variable). */
function configDir(): string {
	return process.env.CLAUDE_CONFIG_DIR || join(homedir(), ".claude");
}

/**
 * The transcript of session `sessionId` run in `cwd`: Claude Code files it under the folder's
 * path with every other character a `-`; failing that (a long path is shortened), any project
 * folder that has it.
 */
export function transcriptPath(sessionId: string, cwd: string): string | undefined {
	const projects = join(configDir(), "projects");
	const direct = join(projects, cwd.replace(/[^a-zA-Z0-9]/g, "-"), `${sessionId}.jsonl`);
	if (existsSync(direct)) return direct;
	let dirs: string[];
	try {
		dirs = readdirSync(projects);
	} catch {
		return undefined;
	}
	for (const dir of dirs) {
		const path = join(projects, dir, `${sessionId}.jsonl`);
		if (existsSync(path)) return path;
	}
	return undefined;
}

/** The chained entries of a transcript, in file order (lines that aren't entries are skipped). */
export function parseEntries(text: string): Entry[] {
	const out: Entry[] = [];
	for (const line of text.split("\n")) {
		if (!line.trim()) continue;
		let raw: unknown;
		try {
			raw = JSON.parse(line);
		} catch {
			continue;
		}
		if (!isObject(raw) || typeof raw.uuid !== "string" || !("parentUuid" in raw) || typeof raw.type !== "string") continue;
		out.push({
			uuid: raw.uuid,
			parentUuid: typeof raw.parentUuid === "string" ? raw.parentUuid : null,
			type: raw.type,
			...(raw.isSidechain === true ? { isSidechain: true } : {}),
			...(isObject(raw.attachment) && typeof raw.attachment.source_uuid === "string" ? { attachment: { source_uuid: raw.attachment.source_uuid } } : {}),
			...(isObject(raw.forkedFrom) && typeof raw.forkedFrom.messageUuid === "string" ? { forkedFrom: { messageUuid: raw.forkedFrom.messageUuid } } : {}),
		});
	}
	return out;
}

/** Session `sessionId`'s entries; none when there's no transcript. */
export function readEntries(sessionId: string, cwd: string): Entry[] {
	const path = transcriptPath(sessionId, cwd);
	if (!path) return [];
	return parseEntries(readFileSync(path, "utf8"));
}

/** How a copy of the conversation up to just before a message is made. */
export type ForkPoint =
	/** It's the first message: a new, empty conversation. */
	| { kind: "fresh" }
	/** Resume the session up to this entry, as a new session (`resumeSessionAt`). */
	| { kind: "resume"; at: string }
	/** Before the last compaction, which a resume can't reach: copy the transcript up to this entry (`forkSession()`). */
	| { kind: "copy"; upTo: string };

function isMessage(e: Entry, id: string): boolean {
	return (e.uuid === id && e.type === "user") || e.attachment?.source_uuid === id || e.forkedFrom?.messageUuid === id;
}

/**
 * Where to copy session `entries`' conversation for message `id` to be left out with everything
 * after it; `undefined` when the message isn't in it.
 */
export function forkPoint(entries: readonly Entry[], id: string): ForkPoint | undefined {
	const main = entries.filter((e) => !e.isSidechain);
	const byUuid = new Map(main.map((e) => [e.uuid, e]));
	// The conversation a resume continues: from the last entry back to the start, or to the
	// last compaction (whose boundary starts a new chain).
	const chain = new Set<string>();
	for (let e = main.at(-1); e && !chain.has(e.uuid); e = e.parentUuid ? byUuid.get(e.parentUuid) : undefined) chain.add(e.uuid);
	const matches = main.filter((e) => isMessage(e, id));
	const target = matches.find((e) => chain.has(e.uuid)) ?? matches.at(-1);
	if (!target) return undefined;
	if (!target.parentUuid) return { kind: "fresh" };
	return chain.has(target.uuid) ? { kind: "resume", at: target.parentUuid } : { kind: "copy", upTo: target.parentUuid };
}
