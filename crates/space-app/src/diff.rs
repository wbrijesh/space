//! A changed file against git's HEAD, as rows for the read-only viewer: unchanged, added and
//! removed lines with their old and new numbers, the changed words of an edited line, and
//! (showing only the changes) how many unchanged lines sit between them.

use std::ops::Range;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use similar::{ChangeTag, DiffOp, TextDiff};

/// Unchanged lines kept around a change, showing only the changes.
pub const CONTEXT: usize = 3;
/// A slower diff gives up and treats the rest as replaced.
const TIMEOUT: Duration = Duration::from_millis(500);
/// Lines changed less than this (0 to 1) don't mark their changed words: they're rewrites.
const WORDS_MIN_RATIO: f32 = 0.4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Same,
    Added,
    Removed,
    /// Unchanged lines left out (`Row::skipped` of them).
    Skipped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub kind: Kind,
    /// 1-based numbers in the old and new file.
    pub old: Option<usize>,
    pub new: Option<usize>,
    /// The line, without its newline.
    pub text: String,
    /// The changed words of an edited line (byte ranges in `text`).
    pub words: Vec<Range<usize>>,
    pub skipped: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diff {
    pub rows: Vec<Row>,
    pub added: usize,
    pub removed: usize,
}

/// The file as committed in HEAD; `None` when git doesn't have it there (new, or not a repo).
pub fn head_text(path: &Path) -> Option<String> {
    let (dir, name) = (path.parent()?, path.file_name()?.to_str()?);
    let out = Command::new("git").arg("-C").arg(dir).arg("show").arg(format!("HEAD:./{name}")).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `old` to `new` as rows: every line (`context: None`), or only the changes with `context`
/// unchanged lines around each.
pub fn diff(old: &str, new: &str, context: Option<usize>) -> Diff {
    let lines = TextDiff::configure().timeout(TIMEOUT).diff_lines(old, new);
    let groups = match context {
        Some(n) => lines.grouped_ops(n),
        None => vec![lines.ops().to_vec()],
    };
    let (old_lines, new_lines): (Vec<&str>, Vec<&str>) = (lines.old_slices().to_vec(), lines.new_slices().to_vec());
    let mut out = Diff::default();
    let mut next_old = 0;
    for group in groups {
        let Some(first) = group.first() else { continue };
        let start = first.old_range().start;
        if start > next_old {
            out.rows.push(skipped(start - next_old));
        }
        for op in &group {
            next_old = op.old_range().end;
            push_op(&mut out, op, &old_lines, &new_lines);
        }
    }
    if context.is_some() && next_old < old_lines.len() && out.rows.iter().any(|r| r.kind != Kind::Same) {
        out.rows.push(skipped(old_lines.len() - next_old));
    }
    out
}

fn skipped(n: usize) -> Row {
    let text = if n == 1 { "1 unchanged line".to_string() } else { format!("{n} unchanged lines") };
    Row { kind: Kind::Skipped, old: None, new: None, text, words: Vec::new(), skipped: n }
}

fn line(text: &str) -> String {
    text.trim_end_matches('\n').trim_end_matches('\r').to_string()
}

fn push_op(out: &mut Diff, op: &DiffOp, old: &[&str], new: &[&str]) {
    let row = |kind, o: Option<usize>, n: Option<usize>, text: &str| Row { kind, old: o.map(|i| i + 1), new: n.map(|i| i + 1), text: line(text), words: Vec::new(), skipped: 0 };
    let (o, n) = (op.old_range(), op.new_range());
    match *op {
        DiffOp::Equal { .. } => {
            for (i, j) in o.zip(n) {
                out.rows.push(row(Kind::Same, Some(i), Some(j), old[i]));
            }
        }
        DiffOp::Delete { .. } => {
            for i in o {
                out.rows.push(row(Kind::Removed, Some(i), None, old[i]));
                out.removed += 1;
            }
        }
        DiffOp::Insert { .. } => {
            for j in n {
                out.rows.push(row(Kind::Added, None, Some(j), new[j]));
                out.added += 1;
            }
        }
        DiffOp::Replace { .. } => {
            // Removed lines, then added ones; an edited line's pair marks its changed words.
            let first_removed = out.rows.len();
            for i in o.clone() {
                out.rows.push(row(Kind::Removed, Some(i), None, old[i]));
            }
            let first_added = out.rows.len();
            for j in n.clone() {
                out.rows.push(row(Kind::Added, None, Some(j), new[j]));
            }
            for k in 0..o.len().min(n.len()) {
                let (a, b) = (first_removed + k, first_added + k);
                if let Some((wa, wb)) = changed_words(&out.rows[a].text, &out.rows[b].text) {
                    out.rows[a].words = wa;
                    out.rows[b].words = wb;
                }
            }
            out.removed += o.len();
            out.added += n.len();
        }
    }
}

/// Byte ranges in a line.
type Ranges = Vec<Range<usize>>;

/// The words that differ between an old and a new line, when they're alike enough.
fn changed_words(old: &str, new: &str) -> Option<(Ranges, Ranges)> {
    let words = TextDiff::configure().timeout(TIMEOUT).diff_unicode_words(old, new);
    if words.ratio() < WORDS_MIN_RATIO {
        return None;
    }
    let (mut a, mut b, mut at_a, mut at_b) = (Vec::new(), Vec::new(), 0, 0);
    for change in words.iter_all_changes() {
        let len = change.value().len();
        match change.tag() {
            ChangeTag::Equal => {
                at_a += len;
                at_b += len;
            }
            ChangeTag::Delete => {
                extend(&mut a, at_a..at_a + len);
                at_a += len;
            }
            ChangeTag::Insert => {
                extend(&mut b, at_b..at_b + len);
                at_b += len;
            }
        }
    }
    Some((a, b))
}

/// Add `r`, joining it to the last range when they touch.
fn extend(ranges: &mut Vec<Range<usize>>, r: Range<usize>) {
    match ranges.last_mut() {
        Some(last) if last.end == r.start => last.end = r.end,
        _ => ranges.push(r),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(d: &Diff) -> Vec<(Kind, Option<usize>, Option<usize>)> {
        d.rows.iter().map(|r| (r.kind, r.old, r.new)).collect()
    }

    #[test]
    fn full_file_keeps_every_line_and_numbers_both_sides() {
        let d = diff("a\nb\nc\n", "a\nB\nc\nd\n", None);
        use Kind::*;
        assert_eq!(kinds(&d), vec![(Same, Some(1), Some(1)), (Removed, Some(2), None), (Added, None, Some(2)), (Same, Some(3), Some(3)), (Added, None, Some(4))]);
        assert_eq!((d.added, d.removed), (2, 1));
    }

    #[test]
    fn changes_only_skip_unchanged_lines_between_and_after() {
        let old: String = (1..=20).map(|i| format!("{i}\n")).collect();
        let new = old.replace("10\n", "ten\n");
        let d = diff(&old, &new, Some(2));
        let rows: Vec<String> = d.rows.iter().map(|r| r.text.clone()).collect();
        assert_eq!(rows, ["7 unchanged lines", "8", "9", "10", "ten", "11", "12", "8 unchanged lines"]);
        assert_eq!(d.rows[0].skipped, 7);
    }

    #[test]
    fn an_edited_line_marks_its_changed_words() {
        let d = diff("let x = foo(1);\n", "let x = bar(1);\n", None);
        let (removed, added) = (&d.rows[0], &d.rows[1]);
        assert_eq!(&removed.text[removed.words[0].clone()], "foo");
        assert_eq!(&added.text[added.words[0].clone()], "bar");
    }

    #[test]
    fn a_rewritten_line_marks_no_words() {
        let d = diff("alpha beta gamma\n", "one two three four\n", None);
        assert!(d.rows.iter().all(|r| r.words.is_empty()));
    }

    #[test]
    fn a_new_file_is_all_added() {
        let d = diff("", "one\ntwo", Some(CONTEXT));
        assert_eq!(d.rows.iter().map(|r| r.kind).collect::<Vec<_>>(), [Kind::Added, Kind::Added]);
        assert_eq!(d.rows[1].text, "two");
    }

    #[test]
    fn same_text_has_no_rows_to_show_as_changes() {
        let d = diff("a\nb\n", "a\nb\n", Some(CONTEXT));
        assert!(d.rows.iter().all(|r| r.kind == Kind::Same));
        assert_eq!((d.added, d.removed), (0, 0));
    }
}
