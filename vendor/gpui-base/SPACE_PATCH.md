# Space's patch to gpui-base 0.9.0

This is gpui-base 0.9.0 from crates.io (Apache-2.0, https://github.com/longbridge/gpui-kit,
`crates/base` at 7591a35e), used through `[patch.crates-io]` in the workspace `Cargo.toml`.

One change, in `src/text/node.rs` (`CodeBlock::render`, marked "Space patch"): when a text
view has `code_block_actions` (and the block doesn't scroll), the element they return is a
full-width header row above the code, inside the block's frame, instead of a box floating over
the block's top-right corner. Space draws the language on the left and Copy on the right there.

Also, in `src/text/text_view.rs`, `TextViewDefaults::style` is public (it was `pub(crate)`),
so Space can adjust the installed style (code blocks' background and border).

Code wrapping, in `src/text/inline_flow.rs` and `src/text/node.rs` (marked "Space patch"): a
code block with that header is laid out by `InlineFlow` in a new code mode (`InlineFlow::code`)
instead of a plain `Inline`. Plain text wraps inside gpui-pre's `StyledText`, which breaks at
Unicode line-break opportunities (after `-` and `/`) and never indents wrapped rows (gpui-pre's
`LineWrapper` computes a `next_indent`, but its wrapped-line layout doesn't use it). In code
mode, `line_ranges` hands each line to `code_rows`, which breaks only after spaces and tabs (a
token wider than a row breaks at the last grapheme that fits) and indents continuation rows by
the line's own indentation plus two spaces; `layout_measured_flow` starts each row at its
indent. The rows cover the line exactly, and a selection maps back to the block's own text, so
copying gives the original code. Tests: `code_wrap_tests` in `inline_flow.rs`.

To update gpui-base, copy the new version here and re-apply that change.
