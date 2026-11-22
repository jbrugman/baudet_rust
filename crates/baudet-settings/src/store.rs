//! Reading and writing the settings file.
//!
//! One rule governs this file: **a file that cannot be read is never written.** `read` answers an
//! unreadable file with defaults, and a caller that then saves must not be able to turn one bad
//! start into the loss of every model and endpoint a person ever configured. So the read reports
//! *why* it fell back, and writing a file that was refused is what the type below makes a caller
//! think about.
//!
//! Kotlin's `SettingsStore` falls back to defaults silently. That is fine for a terminal app whose
//! settings are three numbers, and not fine for a file holding an endpoint and a credential.

use crate::config::Settings;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Why a settings file was not usable, and what was done about it.
#[derive(Debug, Clone, PartialEq)]
pub enum ReadOutcome {
    /// The file held settings, and they were used.
    Loaded(Settings),
    /// There was no file at all: a first start, which is not a problem and must not be shown as one.
    Absent(Settings),
    /// The file exists but is not readable as settings. `reason` names the problem and
    /// `preserved` is where the bytes still are, so the person can be told rather than quietly
    /// reset.
    Ignored { settings: Settings, reason: String, preserved: PathBuf },
}

impl ReadOutcome {
    /// The settings to use, whatever happened to the file.
    pub fn settings(&self) -> &Settings {
        match self {
            ReadOutcome::Loaded(s) | ReadOutcome::Absent(s) | ReadOutcome::Ignored { settings: s, .. } => s,
        }
    }

    /// What to tell the person, and where the bytes still are. `Absent` is deliberately not a
    /// problem: a first run has nothing to report.
    ///
    /// The path travels with the reason on purpose. "Your settings could not be read" invites the
    /// person to type everything again; "…which is still in ~/Library/Application
    /// Support/Baudet/settings.json" tells them one edit will do it.
    pub fn problem(&self) -> Option<(&str, &Path)> {
        match self {
            ReadOutcome::Ignored { reason, preserved, .. } => Some((reason.as_str(), preserved.as_path())),
            _ => None,
        }
    }
}

/// The settings file, rooted in the same directory as the rollouts
/// ([`baudet_store::data_dir`]), so one `BAUDET_HOME` moves everything.
#[derive(Debug, Clone)]
pub struct SettingsFile {
    path: PathBuf,
}

impl SettingsFile {
    /// `<data dir>/settings.json`.
    pub fn default_location() -> SettingsFile {
        SettingsFile { path: baudet_store::data_dir().join("settings.json") }
    }

    /// An explicit path, which is what a test wants and what a future `--settings` flag would use.
    pub fn at(path: impl Into<PathBuf>) -> SettingsFile {
        SettingsFile { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read, never repairing. See [`ReadOutcome`]: an unreadable file keeps its bytes and says so.
    pub fn read(&self) -> ReadOutcome {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return ReadOutcome::Absent(Settings::default())
            }
            Err(error) => {
                return ReadOutcome::Ignored {
                    settings: Settings::default(),
                    reason: format!("could not read {}: {error}", self.path.display()),
                    preserved: self.path.clone(),
                }
            }
        };
        // Empty is a file that was created and never filled, not damage. Treating it as damage
        // would show a warning nobody caused.
        if text.trim().is_empty() {
            return ReadOutcome::Absent(Settings::default());
        }
        match serde_json::from_str::<Settings>(&text) {
            Ok(settings) => ReadOutcome::Loaded(settings),
            Err(error) => ReadOutcome::Ignored {
                settings: Settings::default(),
                reason: format!("{} is not readable as settings: {error}", self.path.display()),
                preserved: self.path.clone(),
            },
        }
    }

