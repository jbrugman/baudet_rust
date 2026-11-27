# Baudet

Baudet is a lightweight desktop coding assistant for models served on your own computer or a self-hosted server.
It connects over HTTP using the OpenAI-compatible Responses or Chat Completions API.
The model server handles inference.

## About

This is the Rust and egui version of [the original Kotlin Baudet](https://github.com/jbrugman/Baudet).
The name is French for donkey, a nod to the local model doing the heavy lifting.

## Why it was started

Local models often have smaller context windows, and servers vary in what they report about tokens, reasoning, and tool calls.
Baudet was started to make those differences manageable in everyday coding.
It lets you choose how much chat history to send, compact older context on the client, and keep streamed reasoning separate from the answer.
Project permissions give the model a defined workspace for reading, editing, and running commands.

## Current state

**What works**

- Model discovery and selection, endpoint configuration, and streaming over both supported APIs.
- Responses can reuse server-side context with `previous_response_id` when the selected settings and server support it.
- Markdown, clipboard images, locally saved chats, a resizable reasoning panel, and client-side compaction.
- **Stop** interrupts a stream and keeps the partial reply.
  **Compact** reports its outcome.
- Projects with folder-level read, write, and execute permissions.
- The file tree and Git changes refresh when files change on disk.
  Double-click a file to open its diff or preview.
- Model-driven file reads and edits, Git operations, shell commands, and MCP servers over stdio.

- Project shell commands use Seatbelt on macOS, Bubblewrap on Linux, and AppContainer on Windows.
  With Execute permission, workspace shell commands run inside the sandbox without a per-command approval prompt.
  MCP uses its own approval rules.

**What's still open**

- Reasoning controls, server-side sessions, and tool calls depend on support from the model server.
- The packaged apps still need a launch check on all three platforms.
  Approval for extra folders or general network access from the shell is still open.
- The diff viewer supports inspection.
  Source edits are made through the file tool or an external editor.

See [TODO.md](TODO.md) for the remaining work and verification steps.

### Git network access

For `git pull` and `git push`, link a repository folder with Read, Write, and Execute permissions. Supply the configured remote name and the checked-out branch, for example `git pull origin main`. Baudet asks for approval for the resolved HTTPS or SSH destination each time. Pull requires a clean working tree and a fast-forward. Push updates one branch without force.

Baudet uses your Git credential helper for HTTPS. For SSH, include the username in the remote URL. Baudet uses OpenSSH to resolve host aliases, ports, `IdentityFile` paths, and `IdentitiesOnly` from your SSH configuration. It tries the listed key files in order and can fall back to the SSH agent when `IdentitiesOnly` allows it. The approval prompt shows the resolved destination. SSH proxy routes require separate support. Use an `ssh://` remote for a custom port.

To override authentication for a particular remote, set `BAUDET_GIT_AUTH_FILE` in Baudet's launch environment to an absolute JSON file outside the project. Use the complete remote URL as the key and refer to credentials by environment variable or key path:

```json
{
  "https://git.example.org/team/project.git": {
    "transport": "https",
    "username": "my-user",
    "token_env": "MY_PROJECT_GIT_TOKEN"
  },
  "ssh://git@git.example.org/team/project.git": {
    "transport": "ssh",
    "username": "git",
    "private_key": "/absolute/path/to/project-key",
    "passphrase_env": "MY_PROJECT_SSH_PASSPHRASE"
  }
}
```

Public HTTPS repositories can be read without a credential entry. Keep SSH keys outside the linked project and omit `passphrase_env` for an unencrypted key. SSH requires the remote host key to be present in the user's `known_hosts`. An SCP-style remote such as `git@git.example.org:team/project.git` uses the corresponding `ssh://` key shown above for credential lookup.

Network operations use a temporary copy of a standalone repository, limited to 256 MiB and 40,000 entries. Linked worktrees, sparse checkouts, and projects containing filesystem links are refused.

## Quick start

Download a package under **Artifacts** from a successful run of
[Build desktop packages](.github/workflows/build-desktop.yml).

| Platform | Package | How to open it |
|---|---|---|
| macOS | `Baudet-macOS-universal.dmg` for Apple Silicon and Intel | Open the DMG and drag `Baudet.app` to Applications. See [Signing on macOS](#signing-on-macos) for Gatekeeper distribution. |
| Windows x64 | `Baudet-windows-x64.zip` | Extract the archive and open `Baudet.exe`. |
| Windows ARM64 | `Baudet-windows-arm64.zip` | Extract the archive and open `Baudet.exe`. |
| Ubuntu 26.04 LTS and Debian 13 Trixie (x64) | `baudet_<version>_amd64.deb` | In your download directory, run `sudo apt install` followed by the downloaded filename prefixed with `./`. Install Bubblewrap for workspace shell commands if it is missing. |

With Rust and the [build prerequisites](#build-from-source), you can also open the demo.
It uses prerecorded replies and runs without a model server.

```sh
cargo run --locked -p baudet-ui -- --demo
```

### Connect a model server

1. Start your model server or check that your self-hosted server is reachable.
2. Open **Settings** and enter the API base URL.
   The default is `http://127.0.0.1:8000/v1`.
3. Enter an API key or its environment variable name if the server requires credentials.
   Use **Scan endpoint** to detect the API protocol, then fetch the model list and pick a model.
4. Click **Save** and open **Chat**.
   The saved connection applies to the next turn.

**Fetch**, **Scan endpoint**, and **Chat** use the entered key first.
If the field is empty, Baudet reads the configured environment variable, which defaults to `OMLX_API_KEY`.
Baudet does not save the value it reads from the environment.

If you export the variable in your terminal, launch Baudet from that same terminal.
On macOS you can run `/Applications/Baudet.app/Contents/MacOS/baudet-ui`.
Launching from Finder does not inherit your terminal's environment.

Under **reasoning-effort**, **Default** uses the server setting, **Off** asks the server to disable reasoning, and an effort level requests that level.
For compatible servers such as oMLX, **Use the server's thinking switch** also sends an explicit on or off setting when you choose one.

### Data directory

Baudet stores settings, projects, and conversations locally.
Use `--home <folder>` to select a different directory for that launch.
It takes precedence over `BAUDET_HOME`.

| Platform | Default directory |
|---|---|
| macOS | `~/Library/Application Support/Baudet` |
| Windows | `%APPDATA%\Baudet` |
| Linux | `$XDG_DATA_HOME/baudet` or `~/.local/share/baudet` |

## Build from source

Install rustup.
[rust-toolchain.toml](rust-toolchain.toml) pins the Rust version.
Run the script on the target platform.
Build artifacts go to `target/dist` by default.

| Platform | Prerequisites | Command |
|---|---|---|
| macOS | Xcode Command Line Tools | `bash packaging/build-macos.sh` |
| Windows x64 | PowerShell 7 and Visual Studio Build Tools with **Desktop development with C++** | `pwsh -NoProfile -File packaging/build-windows.ps1` |
| Windows ARM64 | Windows on ARM, PowerShell 7, and Visual Studio Build Tools with the C++ ARM64 build tools | `pwsh -NoProfile -File packaging/build-windows.ps1 -Architecture arm64` |
| Debian 13 Trixie x64 | The build packages listed below | `bash packaging/build-linux.sh` |

```sh
sudo apt update
sudo apt install build-essential dpkg-dev pkg-config libwayland-dev libxkbcommon-dev
bash packaging/build-linux.sh
```

The macOS app uses system frameworks.
The Windows executable links the Visual C++ runtime statically, and the archive lists its system DLL dependencies.
The Linux release package is built on Debian 13 Trixie. CI installs and starts the same package on Trixie and Ubuntu 26.04 LTS before publishing it. On both distributions, `apt` installs the package's shared library dependencies.

GitHub Actions runs these scripts on pull requests, pushes to `main`, tags starting with `v`, and manual runs.

### Release versions

The first release is `v0.1.0`.
Eligible pushes to `main` increase the patch version after the first release.
All packages must build and the Linux release checks must pass before the workflow updates the workspace version and publishes a GitHub release.
Merges from `norelease`, `norelease/*`, and Dependabot still run CI and do not trigger a release or version bump.

### Signing on macOS

CI builds are signed ad hoc.
To distribute through Gatekeeper, use a **Developer ID Application** certificate and Apple notarization.
Build locally with `bash packaging/build-macos.sh`, then use `Baudet.app` from `target/dist`.

Find your certificate with `security find-identity -v -p codesigning` and replace the placeholders below.
`notarytool store-credentials` prompts for an app-specific password.

```sh
identity='Developer ID Application: Your Name (YOURTEAMID)'
xcrun notarytool store-credentials Baudet-notary --apple-id 'you@example.com' --team-id YOURTEAMID

codesign --force --options runtime --timestamp --sign "$identity" target/dist/Baudet.app
codesign --verify --strict --verbose=2 target/dist/Baudet.app

(
  staging="$(mktemp -d)"
  trap 'rm -rf "$staging"' EXIT
  ditto target/dist/Baudet.app "$staging/Baudet.app"
  ln -s /Applications "$staging/Applications"
  hdiutil create -ov -volname Baudet -format UDZO -srcfolder "$staging" \
    target/dist/Baudet-macOS-universal-signed.dmg
)

codesign --timestamp --sign "$identity" target/dist/Baudet-macOS-universal-signed.dmg
xcrun notarytool submit target/dist/Baudet-macOS-universal-signed.dmg --keychain-profile Baudet-notary --wait
xcrun stapler staple target/dist/Baudet-macOS-universal-signed.dmg
xcrun stapler validate target/dist/Baudet-macOS-universal-signed.dmg
```

See [Apple's macOS distribution guide](https://developer.apple.com/documentation/xcode/packaging-mac-software-for-distribution) and [the notarization workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow).

## Development

```sh
cargo run --locked -p baudet-ui
cargo test --workspace --locked
cargo clippy --locked -p baudet-ui
```

`baudet-ui` contains the egui desktop interface.
`baudet-app` manages chats and turns, and `baudet-backend` handles HTTP and streaming.
Storage, settings, project permissions, and tools live in separate crates.

## Provenance

Most of this code was written by an AI coding assistant (OpenAI Codex) driving the local model Qwen3.8-Flash-Next-oQ4e-mtp served by oMLX, under human direction and review. 
Per-line attribution is not available, so this is stated at project level rather than per file. 
What matters here is the model behind it: every model call went to a local model over oMLX, a local serving platform that runs on your own machine rather than in the cloud. 
I wanted to prove that a local model can be used to write a useful program; this project is the result of that experiment.

### Note: 
I used the frontier version of Codex (Sol GPT-6.2) to fine-tune the code and check for correctness and security.

## License

Baudet is open source under the [MIT License](LICENSE).
The packages include the [licenses for the bundled fonts](packaging/FONT-LICENSES.txt).
