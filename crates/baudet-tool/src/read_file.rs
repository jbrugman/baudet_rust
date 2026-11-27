
use crate::folder_boundary::{FolderBoundary, Resolved};
use crate::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use std::io::Read;

const MAX_FILE_BYTES: u64 = 1_000_000;

const MAX_CHARS: usize = 60_000;

#[derive(Debug, Clone, Copy, Default)]
pub struct ReadFileTool;

impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read a text file inside the project folder and return its current contents. \
         Call this before edit_file so your old_text matches the file exactly."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"file": {"type": "string", "description": "a file inside the project folder"}},
            "required": ["file"]
        })
    }

    fn can_run_parallel(&self, _args: &Value, _ctx: &ToolContext) -> bool {
        true
    }

    fn execute(&self, args: &Value, ctx: &ToolContext) -> ToolResult {
        let rel = match args.get("file").and_then(Value::as_str) {
            Some(rel) => rel,
            None => return fail("needs a file argument"),
        };
        let Some(root) = ctx.root.as_deref() else {
            return fail("no project folder is linked to read from");
        };
        if !ctx.read_granted {
            return fail("the project does not grant the Read right");
        }
        let resolved = match FolderBoundary::resolve_file_within(root, rel) {
            Resolved::Inside(path) => path,
            Resolved::Refused(reason) => return fail(reason),
        };

        let opened = (|| {
            let root = root.canonicalize()?;
            let dir = cap_std::fs::Dir::open_ambient_dir(&root, cap_std::ambient_authority())?;
            let canonical = resolved.canonicalize()?;
            let relative = canonical.strip_prefix(root).map_err(std::io::Error::other)?;
            dir.open(relative)
        })();
        let Ok(file) = opened else {
            return fail(format!("{rel} could not be read"));
        };
        let Ok(metadata) = file.metadata() else {
            return fail(format!("{rel} could not be read"));
        };
        if !metadata.is_file() {
            return fail(format!("{rel} is not a regular file"));
        }
        let too_large = || fail(format!("{rel} is larger than the {} KB read limit", MAX_FILE_BYTES / 1024));
        if metadata.len() > MAX_FILE_BYTES {
            return too_large();
        }
        let mut bytes = Vec::new();
        if file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes).is_err() {
            return fail(format!("{rel} could not be read"));
        }
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return too_large();
        }
        if bytes.contains(&0) {
            return fail(format!("{rel} looks like a binary file and is not shown"));
        }
        let text = String::from_utf8_lossy(&bytes);
        if text.is_empty() {
            return ToolResult::ok(format!("read {rel}: the file is empty"));
        }
        let truncated = text.chars().count() > MAX_CHARS;
        let body: String = if truncated { text.chars().take(MAX_CHARS).collect() } else { text.to_string() };
        let header = if truncated {
            format!("read {rel}, first {MAX_CHARS} of {} chars:", text.chars().count())
        } else {
            format!("read {rel} ({} chars):", text.chars().count())
        };
        ToolResult::ok(format!("{header}\n{body}"))
    }
}

fn fail(reason: impl Into<String>) -> ToolResult {
    ToolResult::fail(format!("read_file: {}", reason.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn read(root: &std::path::Path, file: &str) -> ToolResult {
        let ctx = ToolContext::for_folder(root, true, false, false);
        ReadFileTool.execute(&json!({"file": file}), &ctx)
    }

    #[test]
    fn it_asks_for_a_file_and_for_a_folder_in_model_words() {
        let ctx = ToolContext::default();
        let missing_arg = ReadFileTool.execute(&json!({}), &ctx);
        assert!(!missing_arg.ok);
        assert_eq!(missing_arg.output, "read_file: needs a file argument");

        let no_folder = ReadFileTool.execute(&json!({"file": "a.txt"}), &ctx);
        assert_eq!(no_folder.output, "read_file: no project folder is linked to read from");
    }

    #[test]
    fn given_a_project_file_without_read_permission_when_read_is_requested_then_its_contents_stay_private() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "private note").unwrap();
        let context = ToolContext::for_folder(dir.path(), false, true, false);
        let result = ReadFileTool.execute(&json!({"file": "notes.txt"}), &context);
        assert!(!result.ok);
        assert_eq!(result.output, "read_file: the project does not grant the Read right");
    }

    #[test]
    fn it_reads_a_text_file_and_says_what_it_read() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("notes.txt"), "eerste regel\n").unwrap();
        let result = read(dir.path(), "notes.txt");
        assert!(result.ok);
        assert_eq!(result.output, "read notes.txt (13 chars):\neerste regel\n");
    }

    #[test]
    fn an_empty_file_is_a_success_not_a_refusal() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("leeg.txt"), "").unwrap();
        let result = read(dir.path(), "leeg.txt");
        assert!(result.ok);
        assert_eq!(result.output, "read leeg.txt: the file is empty");
    }

    #[test]
    fn binary_bytes_are_refused_rather_than_dumped() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("blob.bin"), [b'a', 0, b'b']).unwrap();
        let result = read(dir.path(), "blob.bin");
        assert!(!result.ok);
        assert_eq!(result.output, "read_file: blob.bin looks like a binary file and is not shown");
    }

    #[test]
    fn a_file_beyond_the_byte_cap_is_refused_before_it_is_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("groot.txt");
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_FILE_BYTES + 1).unwrap();
        let result = read(dir.path(), "groot.txt");
        assert!(!result.ok);
        assert!(result.output.contains("976 KB read limit"), "noem de cap: {}", result.output);
    }

    #[test]
    fn a_long_file_is_cut_at_the_character_cap_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let text = "x".repeat(MAX_CHARS + 25);
        fs::write(dir.path().join("lang.txt"), &text).unwrap();
        let result = read(dir.path(), "lang.txt");
        assert!(result.ok);
        assert!(result.output.starts_with(&format!("read lang.txt, first {MAX_CHARS} of {} chars:", text.len())));
        let body = result.output.split_once('\n').unwrap().1;
        assert_eq!(body.chars().count(), MAX_CHARS, "de cap gilt op de karakters die truly meegaan");
    }

    #[test]
    fn characters_are_counted_not_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let text = "é".repeat(MAX_CHARS + 1);
        fs::write(dir.path().join("accent.txt"), &text).unwrap();
        let result = read(dir.path(), "accent.txt");
        assert!(
            result.output.contains("first 60000 of 60001 chars"),
            "karakters, geen bytes: {}",
            &result.output[..40.min(result.output.len())]
        );
    }

    #[test]
    fn a_missing_path_returns_the_folder_listing_as_a_hint() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("real.rs"), "x").unwrap();
        let result = read(dir.path(), "real.rs.txt");
        assert!(!result.ok);
        assert!(result.output.contains("Files here: real.rs"), "het model moet kunnen raden: {}", result.output);
    }

    #[test]
    fn reading_needs_no_write_right_and_asks_nothing() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "inhoud").unwrap();
        let ctx = ToolContext::for_folder(dir.path(), true, false, false);
        let result = ReadFileTool.execute(&json!({"file": "a.txt"}), &ctx);
        assert!(result.ok, "lezen vraagt niets: {}", result.output);
    }

    #[test]
    fn it_declares_itself_parallel_safe() {
        let ctx = ToolContext::default();
        assert!(ReadFileTool.can_run_parallel(&json!({"file": "a.txt"}), &ctx));
    }

    #[test]
    fn escaping_the_folder_stays_the_boundarys_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let result = read(dir.path(), "../buiten.txt");
        assert!(!result.ok);
        assert!(result.output.contains("resolves outside the project folder"), "got {}", result.output);
    }
}
