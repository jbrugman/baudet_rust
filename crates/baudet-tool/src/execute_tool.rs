
use crate::command_signature;
use crate::execute_workspace::ExecuteWorkspace;
use crate::git_tool::GitTool;
use crate::process_runner::ProcessRunner;
use crate::safe_commands;
use crate::tool::{Tool, ToolContext, ToolResult};
use baudet_core::item::ItemKind;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_TIMEOUT_SECONDS: u64 = 120;
const MAX_TIMEOUT_SECONDS: u64 = 600;

#[derive(Debug, Default)]
pub struct ExecuteTool {
    workspace: bool,
    git: GitTool,
}

impl ExecuteTool {
    pub fn for_project(git: GitTool) -> Self {
        Self { workspace: true, git }
    }
}

impl Tool for ExecuteTool {
    fn name(&self) -> &str {
        "run_command"
    }

    fn item_kind(&self) -> ItemKind {
        ItemKind::CommandExecution
    }

    fn description(&self) -> &str {
        if self.workspace {
            #[cfg(windows)]
            return "Run a Windows cmd.exe command in the isolated project copy. Successful changes are written to the project. Returns stdout, stderr and the exit code.";
            #[cfg(not(windows))]
            return "Run a shell command within the project sandbox and its granted permissions. Returns stdout, stderr and the exit code.";
        }
        "Run a shell command (bash; python or other interpreters may be invoked from it) \
         with the project folder as its working directory. The command runs only \
         after the user approves it. Scripts written to the system temp folder are \
         allowed. Returns stdout, stderr and the exit code."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "the shell command to run in the project folder" },
                "timeout_seconds": {
                    "type": "integer",
                    "description": "optional run timeout in seconds (default 120, max 600)"
                }
            },
            "required": ["command"]
        })
    }

    fn can_run_parallel(&self, args: &Value, ctx: &ToolContext) -> bool {
        !cfg!(target_os = "windows") && ctx.auto_approve_safe && args.get("command").and_then(Value::as_str).is_some_and(|command| {
            safe_commands::is_safe(command) && crate::git_tool::shell_git_calls(command).is_none()
        })
    }

    fn execute(&self, args: &Value, ctx: &ToolContext) -> ToolResult {
        if !ctx.exec_granted {
            return fail("the project does not grant the Execute right");
        }
        let Some(command) = args.get("command").and_then(Value::as_str).filter(|text| !text.trim().is_empty()) else {
            return fail("needs a command argument");
        };
        if let Some(result) = self.git.execute_shell(command, ctx) {
            return result;
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        if self.workspace {
            return fail("workspace isolation is unavailable on this platform");
        }
        let Some(root) = ctx.root.as_deref() else {
            return fail("no project folder is linked to run in");
        };
        let Some(cwd) = ExecuteWorkspace::project_dir(root) else {
            return fail("the project folder is not an existing directory");
        };
        let timeout = timeout_arg(args).unwrap_or(DEFAULT_TIMEOUT_SECONDS).clamp(1, MAX_TIMEOUT_SECONDS);
        #[cfg(windows)]
        if self.workspace {
            if !ctx.read_granted || !ctx.write_granted {
                return fail("Windows project shell requires Read, Write and Execute rights");
            }
            let outcome = crate::windows_appcontainer::system_cmd().and_then(|exe| {
                crate::windows_appcontainer::run_project(
                    &cwd, true, &exe, &format!("cmd.exe /d /s /c \"{command}\""), (timeout * 1000) as u32,
                )
            });
            return match outcome {
                Ok(outcome) => command_result(outcome, &cwd, timeout),
                Err(error) => fail(format!("Windows workspace sandbox: {error}")),
            };
        }

        let Some(temp) = ExecuteWorkspace::temp_dir() else {
            return fail("no writable temp folder is available to stage the run");
        };

        if !self.workspace {
            let cwd = cwd.to_string_lossy();
            let pin = command_signature::workspace_pin("shell", &cwd, command);
            let allowed = ctx.auto_allow();
            let trusted = command_signature::pinned(command);
            let family = command_signature::trusted_entry(command);
            let pre_approved = !cfg!(target_os = "windows") && (
                allowed.contains(&pin) || allowed.contains(&trusted)
                || (ctx.auto_approve_safe && safe_commands::is_safe(command))
                || family.as_deref().is_some_and(|key| ctx.trusted_commands().iter().any(|t| t == key))
            );

            if !pre_approved && !(ctx.approve_command)(command, &cwd, timeout as u32, &pin) {
                return fail("the user declined to run this command");
            }
        }

        let private_temp = if self.workspace {
            match tempfile::Builder::new().prefix("baudet-workspace-").tempdir_in(&temp) {
                Ok(dir) => Some(dir),
                Err(_) => return fail("could not create a private workspace temp folder"),
            }
        } else {
            None
        };
        let temp = private_temp.as_ref().map_or(temp.as_path(), |dir| dir.path());
        let script = match stage_script(temp, command) {
            Ok(path) => path,
            Err(reason) => return fail(reason),
        };
        let outcome = if self.workspace {
            #[cfg(target_os = "linux")]
            let script_arg = script.file_name().and_then(|name| name.to_str()).map(|name| format!("/tmp/{name}"));
            #[cfg(not(target_os = "linux"))]
            let script_arg = script.to_str().map(str::to_owned);
            match script_arg {
                Some(script_arg) => run_sandboxed(&["/bin/bash", &script_arg], &cwd, temp, timeout, ctx.read_granted, ctx.write_granted, |_| {}),
                None => Err(command_start_error()),
            }
        } else {
            run_staged(&script, &cwd, temp, timeout).ok_or_else(command_start_error)
        };
        fs::remove_file(&script).ok();
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(reason) => return fail(&reason),
        };

        command_result(outcome, &cwd, timeout)
    }
}

