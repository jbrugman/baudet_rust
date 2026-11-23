//! Where a conversation happened, as observed when it started.
//!
//! Split from [`crate::session::Msg`] because it belongs to the transcript rather than to any one
//! turn, and kept in core because both the store that records it and the app that reads it need to
//! name it. A transcript of a coding conversation without a folder and a revision is a text about
//! nothing in particular: the answers reference files that may not exist any more.
//!
//! Recorded at open, never inferred at read. A directory that has since moved or been re-cloned
//! still has to say what it was at the time, and reading it live would silently rewrite history
//! the moment a checkout changed branches.

use serde::Deserialize;
use serde::Serialize;

/// The checkout a conversation ran in.
///
/// Field names follow what a harness `session_meta` block actually carries, because that shape is
/// what a real session file has to be able to say. Every field is optional: `git` may be absent
/// from PATH, the folder may not be a work tree, and a repository that does not answer within the
/// probe's timeout yields nothing rather than blocking — so a session is still a session.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GitMeta {
    /// The remote the checkout came from, verbatim from `remote.origin.url`, so an SSH URL stays
    /// an SSH URL. `None` for a repo with no remote, which is what a local experiment looks like.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_url: Option<String>,
    /// The checked-out branch. `None` on a detached HEAD, where there is no branch to name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The commit everything was read against — the only field that identifies state unambiguously,
    /// since a branch moves and this does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_hash: Option<String>,
}

impl GitMeta {
    /// Whether anything was observed at all, which is how callers distinguish "not a repository"
    /// from "a repository we could not read". Both mean no provenance, but only one is worth
    /// telling a user about.
    pub fn is_empty(&self) -> bool {
        self.repository_url.is_none() && self.branch.is_none() && self.commit_hash.is_none()
    }
}

/// The folder and checkout one conversation belongs to.
///
/// Held next to the transcript rather than in the request that answers, because it is the same for
/// every turn: a turn does not move house. `cwd` outlives `git` deliberately — a folder is known
/// even where no git exists.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionContext {
    /// Absolute path of the project folder.
    pub cwd: Option<String>,
    /// The repository `cwd` was inside, when it was inside one.
    pub git: Option<GitMeta>,
}

impl SessionContext {
    /// A context that names a folder, with the repository left to [`probe_git`] — the usual shape
    /// for a UI that knows which project it opened but has not asked git yet.
    pub fn at(cwd: impl Into<String>) -> SessionContext {
        SessionContext { cwd: Some(cwd.into()), git: None }
    }

    /// No project in mind: a conversation about nothing on disk.
    pub fn none() -> SessionContext {
        SessionContext::default()
    }

    /// The folder the process was started in, with its repository probed.
    ///
    /// The only way a UI gets a real cwd without a back-end, and the reason the right-hand panel
    /// can show a real checkout while the transport is still scripted.
    pub fn current() -> SessionContext {
        let cwd = std::env::current_dir().ok().and_then(|p| p.to_str().map(|s| s.to_owned())).unwrap_or_default();
        if cwd.is_empty() {
            return SessionContext::default();
        }
        SessionContext { cwd: Some(cwd.clone()), git: probe_git(&cwd) }
    }
}

/// One path a working tree holds differently from its commit.
///
/// A `changed files` list is the one panel a reader trusts only when its counts are git's own, so
/// the two fields come from `diff --numstat` and nothing else. They stay `Option`: a path can be
/// changed without `numstat` ever reporting a line count for it, and writing 0 there would claim
/// git counted an empty change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    /// Path relative to the project folder, using Git's slash separators.
    pub path: String,
    /// Lines added, when git counted them. `None` for a binary path, for `numstat`'s `-`.
    pub additions: Option<u32>,
    /// Lines removed, with the same absence.
    pub deletions: Option<u32>,
    /// Git's own one-character state, mapped to a word so the UI never has to read porcelain.
    pub change: ChangeKind,
    /// The name a rename or copy came from, which git reports as a separate record. `None` for
    /// every other state, so the panel can print `old -> new` without parsing porcelain itself.
    pub old_path: Option<String>,
}

