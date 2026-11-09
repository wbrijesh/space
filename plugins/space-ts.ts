// One executable for every bundled TypeScript plugin: `space-ts <plugin id>` runs that plugin.
// `just build` compiles it with Bun into target/<profile>/space-ts; each plugin's manifest runs
// `${target}/space-ts` with its id. Adding a TypeScript plugin: its folder, plus one line here.
const plugins: Record<string, () => Promise<unknown>> = {
	"pi-agent": () => import("./pi-agent/src/main.ts"),
};

const id = process.argv[2] ?? "";
const load = plugins[id];
if (!load) {
	console.error(`space-ts: unknown plugin "${id}" (known: ${Object.keys(plugins).join(", ")})`);
	process.exit(2);
}
await load();