fn command_result(outcome: crate::process_runner::ProcessOutcome, cwd: &Path, timeout: u64) -> ToolResult {
    let head = match outcome.exit_code {
        Some(code) => format!("exit code: {code}"),
        None if outcome.timed_out => format!("run_command timed out after {timeout}s (no exit code)"),
        None => "run_command terminated without an exit code".to_owned(),
    };
    ToolResult {
        ok: !outcome.timed_out && outcome.exit_code == Some(0),
        output: head + &outcome.body(),
        exit_code: outcome.exit_code,
        timed_out: outcome.timed_out,
        duration_ms: outcome.duration_ms,
        cwd: Some(cwd.to_string_lossy().into_owned()),
        ..Default::default()
    }
}

fn command_start_error() -> String {
    "the command could not be started or completed".into()
}

fn stage_script(temp: &Path, command: &str) -> Result<PathBuf, &'static str> {
    let path = tempfile::Builder::new()
        .prefix("baudet-run-")
        .suffix(".sh")
        .tempfile_in(temp)
        .map_err(|_| "the run script could not be staged in the temp folder")?
        .into_temp_path()
        .keep()
        .map_err(|_| "the run script could not be staged in the temp folder")?;
    if !ExecuteWorkspace::within_allowed(&path, &[temp.to_path_buf()]) {
        fs::remove_file(&path).ok();
        return Err("the run script did not stage inside the temp folder");
    }
    if fs::write(&path, format!("#!/bin/bash\n{command}\n")).is_err() {
        fs::remove_file(&path).ok();
        return Err("the run script could not be staged in the temp folder");
    }
    Ok(path)
}

fn run_staged(script: &Path, cwd: &Path, temp: &Path, timeout: u64) -> Option<crate::process_runner::ProcessOutcome> {
    let script_arg = script.to_str()?;
    let argv = ["/bin/bash", script_arg];
    ProcessRunner::run_with(&argv, cwd, Duration::from_secs(timeout), |command| {
        command.env("TMPDIR", temp);
    })
    .ok()
}

