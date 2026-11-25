use std::path::{Component, Path, PathBuf};

use git2::{DiffFindOptions, DiffFlags, DiffFormat, DiffOptions, Repository, Status, StatusOptions};

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

// Inspect local repositories in-process. No application filters are registered with
// libgit2, so Git's repository-configured command filters are never invoked.
fn open_worktree(cwd: &str) -> Option<Repository> {
    if !Path::new(cwd).is_dir() {
        return None;
    }
    let repo = Repository::discover(cwd).ok()?;
    repo.workdir()?;
    Some(repo)
}

fn project_prefix(repo: &Repository, cwd: &str) -> Option<PathBuf> {
    let root = repo.workdir()?.canonicalize().ok()?;
    let project = Path::new(cwd).canonicalize().ok()?;
    Some(project.strip_prefix(root).ok()?.to_owned())
}

pub fn probe_git(cwd: &str) -> Option<GitMeta> {
    let repo = open_worktree(cwd)?;
    let head = repo.head().ok();
    Some(GitMeta {
        repository_url: repo
            .config()
            .ok()
            .and_then(|config| config.get_string("remote.origin.url").ok())
            .and_then(non_empty_trimmed),
        branch: head.as_ref().filter(|head| head.is_branch()).and_then(|head| head.shorthand()).map(str::to_owned),
        commit_hash: head.as_ref().and_then(|head| head.target()).map(|id| id.to_string()),
    })
}