impl ChangedFile {
    /// The last component of the path: what a panel column shows when it has room for one word.
    /// The full path stays available through `path`, which is what a hover or an "open" wants.
    pub fn name(&self) -> String {
        std::path::Path::new(&self.path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.clone())
    }
}

/// What git says happened to a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// Written to, tracked.
    Modified,
    /// On disk, not in the index yet.
    Untracked,
    /// In the index, absent from the commit.
    Added,
    /// On the disk or in the index, absent now.
    Deleted,
    /// Tracked and renamed; `path` names the new spelling.
    Renamed,
    /// Tracked and copied.
    Copied,
    /// The path is in the working tree while the index is mid-conflict.
    Unmerged,
    /// Tracked and gone from the working tree, with the index untouched: git's `" D"`. Not
    /// `Deleted`, which is git's `"D "` — a deletion that was staged — and not `Unmerged`, which
    /// would claim a conflict in a repository that has none.
    Removed,
    /// A porcelain state this version does not name. Shown rather than guessed at.
    Other,
}

impl ChangeKind {
    /// Git's two status bytes for one path, mapped. Whitespace is significant in porcelain, so
    /// this reads the pair it is given and never a trimmed approximation of it.
    pub fn from_xy(x: char, y: char) -> ChangeKind {
        // A written-on-disk file is `" M"`: the space in the first position says the index is
        // clean, and it is a byte of the format rather than padding to remove. Measured against a
        // repository whose tracked file had been rewritten, which is the common case in a panel
        // that is showing what the user is working on.
        match (x, y) {
            ('?', '?') => ChangeKind::Untracked,
            // The two-letters-both-set merge states come first: the single-letter arms below
            // would otherwise swallow them, since `('D', _)` matches `DD`, which git writes for a
            // path deleted in both trees mid-merge. `UU` is the state measured for a content
            // conflict; `AA` and `DD` are git's other two unmerged states.
            ('U', _) | (_, 'U') | ('A', 'A') | ('D', 'D') => ChangeKind::Unmerged,
            ('A', _) => ChangeKind::Added,
            // Each state letter is read from the column that gives it its meaning. X names the
            // index, so a letter there is a staged change; Y names the working tree, so the
            // trailing space there means the index change is the whole story and a letter means
            // the working tree added something of its own.
            ('D', ' ') => ChangeKind::Deleted,
            ('D', _) => ChangeKind::Removed,
            (' ', 'D') => ChangeKind::Removed,
            ('R', _) | (_, 'R') => ChangeKind::Renamed,
            ('C', _) | (_, 'C') => ChangeKind::Copied,
            ('M', _) | (_, 'M') => ChangeKind::Modified,
            _ => ChangeKind::Other,
        }
    }

    /// The word the panel prints.
    pub fn word(&self) -> &'static str {
        match self {
            ChangeKind::Modified => "modified",
            ChangeKind::Untracked => "new",
            ChangeKind::Added => "staged",
            ChangeKind::Deleted => "deleted",
            ChangeKind::Renamed => "renamed",
            ChangeKind::Copied => "copied",
            ChangeKind::Unmerged => "conflict",
            ChangeKind::Removed => "removed",
            ChangeKind::Other => "changed",
        }
    }

    /// Whether the path still exists on disk. A list that shows a deleted path as though it could
    /// be opened is a list that lies about the folder.
    pub fn exists_on_disk(&self) -> bool {
        // Both deletions name a path that is gone; the difference between them is whether the
        // removal was staged, not whether the file is there.
        !matches!(self, ChangeKind::Deleted | ChangeKind::Removed)
    }
}

