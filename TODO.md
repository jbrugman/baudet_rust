# TODO

## Working rule

Keep local models away from the frontend.
Do not give them tasks that modify files under `crates/baudet-ui`.

## Release and smoke tests

- [ ] Run the macOS, Linux, and Windows packaging jobs in GitHub Actions.
      Launch the packaged app on each platform.
      On Windows x64, verify that it loads all required DLLs from the zip.
- [ ] Test a session against a real model server.
      **Stop** should end the stream and keep the partial reply.
      **Compact** should report when there is nothing to compact.
      After saving a different endpoint, send a message and verify that it reaches the new server.

## Reliability

- [ ] Open the window immediately when the protocol is missing or the server is unreachable.
      Let **Scan endpoint** start discovery.
- [ ] Mark interrupted streams as incomplete and prevent the tool loop from acting on an incomplete turn.
- [ ] Finish the turn with an error when a parallel tool fails.
      Enforce thread safety for shared callbacks through Rust's type system.
- [ ] Report process wait failures as execution errors.
      Show MCP startup errors alongside the affected server.

## Workspace permissions

- [ ] Let users grant extra folders and network access to shell commands once or per project.
      Verify isolation in the packaged macOS and Linux apps.
- [ ] Implement and verify Windows workspace isolation before enabling shell commands without individual approval.
- [ ] Enforce project permissions in the Git and file tools.
      Keep writes inside the project folder even when a path changes during an operation.
- [ ] Review which environment variables Git, MCP, and other command runners inherit.
      Pass app secrets only when explicitly configured for that integration.
- [ ] Require an explicit scheme for plain HTTP endpoints.
      Keep support for explicitly configured HTTP servers on the local network.

## Maintenance

- [ ] Save settings through a uniquely named temporary file with private permissions from creation.
      Test concurrent saves.
- [ ] Test `--send` and `--fetch-models` outside screenshot mode and fix their wiring where needed.
- [ ] Translate the remaining user-facing messages and check Enter handling in settings and side panels.
- [ ] Fix the existing formatting and Clippy issues, then enable both checks in CI.
