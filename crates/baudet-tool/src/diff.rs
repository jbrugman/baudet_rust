
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
    FileHeader,
    HunkHeader,
    Meta,
}

impl DiffLineKind {
    // Classify Git file headers before ordinary added or removed lines.
    pub fn of(line: &str) -> DiffLineKind {
        if line.starts_with("@@") {
            DiffLineKind::HunkHeader
        } else if line.starts_with("--- ") || line.starts_with("+++ ") {
            DiffLineKind::FileHeader
        } else if line.starts_with('+') {
            DiffLineKind::Added
        } else if line.starts_with('-') {
            DiffLineKind::Removed
        } else if line.starts_with(' ') {
            DiffLineKind::Context
        } else {
            DiffLineKind::Meta
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Context(String),
    Add(String),
    Remove(String),
}

pub struct LineDiff;

const MAX_LINES: usize = 2000;

impl LineDiff {
    pub fn diff(old_text: &str, new_text: &str) -> Vec<Op> {
        let a = lines(old_text);
        let b = lines(new_text);
        if a.len() > MAX_LINES || b.len() > MAX_LINES {
            return coarse(&a, &b);
        }

        let n = a.len();
        let m = b.len();
        let mut lcs = vec![vec![0u32; m + 1]; n + 1];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                lcs[i][j] = if a[i] == b[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
            }
        }

        let mut ops = Vec::new();
        let (mut i, mut j) = (0usize, 0usize);
        while i < n && j < m {
            if a[i] == b[j] {
                ops.push(Op::Context(a[i].clone()));
                i += 1;
                j += 1;
            } else if lcs[i + 1][j] >= lcs[i][j + 1] {
                ops.push(Op::Remove(a[i].clone()));
                i += 1;
            } else {
                ops.push(Op::Add(b[j].clone()));
                j += 1;
            }
        }
        while i < n {
            ops.push(Op::Remove(a[i].clone()));
            i += 1;
        }
        while j < m {
            ops.push(Op::Add(b[j].clone()));
            j += 1;
        }
        ops
    }
}

fn coarse(a: &[String], b: &[String]) -> Vec<Op> {
    a.iter().map(|line| Op::Remove(line.clone())).chain(b.iter().map(|line| Op::Add(line.clone()))).collect()
}

fn lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.split('\n').map(str::to_owned).collect()
    }
}

pub struct UnifiedDiff;

impl UnifiedDiff {
    pub fn render(old_name: &str, new_name: &str, ops: &[Op]) -> String {
        let mut out = format!("--- {old_name}\n+++ {new_name}\n");
        for op in ops {
            match op {
                Op::Context(text) => out.push_str(&format!(" {text}\n")),
                Op::Add(text) => out.push_str(&format!("+{text}\n")),
                Op::Remove(text) => out.push_str(&format!("-{text}\n")),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(ops: &[Op]) -> Vec<char> {
        ops.iter()
            .map(|op| match op {
                Op::Context(_) => ' ',
                Op::Add(_) => '+',
                Op::Remove(_) => '-',
            })
            .collect()
    }

    #[test]
    fn it_marks_context_added_and_removed_lines() {
        let ops = LineDiff::diff("a\nb\nc\n", "a\nX\nc\n");
        assert_eq!(kinds(&ops), vec![' ', '-', '+', ' ', ' ']);
        assert_eq!(
            ops,
            vec![
                Op::Context("a".into()),
                Op::Remove("b".into()),
                Op::Add("X".into()),
                Op::Context("c".into()),
                Op::Context(String::new()),
            ]
        );
    }

    #[test]
    fn an_empty_text_has_no_lines_so_adding_is_all_adds() {
        assert!(LineDiff::diff("", "nieuwe regel\n").iter().all(|op| matches!(op, Op::Add(_))));
    }

    #[test]
    fn a_trailing_newline_is_a_real_line_and_shows_as_removed() {
        let ops = LineDiff::diff("a\n", "a");
        assert_eq!(kinds(&ops), vec![' ', '-']);
    }

    #[test]
    fn pure_addition_keeps_the_shared_head_and_tail_as_context() {
        let ops = LineDiff::diff("kop\nstaart\n", "kop\nmidden\nstaart\n");
        assert_eq!(kinds(&ops), vec![' ', '+', ' ', ' ']);
    }

    #[test]
    fn identical_text_is_all_context() {
        let ops = LineDiff::diff("gelijk\n", "gelijk\n");
        assert!(ops.iter().all(|op| matches!(op, Op::Context(_))));
    }

    #[test]
    fn it_falls_back_to_a_whole_block_past_the_line_cap() {
        let big_old = "x\n".repeat(MAX_LINES + 1);
        let ops = LineDiff::diff(&big_old, "y\n");
        let first_add = ops.iter().position(|op| matches!(op, Op::Add(_))).expect("er moet een toevoeging in zitten");
        assert_eq!(first_add, MAX_LINES + 2, "alle oude regels eerst weg, dan pas de nieuwe");
        assert!(ops[..first_add].iter().all(|op| matches!(op, Op::Remove(_))));
    }

    #[test]
    fn the_render_carries_the_header_and_the_three_prefixes() {
        let ops = LineDiff::diff("a\nb\nc\n", "a\nX\nc\n");
        assert_eq!(UnifiedDiff::render("a/x.txt", "b/x.txt", &ops), "--- a/x.txt\n+++ b/x.txt\n a\n-b\n+X\n c\n \n");
    }

    #[test]
    fn a_rendered_line_is_classified_for_the_viewer() {
        assert_eq!(DiffLineKind::of("--- a/x.txt"), DiffLineKind::FileHeader);
        assert_eq!(DiffLineKind::of("+++ b/x.txt"), DiffLineKind::FileHeader);
        assert_eq!(DiffLineKind::of("@@ -1,3 +1,3 @@"), DiffLineKind::HunkHeader);
        assert_eq!(DiffLineKind::of("+X"), DiffLineKind::Added);
        assert_eq!(DiffLineKind::of("-b"), DiffLineKind::Removed);
        assert_eq!(DiffLineKind::of(" c"), DiffLineKind::Context);
        assert_eq!(DiffLineKind::of("diff --git a/x b/x"), DiffLineKind::Meta);
    }
}