/// The longest a git probe may take before the folder is reported as unproven.
///
/// Inherited from Kotlin's `GitInspector`, whose rule is that a hanging git yields null and the
/// caller degrades rather than blocks. A startup that waits on a stalled filesystem is a worse bug
/// than a transcript without a branch name, and the value is generous enough that a healthy repo
/// never notices it.
pub const GIT_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Ask git about `cwd`, degrading to `None` on any of: git missing, not a work tree, or no answer
/// within [`GIT_PROBE_TIMEOUT`].
///
/// Shells out rather than linking libgit2. Three fields do not justify a C toolchain in the graph
/// of a GUI that has to build everywhere, and the shell-out is what Kotlin already does for the
/// same reason. Each query is independent so a repository that answers one and not the others
/// still contributes what it can.
pub fn probe_git(cwd: &str) -> Option<GitMeta> {
    if !std::path::Path::new(cwd).is_dir() {
        return None;
    }
    if ask(cwd, &["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true") {
        return None;
    }
    // Reaching this point means git itself answered `true` to being inside a work tree, which is
    // the repository fact. The three fields below are decoration on top of it, and they fail
    // together in the ordinary case of a repository without a commit — measured: `rev-parse HEAD`
    // and `rev-parse --abbrev-ref HEAD` both exit 128 with `bad revision`, while
    // `--is-inside-work-tree` still prints `true`. Returning `None` there told the panel a fresh
    // repository was not a repository at all, so its files vanished from the change list.
    Some(GitMeta {
        repository_url: ask(cwd, &["config", "--get", "remote.origin.url"]),
        branch: ask(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]).filter(|b| b != "HEAD"),
        commit_hash: ask(cwd, &["rev-parse", "HEAD"]),
    })
}

/// Changes inside `cwd`, with paths relative to that folder. Returns `None` if Git cannot answer.
///
/// `status --porcelain=v1 -z` names the paths and their state, and
/// `diff --numstat HEAD` supplies the counts; the `-z` form is what makes this parseable at all,
/// because git then separates records with NUL and stops quoting non-ASCII paths, so a folder
/// called `café` arrives as `café` rather than as an octal escape a caller would have to undo.
///
/// The two queries disagree routinely, and both shapes are correct. A path deleted on disk shows
/// in `status` and in no `numstat` row at all — there is no diff against nothing to count — and a
/// binary path shows as counted-but-uncountable, `-` in both columns. Neither becomes a zero.
pub fn probe_changes(cwd: &str) -> Option<Vec<ChangedFile>> {
    probe_git(cwd)?;
    let prefix = raw(cwd, &["rev-parse", "--show-prefix"])?;
    let prefix = prefix.trim_end_matches('\n');
    let status = raw(cwd, &["status", "--porcelain=v1", "-z", "--untracked-files=all", "--", "."])?;
    // Counting needs a commit to count against, and before the first commit there is none: measured,
    // `diff --numstat HEAD --` exits 128 with `bad revision 'HEAD'`. A query that cannot be made is
    // not a probe that failed, so paths are still listed and carry git's absence of counts.
    let counts =
        raw(cwd, &["diff", "--no-ext-diff", "--no-textconv", "--no-relative", "--numstat", "HEAD", "--", "."])
            .as_deref()
            .and_then(parse_numstat)
            .unwrap_or_default();
    let mut files = Vec::new();
    let mut records = status.split_terminator('\0');
    while let Some(record) = records.next() {
        let [x, y, b' ', ..] = record.as_bytes() else { continue };
        let change = ChangeKind::from_xy(*x as char, *y as char);
        // In NUL-delimited porcelain Git places the source after the destination.
        let old_path = if matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C') { records.next() } else { None };
        let repo_path = &record[3..];
        let Some(path) = repo_path.strip_prefix(prefix).filter(|p| !p.is_empty()) else { continue };
        let (additions, deletions) = counts.get(old_path.unwrap_or(repo_path)).copied().unwrap_or((None, None));
        let old_path = old_path.and_then(|path| path.strip_prefix(prefix)).map(str::to_owned);
        files.push(ChangedFile { path: path.to_owned(), additions, deletions, change, old_path });
    }
    // Git emits status in path order already; sorting again keeps that true on a repository whose
    // collation differs, which is the only reason it is here.
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Some(files)
}

#[derive(Debug, PartialEq, Eq)]
pub enum FilePatch {
    Diff(String),
    Untracked,
    Unchanged,
}

