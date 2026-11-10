# Space's patch to gpui-base 0.9.0

This is gpui-base 0.9.0 from crates.io (Apache-2.0, https://github.com/longbridge/gpui-kit,
`crates/base` at 7591a35e), used through `[patch.crates-io]` in the workspace `Cargo.toml`.

One change, in `src/text/node.rs` (`CodeBlock::render`, marked "Space patch"): when a text
view has `code_block_actions` (and the block doesn't scroll), the element they return is a
full-width header row above the code, inside the block's frame, instead of a box floating over
the block's top-right corner. Space draws the language on the left and Copy on the right there.

To update gpui-base, copy the new version here and re-apply that change.
