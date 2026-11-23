
use crate::config::Settings;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub enum ReadOutcome {
    Loaded(Settings),
    Absent(Settings),
    Ignored { settings: Settings, reason: String, preserved: PathBuf },
}

impl ReadOutcome {
    pub fn settings(&self) -> &Settings {
        match self {
            ReadOutcome::Loaded(s) | ReadOutcome::Absent(s) | ReadOutcome::Ignored { settings: s, .. } => s,
        }
    }

    pub fn problem(&self) -> Option<(&str, &Path)> {
        match self {
            ReadOutcome::Ignored { reason, preserved, .. } => Some((reason.as_str(), preserved.as_path())),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SettingsFile {
    path: PathBuf,
}

impl SettingsFile {
    pub fn default_location() -> SettingsFile {
        SettingsFile { path: baudet_store::data_dir().join("settings.json") }
    }

    pub fn at(path: impl Into<PathBuf>) -> SettingsFile {
        SettingsFile { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

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

    // Keep the original file when it could not be parsed.
    pub fn write(&self, settings: &Settings) -> Result<(), WriteError> {
        if let Some(reason) = self.refusal_reason() {
            return Err(WriteError::WouldDiscard { path: self.path.clone(), reason });
        }
        let parent = self.path.parent().filter(|p| !p.as_os_str().is_empty());
        if let Some(parent) = parent {
            std::fs::create_dir_all(parent)
                .map_err(|e| WriteError::Io { path: self.path.clone(), detail: e.to_string() })?;
        }
        let body = serde_json::to_string_pretty(settings)
            .map_err(|e| WriteError::Io { path: self.path.clone(), detail: e.to_string() })?
            + "\n";
        let temp = sibling_temp(&self.path);
        let result = (|| -> Result<(), std::io::Error> {
            let mut file = std::fs::File::create(&temp)?;
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
                let _ = std::fs::remove_file(&temp);
                Err(WriteError::Io { path: self.path.clone(), detail: error.to_string() })
            }
        }
    }

    fn refusal_reason(&self) -> Option<String> {
        match self.read() {
            ReadOutcome::Ignored { reason, .. } => Some(reason),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WriteError {
    #[error("could not write {path}: {detail}")]
    Io { path: PathBuf, detail: String },
    #[error("refusing to overwrite {path}, which is not readable as settings: {reason}")]
    WouldDiscard { path: PathBuf, reason: String },
}

fn sibling_temp(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "settings".into());
    path.with_file_name(format!("{name}.tmp"))
}

#[cfg(unix)]
mod permissions {
    use std::fs::File;
    use std::io;
    use std::os::unix::fs::PermissionsExt;

    pub fn restrict(file: &mut File) -> io::Result<()> {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
    }
}

#[cfg(not(unix))]
mod permissions {
    use std::fs::File;
    use std::io;

    pub fn restrict(_file: &mut File) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            max_tool_iterations: 3,
            temperature: 0.35,
            top_p: Some(0.92),
            max_parallel_turns: 2,
            context_budget_tokens: 24_000,
            context_strategy: crate::config::ContextStrategy::RollingWindow,
            context_keep_recent_messages: 4,
            use_server_session: false,
            use_responses_api: false,
            store_responses: false,
            auto_approve_safe_commands: false,
            git_allow_writes: true,
            transcript_wrap: false,
            honesty_footer: false,
            system_prompt: "Regel een.\nRegel twee.".into(),
            trusted_commands: vec!["git diff".into(), "mvn".into()],
            mcp_servers: Vec::new(),
            mcp_allow_servers: Vec::new(),
            auto_allow_commands: vec!["cargo test".into()],
            learned_context_window: 32_768,
            learned_context_key: "http://studio:8000/v1|Qwen3.8-Flash".into(),
            left_divider: 301,
            right_divider: 745,
            right_panel_width: 320,
            diff_window_size: Some([720.0, 460.0]),
            files_divider: 214,
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