/// Read one project-relative patch, preferring the working tree to the index.
pub fn probe_patch(cwd: &str, path: &str, old_path: Option<&str>) -> Option<FilePatch> {
    use std::path::{Component, Path};

    let literal_path = |path: &str| {
        !path.is_empty() && Path::new(path).components().all(|part| matches!(part, Component::Normal(_)))
    };
    if !literal_path(path) || old_path.is_some_and(|path| !literal_path(path)) {
        return None;
    }
    const PATCH_LIMIT: u64 = 1_000_000;
    let read_patch = |staged: bool| {
        let mut args = vec!["--literal-pathspecs", "-c", "core.quotePath=false", "diff"];
        args.extend(staged.then_some("--cached"));
        args.extend(["--no-ext-diff", "--no-textconv", "--no-color", "--find-renames", "--", path]);
        args.extend(old_path);
        raw_with_limit(cwd, &args, Some(PATCH_LIMIT))
    };
    for staged in [false, true] {
        let output = read_patch(staged)?;
        if !output.is_empty() {
            return Some(FilePatch::Diff(output));
        }
    }
    // Resolve this at open time so a file created since the last status refresh still has a patch.
    let untracked = raw(cwd, &["--literal-pathspecs", "ls-files", "--others", "--exclude-standard", "--", path])?;
    Some(if untracked.is_empty() { FilePatch::Unchanged } else { FilePatch::Untracked })
}

/// `numstat` rows as a path map. `None` only when the query itself failed, so an empty repository
/// with nothing to count yields an empty map rather than nothing.
/// The path git's `diff --numstat` printed, back to the bytes on disk.
///
/// Measured: `status -z` writes paths verbatim — `"sub/caf\303\251.md"` never appears there, and a
/// name with a space stays plain — while `numstat` quotes any path with a non-ASCII byte as
/// `"sub/caf\303\251.md"`. Those two spellings have to be reconciled before one can look up the
/// other, or every accented path silently loses its counts while plain paths keep theirs.
fn unquote_git_path(path: &str) -> String {
    let Some(rest) = path.strip_prefix('"').and_then(|p| p.strip_suffix('"')) else {
        return path.to_string();
    };
    let bytes = rest.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        // C-style escapes, git's source being `quote_c_style`. Only the forms git emits here are
        // handled; anything else keeps its backslash rather than guessing at a byte.
        let Some(&next) = bytes.get(i + 1) else {
            out.push(bytes[i]);
            i += 1;
            continue;
        };
        match next {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'0' => out.push(0),
            b'\\' => out.push(b'\\'),
            b'"' => out.push(b'"'),
            b if b.is_ascii_digit() => {
                // One to three octal digits, which is how git writes every byte above 0x7f.
                let mut value: u32 = 0;
                let mut taken = 0;
                while taken < 3 {
                    match bytes.get(i + 1 + taken) {
                        Some(d @ b'0'..=b'7') => {
                            value = value * 8 + u32::from(d - b'0');
                            taken += 1;
                        }
                        _ => break,
                    }
                }
                out.push(value as u8);
                // `i` sits on the backslash and the digits start after it, so the escape is
                // consumed as one plus the digits taken; counting the backslash twice decodes
                // `\303\251` into the wrong bytes.
                i += 1 + taken;
                continue;
            }
            _ => out.push(b'\\'),
        }
        i += 2;
    }
    // A path is bytes, not necessarily UTF-8; a name git escaped from invalid UTF-8 stays readable
    // through `from_utf8_lossy` rather than being dropped.
    String::from_utf8_lossy(&out).into_owned()
}

/// Line counts per path, keyed by the path as it appears on disk. A count is `None` when git
/// declined to give one, which `numstat` spells `-`.
type Numstat = std::collections::HashMap<String, (Option<u32>, Option<u32>)>;

fn parse_numstat(output: &str) -> Option<Numstat> {
    let mut map = std::collections::HashMap::new();
    for line in output.lines() {
        let mut fields = line.split('\t');
        let (Some(add), Some(del), Some(path)) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        // A binary path is reported as `-` in both columns. That is git declining to count, which
        // is not the same claim as "zero lines".
        map.insert(unquote_git_path(path), (add.parse::<u32>().ok(), del.parse::<u32>().ok()));
    }
    Some(map)
}

