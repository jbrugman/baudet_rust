
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GitMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_hash: Option<String>,
}

impl GitMeta {
    pub fn is_empty(&self) -> bool {
        self.repository_url.is_none() && self.branch.is_none() && self.commit_hash.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionContext {
    pub cwd: Option<String>,
    pub git: Option<GitMeta>,
}

impl SessionContext {
    pub fn at(cwd: impl Into<String>) -> SessionContext {
        SessionContext { cwd: Some(cwd.into()), git: None }
    }

    pub fn none() -> SessionContext {
        SessionContext::default()
    }

    pub fn current() -> SessionContext {
        let cwd = std::env::current_dir().ok().and_then(|p| p.to_str().map(|s| s.to_owned())).unwrap_or_default();
        if cwd.is_empty() {
            return SessionContext::default();
        }
        SessionContext { cwd: Some(cwd.clone()), git: probe_git(&cwd) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    pub additions: Option<u32>,
    pub deletions: Option<u32>,
    pub change: ChangeKind,
    pub old_path: Option<String>,
}

impl ChangedFile {
    pub fn name(&self) -> String {
        std::path::Path::new(&self.path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.clone())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Modified,
    Untracked,
    Added,
    Deleted,
    Renamed,
    Copied,
    Unmerged,
    Removed,
    Other,
}

impl ChangeKind {
    pub fn from_xy(x: char, y: char) -> ChangeKind {
        match (x, y) {
            ('?', '?') => ChangeKind::Untracked,
            ('U', _) | (_, 'U') | ('A', 'A') | ('D', 'D') => ChangeKind::Unmerged,
            ('A', _) => ChangeKind::Added,
            ('D', ' ') => ChangeKind::Deleted,
            ('D', _) => ChangeKind::Removed,
            (' ', 'D') => ChangeKind::Removed,
            ('R', _) | (_, 'R') => ChangeKind::Renamed,
            ('C', _) | (_, 'C') => ChangeKind::Copied,
            ('M', _) | (_, 'M') => ChangeKind::Modified,
            _ => ChangeKind::Other,
        }
    }

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

    pub fn exists_on_disk(&self) -> bool {
        !matches!(self, ChangeKind::Deleted | ChangeKind::Removed)
    }
}

pub const GIT_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

pub fn probe_git(cwd: &str) -> Option<GitMeta> {
    if !std::path::Path::new(cwd).is_dir() {
        return None;
    }
    if ask(cwd, &["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true") {
        return None;
    }
    Some(GitMeta {
        repository_url: ask(cwd, &["config", "--get", "remote.origin.url"]),
        branch: ask(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]).filter(|b| b != "HEAD"),
        commit_hash: ask(cwd, &["rev-parse", "HEAD"]),
    })
}

// Return project-relative paths. NUL-delimited Git status itself is repository-relative.
pub fn probe_changes(cwd: &str) -> Option<Vec<ChangedFile>> {
    probe_git(cwd)?;
    let prefix = raw(cwd, &["rev-parse", "--show-prefix"])?;
    let prefix = prefix.trim_end_matches('\n');
    let status = raw(cwd, &["status", "--porcelain=v1", "-z", "--untracked-files=all", "--", "."])?;
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
        // NUL-delimited rename records contain the destination before the source.
        let old_path = if matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C') { records.next() } else { None };
        let repo_path = &record[3..];
        let Some(path) = repo_path.strip_prefix(prefix).filter(|p| !p.is_empty()) else { continue };
        let (additions, deletions) = counts.get(old_path.unwrap_or(repo_path)).copied().unwrap_or((None, None));
        let old_path = old_path.and_then(|path| path.strip_prefix(prefix)).map(str::to_owned);
        files.push(ChangedFile { path: path.to_owned(), additions, deletions, change, old_path });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Some(files)
}

#[derive(Debug, PartialEq, Eq)]
pub enum FilePatch {
    Diff(String),
    Untracked,
    Unchanged,
}

// Prefer the working-tree patch and fall back to the index.
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
    // Check again at open time because status may predate the new file.
    let untracked = raw(cwd, &["--literal-pathspecs", "ls-files", "--others", "--exclude-standard", "--", path])?;
    Some(if untracked.is_empty() { FilePatch::Unchanged } else { FilePatch::Untracked })
}

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
                i += 1 + taken;
                continue;
            }
            _ => out.push(b'\\'),
        }
        i += 2;
    }
    String::from_utf8_lossy(&out).into_owned()
}

type Numstat = std::collections::HashMap<String, (Option<u32>, Option<u32>)>;

fn parse_numstat(output: &str) -> Option<Numstat> {
    let mut map = std::collections::HashMap::new();
    for line in output.lines() {
        let mut fields = line.split('\t');
        let (Some(add), Some(del), Some(path)) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        map.insert(unquote_git_path(path), (add.parse::<u32>().ok(), del.parse::<u32>().ok()));
    }
    Some(map)
}

fn ask(cwd: &str, args: &[&str]) -> Option<String> {
    raw(cwd, args).and_then(non_empty_trimmed)
}

fn non_empty_trimmed(output: String) -> Option<String> {
    let trimmed = output.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn raw(cwd: &str, args: &[&str]) -> Option<String> {
    raw_with_limit(cwd, args, None)
}

fn raw_with_limit(cwd: &str, args: &[&str], limit: Option<u64>) -> Option<String> {
    let child = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_PAGER", "cat")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    wait_output(child, limit)
}

// Drain stdout concurrently so Git cannot block on a full pipe.
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
            _ => None,
        },
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
        assert_eq!(gone.change, ChangeKind::Removed);
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
        if !clean_repo_setup() {
            return;
        }
        let dir = CLEAN_DIR.with(|cell| cell.borrow_mut().take()).expect("setup promises a folder");
        assert_eq!(probe_changes(&dir.path().to_string_lossy()), Some(Vec::new()));
    }

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
        let found = probe_changes(&dir.path().to_string_lossy()).expect("an unborn HEAD is still a repo");
        assert!(found.iter().any(|f| f.path == "a.txt"));
        assert_eq!(found.iter().find(|f| f.path == "a.txt").unwrap().additions, None);
    }
}
