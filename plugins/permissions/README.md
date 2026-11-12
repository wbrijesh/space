# Permissions plugin

A Rust service plugin (`space-permissions`) with the three permission modes the composer offers.
It's `required`: Space doesn't run tools without it.

| Mode | How it's enforced |
|---|---|
| **Read Only** | Commands run under `sandbox-exec` with a profile that denies every file write except `$TMPDIR`, `/dev/null` and the terminal devices; the edit and write tools are denied. No model involved. |
| **Workspace Write** (default) | No sandbox. Plainly safe calls are allowed by rules; every other call is reviewed by the decision model: changes inside the project folder are fine, outside it or sending secrets out is not. |
| **Full access** | No sandbox. The same rules, then the model: anything goes except irreversible deletion of existing work, history rewrites, production changes you didn't ask for, and exfiltration. Needs confirming once per thread. |

Each mode adds one hidden line to the prompt saying which mode it is.

## Rules (`src/rules.rs`)

Before the model, rules allow what is plainly safe, instantly and also while the model can't be
used. They never deny: anything they can't vouch for goes to the model, with what they found.

- **Reading anywhere**: the file tools, or shell lines whose every command only reads (`cat`,
  `grep`, `ls`, `sed` without `-i` or its `w`/`e` commands, `find` without `-exec`/`-delete`,
  read-only `git`, `xargs` of those…, `$(…)` checked the same way). Output may go to `/dev/null` or a
  file in a temporary folder.
- **Edits and writes inside the project** with the file tools (symlinks followed).
- **Never by rules**: other commands, other redirects, `eval`, `bash -c`, heredocs, functions,
  anything that doesn't parse, and anything touching credentials or secrets (`~/.ssh`, `~/.aws`,
  `.npmrc`, `.env`, keys, `printenv`, `security`, `gh auth token`…).

What they found ("Commands run: cd, cat, curl. Reaches the network: curl.") goes to the model with
the call.

## The decision model

OpenAI's Decisions API (`POST https://api.openai.com/v1/decisions`, model `gpt-6-luna`), called directly
over HTTPS (reqwest, rustls on ring; our own request/response types in `src/luna.rs`). One request per
tool call with a choice question `verdict` (allow / deny / ask) and `concern` (the main risk, used as the
reason).

- **Sent:** the mode's policy, the user's messages, earlier tool calls (arguments only), the pending call.
- **Never sent:** file contents (an edit or write becomes its path and size) and tool outputs.
- **Verdicts:** allow needs confidence ≥ 0.6 when it names no risk, else ≥ 0.8; deny ≥ 0.8; under
  that, or a refusal, the user is asked.
- **Size:** history is capped (40 items, ~60 KB, newest first); a request the API finds too long is
  sent again with less history; a pending call with an argument over 16 KB is denied ("too large to
  review": retrying can't help).

## When the model can't be used: fail closed

The plugin keeps one health state (`src/health.rs`):

| State | Cause | Blocker links |
|---|---|---|
| NotSet | no key (or the Keychain wouldn't give it) | Open Settings |
| Checking | a key was just set / Space started | none: quiet, the modes work as usual meanwhile |
| Unreachable | timeout, offline, 5xx, rate limit, unreadable answers | Retry now |
| KeyRejected | 401 | Update key · Retry now |
| NoAccess | 403, endpoint missing for the account | Open Settings · Retry now |
| NoCredit | 429 `insufficient_quota` | Open billing · Retry now |
| Incompatible | 400, model gone, 3 unreadable answers in a row | Retry now |

In every state but Healthy and Checking, Workspace Write and Full access carry a blocker (sending is blocked) and
tool calls are answered `unavailable`, so the host holds them. Blips are retried twice within a review
first. A tiny probe request retries every 5 minutes (or at `Retry-After` if later), at once on "Retry
now" or a new key; any change of state sends `hooks/ready`, so held calls go on after a recovery and
show the current reason otherwise. Switching a paused thread to Read Only lets its held call run there.

A new key is checked with the same probe before it's stored (`settings/validate`): rejected, no access
and no credit aren't saved; when OpenAI can't be reached the user may save anyway.

## Environment (tests and scenarios)

- `SPACE_LUNA_BASE_URL`: another server instead of `https://api.openai.com/v1`.
- `SPACE_GUARD_RETRY_SECS`: the probe interval (default 300).

## The evaluation set (`tests/eval`)

Real calls (anonymised; labelled allow, or ask for the ones that use credentials), dangerous ones
aimed at the rules' gaps, and the labelled set. `cargo test` checks that rules never allow one
labelled deny or ask, and how many false alarms they settle (19 of the 21 real ones). With a key,
`OPENAI_API_KEY=… cargo test -p space-permissions live_evaluation -- --ignored --nocapture` runs
everything through the model as well (`SPACE_EVAL_RULES=0`: without the rules, to compare).

## Tests

- `cargo test -p space-permissions`: health transitions and error classification, request privacy,
  verdict mapping, retries, size limits, the sandbox profile (runs real commands under `sandbox-exec`),
  and `tests/guardrail.rs` end to end with the host, the stub agent and a scripted stand-in for OpenAI.
- With a real key: `OPENAI_API_KEY=… cargo test -p space-permissions labelled -- --ignored --nocapture`
  runs 20 labelled calls against the live API.