/// One git query in `cwd`, trimmed, or `None` for anything that is not a clean, timely,
/// successful answer.
///
/// The trim is what makes this the right accessor for a single fact — a branch name, a hash, a
/// `true` — where git's trailing newline is noise. It is deliberately *not* what the change list
/// reads through: trimming a NUL-separated porcelain stream destroys the separator between records
/// and shifts every field by one byte, which is the difference between a path and a state letter.
fn ask(cwd: &str, args: &[&str]) -> Option<String> {
    raw(cwd, args).and_then(non_empty_trimmed)
}

/// Git's trailing newline removed, and an answer that is whitespace alone read as no answer.
fn non_empty_trimmed(output: String) -> Option<String> {
    let trimmed = output.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// One git query in `cwd`, verbatim. `None` on a missing git, a non-zero exit, or a stalled child.
///
/// Used where the bytes between records are the data: `status -z` delimits with NUL and `numstat`
/// delimits with tabs, and neither survives a trim of the whole stream.
fn raw(cwd: &str, args: &[&str]) -> Option<String> {
    raw_with_limit(cwd, args, None)
}

fn raw_with_limit(cwd: &str, args: &[&str], limit: Option<u64>) -> Option<String> {
    let child = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        // git's own config is irrelevant to these three facts, and a user's alias or pager must
        // not be able to change or stall them.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_PAGER", "cat")
        .stdout(std::process::Stdio::piped())
        // Inherited stderr would leak "not a git repository" into the app's own console for the
        // entirely normal case of a folder that is not a repo.
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    wait_output(child, limit)
}

/// Collect a child's stdout without ever blocking past the probe's budget.
///
/// `Child::output()` blocks with no deadline, so a git that holds the pipe open would hang app
/// startup — the exact failure Kotlin's `GitInspector` bounds with a reader thread plus a forced
/// kill. Same shape here: stdout drains on a worker, the wait has a timeout, and a child that
/// outlives it is killed. `recv_timeout` rather than a blocking `recv` because the worker can
/// legitimately outlive a stalled child, and the deadline must end the probe either way.
fn wait_output(mut child: std::process::Child, limit: Option<u64>) -> Option<String> {
    use std::io::Read;

    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let output = if let Some(limit) = limit {
            let mut bytes = Vec::new();
            stdout.take(limit + 1).read_to_end(&mut bytes).ok().filter(|&size| size as u64 <= limit)
                .map(|_| String::from_utf8_lossy(&bytes).into_owned())
        } else {
            let mut text = String::new();
            stdout.read_to_string(&mut text).ok().map(|_| text)
        };
        let _ = tx.send(output);
    });
    match rx.recv_timeout(GIT_PROBE_TIMEOUT) {
        Ok(Some(output)) => match child.wait().ok() {
            Some(status) if status.success() => Some(output),
            // Non-zero means "no answer here", which is an ordinary answer from git.
            _ => None,
        },
        // Timed out, or the reader died: kill so no probe leaves a process behind.
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repository only used for provenance probing, so a failed probe cannot be confused with a
    /// missing git binary.
    fn scratch_repo() -> Option<tempfile::TempDir> {
        let dir = tempfile::TempDir::new().ok()?;
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        // An isolated config keeps a user's identity, aliases and hooks out of the probe.
        if !run(&["init", "-q"])
            || !run(&["config", "user.email", "test@example.com"])
            || !run(&["config", "user.name", "Baudet Test"])
        {
            return None;
        }
        std::fs::write(dir.path().join("f.txt"), "inhoud").ok()?;
        run(&["add", "f.txt"]);
        if !run(&["commit", "-q", "-m", "begin"]) {
            return None;
        }
        Some(dir)
    }

    #[test]
    fn a_repository_reports_its_branch_and_commit() {
        let Some(dir) = scratch_repo() else {
            return;
        };
        let meta = probe_git(&dir.path().to_string_lossy()).expect("a probed repository must answer");
        assert!(meta.commit_hash.as_deref().map(|h| h.len()) == Some(40), "a commit hash is 40 hex");
        // The branch name is the one the machine's default gives a fresh repo, whatever it is
        // called there, so the test asserts a name exists rather than assuming which.
        assert!(!meta.branch.as_deref().unwrap_or_default().is_empty());
        assert_eq!(meta.repository_url, None, "a fresh repo has no remote, and nothing invents one");
    }

    #[test]
    fn a_plain_folder_probes_as_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(probe_git(&dir.path().to_string_lossy()), None);
    }

    #[test]
    fn a_folder_that_does_not_exist_probes_as_nothing() {
        assert_eq!(probe_git("/definitely/not/here/baudet"), None);
    }

    #[test]
    fn an_observed_context_keeps_its_folder_even_without_a_repository() {
        let ctx = SessionContext::at("/tmp/plotwerk");
        assert_eq!(ctx.cwd.as_deref(), Some("/tmp/plotwerk"));
        assert!(ctx.git.is_none());
        assert!(SessionContext::none().cwd.is_none());
    }

    #[test]
    fn emptiness_is_distinguished_from_absence() {
        assert!(GitMeta::default().is_empty());
        assert!(!GitMeta { commit_hash: Some("abc".into()), ..Default::default() }.is_empty());
    }
}