    /// Write, atomically, and refuse to do so over a file this reader would not accept.
    ///
    /// The refusal is the point of the whole module. It is checked by reading the file again rather
    /// than trusting an earlier `read`, because the window is exactly the interesting one: the
    /// person edits the file in another program, the panel still holds its stale in-memory copy, and
    /// a save would silently overwrite their edit.
    ///
    /// The write goes to a sibling temporary file and is renamed into place, so a crash mid-write
    /// leaves the previous settings rather than a half-written file. That is the failure this
    /// module exists to survive.
    pub fn write(&self, settings: &Settings) -> Result<(), WriteError> {
        if let Some(reason) = self.refusal_reason() {
            return Err(WriteError::WouldDiscard { path: self.path.clone(), reason });
        }
        let parent = self.path.parent().filter(|p| !p.as_os_str().is_empty());
        if let Some(parent) = parent {
            std::fs::create_dir_all(parent)
                .map_err(|e| WriteError::Io { path: self.path.clone(), detail: e.to_string() })?;
        }
        // Trailing newline so the file is diffable and readable with `cat`.
        let body = serde_json::to_string_pretty(settings)
            .map_err(|e| WriteError::Io { path: self.path.clone(), detail: e.to_string() })?
            + "\n";
        let temp = sibling_temp(&self.path);
        let result = (|| -> Result<(), std::io::Error> {
            let mut file = std::fs::File::create(&temp)?;
            // A file holding a credential should not be world-readable on a shared machine.
            permissions::restrict(&mut file)?;
            file.write_all(body.as_bytes())?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temp, &self.path)?;
            Ok(())
        })();
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                // Leave nothing behind that a later save could mistake for the settings file.
                let _ = std::fs::remove_file(&temp);
                Err(WriteError::Io { path: self.path.clone(), detail: error.to_string() })
            }
        }
    }

    /// Why a save would be refused, if it would be.
    fn refusal_reason(&self) -> Option<String> {
        match self.read() {
            ReadOutcome::Ignored { reason, .. } => Some(reason),
            _ => None,
        }
    }
}

/// What stopped a save.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WriteError {
    #[error("could not write {path}: {detail}")]
    Io { path: PathBuf, detail: String },
    /// Refused, on purpose. Named so a panel can say "your file is broken, fix it first" instead of
    /// reporting an I/O failure it cannot explain.
    #[error("refusing to overwrite {path}, which is not readable as settings: {reason}")]
    WouldDiscard { path: PathBuf, reason: String },
}

/// `<name>.tmp`, in the same directory, so the rename cannot cross a filesystem.
fn sibling_temp(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "settings".into());
    path.with_file_name(format!("{name}.tmp"))
}

/// File-mode handling, which is a unix idea with a real answer and a non-unix answer of "not mine".
#[cfg(unix)]
mod permissions {
    use std::fs::File;
    use std::io;
    use std::os::unix::fs::PermissionsExt;

    /// Owner read/write only (`0600`).
    ///
    /// Set on the descriptor rather than after the rename, so the file is never briefly readable by
    /// others while it is being created.
    pub fn restrict(file: &mut File) -> io::Result<()> {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
    }
}

#[cfg(not(unix))]
mod permissions {
    use std::fs::File;
    use std::io;

    /// Nothing portable to set here, and inventing an ACL shape per platform is not this crate's
    /// job. Stated rather than pretended at: on Windows the file inherits its folder's ACL, which
    /// is the platform's own answer.
    pub fn restrict(_file: &mut File) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Het taalveld in de letterijke initializer hieronder is geen typewerk: `super::*` bringt
    // `Settings` mee maar niet het type van zijn veld.
    use baudet_i18n::Language;

