// Tests of the model list the composer's picker shows. Run: bun test (from plugins/pi-agent).

import assert from "node:assert/strict";
import { test } from "node:test";
import { latestModels, modelFamily } from "./status.js";

const ids = [
	"claude-fable-5",
	"claude-fable-5-1",
	"claude-haiku-4-5",
	"claude-haiku-4-5-20251001",
	"claude-haiku-5-5",
	"claude-opus-4-5",
	"claude-opus-4-5-20251101",
	"claude-opus-4-8",
	"claude-opus-5",
	"claude-opus-5-5",
	"claude-sonnet-4-6",
	"claude-sonnet-5",
	"claude-sonnet-5-5",
].map((id) => ({ id }));

test("families", () => {
	assert.equal(modelFamily("claude-opus-5-5"), "claude-opus");
	assert.equal(modelFamily("claude-fable-5"), "claude-fable");
});

test("only the newest of each family, Fable first", () => {
	assert.deepEqual(latestModels(ids).map((m) => m.id), [
		"claude-fable-5-1",
		"claude-opus-5-5",
		"claude-sonnet-5-5",
		"claude-haiku-5-5",
	]);
});

test("an older current model stays listed after its family's newest", () => {
	assert.deepEqual(latestModels(ids, "claude-opus-4-8").map((m) => m.id), [
		"claude-fable-5-1",
		"claude-opus-5-5",
		"claude-opus-4-8",
		"claude-sonnet-5-5",
		"claude-haiku-5-5",
	]);
});
