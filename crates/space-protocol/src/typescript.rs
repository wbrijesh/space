//! TypeScript bindings for TypeScript plugins, generated from the Rust types: the `_space/*`
//! ACP extensions, their method names and [`crate::PROTOCOL_VERSION`].
//!
//! The test below fails while the committed file differs from what the types generate, so the
//! two can't drift apart. `just protocol` rewrites it.

#![cfg(test)]

use ts_rs::{Config, TS};

use crate::ext::*;

const OUT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../plugins/pi-agent/src/generated/protocol.ts");
/// Set to write the file instead of checking it.
const WRITE_ENV: &str = "SPACE_WRITE_PROTOCOL";

fn generate() -> String {
    let cfg = Config::new().with_large_int("number");
    let mut out = String::from(
        "// Generated from crates/space-protocol by `just protocol`. Do not edit: change the Rust types.\n\n",
    );
    let consts: &[(&str, String)] = &[
        ("PROTOCOL_VERSION", crate::PROTOCOL_VERSION.to_string()),
        ("PROTOCOL_VERSION_META_KEY", format!("{PROTOCOL_VERSION_META_KEY:?}")),
        ("STATUS_METHOD", format!("{STATUS_METHOD:?}")),
        ("CONFIGURE_METHOD", format!("{CONFIGURE_METHOD:?}")),
        ("PRE_EXECUTE_METHOD", format!("{PRE_EXECUTE_METHOD:?}")),
        ("CONFINE_METHOD", format!("{CONFINE_METHOD:?}")),
        ("COMPLETE_METHOD", format!("{COMPLETE_METHOD:?}")),
        ("STEER_METHOD", format!("{STEER_METHOD:?}")),
        ("UNSTEER_METHOD", format!("{UNSTEER_METHOD:?}")),
        ("STEERED_METHOD", format!("{STEERED_METHOD:?}")),
        ("RETRY_METHOD", format!("{RETRY_METHOD:?}")),
        ("STEER_META_KEY", format!("{STEER_META_KEY:?}")),
        ("CONTEXT_META_KEY", format!("{CONTEXT_META_KEY:?}")),
        ("DATA_DIR_META_KEY", format!("{DATA_DIR_META_KEY:?}")),
        ("USAGE_META_KEY", format!("{USAGE_META_KEY:?}")),
        ("TRACE_METHOD", format!("{:?}", crate::trace::TRACE_METHOD)),
    ];
    for (name, value) in consts {
        out.push_str(&format!("export const {name} = {value};\n"));
    }
    for decl in [
        item::<ModelInfo>(&cfg),
        item::<SpaceStatus>(&cfg),
        item::<ContinueIn>(&cfg),
        item::<ConfigureSession>(&cfg),
        item::<PreExecute>(&cfg),
        item::<PreExecuteResult>(&cfg),
        item::<Verdict>(&cfg),
        item::<Confine>(&cfg),
        item::<ConfineResult>(&cfg),
        item::<Complete>(&cfg),
        item::<CompleteResult>(&cfg),
        item::<SteerImage>(&cfg),
        item::<SteerText>(&cfg),
        item::<SteerSession>(&cfg),
        item::<SteerResult>(&cfg),
        item::<UnsteerSession>(&cfg),
        item::<UnsteerResult>(&cfg),
        item::<Steered>(&cfg),
        item::<Retry>(&cfg),
        item::<RetryState>(&cfg),
        item::<RetryKind>(&cfg),
        item::<ContextSection>(&cfg),
        item::<TurnUsage>(&cfg),
        item::<crate::trace::TraceBatch>(&cfg),
        item::<crate::trace::TraceEvent>(&cfg),
        item::<crate::trace::HeaderReason>(&cfg),
        item::<crate::trace::ChunkKind>(&cfg),
        item::<crate::trace::GuardVerdict>(&cfg),
        item::<crate::trace::ToolDef>(&cfg),
        item::<crate::trace::RequestOptions>(&cfg),
        item::<crate::trace::ContextFact>(&cfg),
        item::<serde_json::Value>(&cfg),
    ] {
        out.push('\n');
        out.push_str(&decl);
    }
    out
}

/// `export type …`, with the type's doc comment.
fn item<T: TS>(cfg: &Config) -> String {
    format!("{}export {}\n", T::docs().unwrap_or_default(), T::decl(cfg))
}

#[test]
fn typescript_bindings_are_current() {
    let fresh = generate();
    if std::env::var_os(WRITE_ENV).is_some() {
        std::fs::create_dir_all(std::path::Path::new(OUT).parent().unwrap()).unwrap();
        std::fs::write(OUT, &fresh).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(OUT).unwrap_or_default();
    assert!(committed == fresh, "plugins/pi-agent/src/generated/protocol.ts is stale: run `just protocol`");
}