#[cfg(target_os = "macos")]
pub(crate) fn run_sandboxed(
    command: &[&str],
    cwd: &Path,
    temp: &Path,
    timeout: u64,
    read_granted: bool,
    write_granted: bool,
    configure: impl FnOnce(&mut std::process::Command),
) -> Result<crate::process_runner::ProcessOutcome, String> {
    let mut profile = String::from(
        r#"(version 1)
(deny default)
(import "dyld-support.sb")
(allow process-exec process-fork sysctl-read)
(allow file-read-metadata)
(allow file-read-data
    (subpath "/System/Library")
    (subpath "/usr/lib")
    (subpath "/usr/share")
    (subpath "/bin")
    (subpath "/usr/bin")
    (literal "/dev/null")
    (literal "/dev/urandom")
    (literal (param "PROGRAM"))
    (subpath (param "TEMP")))
(allow file-write* (subpath (param "TEMP")) (literal "/dev/null"))
"#,
    );
    if read_granted {
        profile.push_str("(allow file-read-data (subpath (param \"PROJECT\")))\n");
    }
    if write_granted {
        profile.push_str("(allow file-write* (subpath (param \"PROJECT\")))\n");
    }
    let cwd_param = cwd.to_str().ok_or_else(command_start_error)?;
    let temp_param = temp.to_str().ok_or_else(command_start_error)?;
    let project_param = format!("PROJECT={cwd_param}");
    let temp_param = format!("TEMP={temp_param}");
    let program_param = format!("PROGRAM={}", command.first().ok_or_else(command_start_error)?);
    let mut argv = vec![
        "/usr/bin/sandbox-exec", "-D", &project_param, "-D", &temp_param, "-D", &program_param, "-p", &profile,
    ];
    argv.extend_from_slice(command);
    ProcessRunner::run_with(&argv, cwd, Duration::from_secs(timeout), |command| {
        command.env_clear().env("PATH", "/usr/bin:/bin").env("HOME", temp).env("TMPDIR", temp);
        configure(command);
    })
    .map_err(|error| format!("could not start the macOS workspace sandbox: {error}"))
}

