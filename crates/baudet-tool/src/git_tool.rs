
use crate::command_signature;
use crate::git_command::{self, GitWrites};
use crate::process_runner::ProcessRunner;
use crate::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use std::time::Duration;

const TIMEOUT_SECONDS: u64 = 15;

#[derive(Debug, Clone, Copy)]
pub struct GitTool {
    writes: GitWrites,
}

impl Default for GitTool {
    fn default() -> Self {
        Self::read_only()
    }
}

impl GitTool {
    pub fn read_only() -> Self {
        Self { writes: GitWrites::Disabled }
    }

    pub fn with_writes() -> Self {
        Self { writes: GitWrites::Enabled }
    }

    pub fn writes_allowed(&self) -> bool {
        self.writes == GitWrites::Enabled
    }
}

impl Tool for GitTool {
    fn name(&self) -> &str {
        "git"
    }

    fn description(&self) -> &str {
        if self.writes_allowed() {
            "Run a git command in the project repository. Read-only commands: status, diff, \
             diff_staged, log, show, branch, ls_files, rev_parse, blame. Write commands: add, \
             commit, checkout, stash, push -- each runs only after the user approves it. For \
             add, pass the paths in args; for commit, pass the message in the message field; for \
             checkout, one branch (optionally with -b to create it); for stash, one of push, pop, \
             apply, save or list; for push, an optional remote and branch. Returns stdout, stderr and the exit code."
        } else {
            "Run a read-only git command in the project repository. Available commands: \
             status, diff, diff_staged, log, show, branch, ls_files, rev_parse, blame. \
             Optional args are extra read-only arguments or paths (for example a file path \
             for diff, or a commit for show). Git writes are disabled for this project. \
             Returns stdout, stderr and the exit code."
        }
    }

    fn parameters(&self) -> Value {
        let mut properties = json!({
            "command": {
                "type": "string",
                "enum": git_command::advertised(self.writes),
                "description": "the git command to run"
            },
            "args": {
                "type": "array",
                "items": { "type": "string" },
                "description": "optional extra arguments or paths for the command"
            }
        });
        if self.writes_allowed() {
            properties["message"] = json!({
                "type": "string",
                "description": "the commit message, for the commit command only"
            });
        }
        json!({ "type": "object", "properties": properties, "required": ["command"] })
    }

    fn can_run_parallel(&self, args: &Value, ctx: &ToolContext) -> bool {
        let Some(command) = string(args.get("command")) else {
            return false;
        };
        if git_command::is_write(command) {
            return false;
        }
        ctx.read_granted
            && git_command::check_allowed(command, &strings(args.get("args")), self.writes, string(args.get("message")))
                .is_none()
    }

    fn execute(&self, args: &Value, ctx: &ToolContext) -> ToolResult {
        if !ctx.read_granted {
            return fail("the project does not grant the Read right");
        }
        let Some(command) = string(args.get("command")).filter(|text| !text.trim().is_empty()) else {
            return fail("needs a command argument");
        };
        let extra = strings(args.get("args"));
        let message = string(args.get("message"));

        if let Some(refusal) = git_command::check_allowed(command, &extra, self.writes, message) {
            return ToolResult::fail(refusal);
        }
        let Some(root) = ctx.root.as_deref() else {
            return fail("no project folder is linked to inspect");
        };

        let argv = git_command::argv(root, command, &extra, message);
        if git_command::is_write(command) {
            if !ctx.write_granted {
                return fail("the project does not grant the Write right");
            }
            let summary = git_command::summary(command, &extra, message);
            let cwd = root.to_string_lossy();
            let pin = command_signature::workspace_pin("git", &cwd, &argv);
            let allowed = ctx.auto_allow();
            let pinned = allowed.contains(&pin) || allowed.contains(&command_signature::pinned(&summary));
            if !pinned && !(ctx.approve_command)(&summary, &cwd, TIMEOUT_SECONDS as u32, &pin) {
                return fail("the user declined this git write");
            }
        }

        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let outcome = match ProcessRunner::run(&refs, root, Duration::from_secs(TIMEOUT_SECONDS)) {
            Ok(outcome) => outcome,
            Err(_) => return fail("the git command could not be started or completed"),
        };

        let head = match outcome.exit_code {
            Some(code) => format!("exit code: {code}"),
            None => format!("git timed out after {TIMEOUT_SECONDS}s (no exit code)"),
        };
        ToolResult {
            ok: !outcome.timed_out && outcome.exit_code == Some(0),
            output: head + &outcome.body(),
            exit_code: outcome.exit_code,
            timed_out: outcome.timed_out,
            duration_ms: outcome.duration_ms,
            cwd: Some(root.to_string_lossy().into_owned()),
            ..Default::default()
        }
    }
}