#[cfg(test)]
mod changes_tests {
    use super::*;

    /// A repository shaped like the ones the panel has to survive: a written file, a deleted one,
    /// an untracked one, a binary, and a non-ASCII name. Built here rather than assumed, because
    /// the shapes are the thing under test.
    fn scratch() -> Option<tempfile::TempDir> {
        let dir = tempfile::TempDir::new().ok()?;
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        if !run(&["init", "-q"])
            || !run(&["config", "user.email", "t@example.com"])
            || !run(&["config", "user.name", "Baudet Test"])
        {
            return None;
        }
        std::fs::write(dir.path().join("f.txt"), "a\nb\nc\n").ok()?;
        std::fs::write(dir.path().join("bin.dat"), [0u8, 1, 2, 3]).ok()?;
        std::fs::create_dir_all(dir.path().join("sub")).ok()?;
        std::fs::write(dir.path().join("sub").join("g.md"), "x\n").ok()?;
        std::fs::write(dir.path().join("café.txt"), "koffie\n").ok()?;
        run(&["add", "-A"]);
        if !run(&["commit", "-q", "-m", "begin"]) {
            return None;
        }
        std::fs::write(dir.path().join("f.txt"), "a\nB\nc\nd\n").ok()?;
        std::fs::write(dir.path().join("bin.dat"), [0u8, 1, 2, 3, 4, 5]).ok()?;
        std::fs::remove_file(dir.path().join("sub").join("g.md")).ok()?;
        std::fs::write(dir.path().join("brandnew.rs"), "nieu\n").ok()?;
        std::fs::write(dir.path().join("café.txt"), "koffie\nmet melk\n").ok()?;
        Some(dir)
    }

    /// The probe run over a fresh scratch repository, with the folder kept alive until the paths
    /// have been read: a temporary directory removed before the probe answered describes nothing.
    fn probe() -> Vec<ChangedFile> {
        let Some(dir) = scratch() else { return Vec::new() };
        let found = probe_changes(&dir.path().to_string_lossy());
        let found = found.expect("a repository must answer");
        drop(dir);
        found
    }

    #[test]
    fn a_written_file_carries_git_counts() {
        let found = probe();
        if found.is_empty() {
            return;
        }
        let f = found.iter().find(|f| f.path == "f.txt").expect("f.txt must appear");
        assert_eq!(f.change, ChangeKind::Modified);
        assert_eq!(f.additions, Some(2), "measured: two added lines");
        assert_eq!(f.deletions, Some(1), "measured: one removed line");
    }

    #[test]
    fn a_path_removed_on_disk_is_listed_without_invented_counts() {
        let found = probe();
        if found.is_empty() {
            return;
        }
        let gone = found.iter().find(|f| f.path == "sub/g.md").expect("the removed path must appear");
        // Status reports `" D"`, and the truth table says that is not git's staged `"D "` deletion
        // nor a conflict: the path is tracked and simply gone from the working tree.
        assert_eq!(gone.change, ChangeKind::Removed);
        // `diff --numstat HEAD --` does count the removal against the commit — measured as `0\t1` —
        // so it is reported with git's own numbers rather than as an absence. The case with no
        // counts at all is the unborn-HEAD test, and the uncountable one is the binary test.
        assert_eq!(gone.additions, Some(0));
        assert_eq!(gone.deletions, Some(1));
        assert!(!gone.change.exists_on_disk());
    }

