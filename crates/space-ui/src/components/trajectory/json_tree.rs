//! A collapsible JSON tree: objects and arrays open and close, long strings show their first
//! 12 lines until expanded. Its open/closed state is keyed element state.

use std::collections::HashSet;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use serde_json::Value;

use crate::theme::{color, text};

/// Strings longer than this many lines are cut until expanded.
const STRING_LINES: usize = 12;
/// Levels open at first.
const OPEN_DEPTH: usize = 2;

#[derive(Default)]
struct TreeState {
    /// Paths toggled from their default (open above `OPEN_DEPTH`, closed below).
    toggled: HashSet<String>,
    /// Long strings shown in full.
    expanded: HashSet<String>,
}

/// The tree of `value`. `id` keys its open/closed state (use the record's id).
pub fn json_tree(id: impl Into<SharedString>, value: &Value, window: &mut Window, cx: &mut App) -> Div {
    let id: SharedString = id.into();
    let state = window.use_keyed_state(ElementId::Name(format!("json-{id}").into()), cx, |_, _| TreeState::default());
    let (toggled, expanded) = {
        let s = state.read(cx);
        (s.toggled.clone(), s.expanded.clone())
    };
    let mut lines = Vec::new();
    node(&mut lines, &id, &state, &toggled, &expanded, None, value, "$".into(), 0, false);
    div().flex().flex_col().font_family(text::MONO_FONT).text_size(text::CODE).line_height(relative(1.5)).children(lines)
}

#[allow(clippy::too_many_arguments)]
fn node(
    out: &mut Vec<AnyElement>,
    id: &SharedString,
    state: &Entity<TreeState>,
    toggled: &HashSet<String>,
    expanded: &HashSet<String>,
    key: Option<String>,
    value: &Value,
    path: String,
    depth: usize,
    comma: bool,
) {
    let indent = px(depth as f32 * 14.);
    let key_el = |k: &Option<String>| k.as_ref().map(|k| div().text_color(color::blue()).child(format!("{k}: ")));
    let tail = if comma { "," } else { "" };
    match value {
        Value::Object(_) | Value::Array(_) => {
            let (open_b, close_b, len) = match value {
                Value::Object(m) => ("{", "}", m.len()),
                Value::Array(a) => ("[", "]", a.len()),
                _ => unreachable!(),
            };
            let default_open = depth < OPEN_DEPTH;
            let open = default_open != toggled.contains(&path) && len > 0;
            let (st, p) = (state.clone(), path.clone());
            let summary = if open { open_b.to_string() } else { format!("{open_b}…{close_b} {len}{tail}") };
            out.push(
                div()
                    .id(ElementId::Name(format!("{id}-{path}").into()))
                    .pl(indent)
                    .flex()
                    .cursor_pointer()
                    .hover(|s| s.bg(color::hover()))
                    .child(div().w(px(12.)).flex_none().text_color(color::text_3()).child(if len == 0 { " " } else if open { "▾" } else { "▸" }))
                    .children(key_el(&key))
                    .child(div().text_color(color::text_2()).child(summary))
                    .on_click(move |_, window, cx| {
                        st.update(cx, |s, _| {
                            if !s.toggled.remove(&p) {
                                s.toggled.insert(p.clone());
                            }
                        });
                        window.refresh();
                    })
                    .into_any_element(),
            );
            if open {
                match value {
                    Value::Object(m) => {
                        let n = m.len();
                        for (i, (k, v)) in m.iter().enumerate() {
                            node(out, id, state, toggled, expanded, Some(k.clone()), v, format!("{path}.{k}"), depth + 1, i + 1 < n);
                        }
                    }
                    Value::Array(a) => {
                        let n = a.len();
                        for (i, v) in a.iter().enumerate() {
                            node(out, id, state, toggled, expanded, None, v, format!("{path}[{i}]"), depth + 1, i + 1 < n);
                        }
                    }
                    _ => {}
                }
                out.push(div().pl(indent + px(12.)).text_color(color::text_2()).child(format!("{close_b}{tail}")).into_any_element());
            }
        }
        Value::String(s) => {
            let lines: Vec<&str> = s.lines().collect();
            let long = lines.len() > STRING_LINES;
            let full = expanded.contains(&path);
            let shown = if long && !full { lines[..STRING_LINES].join("\n") } else { s.clone() };
            let (st, p) = (state.clone(), path.clone());
            out.push(
                div()
                    .pl(indent + px(12.))
                    .flex()
                    .children(key_el(&key))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().text_color(color::green()).whitespace_normal().child(format!("\"{shown}\"{tail}")))
                            .when(long, |d| {
                                d.child(
                                    div()
                                        .id(ElementId::Name(format!("{id}-{path}-more").into()))
                                        .text_color(color::accent_hover())
                                        .cursor_pointer()
                                        .child(if full { "Show less".to_string() } else { format!("Show all {} lines", lines.len()) })
                                        .on_click(move |_, window, cx| {
                                            st.update(cx, |s, _| {
                                                if !s.expanded.remove(&p) {
                                                    s.expanded.insert(p.clone());
                                                }
                                            });
                                            window.refresh();
                                        }),
                                )
                            }),
                    )
                    .into_any_element(),
            );
        }
        scalar => {
            let c = match scalar {
                Value::Number(_) => color::yellow(),
                Value::Bool(_) => color::violet(),
                _ => color::text_3(),
            };
            out.push(
                div()
                    .pl(indent + px(12.))
                    .flex()
                    .children(key_el(&key))
                    .child(div().text_color(c).child(format!("{scalar}{tail}")))
                    .into_any_element(),
            );
        }
    }
}
