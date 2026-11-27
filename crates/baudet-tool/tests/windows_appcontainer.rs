#![cfg(windows)]

use baudet_tool::windows_appcontainer;
use baudet_tool::windows_workspace::Workspace;
use baudet_tool::{
    execute_tool::ExecuteTool,
    git_tool::GitTool,
    tool::{Tool, ToolContext},
};
use serde_json::json;

#[test]
fn given_an_appcontainer_with_no_capabilities_when_a_native_process_runs_then_it_has_an_appcontainer_token() {
    let system_directory = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
    let status = windows_appcontainer::run(
        &system_directory.join("cmd.exe"),
        &system_directory,
        "cmd.exe /d /c echo stdout& echo stderr>&2& exit 23",
        10_000,
    )
    .unwrap();

    assert_eq!(status.exit_code, Some(23));
    assert_eq!(status.stdout.text.trim_end(), "stdout");
    assert_eq!(status.stderr.text.trim_end(), "stderr");
}

#[test]
fn given_windows_project_rights_when_a_command_changes_multiple_entries_then_all_changes_reach_the_project() {
    let original = tempfile::tempdir().unwrap();
    std::fs::write(original.path().join("old.txt"), b"old").unwrap();
    std::fs::write(original.path().join("remove.txt"), b"remove").unwrap();
    let context = ToolContext {
        root: Some(original.path().to_path_buf()),
        read_granted: true,
        write_granted: true,
        exec_granted: true,
        ..Default::default()
    };
    let result = ExecuteTool::for_project(GitTool::read_only()).execute(
        &json!({"command": "echo changed>old.txt && mkdir sub && echo new>sub\\new.txt && del remove.txt && echo finished"}),
        &context,
    );
    assert!(result.ok, "{}", result.output);
    assert!(result.output.contains("finished"));
    assert_eq!(std::fs::read(original.path().join("old.txt")).unwrap(), b"changed \r\n");
    assert_eq!(std::fs::read(original.path().join("sub/new.txt")).unwrap(), b"new \r\n");
    assert!(!original.path().join("remove.txt").exists());
}

#[test]
fn given_project_execute_rights_when_a_command_targets_a_private_file_outside_then_access_is_denied() {
    let project = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let file = outside.path().join("outside.txt");
    std::fs::write(&file, b"outside fixture").unwrap();
    let context = ToolContext {
        root: Some(project.path().to_path_buf()),
        read_granted: true,
        write_granted: true,
        exec_granted: true,
        ..Default::default()
    };
    let tool = ExecuteTool::for_project(GitTool::read_only());

    for command in [format!("type \"{}\"", file.display()), format!("echo edited>\"{}\"", file.display())] {
        let result = tool.execute(&json!({"command": command}), &context);
        assert!(result.exit_code.is_some_and(|code| code != 0), "{}", result.output);
        assert!(!result.output.contains("outside fixture"));
    }
    assert_eq!(std::fs::read(file).unwrap(), b"outside fixture");
}

#[test]
fn given_workspace_grants_when_a_container_writes_then_only_the_writable_copy_changes() {
    let original = tempfile::tempdir().unwrap();
    std::fs::write(original.path().join("input.txt"), b"original").unwrap();
    let cmd = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32").join("cmd.exe");
    let command = "cmd.exe /d /c if exist input.txt (echo changed>input.txt && exit 23) else exit 24";
    let read_only = Workspace::stage(original.path()).unwrap();
    let writable = Workspace::stage(original.path()).unwrap();

    let denied = windows_appcontainer::run_in_workspace(&read_only, false, &cmd, command, 10_000).unwrap();
    assert_ne!(denied.exit_code, Some(23));
    assert_ne!(denied.exit_code, Some(24), "the read-only copy must be visible");
    assert_eq!(
        windows_appcontainer::run_in_workspace(&writable, true, &cmd, command, 10_000).unwrap().exit_code,
        Some(23)
    );
    assert_eq!(std::fs::read(read_only.path().join("input.txt")).unwrap(), b"original");
    assert_eq!(std::fs::read(writable.path().join("input.txt")).unwrap(), b"changed \r\n");
    assert_eq!(std::fs::read(original.path().join("input.txt")).unwrap(), b"original");
}