pub fn probe_changes(cwd: &str) -> Option<Vec<ChangedFile>> {
    let repo = open_worktree(cwd)?;
    let prefix = project_prefix(&repo, cwd)?;
    let mut options = StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true)
        .disable_pathspec_match(true);
    if !prefix.as_os_str().is_empty() {
        options.pathspec(&prefix);
    }
    let statuses = repo.statuses(Some(&mut options)).ok()?;
    let mut diff_options = DiffOptions::new();
    diff_options.disable_pathspec_match(true);
    if !prefix.as_os_str().is_empty() {
        diff_options.pathspec(&prefix);
    }
    let counts = change_counts(&repo, &mut diff_options).unwrap_or_default();

    let mut files = Vec::new();
    for entry in statuses.iter() {
        let status = entry.status();
        let rename = status.contains(Status::INDEX_RENAMED).then(|| entry.head_to_index()).flatten();
        let path = rename.as_ref().and_then(|delta| delta.new_file().path()).or_else(|| entry.path().map(Path::new));
        let Some(path) = path else { continue };
        let Some(relative) = path.strip_prefix(&prefix).ok().and_then(Path::to_str) else { continue };
        let old_path = rename.as_ref().and_then(|delta| delta.old_file().path());
        let (additions, deletions) =
            counts.get(path).or_else(|| old_path.and_then(|path| counts.get(path))).copied().unwrap_or_default();
        files.push(ChangedFile {
            path: relative.to_owned(),
            additions,
            deletions,
            change: change_kind(status),
            old_path: old_path
                .and_then(|path| path.strip_prefix(&prefix).ok())
                .and_then(Path::to_str)
                .map(str::to_owned),
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Some(files)
}

fn change_kind(status: Status) -> ChangeKind {
    if status.contains(Status::CONFLICTED) {
        ChangeKind::Unmerged
    } else if status.contains(Status::INDEX_NEW) {
        ChangeKind::Added
    } else if status.contains(Status::INDEX_DELETED) {
        if status.contains(Status::WT_NEW) {
            ChangeKind::Removed
        } else {
            ChangeKind::Deleted
        }
    } else if status.contains(Status::INDEX_RENAMED) {
        ChangeKind::Renamed
    } else if status.intersects(Status::INDEX_MODIFIED | Status::WT_MODIFIED) {
        ChangeKind::Modified
    } else if status.contains(Status::WT_DELETED) {
        ChangeKind::Removed
    } else if status.contains(Status::WT_NEW) {
        ChangeKind::Untracked
    } else {
        ChangeKind::Other
    }
}

type Numstat = std::collections::HashMap<PathBuf, (Option<u32>, Option<u32>)>;

fn change_counts(repo: &Repository, options: &mut DiffOptions) -> Option<Numstat> {
    let tree = repo.head().ok()?.peel_to_tree().ok()?;
    let mut diff = repo.diff_tree_to_workdir_with_index(Some(&tree), Some(options)).ok()?;
    diff.find_similar(Some(DiffFindOptions::new().renames(true))).ok()?;
    let mut counts = Numstat::new();
    for (index, delta) in diff.deltas().enumerate() {
        let Some(path) = delta.new_file().path() else { continue };
        if let Ok(Some(patch)) = git2::Patch::from_diff(&diff, index) {
            if let Ok((_, additions, deletions)) = patch.line_stats() {
                if !patch.delta().flags().contains(DiffFlags::BINARY) {
                    counts.insert(path.to_owned(), (additions.try_into().ok(), deletions.try_into().ok()));
                }
            }
        }
    }
    Some(counts)
}

#[derive(Debug, PartialEq, Eq)]
pub enum FilePatch {
    Diff(String),
    Untracked,
    Unchanged,
}

// Prefer the working-tree patch and fall back to the index.
pub fn probe_patch(cwd: &str, path: &str, old_path: Option<&str>) -> Option<FilePatch> {
    let literal_path = |path: &str| {
        !path.is_empty()
            && !path.contains('\0')
            && Path::new(path).components().all(|part| matches!(part, Component::Normal(_)))
    };
    if !literal_path(path) || old_path.is_some_and(|path| !literal_path(path)) {
        return None;
    }

    const PATCH_LIMIT: usize = 1_000_000;
    let repo = open_worktree(cwd)?;
    let prefix = project_prefix(&repo, cwd)?;
    let path = prefix.join(path);
    let mut options = DiffOptions::new();
    options.disable_pathspec_match(true).pathspec(&path);
    if let Some(old_path) = old_path {
        options.pathspec(prefix.join(old_path));
    }
    let tree = match repo.head() {
        Ok(head) => Some(head.peel_to_tree().ok()?),
        Err(error) if error.code() == git2::ErrorCode::UnbornBranch => None,
        Err(_) => return None,
    };
    for staged in [false, true] {
        let mut diff = if staged {
            repo.diff_tree_to_index(tree.as_ref(), None, Some(&mut options))
        } else {
            repo.diff_index_to_workdir(None, Some(&mut options))
        }
        .ok()?;
        diff.find_similar(Some(DiffFindOptions::new().renames(true))).ok()?;
        let mut output = Vec::new();
        diff.print(DiffFormat::Patch, |_, _, line| {
            let origin = line.origin();
            let has_prefix = matches!(origin, '+' | '-' | ' ');
            let content = line.content();
            if output.len() + content.len() + usize::from(has_prefix) > PATCH_LIMIT {
                return false;
            }
            if has_prefix {
                output.push(origin as u8);
            }
            output.extend_from_slice(content);
            true
        })
        .ok()?;
        if !output.is_empty() {
            return Some(FilePatch::Diff(unquote_rename_headers(&String::from_utf8_lossy(&output))));
        }
    }
    let status = repo.status_file(&path).ok();
    Some(if status.is_some_and(|status| status.contains(Status::WT_NEW)) {
        FilePatch::Untracked
    } else {
        FilePatch::Unchanged
    })
}

fn unquote_rename_headers(patch: &str) -> String {
    let mut output = String::with_capacity(patch.len());
    for line in patch.split_inclusive('\n') {
        if let Some((label, path)) = ["rename from ", "rename to "]
            .into_iter()
            .find_map(|label| line.strip_prefix(label).map(|path| (label, path)))
        {
            let decoded = unquote_git_path(path.trim_end_matches('\n'));
            // Match Git's core.quotePath=false without putting control characters in headers.
            if !decoded.contains(|c: char| c.is_control() || c == '"' || c == '\\') {
                output.push_str(label);
                output.push_str(&decoded);
                output.push('\n');
                continue;
            }
        }
        output.push_str(line);
    }
    output
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

fn non_empty_trimmed(output: String) -> Option<String> {
    let trimmed = output.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
fn raw(cwd: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
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
    fn local_inspection_ignores_repository_command_filters() {
        let folder = scratch().expect("Git fixture");
        let cwd = folder.path().to_str().unwrap();
        let repo = Repository::open(folder.path()).unwrap();
        let mut config = repo.config().unwrap();
        config.set_str("filter.fixture.clean", "baudet-unavailable-test-filter").unwrap();
        config.set_str("filter.fixture.process", "baudet-unavailable-test-filter").unwrap();
        config.set_bool("filter.fixture.required", true).unwrap();
        std::fs::write(folder.path().join(".gitattributes"), "f.txt filter=fixture\n").unwrap();

        let changes = probe_changes(cwd).unwrap();
        let file = changes.iter().find(|file| file.path == "f.txt").unwrap();
        assert_eq!((file.change, file.additions, file.deletions), (ChangeKind::Modified, Some(2), Some(1)));
        let Some(FilePatch::Diff(patch)) = probe_patch(cwd, "f.txt", None) else {
            panic!("expected a patch with the fixture filter configured");
        };
        assert!(patch.contains("-b\n+B\n"));
    }

    #[test]
    fn an_oversized_patch_is_unavailable() {
        let folder = scratch().expect("Git fixture");
        let cwd = folder.path().to_str().unwrap();
        std::fs::write(folder.path().join("f.txt"), "long line\n".repeat(120_000)).unwrap();
        assert_eq!(probe_patch(cwd, "f.txt", None), None);
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
