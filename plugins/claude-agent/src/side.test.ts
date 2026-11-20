import { expect, test } from "bun:test";
import { leftOff, mainUpdate, type SavedMessage } from "./side.js";

const msg = (uuid: string, type: string, content: unknown, parent: string | null = null): SavedMessage => ({ uuid, type, parent_tool_use_id: parent, message: { role: type, content } });

const MAIN: SavedMessage[] = [
	msg("u1", "user", "make the tree resizable"),
	msg("a1", "assistant", [{ type: "thinking", thinking: "hmm" }, { type: "text", text: "On it." }, { type: "tool_use", id: "t1", name: "Bash", input: { command: "just test" } }]),
	msg("r1", "user", [{ type: "tool_result", tool_use_id: "t1", content: "254 passed" }]),
	msg("x1", "assistant", [{ type: "text", text: "a subagent's work" }], "t9"),
	msg("a2", "assistant", [{ type: "tool_use", id: "t2", name: "Edit", input: { file_path: "/p/a.rs" } }]),
	msg("r2", "user", [{ type: "tool_result", tool_use_id: "t2", is_error: true, content: [{ type: "text", text: "no such file" }] }]),
	msg("a3", "assistant", [{ type: "text", text: "Done." }]),
];

test("an update has what the main thread said and did after the last message seen", () => {
	const update = mainUpdate(MAIN, "u1");
	expect(update?.through).toBe("a3");
	expect(update?.text).toBe(
		[
			'<main-thread-update through="a3">',
			"Claude: On it.",
			"Claude used bash: just test",
			"  → 254 passed",
			"Claude used edit: /p/a.rs",
			"  failed: no such file",
			"Claude: Done.",
			"</main-thread-update>",
		].join("\n"),
	);
	expect(update?.steps).toBe(6);
	expect(mainUpdate(MAIN, "a3")).toBeUndefined();
	expect(mainUpdate([], undefined)).toBeUndefined();
});

test("from the start, or all of it when the last message seen is gone (compacted)", () => {
	expect(mainUpdate(MAIN, undefined)?.text).toContain("User: make the tree resizable");
	const gone = mainUpdate(MAIN, "zz")?.text ?? "";
	expect(gone).toContain("compacted or started over");
	expect(gone).toContain("User: make the tree resizable");
});

test("a long update keeps the latest steps", () => {
	const many = Array.from({ length: 40 }, (_, i) => msg(`m${i}`, "assistant", [{ type: "text", text: `${i} ${"x".repeat(3000)}` }]));
	const update = mainUpdate(many, undefined);
	expect(update?.text.length).toBeLessThan(62_000);
	expect(update?.text).toContain("earlier steps left out");
	expect(update?.text).toContain("Claude: 39 ");
	expect(update?.steps).toBe(40);
});

test("where a side chat left off: its last update, else where it was copied", () => {
	const copied = [MAIN[0], MAIN[1]] as SavedMessage[];
	expect(leftOff([...copied, msg("s1", "user", "q"), msg("s2", "assistant", [{ type: "text", text: "a" }])], MAIN)).toBe("a1");
	const told = msg("s3", "user", [{ type: "text", text: '<main-thread-update through="r2">\n…\n</main-thread-update>' }, { type: "text", text: "q" }]);
	expect(leftOff([...copied, told, msg("s4", "assistant", [{ type: "text", text: "a" }])], MAIN)).toBe("r2");
	expect(leftOff([], MAIN)).toBeUndefined();
});
