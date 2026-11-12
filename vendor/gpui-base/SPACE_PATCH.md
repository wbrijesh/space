# Space's patches to gpui-base 0.9.0

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

Markdown rhythm, in `src/text/node.rs` and `src/text/utils.rs` (marked "Space patch"):
headings' gaps follow the body size (`heading_gaps`: about 24px above and 11px below for
levels 1–3, 16px and 7px for 4–6) instead of each heading's own size; list items are 0.4 of
the body size apart (was 0.25); a list item's second paragraph keeps half a body size above
it; and nested ordered lists are numbered at every depth (`list_item_prefix`; it used letters
from the second level). Tests updated to match.

Task lists, in `src/text/node.rs` (`render_list_item_row`): a done task's checkbox is filled
with the list marker color (`with_list_style`'s text color, else the muted foreground), like
its outline, instead of the link color.

Links, in `src/text/style.rs` and `src/text/node.rs`: `TextViewStyle::with_link_underline`
and `with_link_weight`, applied by `apply_link_style` wherever a link is styled (underlined,
at the text's weight, by default).

HTML, in `src/text/format/html.rs`: `<mark>`'s default color is a faint amber
(`default_mark_color`, was a solid pale yellow), and a `<details>` block's `<summary>` is a
bold line (it was a plain paragraph; the block still doesn't collapse). Other unknown inline
tags (`<kbd>`, `<sub>`, `<sup>`) already render as their text.

Images, in `src/text/node.rs` (`Paragraph::render`): at most 560px wide (GPUI derives an
image's height from its width, so there's no height cap), rounded, a bordered placeholder
while loading and the alt text in a box if loading fails; an image without a link opens
itself when clicked (`image_open_url`: a local path as `file://`). Which images load at all
is the app's `image_source` resolver.

Footnotes, in `src/text/format/markdown.rs` (`footnote_label`): a numbered footnote's
reference and note read as superscript digits ("claim¹", "¹ The source") instead of italic
"[1]"; other labels keep their brackets. Test: `footnotes_are_superscript_numbers`.

Selection end, in `src/text_selection.rs`: `TextSelection::end_caret` returns where a finished
selection ends on screen (the caret box after its last character, from the edges participants
paint for touch handles), so the app can float a button there. It counts a finished window
selection, or without one a participant's local selection (a `TextView`'s double or triple click).

`SelectableText`, in `src/selectable_text.rs`: registers, projects the selection, then registers
again with its painted ends; `in_paint_order` orders it among `TextView`s; `highlights` and
`layout` for find.

Editor gutter, in `src/input/base/element.rs`: `LINE_NUMBER_RIGHT_MARGIN` is 14px (was 6px), so
line numbers don't run into the code (the Files view's code preview). A 1px line in the border
color divides the numbers from the code, 6px left of the code; indent guides are drawn at 0.4 of
the border color (was 0.85).

Read-only inputs, in `src/input/base/state.rs` (`show_cursor`) and `element.rs`: no caret and no
current-line highlight, so the Files view's code reads as a document; selecting still works.

Line labels and backgrounds, in `state.rs` and `element.rs`: `set_line_labels` replaces the line
numbers with given labels (the diff's old and new numbers; the gutter is sized to the longest),
and `set_line_backgrounds` tints given lines across the text and the gutter (added and removed).

Selection redraw, in `src/text_selection.rs` (`set_snapshot`): a participant whose selection
changes notifies, so whatever painted it draws again. A plain `SelectableText` (the user's
message bubbles) had no listener, so a drag showed nothing until mouse-up. Test:
`a_drag_redraws_the_window_as_it_goes`.

To update gpui-base, copy the new version here and re-apply these changes.