    #[test]
    fn a_binary_path_is_listed_but_never_reads_as_an_empty_change() {
        let found = probe();
        if found.is_empty() {
            return;
        }
        let bin = found.iter().find(|f| f.path == "bin.dat").expect("the binary must appear");
        assert_eq!(bin.change, ChangeKind::Modified);
        assert_eq!(bin.additions, None, "git writes `-` for a binary, which is not zero");
        assert_eq!(bin.deletions, None);
    }

    #[test]
    fn an_untracked_path_is_reported_as_new() {
        let found = probe();
        if found.is_empty() {
            return;
        }
        let fresh = found.iter().find(|f| f.path == "brandnew.rs").expect("untracked must appear");
        assert_eq!(fresh.change, ChangeKind::Untracked);
        assert!(fresh.change.exists_on_disk());
    }

    #[test]
    fn a_non_ascii_path_survives_the_probe_unquoted() {
        let found = probe();
        if found.is_empty() {
            return;
        }
        // With `-z` git stops octal-escaping, so the accented name must arrive as itself, and its
        // counts must still be found: `numstat` does quote it, and a lookup keyed on the escaped
        // spelling silently returns nothing while every plain path keeps its numbers.
        let coffee = found.iter().find(|f| f.path == "café.txt").unwrap_or_else(|| {
            panic!(
                "a `café` path must not come back as an escape: {:?}",
                found.iter().map(|f| f.path.as_str()).collect::<Vec<_>>()
            )
        });
        assert_eq!(coffee.change, ChangeKind::Modified);
        assert_eq!(coffee.additions, Some(1), "measured: one line added to café.txt");
        assert_eq!(coffee.deletions, Some(0));
    }

    /// The complete mapping, as measured over the state letters git writes. A content conflict
    /// measures as `UU`; `AA` and `DD` are git's other two unmerged states.
    #[test]
    fn the_measured_status_letters_map_to_their_kinds() {
        use ChangeKind::*;
        let cases = [
            ((' ', 'M'), Modified),
            ((' ', 'D'), Removed),
            (('D', ' '), Deleted),
            (('D', 'D'), Unmerged),
            (('A', ' '), Added),
            (('R', ' '), Renamed),
            (('C', ' '), Copied),
            (('U', 'U'), Unmerged),
            (('A', 'A'), Unmerged),
        ];
        for (pair, want) in cases {
            assert_eq!(ChangeKind::from_xy(pair.0, pair.1), want, "{:?}", pair);
        }
        assert_eq!(Removed.word(), "removed");
        assert!(!Removed.exists_on_disk());
    }

    /// Git quotes a non-ASCII path in `numstat` and prints it verbatim in `status`; the counts only
    /// reach the panel once the two spellings agree. The first case is the measured row
    /// `3\t0\t"sub/caf\303\251.md"`.
    #[test]
    fn a_quoted_numstat_path_becomes_the_path_on_disk() {
        assert_eq!(unquote_git_path("\"sub/caf\\303\\251.md\""), "sub/café.md");
        assert_eq!(unquote_git_path("plain/path.txt"), "plain/path.txt");
        assert_eq!(unquote_git_path("\"a\\tb\""), "a\tb");
    }

