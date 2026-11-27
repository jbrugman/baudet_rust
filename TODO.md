# TODO

## Working rule

Keep local models away from the frontend.
Do not give them tasks that modify files under `crates/baudet-ui`.

For each implementation prompt, check all affected usages first, keep the diff minimal, and mark the task complete only after a clean compile and passing relevant tests.

## P2 — Workspace security

- [x] Enforce project permissions in the Git and file tools.
      Project access levels are Read, Read + Write, and Read + Write + Execute.
      File tools enforce the required grants and use project-relative handles. Edit and Undo check the target identity and content before writing. The Git tool checks repository containment and runs available local commands in the macOS, Linux, or Windows workspace sandbox, with read-only access for Git reads.
- [x] Review which environment variables Git, MCP, and other command runners inherit.
      Pass app secrets only when explicitly configured for that integration.
      **Prompt:** Inventory the environment inherited by shell, Git, and MCP child processes. Limit secret forwarding to integrations explicitly configured to receive those secrets. Verify that normal command and server startup still work.
- [x] Implement and verify Windows workspace isolation.
      Windows project shell and local Git commands run in an AppContainer with a temporary project copy. Successful writable commands write back after conflict and junction checks. Verified end to end in Wintest on Windows 11 ARM64, including x64 test binaries under emulation.
- [x] Add and verify an isolated network route for Git pull and push.
      Require Read + Write + Execute, explicit approval of the resolved remote, and credentials scoped to its URL. SSH remotes require an explicit username in both URL and SCP notation. HTTPS token and SSH key authentication were verified end to end against temporary Linux test remotes for both pull and push.
- [ ] Recover interrupted project writeback automatically.
      A backup manifest records the original paths before writeback. Restore interrupted updates without overwriting independent edits or trusting a manifest supplied by the project. Verify recovery after forced termination.

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
