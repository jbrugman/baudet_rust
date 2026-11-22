//! Eén tekstbestand lezen binnen de projectmap, zodat het model het kan zien vóór het het verandert.
//!
//! Poort van `tool/ReadFileTool.kt`. Dit is de lees-helft van de edit-loop: zonder deze tool kan een model
//! alleen de huidige bytes raden en zijn patch matcht nooit — precies zo faalt een eerste echte edit.
//!
//! Dezelfde grens als de edit-tool: er moet een map gekoppeld zijn en het pad moet binnen die map oplossen.
//! Omdat lezen niets schrijft, heeft deze tool geen Write-recht nodig, vraagt hij niets, schrijft hij niets en
//! biedt hij geen undo. Een pad dat mist geeft de begrensde map-opsomming terug, zodat het model de echte naam
//! kan kiezen. Een bestand te groot om goedkoop terug te geven, of met bytes die geen tekst zijn, wordt
//! geweigerd in plaats van gedumpt: megabytes of binaire ruis in de context kost een beurt en helpt niemand.
//! Elke weigering is een [`ToolResult`] met een model-gerichte reden, nooit een exception.

use crate::folder_boundary::{FolderBoundary, Resolved};
use crate::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};

/// Harde cap op de bytes van schijf, dus een enorm bestand wordt geweigerd vóór het geladen wordt.
const MAX_FILE_BYTES: u64 = 1_000_000;

/// Cap op de karakters die aan het model worden gegeven, dus één read kan niet de hele context verdringen.
const MAX_CHARS: usize = 60_000;

/// De tool zelf; staatsloos, zoals in de origineler.
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

    /// Lezen schrijft niets en wacht op niemand, dus twee reads mogen parallel.
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
        let resolved = match FolderBoundary::resolve_file_within(root, rel) {
            Resolved::Inside(path) => path,
            Resolved::Refused(reason) => return fail(reason),
        };

        let Ok(size) = std::fs::metadata(&resolved).map(|m| m.len()) else {
            return fail(format!("{rel} could not be read"));
        };
        if size > MAX_FILE_BYTES {
            return fail(format!("{rel} is larger than the {} KB read limit", MAX_FILE_BYTES / 1024));
        }
        let Ok(bytes) = std::fs::read(&resolved) else {
            return fail(format!("{rel} could not be read"));
        };
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

/// De weigeringstekst zoals het model die hoort: de toolnaam erbij, zodat een observatie laat zien wát er
/// weigerde.
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
        // Een read van 60 000 karakters mag niet korter afkappen omdat Nederlandse tekst méér bytes kost.
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
        // Zelfs een context zonder één recht mag lezen: lezen is geen schrijven, en de origineler eist hier
        // alleen de map, niet het Write-recht.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "inhoud").unwrap();
        let ctx = ToolContext { root: Some(dir.path().to_path_buf()), ..Default::default() };
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