    #[test]
    fn a_plain_folder_has_no_changes_to_show() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(probe_changes(&dir.path().to_string_lossy()), None);
    }

    #[test]
    fn patch_reads_the_worktree_before_the_index_and_treats_paths_literally() {
        let folder = scratch().expect("Git fixture");
        let cwd = folder.path().to_str().unwrap();
        let path = "[f].txt";
        std::fs::write(folder.path().join(path), "staged\n").unwrap();
        raw(cwd, &["--literal-pathspecs", "add", "--", path]).expect("stage the literal path");
        let diff = |path| match probe_patch(cwd, path, None).unwrap() {
            FilePatch::Diff(patch) => patch,
            other => panic!("expected a patch for {path}, got {other:?}"),
        };
        let staged = diff(path);
        assert!(staged.contains("+staged\n"));
        std::fs::write(folder.path().join(path), "unstaged\n").unwrap();
        let unstaged = diff(path);
        assert!(unstaged.contains("-staged\n+unstaged\n"));
        assert!(!unstaged.contains("f.txt"));
        assert!(probe_patch(cwd, "../other.txt", None).is_none());
        assert!(diff("bin.dat").contains("Binary files"));
        assert!(diff("sub/g.md").contains("deleted file mode"));
        assert_eq!(probe_patch(cwd, "brandnew.rs", None), Some(FilePatch::Untracked));
    }

    #[test]
    fn a_project_subfolder_gets_its_own_paths_and_both_sides_of_a_rename() {
        let folder = scratch().expect("Git fixture");
        let cwd = folder.path().to_str().unwrap();
        raw(cwd, &["restore", "--", "sub/g.md"]).expect("restore the tracked file");
        raw(cwd, &["mv", "sub/g.md", "sub/naïve.md"]).expect("stage a rename");
        let subfolder = folder.path().join("sub");
        let subfolder = subfolder.to_str().unwrap();
        let changes = probe_changes(subfolder).unwrap();
        assert_eq!(changes.len(), 1, "outside changes are excluded");
        let change = &changes[0];
        assert_eq!(change.change, ChangeKind::Renamed);
        assert_eq!(change.path, "naïve.md");
        assert_eq!(change.old_path.as_deref(), Some("g.md"));
        let Some(FilePatch::Diff(patch)) = probe_patch(subfolder, &change.path, change.old_path.as_deref()) else {
            panic!("expected a rename patch");
        };
        assert!(patch.contains("rename from sub/g.md\nrename to sub/naïve.md\n"), "{patch}");
    }

    #[test]
    fn a_clean_repository_answers_with_an_empty_list() {
        // Built inline rather than through `scratch`, because this case needs a repository with
        // nothing to report and every other fixture deliberately has something.
        if !clean_repo_setup() {
            return;
        }
        let dir = CLEAN_DIR.with(|cell| cell.borrow_mut().take()).expect("setup promises a folder");
        assert_eq!(probe_changes(&dir.path().to_string_lossy()), Some(Vec::new()));
    }

    /// A committed, untouched repository, kept in a thread-local so the test above can probe it
    /// without the folder being removed before the probe has answered.
    fn clean_repo_setup() -> bool {
        let Ok(dir) = tempfile::TempDir::new() else { return false };
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        if !run(&["init", "-q"])
            || !run(&["config", "user.email", "t@example.com"])
            || !run(&["config", "user.name", "Baudet Test"])
        {
            return false;
        }
        if std::fs::write(dir.path().join("only.txt"), "x\n").is_err() {
            return false;
        }
        run(&["add", "-A"]);
        if !run(&["commit", "-q", "-m", "begin"]) {
            return false;
        }
        CLEAN_DIR.with(|d| *d.borrow_mut() = Some(dir));
        true
    }

    thread_local! {
        static CLEAN_DIR: std::cell::RefCell<Option<tempfile::TempDir>> = const {
            std::cell::RefCell::new(None)
        };
    }

    #[test]
    fn a_repository_without_a_commit_is_a_repository() {
        let Ok(dir) = tempfile::TempDir::new() else { return };
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        if !run(&["init", "-q"]) {
            return;
        }
        if std::fs::write(dir.path().join("a.txt"), "x\n").is_err() {
            return;
        }
        // `git rev-parse --is-inside-work-tree`, unlike `rev-parse HEAD`, succeeds before the first
        // commit — measured exit 0 — so an unborn HEAD is a repository and its untracked file is a
        // path the panel should show. It carries no counts because there is no commit to count
        // against, which is git's absence rather than a zero invented here.
        let found = probe_changes(&dir.path().to_string_lossy()).expect("an unborn HEAD is still a repo");
        assert!(found.iter().any(|f| f.path == "a.txt"));
        assert_eq!(found.iter().find(|f| f.path == "a.txt").unwrap().additions, None);
    }
}