    #[test]
    fn a_saved_file_reads_back_identical() {
        let dir = tempfile::tempdir().unwrap();
        let file = SettingsFile::at(dir.path().join("settings.json"));
        let settings = Settings {
            base_url: "http://studio:8000/v1".into(),
            model: Some("Qwen3.8-Flash".into()),
            reasoning: Some("high".into()),
            max_output_tokens: Some(4096),
            api_key: Some("sk-local".into()),
            api_key_env_var: "SOME_OTHER_KEY".into(),
            protocol: Some("chat_completions".into()),
            tool_mode: crate::config::ToolMode::Auto,
            // Niet de default, zodat dit rondreisje bewijst dat het veld meegaat en niet op 8
            // terugvalt wanneer het bestand het wel bevatte.
            max_tool_iterations: 3,
            // De drie generatie-parameters (`AppConfig.kt:70-71,97`) reizen door hetzelfde bestand
            // en staan hier niet op de default: `top_p` Some bewijst dat een marge wordt
            // weggeschreven, en niet dat hij op null terugvalt.
            temperature: 0.35,
            top_p: Some(0.92),
            max_parallel_turns: 2,
            // Ook de contextvelden moeten meegaan: een geleerd venster dat het schrijven kwijt
            // raakt, betekent dat de app opnieuw op dezelfde manier faalt na een herstart.
            context_budget_tokens: 24_000,
            context_strategy: crate::config::ContextStrategy::RollingWindow,
            context_keep_recent_messages: 4,
            use_server_session: false,
            learned_context_window: 32_768,
            learned_context_key: "http://studio:8000/v1|Qwen3.8-Flash".into(),
            // De verdeelstanden reizen mee door hetzelfde bestand (`AppConfig.kt:152-158`) en hun
            // fabrikaat is 0, dus waarden die hier niet staan, bewijzen dat ze zijn weggeschreven.
            left_divider: 301,
            right_divider: 745,
            files_divider: 214,
            // Evenmin de default: de taal gaat door een eigen leespoort (`parse_language`), en een
            // rondreisje langs de default bewijst van zo'n veld niets - dan kan het zowel zijn
            // weggeschreven als stil teruggevallen.
            language: Language::Nl,
        };
        file.write(&settings).unwrap();
        assert_eq!(file.read().settings(), &settings);
    }

    #[test]
    fn a_missing_file_is_an_absent_start_and_not_a_problem() {
        let dir = tempfile::tempdir().unwrap();
        let file = SettingsFile::at(dir.path().join("nested").join("settings.json"));
        let outcome = file.read();
        assert_eq!(outcome.problem().map(|(reason, _)| reason), None);
        assert!(matches!(outcome, ReadOutcome::Absent(_)));
        assert_eq!(outcome.settings(), &Settings::default());
        // The first save creates the parent it needs.
        file.write(&Settings::default()).unwrap();
        assert!(file.path().exists());
    }

    #[test]
    fn an_untouched_empty_file_is_not_damage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "\n").unwrap();
        assert_eq!(SettingsFile::at(path).read().problem().map(|(reason, _)| reason), None);
    }

    #[test]
    fn a_corrupt_file_is_kept_and_a_save_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{ half a file").unwrap();
        let file = SettingsFile::at(&path);

        let outcome = file.read();
        let (reason, preserved) = outcome.problem().expect("a person must be told, not reset");
        assert!(reason.contains("not readable as settings"), "{reason}");
        assert_eq!(preserved, path, "the message has to name the file that still holds the bytes");
        assert_eq!(outcome.settings(), &Settings::default());
        let Err(WriteError::WouldDiscard { .. }) = file.write(&Settings::default()) else {
            panic!("a save over an unreadable file must be refused");
        };
        // The bytes are the whole point: refused, and still exactly what they were.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ half a file");
    }

    #[test]
    fn a_save_survives_a_crash_shape_by_leaving_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = SettingsFile::at(dir.path().join("settings.json"));
        file.write(&Settings::default()).unwrap();
        let leftovers: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(leftovers, vec!["settings.json".to_string()], "the sibling temp must be renamed away");
    }

    #[cfg(unix)]
    #[test]
    fn a_file_holding_a_credential_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = SettingsFile::at(dir.path().join("settings.json"));
        file.write(&Settings { api_key: Some("sk-secret".into()), ..Default::default() }).unwrap();
        let mode = std::fs::metadata(file.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "a credential file must not be group or world readable");
    }

    #[test]
    fn the_temp_name_stays_in_the_same_folder() {
        assert_eq!(sibling_temp(Path::new("/a/b/settings.json")), PathBuf::from("/a/b/settings.json.tmp"));
    }
}