#[cfg(target_os = "linux")]
pub(crate) fn run_sandboxed(
    command: &[&str],
    cwd: &Path,
    temp: &Path,
    timeout: u64,
    read_granted: bool,
    write_granted: bool,
    configure: impl FnOnce(&mut std::process::Command),
) -> Result<crate::process_runner::ProcessOutcome, String> {
    let (Some(project_path), Some(temp_path)) = (cwd.to_str(), temp.to_str()) else {
        return Err("Linux workspace paths must be valid UTF-8".into());
    };
    if write_granted && !read_granted {
        return Err("Linux workspace shell writes also require project Read permission".into());
    }

    let mut argv = vec![
        "/usr/bin/bwrap",
        "--unshare-user", "--disable-userns", "--unshare-pid", "--unshare-net",
        "--unshare-ipc", "--unshare-uts", "--die-with-parent", "--new-session",
        "--ro-bind", "/usr", "/usr",
        "--ro-bind-try", "/bin", "/bin",
        "--ro-bind-try", "/lib", "/lib",
        "--ro-bind-try", "/lib64", "/lib64",
        "--ro-bind-try", "/etc/ld.so.cache", "/etc/ld.so.cache",
        "--dev", "/dev",
        "--proc", "/proc",
        "--bind", temp_path, "/tmp",
    ];
    if read_granted {
        let mount = if write_granted { "--bind" } else { "--ro-bind" };
        argv.extend([mount, project_path, "/workspace"]);
    } else {
        argv.extend(["--dir", "/workspace"]);
    }
    argv.extend(["--chdir", "/workspace", "--"]);
    argv.extend_from_slice(command);
    ProcessRunner::run_with(&argv, cwd, Duration::from_secs(timeout), |command| {
        command.env_clear().env("PATH", "/usr/bin:/bin").env("HOME", "/tmp").env("TMPDIR", "/tmp");
        configure(command);
    })
    .map_err(|error| format!("could not start the Linux workspace sandbox (install Bubblewrap): {error}"))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn run_sandboxed(
    _command: &[&str],
    _cwd: &Path,
    _temp: &Path,
    _timeout: u64,
    _read_granted: bool,
    _write_granted: bool,
    _configure: impl FnOnce(&mut std::process::Command),
) -> Result<crate::process_runner::ProcessOutcome, String> {
    Err("workspace isolation is unavailable on this platform".into())
}

fn timeout_arg(args: &Value) -> Option<u64> {
    match args.get("timeout_seconds")? {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn fail(reason: impl Into<String>) -> ToolResult {
    ToolResult::fail(format!("run_command: {}", reason.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    struct Recorder {
        asked: Mutex<Vec<(String, String, u64)>>,
        answer: bool,
    }

    fn ctx(root: &std::path::Path, recorder: &Arc<Recorder>, auto_safe: bool) -> ToolContext {
        let shared = Arc::clone(recorder);
        ToolContext {
            root: Some(root.to_path_buf()),
            exec_granted: true,
            auto_approve_safe: auto_safe,
            approve_command: Box::new(move |command: &str, cwd: &str, timeout: u32, _| {
                shared.asked.lock().unwrap().push((command.to_owned(), cwd.to_owned(), u64::from(timeout)));
                shared.answer
            }),
            ..Default::default()
        }
    }

    fn recorder(answer: bool) -> Arc<Recorder> {
        Arc::new(Recorder { asked: Mutex::new(Vec::new()), answer })
    }

    fn run(args: Value, ctx: &ToolContext) -> ToolResult {
        ExecuteTool::default().execute(&args, ctx)
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn given_windows_without_write_access_when_a_project_command_is_requested_then_it_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let approvals = recorder(true);
        let result = ExecuteTool::for_project(GitTool::default())
            .execute(&json!({"command": "echo test"}), &ctx(dir.path(), &approvals, true));
        assert!(!result.ok);
        assert_eq!(result.output, "run_command: Windows project shell requires Read, Write and Execute rights");
        assert!(approvals.asked.lock().unwrap().is_empty());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn given_an_unisolated_windows_workspace_when_a_safe_command_is_requested_then_it_still_needs_approval() {
        let dir = tempfile::tempdir().unwrap();
        let approvals = recorder(false);
        let result = ExecuteTool::default().execute(&json!({"command": "ls"}), &ctx(dir.path(), &approvals, true));
        assert!(!result.ok);
        assert_eq!(approvals.asked.lock().unwrap().len(), 1);
    }

    #[test]
    fn it_refuses_without_the_execute_right_and_without_a_command() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::default();
        let no_right = ExecuteTool::default().execute(&json!({"command": "true"}), &ctx);
        assert_eq!(no_right.output, "run_command: the project does not grant the Execute right");

        let granted = ToolContext { root: Some(dir.path().to_path_buf()), exec_granted: true, ..Default::default() };
        for args in [json!({}), json!({"command": ""}), json!({"command": "   "})] {
            let result = ExecuteTool::default().execute(&args, &granted);
            assert_eq!(result.output, "run_command: needs a command argument", "{args}");
        }
    }

    #[test]
    fn it_refuses_a_folder_that_is_not_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let bestand = dir.path().join("niet-een-map");
        fs::write(&bestand, "x\n").unwrap();
        let rec = recorder(true);
        let mut ctx = ctx(dir.path(), &rec, false);
        ctx.root = Some(bestand);
        assert_eq!(
            ExecuteTool::default().execute(&json!({"command": "true"}), &ctx).output,
            "run_command: the project folder is not an existing directory"
        );
    }

    #[test]
    fn a_decline_runs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("geland");
        let rec = recorder(false);
        let result = run(json!({"command": format!("touch {}", marker.display())}), &ctx(dir.path(), &rec, false));
        assert_eq!(result.output, "run_command: the user declined to run this command");
        assert!(!marker.exists(), "geweigerd betekent niets gedraaid");
        assert_eq!(rec.asked.lock().unwrap().len(), 1);
    }

    #[test]
    fn it_reports_the_exit_code_and_both_streams_and_the_working_directory() {
        let dir = tempfile::tempdir().unwrap();
        let rec = recorder(true);
        let result =
            run(json!({"command": "pwd; echo uit; zoem 2>&1 || echo weg; exit 4"}), &ctx(dir.path(), &rec, false));
        assert!(!result.ok, "een exit code van 4 is geen succes");
        assert_eq!(result.exit_code, Some(4));
        assert!(!result.timed_out);
        assert!(result.output.starts_with("exit code: 4\n--- stdout ---\n"), "{}", result.output);
        assert!(
            result.output.contains(&fs::canonicalize(dir.path()).unwrap().to_string_lossy().into_owned()),
            "de run moet in de projectmap draaien: {}",
            result.output
        );
        assert_eq!(
            result.cwd.as_deref().map(|p| fs::canonicalize(p).unwrap()),
            Some(fs::canonicalize(dir.path()).unwrap())
        );
    }

    #[test]
    fn a_clean_run_is_ok_and_says_exit_zero() {
        let dir = tempfile::tempdir().unwrap();
        let rec = recorder(true);
        let result = run(json!({"command": "echo hallo"}), &ctx(dir.path(), &rec, false));
        assert!(result.ok);
        assert_eq!(result.exit_code, Some(0));
        assert!(result.output.contains("hallo"));
    }

    #[test]
    fn a_read_only_command_needs_no_person_when_the_safe_gate_is_on() {
        let dir = tempfile::tempdir().unwrap();
        let rec = recorder(false);
        let result = run(json!({"command": "ls"}), &ctx(dir.path(), &rec, true));
        assert!(result.ok, "een bewezen read-only commando draait zonder vraag");
        assert!(rec.asked.lock().unwrap().is_empty());
    }

    #[test]
    fn the_same_read_only_command_still_asks_when_the_gate_is_off() {
        let dir = tempfile::tempdir().unwrap();
        let rec = recorder(false);
        let result = run(json!({"command": "ls"}), &ctx(dir.path(), &rec, false));
        assert_eq!(result.output, "run_command: the user declined to run this command");
        assert_eq!(rec.asked.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_command_that_can_write_is_asked_even_when_its_family_is_trusted_as_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let rec = recorder(false);
        let result = run(json!({"command": "rm -r iets"}), &ctx(dir.path(), &rec, true));
        assert_eq!(result.output, "run_command: the user declined to run this command");
        assert_eq!(rec.asked.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_pin_from_an_earlier_always_skips_the_prompt_and_reaches_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let command = "echo gepind";
        let shared = Arc::new(Mutex::new(vec![command_signature::pinned(command)]));
        let source = Arc::clone(&shared);
        let rec = recorder(false);
        let mut context = ctx(dir.path(), &rec, false);
        context.auto_allow_provider = Box::new(move || source.lock().unwrap().clone());
        let result = ExecuteTool::default().execute(&json!({"command": command}), &context);
        assert!(result.ok);
        assert!(result.output.contains("gepind"));
        assert!(rec.asked.lock().unwrap().is_empty());
    }

    #[test]
    fn the_timeout_reaches_the_run_and_is_capped() {
        assert_eq!(timeout_arg(&json!({"timeout_seconds": 5})), Some(5));
        assert_eq!(timeout_arg(&json!({"timeout_seconds": "7"})), Some(7));
        assert_eq!(timeout_arg(&json!({"timeout_seconds": "niet-getald"})), None);
        assert_eq!(timeout_arg(&json!({})), None);
        let capped = timeout_arg(&json!({"timeout_seconds": 10_000})).unwrap().clamp(1, MAX_TIMEOUT_SECONDS);
        assert_eq!(capped, MAX_TIMEOUT_SECONDS);
    }

    #[test]
    fn a_slow_run_is_killed_and_says_so_without_faking_an_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let rec = recorder(true);
        let started = std::time::Instant::now();
        let result = run(json!({"command": "sleep 30", "timeout_seconds": 1}), &ctx(dir.path(), &rec, false));
        assert!(result.timed_out);
        assert!(!result.ok);
        assert_eq!(result.exit_code, None, "een gedoofd proces heeft geen exit code");
        assert!(result.output.starts_with("run_command timed out after 1s (no exit code)"), "{}", result.output);
        assert!(started.elapsed() < std::time::Duration::from_secs(10), "de cap moet echt doden");
    }

    #[test]
    fn it_leaves_no_run_script_behind() {
        let dir = tempfile::tempdir().unwrap();
        let temp = ExecuteWorkspace::temp_dir().unwrap();

        for command in ["echo netjes", "exit 4", "sleep 30"] {
            let rec = recorder(true);
            let mut args = json!({"command": command});
            if command.starts_with("sleep") {
                args["timeout_seconds"] = json!(1);
            }
            let script = stage_script(&temp, command).unwrap();
            let outcome = run_staged(&script, dir.path(), &temp, 1);
            fs::remove_file(&script).ok();
            assert!(!script.exists(), "opruimen hoort bij de run, niet bij het succes: {command}");
            drop(outcome);
            drop(rec);
        }

        let rec = recorder(false);
        run(json!({"command": "echo Geen-Tmp-Run"}), &ctx(dir.path(), &rec, false));
        let achtergebleven: Vec<_> = fs::read_dir(&temp)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry.file_name().to_string_lossy().starts_with("baudet-run-")
                    && entry.path().extension().is_some_and(|ext| ext == "sh")
            })
            .filter(|entry| fs::read_to_string(entry.path()).is_ok_and(|text| text.contains("Geen-Tmp-Run")))
            .collect();
        assert!(achtergebleven.is_empty(), "geen script voor een geweigerde run");
    }

    #[test]
    fn a_script_sees_the_controlled_temp_as_its_tmpdir() {
        let dir = tempfile::tempdir().unwrap();
        let rec = recorder(true);
        let result = run(json!({"command": "echo $TMPDIR"}), &ctx(dir.path(), &rec, true));
        let temp = ExecuteWorkspace::temp_dir().unwrap();
        assert!(result.output.contains(&temp.to_string_lossy().into_owned()), "{}", result.output);
    }

    #[test]
    fn it_declares_the_command_card_and_stays_serial_unless_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let rec = recorder(true);
        let context = ctx(dir.path(), &rec, true);
        assert_eq!(ExecuteTool::default().item_kind(), ItemKind::CommandExecution);
        assert!(
            ExecuteTool::default().can_run_parallel(&json!({"command": "ls"}), &context)
                == !cfg!(target_os = "windows"),
            "zonder isolatie op Windows vraagt ook een leescommando individuele goedkeuring"
        );
        assert!(
            !ExecuteTool::default().can_run_parallel(&json!({"command": "rm -r iets"}), &context),
            "iets dat kan schrijven racet niet"
        );
        assert!(
            !ExecuteTool::default().can_run_parallel(&json!({"command": "ls"}), &ctx(dir.path(), &rec, false)),
            "met de gate uit is er wél een goedkeuring nodig, dus geen fan-out"
        );
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod workspace_tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    #[cfg_attr(target_os = "linux", ignore = "requires Bubblewrap and user namespaces")]
    fn workspace_enforces_project_permissions_without_command_approval() {
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let marker = outside.path().join("private.txt");
        fs::write(&marker, "private").unwrap();
        fs::write(project.path().join("input.txt"), "input").unwrap();
        symlink(&marker, project.path().join("outside-link")).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let mut ctx = ToolContext::for_folder(project.path(), true, true, true);
        ctx.approve_command = Box::new(|_, _, _, _| panic!("workspace command requested approval"));
        let command = format!(
            r#"test "$(cat input.txt)" = input &&
               echo written > output.txt &&
               test -z "${{BAUDET_WORKSPACE_TEST_SECRET+x}}" &&
               test "$HOME" = "$TMPDIR" &&
               /bin/bash -c 'echo temp > "$TMPDIR/child.txt"' &&
               ! /bin/bash -c 'cat outside-link' &&
               ! /bin/bash -c 'echo escaped > outside-link' &&
               ! /bin/bash -c 'echo escaped > "$1"' _ '{}' &&
               ! /bin/bash -c 'echo escaped > /dev/tcp/127.0.0.1/{}'"#,
            marker.display(),
            address.port(),
        );
        let tool = ExecuteTool::for_project(GitTool::read_only());
        let result = tool.execute(&json!({"command": command}), &ctx);
        assert!(result.ok, "{}", result.output);
        assert_eq!(fs::read_to_string(&marker).unwrap(), "private");
        assert_eq!(fs::read_to_string(project.path().join("output.txt")).unwrap(), "written\n");

        ctx.write_granted = false;
        let result = tool.execute(&json!({"command": "echo denied > output.txt"}), &ctx);
        assert!(!result.ok);
        assert_eq!(fs::read_to_string(project.path().join("output.txt")).unwrap(), "written\n");

        ctx.read_granted = false;
        let result = tool.execute(&json!({"command": "cat input.txt"}), &ctx);
        assert!(!result.ok);
    }
}
