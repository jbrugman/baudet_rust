use baudet_tool::{
    git_tool::GitTool,
    tool::{Tool, ToolContext},
};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(folder: &Path, args: &[&str]) -> String {
    let output = Command::new("git").current_dir(folder).args(args).output().unwrap();
    assert!(output.status.success(), "{args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap()
}

#[test]
#[ignore = "requires temporary HTTPS and SSH Git servers with the fixture credentials"]
fn given_approved_https_and_ssh_remotes_when_pushing_and_pulling_then_each_remote_branch_updates() {
    let fixture = PathBuf::from(std::env::var_os("BAUDET_GIT_FIXTURE_DIR").unwrap());
    for transport in ["https", "ssh"] {
        let client = fixture.join(format!("{transport}-client"));
        let server = fixture.join(format!("{transport}-server"));
        let remote = fixture.join(format!("{transport}.git"));
        let context = ToolContext {
            root: Some(client.clone()),
            read_granted: true,
            write_granted: true,
            exec_granted: true,
            approve_command: Box::new(|_, _, _, _| true),
            ..Default::default()
        };
        let tool = GitTool::with_writes();

        std::fs::write(client.join("client.txt"), "pushed by Baudet\n").unwrap();
        git(&client, &["add", "client.txt"]);
        git(&client, &["commit", "-m", "Client commit"]);
        let push = tool.execute(&json!({"command": "push", "args": ["origin", "main"]}), &context);
        assert!(push.ok, "{transport}: {}", push.output);
        assert_eq!(git(&remote, &["show", "main:client.txt"]), "pushed by Baudet\n");

        git(&server, &["pull", "--ff-only"]);
        std::fs::write(server.join("server.txt"), "pulled by Baudet\n").unwrap();
        git(&server, &["add", "server.txt"]);
        git(&server, &["commit", "-m", "Server commit"]);
        git(&server, &["push"]);
        let pull = tool.execute(&json!({"command": "pull", "args": ["origin", "main"]}), &context);
        assert!(pull.ok, "{transport}: {}", pull.output);
        assert_eq!(std::fs::read_to_string(client.join("server.txt")).unwrap(), "pulled by Baudet\n");
        assert_eq!(git(&client, &["rev-parse", "HEAD"]), git(&server, &["rev-parse", "HEAD"]));
        assert_eq!(git(&client, &["rev-parse", "refs/remotes/origin/main"]), git(&server, &["rev-parse", "HEAD"]));
    }
}
