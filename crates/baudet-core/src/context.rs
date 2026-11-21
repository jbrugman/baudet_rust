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
    let meta = GitMeta {
        repository_url: ask(cwd, &["config", "--get", "remote.origin.url"]),
        branch: ask(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]).filter(|b| b != "HEAD"),
        commit_hash: ask(cwd, &["rev-parse", "HEAD"]),
    };
    if meta.is_empty() {
        None
    } else {
        Some(meta)
    }
}

/// One git query in `cwd`, or `None` for anything that is not a clean, timely, successful answer.
fn ask(cwd: &str, args: &[&str]) -> Option<String> {
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
    wait_output(child)
}

/// Collect a child's stdout without ever blocking past the probe's budget.
///
/// `Child::output()` blocks with no deadline, so a git that holds the pipe open would hang app
/// startup — the exact failure Kotlin's `GitInspector` bounds with a reader thread plus a forced
/// kill. Same shape here: stdout drains on a worker, the wait has a timeout, and a child that
/// outlives it is killed. `recv_timeout` rather than a blocking `recv` because the worker can
/// legitimately outlive a stalled child, and the deadline must end the probe either way.
fn wait_output(mut child: std::process::Child) -> Option<String> {
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buffer = String::new();
        let read = std::io::Read::read_to_string(&mut stdout, &mut buffer);
        let _ = tx.send(read.ok().map(|_| buffer));
    });
    match rx.recv_timeout(GIT_PROBE_TIMEOUT) {
        Ok(Some(output)) => match child.wait().ok() {
            Some(status) if status.success() => {
                let trimmed = output.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_string())
            }
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
