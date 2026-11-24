// Where a copy of the conversation goes up to, for going back to an earlier message.

import { describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { type Entry, forkPoint, parseEntries, readEntries } from "./rewind.js";

const user = (uuid: string, parentUuid: string | null): Entry => ({ uuid, parentUuid, type: "user" });
const entry = (uuid: string, parentUuid: string | null, type = "assistant"): Entry => ({ uuid, parentUuid, type });

describe("fork point", () => {
	const chain: Entry[] = [user("p1", null), entry("a1", "p1"), entry("x1", "a1", "attachment"), user("p2", "x1"), entry("a2", "p2"), user("p3", "a2"), entry("a3", "p3")];

	test("the entry before the message, whatever it is", () => {
		expect(forkPoint(chain, "p2")).toEqual({ kind: "resume", at: "x1" });
		expect(forkPoint(chain, "p3")).toEqual({ kind: "resume", at: "a2" });
	});

	test("the first message: a new conversation", () => {
		expect(forkPoint(chain, "p1")).toEqual({ kind: "fresh" });
	});

	test("an unknown message, or an assistant's uuid: not there", () => {
		expect(forkPoint(chain, "nope")).toBeUndefined();
		expect(forkPoint(chain, "a2")).toBeUndefined();
	});

	test("a message that went to a running turn is its queued-command attachment", () => {
		const steered: Entry = { uuid: "q1", parentUuid: "t1", type: "attachment", attachment: { source_uuid: "s1" } };
		const entries = [user("p1", null), entry("t1", "p1"), steered, entry("a1", "q1")];
		expect(forkPoint(entries, "s1")).toEqual({ kind: "resume", at: "t1" });
	});

	test("before a compaction, which a resume can't reach: a copy of the transcript", () => {
		const compacted = [...chain, entry("b", null, "system"), user("summary", "b"), user("p4", "summary"), entry("a4", "p4")];
		expect(forkPoint(compacted, "p2")).toEqual({ kind: "copy", upTo: "x1" });
		expect(forkPoint(compacted, "p4")).toEqual({ kind: "resume", at: "summary" });
		expect(forkPoint(compacted, "p1")).toEqual({ kind: "fresh" });
	});

	test("a copy made by forkSession() knows the messages by their old uuids", () => {
		const copy: Entry[] = [
			{ uuid: "n1", parentUuid: null, type: "user", forkedFrom: { messageUuid: "p1" } },
			{ uuid: "n2", parentUuid: "n1", type: "assistant", forkedFrom: { messageUuid: "a1" } },
			{ uuid: "n3", parentUuid: "n2", type: "user", forkedFrom: { messageUuid: "p2" } },
		];
		expect(forkPoint(copy, "p2")).toEqual({ kind: "resume", at: "n2" });
	});

	test("a subagent's entries don't count", () => {
		const entries = [...chain, { ...user("side", "a3"), isSidechain: true }];
		expect(forkPoint(entries, "p3")).toEqual({ kind: "resume", at: "a2" });
	});
});

describe("transcripts", () => {
	test("only chained entries are read; a broken line is skipped", () => {
		const text = [
			JSON.stringify({ type: "user", uuid: "p1", parentUuid: null, message: {} }),
			JSON.stringify({ type: "file-history-snapshot", messageId: "p1" }),
			"{not json",
			JSON.stringify({ type: "attachment", uuid: "q", parentUuid: "p1", attachment: { type: "queued_command", source_uuid: "s" } }),
		].join("\n");
		expect(parseEntries(text)).toEqual([user("p1", null), { uuid: "q", parentUuid: "p1", type: "attachment", attachment: { source_uuid: "s" } }]);
	});

	test("found under Claude Code's folder for the project, or any that has it", () => {
		const config = mkdtempSync(join(tmpdir(), "claude-config-"));
		const before = process.env.CLAUDE_CONFIG_DIR;
		process.env.CLAUDE_CONFIG_DIR = config;
		try {
			const line = `${JSON.stringify({ type: "user", uuid: "p1", parentUuid: null })}\n`;
			mkdirSync(join(config, "projects", "-work-my-project"), { recursive: true });
			writeFileSync(join(config, "projects", "-work-my-project", "s1.jsonl"), line);
			expect(readEntries("s1", "/work/my.project")).toEqual([user("p1", null)]);
			mkdirSync(join(config, "projects", "elsewhere"), { recursive: true });
			writeFileSync(join(config, "projects", "elsewhere", "s2.jsonl"), line);
			expect(readEntries("s2", "/work/my.project")).toEqual([user("p1", null)]);
			expect(readEntries("s3", "/work/my.project")).toEqual([]);
		} finally {
			if (before === undefined) delete process.env.CLAUDE_CONFIG_DIR;
			else process.env.CLAUDE_CONFIG_DIR = before;
		}
	});
});