fn strings(element: Option<&Value>) -> Vec<String> {
    element
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(|item| item.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

fn string(element: Option<&Value>) -> Option<&str> {
    element.and_then(Value::as_str)
}

fn fail(reason: impl Into<String>) -> ToolResult {
    ToolResult::fail(format!("git: {}", reason.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use std::sync::{Arc, Mutex};

    struct Recorder {
        asked: Mutex<Vec<String>>,
        answer: bool,
    }

    fn ctx(root: &Path, read: bool, write: bool, recorder: &Arc<Recorder>) -> ToolContext {
        let shared = Arc::clone(recorder);
        ToolContext {
            root: Some(root.to_path_buf()),
            read_granted: read,
            write_granted: write,
            approve_command: Box::new(move |summary: &str, _cwd: &str, _timeout: u32, _| {
                shared.asked.lock().unwrap().push(summary.to_owned());
                shared.answer
            }),
            ..Default::default()
        }
    }

    fn recorder(answer: bool) -> Arc<Recorder> {
        Arc::new(Recorder { asked: Mutex::new(Vec::new()), answer })
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let run = |argv: &[&str]| {
            let status = Command::new("git")
                .args(argv)
                .current_dir(root)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
                .status()
                .unwrap();
            assert!(status.success(), "git {argv:?}");
        };
        run(&["init", "-q", "-b", "main"]);
        fs::write(root.join("a.txt"), "een\n").unwrap();
        run(&["add", "--", "a.txt"]);
        run(&["commit", "-q", "-m", "eerste"]);
        fs::write(root.join("a.txt"), "twee\n").unwrap();
        dir
    }

    #[test]
    fn it_refuses_without_the_read_right_and_without_a_command() {
        let dir = repo();
        let no_read = GitTool::default().execute(&json!({"command": "status"}), &ToolContext::default());
        assert_eq!(no_read.output, "git: the project does not grant the Read right");

        let granted = ctx(dir.path(), true, false, &recorder(true));
        for args in [json!({}), json!({"command": ""}), json!({"command": "  "})] {
            assert_eq!(GitTool::default().execute(&args, &granted).output, "git: needs a command argument", "{args}");
        }
    }

    #[test]
    fn a_read_verb_needs_no_approval_and_shows_what_git_thinks() {
        let dir = repo();
        let rec = recorder(false);
        let result = GitTool::default().execute(&json!({"command": "status"}), &ctx(dir.path(), true, false, &rec));
        assert!(result.ok, "{}", result.output);
        assert!(result.output.contains("exit code: 0"), "{}", result.output);
        assert!(result.output.contains("## main"), "{}", result.output);
        assert!(result.output.contains(" M a.txt"), "{}", result.output);
        assert!(rec.asked.lock().unwrap().is_empty(), "lezen vraagt nooit");
        assert_eq!(result.cwd.as_deref(), dir.path().to_str());
    }

    #[test]
    fn a_non_zero_git_exit_is_feedback_not_a_failure_of_the_tool() {
        let dir = tempfile::tempdir().unwrap();
        let rec = recorder(true);
        let result = GitTool::default().execute(&json!({"command": "status"}), &ctx(dir.path(), true, false, &rec));
        assert!(!result.ok);
        assert_ne!(result.exit_code, Some(0), "git klaagt over een map zonder repository");
        assert!(result.output.starts_with("exit code: "), "de kop blijft een echte code: {}", result.output);
    }

    #[test]
    fn the_advertised_list_follows_the_setting() {
        let read = GitTool::read_only().parameters();
        let words = read["properties"]["command"]["enum"].as_array().unwrap();
        assert_eq!(words.len(), 9);
        assert!(!words.contains(&json!("commit")));
        assert!(read["properties"].get("message").is_none(), "geen message-veld zonder writes");

        let with = GitTool::with_writes().parameters();
        assert_eq!(with["properties"]["command"]["enum"].as_array().unwrap().len(), 14);
        assert!(with["properties"]["message"].is_object());
        assert_eq!(with["required"].as_array().unwrap(), &vec![json!("command")]);
        assert!(GitTool::read_only().description().contains("read-only git command"));
        assert!(GitTool::with_writes().description().contains("Write commands: add"));
    }

    #[test]
    fn a_write_needs_the_write_right_even_when_writes_are_enabled() {
        let dir = repo();
        let rec = recorder(true);
        let read_only_ctx = ctx(dir.path(), true, false, &rec);
        let result = GitTool::with_writes().execute(&json!({"command": "add", "args": ["a.txt"]}), &read_only_ctx);
        assert_eq!(result.output, "git: the project does not grant the Write right");
        assert!(rec.asked.lock().unwrap().is_empty(), "zonder recht wordt er niet eens gevraagd");
    }

    #[test]
    fn a_write_asks_on_a_readable_summary_and_a_decline_changes_nothing() {
        let dir = repo();
        let rec = recorder(false);
        let result = GitTool::with_writes()
            .execute(&json!({"command": "add", "args": ["a.txt"]}), &ctx(dir.path(), true, true, &rec));
        assert_eq!(result.output, "git: the user declined this git write");
        assert_eq!(rec.asked.lock().unwrap().as_slice(), ["git add -- a.txt".to_owned()]);
        let staged =
            Command::new("git").args(["diff", "--cached", "--name-only"]).current_dir(dir.path()).output().unwrap();
        assert!(String::from_utf8(staged.stdout).unwrap().trim().is_empty(), "geweigerd betekent niets gestaget");
    }

    #[test]
    fn an_approved_add_stages_the_path() {
        let dir = repo();
        let rec = recorder(true);
        let result = GitTool::with_writes()
            .execute(&json!({"command": "add", "args": ["a.txt"]}), &ctx(dir.path(), true, true, &rec));
        assert!(result.ok, "{}", result.output);
        assert_eq!(rec.asked.lock().unwrap().len(), 1);
        let staged =
            Command::new("git").args(["diff", "--cached", "--name-only"]).current_dir(dir.path()).output().unwrap();
        assert_eq!(String::from_utf8(staged.stdout).unwrap().trim(), "a.txt");
    }

    #[test]
    fn a_workspace_pin_only_allows_the_approved_git_arguments_and_folder() {
        let folder = repo();
        let other = repo();
        let tool = GitTool::with_writes();
        let record = recorder(false);
        let argv = git_command::argv(folder.path(), "add", &["a.txt".to_owned()], None);
        let pin = command_signature::workspace_pin("git", &folder.path().to_string_lossy(), &argv);
        let mut approved = ctx(folder.path(), true, true, &record);
        let shared = pin.clone();
        approved.auto_allow_provider = Box::new(move || vec![shared.clone()]);
        let mut another_folder = ctx(other.path(), true, true, &record);
        another_folder.auto_allow_provider = Box::new(move || vec![pin.clone()]);

        let add = json!({"command": "add", "args": ["a.txt"]});
        assert!(tool.execute(&add, &approved).ok);
        assert_eq!(tool.execute(&add, &another_folder).output, "git: the user declined this git write");
        let commit = json!({"command": "commit", "message": "Update README"});
        assert_eq!(tool.execute(&commit, &approved).output, "git: the user declined this git write");
        assert_eq!(record.asked.lock().unwrap().len(), 2);
    }

    #[test]
    fn an_approved_commit_records_the_staged_change() {
        let folder = repo();
        for (key, value) in [("user.name", "Test"), ("user.email", "test@example.invalid")] {
            assert!(Command::new("git").args(["config", key, value])
                .current_dir(folder.path()).status().unwrap().success());
        }
        let record = recorder(true);
        let context = ctx(folder.path(), true, true, &record);
        let tool = GitTool::with_writes();
        assert!(tool.execute(&json!({"command": "add", "args": ["a.txt"]}), &context).ok);
        let committed = tool.execute(&json!({"command": "commit", "message": "Update fixture"}), &context);
        assert!(committed.ok, "{}", committed.output);
        let log = Command::new("git").args(["log", "-1", "--format=%s"]).current_dir(folder.path()).output().unwrap();
        assert_eq!(String::from_utf8(log.stdout).unwrap().trim(), "Update fixture");
        assert_eq!(record.asked.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_pin_from_an_earlier_always_skips_the_prompt() {
        let dir = repo();
        let shared = Arc::new(Mutex::new(Vec::new()));
        let source = Arc::clone(&shared);
        let rec = recorder(false);
        let mut context = ctx(dir.path(), true, true, &rec);
        context.auto_allow_provider = Box::new(move || source.lock().unwrap().clone());

        let args = json!({"command": "add", "args": ["a.txt"]});
        let result = GitTool::with_writes().execute(&args, &context);
        assert_eq!(result.output, "git: the user declined this git write", "onbekend teken vraagt");

        let key = command_signature::pinned(&git_command::summary("add", &["a.txt".to_owned()], None));
        *shared.lock().unwrap() = vec![key];
        let result = GitTool::with_writes().execute(&args, &context);
        assert!(result.ok, "{}", result.output);
        assert_eq!(rec.asked.lock().unwrap().len(), 1, "de tweede keer vraagt niet meer");
    }

    #[test]
    fn a_write_needs_a_commit_message_and_a_refusal_never_reaches_a_process() {
        let dir = repo();
        let rec = recorder(true);
        let context = ctx(dir.path(), true, true, &rec);
        let tool = GitTool::with_writes();

        let no_message = tool.execute(&json!({"command": "commit"}), &context);
        assert_eq!(no_message.output, "git: commit needs a message in the 'message' argument.");
        let amend = tool.execute(&json!({"command": "commit", "message": "x", "args": ["--amend"]}), &context);
        assert!(amend.output.contains("not allowed"), "{amend:?}");
        assert!(rec.asked.lock().unwrap().is_empty(), "een geweigerde vorm opent geen kaart");

        let ok = tool.execute(&json!({"command": "add", "args": ["a.txt"]}), &context);
        assert!(ok.ok);
    }

    #[test]
    fn a_disabled_write_is_refused_with_an_honest_reason() {
        let dir = repo();
        let rec = recorder(true);
        let result = GitTool::read_only()
            .execute(&json!({"command": "commit", "message": "x"}), &ctx(dir.path(), true, true, &rec));
        assert!(result.output.contains("changes the repository"), "{}", result.output);
        assert!(!result.output.contains("run_command"), "{}", result.output);
        assert!(rec.asked.lock().unwrap().is_empty());
    }

    #[test]
    fn fan_out_only_for_a_read_that_would_actually_run() {
        let dir = repo();
        let rec = recorder(true);
        let read = ctx(dir.path(), true, true, &rec);
        assert!(GitTool::default().can_run_parallel(&json!({"command": "status"}), &read));
        assert!(
            !GitTool::default().can_run_parallel(&json!({"command": "add", "args": ["a.txt"]}), &read),
            "een write-woord racet niet, ook al staat writes uit"
        );
        assert!(
            !GitTool::with_writes().can_run_parallel(&json!({"command": "commit", "message": "x"}), &read),
            "een write-woord racet nooit"
        );
        assert!(
            !GitTool::default().can_run_parallel(&json!({"command": "status"}), &ToolContext::default()),
            "zonder Read-recht draait er niets, dus ook geen fan-out"
        );
        assert!(
            !GitTool::default().can_run_parallel(&json!({"command": "reset", "args": ["--hard"]}), &read),
            "een vorm die de poort weigert, mag niet parallel beginnen"
        );
    }

    #[test]
    fn non_string_arguments_are_ignored_like_the_origineler() {
        let dir = repo();
        let rec = recorder(true);
        let result = GitTool::default().execute(
            &json!({"command": "ls_files", "args": ["a.txt", 7, null, {"x": 1}]}),
            &ctx(dir.path(), true, false, &rec),
        );
        assert!(result.ok, "{}", result.output);
        assert!(result.output.contains("a.txt"));
    }
}
