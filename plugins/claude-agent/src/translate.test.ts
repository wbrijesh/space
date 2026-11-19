import { describe, expect, test } from "bun:test";
import { addUsage, commandOf, contextTokens, promptContent, shortTokens, toolKind, toolResultText, toolTitle, truncate, turnUsage } from "./translate.js";

describe("tools", () => {
	test("kinds: edits are `edit` (the decision model never gets their contents), shell is `execute`", () => {
		expect(["Edit", "Write", "NotebookEdit"].map(toolKind)).toEqual(["edit", "edit", "edit"]);
		expect(toolKind("Bash")).toBe("execute");
		expect(toolKind("Read")).toBe("read");
		expect(toolKind("WebFetch")).toBe("fetch");
		expect(toolKind("Task")).toBe("other");
	});

	test("titles name the tool and what it acts on", () => {
		expect(toolTitle("Bash", { command: "ls   -la\n", description: "List" })).toBe("bash: ls -la");
		expect(toolTitle("Edit", { file_path: "/p/src/a.rs", old_string: "x", new_string: "y" })).toBe("edit: /p/src/a.rs");
		expect(toolTitle("WebSearch", { query: "bun test" })).toBe("web search: bun test");
		expect(toolTitle("Task", { description: "Find the parser", prompt: "…" })).toBe("agent: Find the parser");
		expect(toolTitle("Read", {})).toBe("read");
		expect(toolTitle("Bash", { command: "x".repeat(300) }).length).toBe(126);
	});

	test("results as text", () => {
		expect(toolResultText("out")).toBe("out");
		expect(toolResultText([{ type: "text", text: "a" }, { type: "image" }, { type: "text", text: "b" }])).toBe("a\n[image]\nb");
		expect(toolResultText(undefined)).toBe("");
		const long = truncate("y".repeat(10_000), 1000);
		expect(long.length).toBeLessThan(1100);
		expect(long).toContain("characters truncated");
	});
});

describe("prompts", () => {
	test("images go first, each with a line in the text where it was", () => {
		const content = promptContent([
			{ type: "image", data: "AAAA", mimeType: "image/png", uri: "file:///t/a%20b.png" },
			{ type: "resource", resource: { text: "pasted", mimeType: "text/plain", uri: "file:///t/p.txt" } },
			{ type: "text", text: "Look" },
			{ type: "resource_link", name: "x.rs", uri: "file:///p/x.rs" },
			{ type: "image", data: "BBBB", mimeType: "image/tiff" },
		]);
		expect(content).toEqual([
			{ type: "image", source: { type: "base64", media_type: "image/png", data: "AAAA" } },
			{
				type: "text",
				text: "[Attached image 1: /t/a b.png]\n\n[Resource file:///t/p.txt]\npasted\n\nLook\n\n[Referenced resource: x.rs <file:///p/x.rs>]\n\n[Image (image/tiff) omitted: unsupported type]",
			},
		]);
	});

	test("a slash command is a lone text block naming an offered command", () => {
		const names = ["compact", "goal"];
		const text = (t: string) => [{ type: "text" as const, text: t }];
		expect(commandOf(text("/compact"), names)).toEqual({ name: "compact", text: "/compact" });
		expect(commandOf(text("  /goal tests pass \n"), names)).toEqual({ name: "goal", text: "/goal tests pass" });
		expect(commandOf(text("/compacted"), names)).toBeUndefined();
		expect(commandOf(text("/model opus"), names)).toBeUndefined();
		expect(commandOf(text("please /compact"), names)).toBeUndefined();
		expect(commandOf([...text("/compact"), { type: "resource_link", name: "a", uri: "file:///a" }], names)).toBeUndefined();
	});
});

describe("usage", () => {
	test("summed per turn, models kept in order", () => {
		const a = turnUsage({ input_tokens: 2, output_tokens: 5, cache_read_input_tokens: 100, cache_creation_input_tokens: 10 }, "claude-cli/x");
		const b = turnUsage({ input_tokens: 1, output_tokens: 1 }, "claude-cli/y");
		expect(addUsage(addUsage(undefined, a), b)).toEqual({ input: 3, output: 6, cacheRead: 100, cacheWrite: 10, models: ["claude-cli/x", "claude-cli/y"] });
		expect(contextTokens({ input_tokens: 2, output_tokens: 5, cache_read_input_tokens: 100, cache_creation_input_tokens: 10 })).toBe(117);
		expect([999, 9758, 24_170, 1_500_000].map(shortTokens)).toEqual(["999", "9.8k", "24k", "1.5M"]);
	});
});
