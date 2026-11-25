# TODO

## Working rule

Keep local models away from the frontend.
Do not give them tasks that modify files under `crates/baudet-ui`.

For each implementation prompt, check all affected usages first, keep the diff minimal, and mark the task complete only after a clean compile and passing relevant tests.

## P1 — Automatic Git inspection

- [ ] Prevent automatic Git inspection from starting external processes based on project Git configuration.
      **Prompt:** Trace all callers for the changes list and diff view. Harden automatic inspection against repository-configured external process execution. Verify that ordinary repositories still show the expected changes and patches.

## P2 — Workspace security

- [ ] Enforce project permissions in the Git and file tools.
      Keep writes inside the project folder even when a path changes during an operation.
      **Prompt:** In `edit_file`, bind project containment, the preview content check, and the write to the same securely opened file. Apply the same containment guarantees to undo. Verify that changes to the target or its parent path cannot redirect a write outside the project.
- [ ] Review which environment variables Git, MCP, and other command runners inherit.
      Pass app secrets only when explicitly configured for that integration.
      **Prompt:** Inventory the environment inherited by shell, Git, and MCP child processes. Limit secret forwarding to integrations explicitly configured to receive those secrets. Verify that normal command and server startup still work.
- [ ] Implement and verify Windows workspace isolation before enabling shell commands without individual approval.

## P3 — Reliability and storage

- [ ] Report process wait failures as execution errors.
      Show MCP startup errors alongside the affected server.
      **Prompt:** Report when stdout or stderr capture ends before EOF, keep the drain wait bounded, and count omitted bytes from the raw byte buffers. Verify the reporting with incomplete output and truncated multibyte text.
- [ ] Save settings through a uniquely named temporary file with private permissions from creation.
      Test concurrent saves.
      **Prompt:** Cover settings, transcript compaction, and other storage writes that use predictable temporary names. Create unique temporary files with private permissions in the destination directory and preserve atomic replacement. Verify concurrent saves and error handling.
- [ ] Open the window immediately when the protocol is missing or the server is unreachable.
      Let **Scan endpoint** start discovery.
- [ ] Mark interrupted streams as incomplete and prevent the tool loop from acting on an incomplete turn.
- [ ] Finish the turn with an error when a parallel tool fails.
      Enforce thread safety for shared callbacks through Rust's type system.
- [ ] Require an explicit scheme for plain HTTP endpoints.
      Keep support for explicitly configured HTTP servers on the local network.

## Release and smoke tests

- [ ] Run the macOS, Linux, and Windows packaging jobs in GitHub Actions.
      Launch the packaged app on each platform.
      On Windows x64, verify that it loads all required DLLs from the zip.
      **Prompt:** Build one Linux amd64 `.deb` on Debian 13 Trixie for Debian Trixie and Ubuntu 26.04 LTS. Verify a clean build and passing tests, install the same artifact and launch the packaged app on both distributions, and verify Bubblewrap isolation on both before release.
- [ ] Test a session against a real model server.
      **Stop** should end the stream and keep the partial reply.
      **Compact** should report when there is nothing to compact.
      After saving a different endpoint, send a message and verify that it reaches the new server.

## Workspace permissions

- [ ] Let users grant extra folders and network access to shell commands once or per project.
      Verify isolation in the packaged macOS and Linux apps.

## Local model tooling

- [ ] Add programmatic tool calling so model-written scripts can call registered tools and filter their results locally.
      Return only the script's final output to the model to reduce context use and model round trips.
      Apply the existing project permissions and approval rules to every tool call.

## Maintenance

- [ ] Translate the remaining user-facing messages and check Enter handling in settings and side panels.
- [ ] Fix the existing formatting and Clippy issues, then enable both checks in CI.
