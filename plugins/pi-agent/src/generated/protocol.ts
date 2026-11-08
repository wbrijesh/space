// Generated from crates/space-protocol by `just protocol`. Do not edit: change the Rust types.

export const PROTOCOL_VERSION = 1;
export const PROTOCOL_VERSION_META_KEY = "space/protocolVersion";
export const STATUS_METHOD = "_space/status";
export const CONFIGURE_METHOD = "_space/session/configure";
export const PRE_EXECUTE_METHOD = "_space/tools/preExecute";
export const CONFINE_METHOD = "_space/sandbox/confine";
export const COMPLETE_METHOD = "_space/complete";
export const CONTEXT_META_KEY = "space/context";
export const DATA_DIR_META_KEY = "space/dataDir";
export const USAGE_META_KEY = "space/usage";

/**
 * A model the agent can switch a session to.
 */
export type ModelInfo = { provider: string, id: string, 
/**
 * Display name as the provider reports it (e.g. `"Claude Opus 5.5"`).
 */
name?: string, };

/**
 * `_space/status` (agent → client notification, also the response of
 * `_space/session/configure`): what the session is running on and what it could run on.
 *
 * All fields except `sessionId` are optional so an agent can send partial updates; the
 * client merges them.
 */
export type SpaceStatus = { sessionId: string, provider?: string, model?: string, thinkingLevel?: string, 
/**
 * Models the session can be switched to with `_space/session/configure`.
 */
availableModels?: Array<ModelInfo>, 
/**
 * Thinking levels the current model supports (e.g. `["low", "medium", "high"]`).
 */
thinkingLevels?: Array<string>, 
/**
 * Estimated tokens currently in the model context.
 */
contextTokens?: number, 
/**
 * Context window of the current model, in tokens.
 */
contextWindow?: number, };

/**
 * `_space/session/configure` (client → agent request): change a session's model and/or
 * thinking level. Omitted fields stay as they are. `provider` defaults to the session's
 * current provider. The agent answers with the session's full [`SpaceStatus`] after the
 * change (a thinking level the model does not support may be clamped).
 */
export type ConfigureSession = { sessionId: string, provider?: string, model?: string, thinkingLevel?: string, };

/**
 * `_space/tools/preExecute` (agent → client request): may the agent run this tool call now?
 * The client runs its tool pipeline (and may ask the user) before answering, so the request
 * can take as long as the user does. An agent that gets an error must not run the call.
 */
export type PreExecute = { sessionId: string, toolCallId: string, 
/**
 * The agent's tool name (`bash`, `read`, `edit`, ...).
 */
tool: string, 
/**
 * ACP tool kind (`read`, `edit`, `execute`, `search`, `fetch`, `other`, ...).
 */
kind: string, 
/**
 * The tool's arguments as the agent will run them.
 */
args: JsonValue, 
/**
 * Working directory the call runs in, if not the session's.
 */
cwd?: string, };

/**
 * The answer to [`PreExecute`]: `allow` or `deny` (an "ask" has already been put to the user).
 */
export type PreExecuteResult = { decision: Verdict, 
/**
 * Why a call is denied, for the model and the transcript.
 */
reason?: string, };

export type Verdict = "allow" | "deny";

/**
 * `_space/sandbox/confine` (agent → client request): the argv to spawn instead of `argv` for
 * this session (wrapped in a sandbox, or unchanged). An agent that gets an error must not
 * spawn the command.
 */
export type Confine = { sessionId: string, argv: Array<string>, cwd: string, };

export type ConfineResult = { argv: Array<string>, };

/**
 * `_space/complete` (client → agent request): one short, tool-free completion of `prompt` by
 * the model of session `session_id`, or by `provider`/`model` when given (`provider` defaults
 * to the session's). It doesn't touch the session's conversation. The host forwards plugins'
 * `agent/complete` calls here, with the same params.
 */
export type Complete = { sessionId: string, prompt: string, provider?: string, model?: string, };

/**
 * The answer to [`Complete`]: the model's reply as plain text.
 */
export type CompleteResult = { text: string, };

/**
 * Hidden text a plugin adds to a prompt (the agent puts it in its system prompt, never in
 * the user's message or the transcript).
 */
export type ContextSection = { 
/**
 * Stable id (the agent may use it as a prompt-section name).
 */
id: string, text: string, };

/**
 * Tokens a turn used, summed over every model request it made (retries and tool steps
 * included). The input buckets don't overlap: the whole prompt is `input + cacheRead +
 * cacheWrite`. A bucket is left out when the provider didn't report it.
 */
export type TurnUsage = { 
/**
 * Prompt tokens read without the cache.
 */
input: number, output: number, cacheRead?: number, cacheWrite?: number, 
/**
 * The part of `output` spent reasoning.
 */
reasoning?: number, 
/**
 * The models that answered (`provider/model`), in order, without repeats.
 */
models: Array<string>, };

export type JsonValue = number | string | boolean | Array<JsonValue> | { [key in string]: JsonValue } | null;
