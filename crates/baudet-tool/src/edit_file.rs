use crate::diff::{LineDiff, UnifiedDiff};
use crate::folder_boundary::FolderBoundary;
use crate::folder_listing;
use crate::patch::{Edit, PatchApplier, PatchOutcome};
use crate::tool::{Tool, ToolContext, ToolResult, Undo};
use baudet_core::item::ItemKind;
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use same_file::Handle;
use serde_json::{json, Value};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Default)]
pub struct EditFileTool;

impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Change one file in the project folder by replacing exact text. \
         Give the exact old_text to find and the new_text to put in its place. \
         old_text must appear once, or be unambiguous when replace_all is true."
    }

    fn item_kind(&self) -> ItemKind {
        ItemKind::FileChange
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file": { "type": "string", "description": "a file inside the project folder" },
                "edits": {
                    "type": "array",
                    "description": "the edits to apply, in order",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_text": { "type": "string" },
                            "new_text": { "type": "string" },
                            "replace_all": { "type": "boolean" }
                        },
                        "required": ["old_text", "new_text"]
                    }
                }
            },
            "required": ["file", "edits"]
        })
    }

    // Recheck the file after approval before writing a patch.
    fn execute(&self, args: &Value, ctx: &ToolContext) -> ToolResult {
        let Some(rel) = args.get("file").and_then(Value::as_str) else {
            return fail("needs a file argument");
        };
        let Some(root) = ctx.root.as_deref() else {
            return fail("no project folder is linked to write into");
        };
        if !ctx.write_granted {
            return fail("write access to the project folder has not been granted");
        }
        if !ctx.read_granted {
            return fail("read access is required to verify the edit");
        }

        let edits = parse_edits(args.get("edits"));
        if edits.is_empty() {
            return fail("needs at least one edit");
        }

        let resolved = match FolderBoundary::resolve_file_within(root, rel) {
            crate::folder_boundary::Resolved::Inside(path) => path,
            crate::folder_boundary::Resolved::Refused(reason) => return fail(reason),
        };

        let mut target = match EditTarget::open(root, resolved) {
            Ok(target) => target,
            Err(_) => return fail(format!("{rel} could not be read.{}", folder_listing::hint(root))),
        };
        let original = match contents(&mut target.preview) {
            Ok(text) => text,
            Err(_) => return fail(format!("{rel} could not be read.{}", folder_listing::hint(root))),
        };

        match PatchApplier::apply(&original, &edits) {
            PatchOutcome::Rejected { reason } => fail(reason),
            PatchOutcome::Applied { content } => write_if_approved(ctx, target, rel, &original, &content),
        }
    }
}

struct EditTarget {
    project: Dir,
    root: PathBuf,
    resolved: PathBuf,
    preview: Handle,
}

impl EditTarget {
    fn open(root: &Path, resolved: PathBuf) -> io::Result<Self> {
        let root = root.canonicalize()?;
        let project = Dir::open_ambient_dir(&root, ambient_authority())?;
        let preview = Self::open_file(&project, &root, &resolved, false)?;
        Ok(Self { project, root, resolved, preview })
    }

    fn open_file(project: &Dir, root: &Path, resolved: &Path, writable: bool) -> io::Result<Handle> {
        // Canonicalization supports in-project absolute symlinks. The directory
        // handle, rather than the resolved string, enforces containment at open time.
        let canonical = resolved.canonicalize()?;
        let relative = canonical
            .strip_prefix(root)
            .map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, "path leaves the project folder"))?;
        let file = project.open_with(relative, OpenOptions::new().read(true).write(writable))?.into_std();
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "target is not a regular file"));
        }
        Handle::from_file(file)
    }

    fn reopen(&self, writable: bool) -> io::Result<Handle> {
        let original_root = Handle::from_file(self.project.try_clone()?.into_std_file())?;
        let current_root = Dir::open_ambient_dir(&self.root, ambient_authority())?;
        if Handle::from_file(current_root.into_std_file())? != original_root {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "project folder changed since the preview"));
        }
        Self::open_file(&self.project, &self.root, &self.resolved, writable)
    }
}

fn contents(handle: &mut Handle) -> io::Result<String> {
    let file = handle.as_file_mut();
    file.rewind()?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(text)
}

fn replace_contents(handle: &mut Handle, text: &str) -> io::Result<()> {
    let file = handle.as_file_mut();
    file.rewind()?;
    file.write_all(text.as_bytes())?;
    file.set_len(text.len() as u64)
}

fn write_if_approved(ctx: &ToolContext, target: EditTarget, rel: &str, original: &str, updated: &str) -> ToolResult {
    if original == updated {
        return ToolResult::ok(format!("no change: new_text equals old_text for {rel}"));
    }
    let diff = UnifiedDiff::render(&format!("a/{rel}"), &format!("b/{rel}"), &LineDiff::diff(original, updated));
    if !(ctx.approve)(rel, &diff) {
        return ToolResult {
            ok: false,
            output: format!("the user declined the change to {rel}; do not retry it, tell the user instead"),
            diff: Some(diff),
            ..Default::default()
        };
    }
    let mut file = match target.reopen(true) {
        Ok(file) if file == target.preview => file,
        Ok(_) => return fail(format!("{rel} no longer refers to the file shown in the preview")),
        Err(error) => return fail(format!("{rel} could not be safely opened for writing: {error}")),
    };
    let current = match contents(&mut file) {
        Ok(text) => text,
        Err(_) => return fail(format!("{rel} could not be re-read before writing")),
    };
    if current != original {
        return fail(format!("{rel} changed since the preview; not overwriting a newer edit"));
    }
    if let Err(error) = replace_contents(&mut file, updated) {
        return fail(format!("writing {rel} failed: {error}"));
    }
    let file = Mutex::new(file);
    let restore = original.to_owned();
    let expected = updated.to_owned();
    let undo: Undo = Box::new(move || {
        let Ok(mut file) = file.lock() else { return };
        if target.reopen(false).is_ok_and(|current| current == *file)
            && contents(&mut file).is_ok_and(|current| current == expected)
        {
            let _ = replace_contents(&mut file, &restore);
        }
    });
    ToolResult { ok: true, output: format!("wrote {rel}"), diff: Some(diff), undo: Some(undo), ..Default::default() }
}

fn parse_edits(value: Option<&Value>) -> Vec<Edit> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let old_text = item.get("old_text")?.as_str()?.to_owned();
            let new_text = item.get("new_text")?.as_str()?.to_owned();
            let replace_all = item.get("replace_all").and_then(Value::as_bool).unwrap_or(false);
            Some(Edit { old_text, new_text, replace_all })
        })
        .collect()
}

fn fail(reason: impl Into<String>) -> ToolResult {
    ToolResult::fail(format!("edit_file: {}", reason.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::path::{Path as StdPath, PathBuf as StdPathBuf};
    use std::sync::{Arc, Mutex};

    struct Recorder {
        asked: Mutex<Vec<(String, String)>>,
        answer: bool,
    }

    impl Recorder {
        fn new(answer: bool) -> Arc<Self> {
            Arc::new(Recorder { asked: std::sync::Mutex::new(Vec::new()), answer })
        }
    }

    fn write_ctx(root: &StdPath, recorder: &Arc<Recorder>) -> ToolContext {
        let shared = Arc::clone(recorder);
        ToolContext {
            root: Some(root.to_path_buf()),
            write_granted: true,
            read_granted: true,
            approve: Box::new(move |rel: &str, diff: &str| {
                shared.asked.lock().unwrap().push((rel.to_owned(), diff.to_owned()));
                shared.answer
            }),
            ..Default::default()
        }
    }

    fn edit(root: &StdPath, args: Value, recorder: &Arc<Recorder>) -> ToolResult {
        EditFileTool.execute(&args, &write_ctx(root, recorder))
    }

    fn edit_with(answer: bool, root: &StdPath, args: Value) -> (ToolResult, Arc<Recorder>) {
        let recorder = Recorder::new(answer);
        let result = edit(root, args, &recorder);
        (result, recorder)
    }

    #[test]
    fn it_refuses_without_a_file_an_edits_or_a_folder_or_write() {
        let recorder = Recorder::new(true);
        let dir = tempfile::tempdir().unwrap();

        let no_file = edit(dir.path(), json!({"edits": [{"old_text": "a", "new_text": "b"}]}), &recorder);
        assert_eq!(no_file.output, "edit_file: needs a file argument");

        let no_edits = edit(dir.path(), json!({"file": "x.txt"}), &recorder);
        assert_eq!(no_edits.output, "edit_file: needs at least one edit");

        let ctx = ToolContext::default();
        let no_folder =
            EditFileTool.execute(&json!({"file": "x.txt", "edits": [{"old_text": "a", "new_text": "b"}]}), &ctx);
        assert_eq!(no_folder.output, "edit_file: no project folder is linked to write into");

        let ctx = ToolContext { root: Some(dir.path().to_path_buf()), ..Default::default() };
        let no_write =
            EditFileTool.execute(&json!({"file": "x.txt", "edits": [{"old_text": "a", "new_text": "b"}]}), &ctx);
        assert_eq!(no_write.output, "edit_file: write access to the project folder has not been granted");
    }

    #[test]
    fn it_refuses_a_path_that_leaves_the_folder_and_never_asks() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = Recorder::new(true);
        let result = edit(
            dir.path(),
            json!({"file": "../buiten.txt", "edits": [{"old_text": "a", "new_text": "b"}]}),
            &recorder,
        );
        assert!(!result.ok);
        assert!(result.output.starts_with("edit_file: "), "{}", result.output);
        assert!(recorder.asked.lock().unwrap().is_empty(), "buiten de map mag er niet eens om gevraagd worden");
    }

    #[test]
    fn it_hints_the_folder_listing_when_the_file_cannot_be_read() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("bestaand.txt"), "x\n").unwrap();
        let recorder = Recorder::new(true);
        let result = edit(
            dir.path(),
            json!({"file": "ontbreekt.txt", "edits": [{"old_text": "a", "new_text": "b"}]}),
            &recorder,
        );
        assert_eq!(
            result.output,
            format!("edit_file: ontbreekt.txt does not exist inside the folder.{}", folder_listing::hint(dir.path()))
        );
        assert!(result.output.contains("bestaand.txt"), "de opsomming moet het echte bestand noemen");
    }

    #[test]
    fn it_shows_a_unified_diff_and_asks_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("notes.txt"), "eerste regel\n").unwrap();
        let (result, recorder) = edit_with(
            true,
            dir.path(),
            json!({"file": "notes.txt", "edits": [{"old_text": "eerste", "new_text": "tweede"}]}),
        );

        assert!(result.ok);
        assert_eq!(result.output, "wrote notes.txt");
        assert_eq!(result.diff.as_deref(), Some("--- a/notes.txt\n+++ b/notes.txt\n-eerste regel\n+tweede regel\n \n"));
        let asked = recorder.asked.lock().unwrap();
        assert_eq!(asked.len(), 1, "één vraag per schrijfbare run");
        assert_eq!(asked[0].0, "notes.txt");
        assert_eq!(asked[0].1, *result.diff.as_ref().unwrap(), "de vraag toont dezelfde tekst als de kaart");
        assert_eq!(fs::read_to_string(dir.path().join("notes.txt")).unwrap(), "tweede regel\n");
    }

    #[test]
    fn a_decline_writes_nothing_and_says_do_not_retry() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("notes.txt"), "eerste regel\n").unwrap();
        let (result, _) = edit_with(
            false,
            dir.path(),
            json!({"file": "notes.txt", "edits": [{"old_text": "eerste", "new_text": "tweede"}]}),
        );
        assert!(!result.ok);
        assert_eq!(result.output, "the user declined the change to notes.txt; do not retry it, tell the user instead");
        assert!(result.diff.is_some());
        assert_eq!(fs::read_to_string(dir.path().join("notes.txt")).unwrap(), "eerste regel\n");
    }

    #[test]
    fn an_edit_that_changes_nothing_writes_and_asks_nothing() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("notes.txt"), "ongewijzigd\n").unwrap();
        let (result, recorder) = edit_with(
            true,
            dir.path(),
            json!({"file": "notes.txt", "edits": [{"old_text": "ongewijzigd", "new_text": "ongewijzigd"}]}),
        );
        assert!(result.ok);
        assert_eq!(result.output, "no change: new_text equals old_text for notes.txt");
        assert!(result.diff.is_none());
        assert!(recorder.asked.lock().unwrap().is_empty());
    }

    #[test]
    fn it_refuses_to_overwrite_an_edit_that_landed_since_the_preview() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.txt");
        fs::write(&file, "eerste regel\n").unwrap();

        let recorder = Arc::new(Recorder { asked: std::sync::Mutex::new(Vec::new()), answer: true });
        let shared = Arc::clone(&recorder);
        let path: StdPathBuf = file.clone();
        let ctx = ToolContext {
            root: Some(dir.path().to_path_buf()),
            write_granted: true,
            read_granted: true,
            approve: Box::new(move |rel: &str, diff: &str| {
                shared.asked.lock().unwrap().push((rel.to_owned(), diff.to_owned()));
                fs::write(&path, "handmatig gewijzigd\n").unwrap();
                true
            }),
            ..Default::default()
        };
        let result = EditFileTool
            .execute(&json!({"file": "notes.txt", "edits": [{"old_text": "eerste", "new_text": "tweede"}]}), &ctx);
        assert!(!result.ok);
        assert_eq!(result.output, "edit_file: notes.txt changed since the preview; not overwriting a newer edit");
        assert_eq!(fs::read_to_string(&file).unwrap(), "handmatig gewijzigd\n");
    }

    #[test]
    fn it_refuses_a_replacement_file_even_when_its_contents_match_the_preview() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.txt");
        let original = dir.path().join("original.txt");
        fs::write(&file, "before").unwrap();
        let mut ctx = write_ctx(dir.path(), &Recorder::new(true));
        let (current, parked) = (file.clone(), original.clone());
        ctx.approve = Box::new(move |_, _| {
            fs::rename(&current, &parked).unwrap();
            fs::write(&current, "before").unwrap();
            true
        });

        let result = EditFileTool
            .execute(&json!({"file": "notes.txt", "edits": [{"old_text": "before", "new_text": "after"}]}), &ctx);
        assert!(!result.ok);
        assert!(result.output.contains("no longer refers to the file shown in the preview"), "{}", result.output);
        assert_eq!(fs::read_to_string(&file).unwrap(), "before");
        assert_eq!(fs::read_to_string(&original).unwrap(), "before");
    }

    #[cfg(unix)]
    #[cfg(unix)]
    #[test]
    fn it_refuses_an_approved_path_whose_parent_now_points_outside_the_project() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        let parked = dir.path().join("original");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("notes.txt"), "before").unwrap();
        fs::write(outside.path().join("notes.txt"), "before").unwrap();
        let mut ctx = write_ctx(dir.path(), &Recorder::new(true));
        let (backup, external) = (parked.clone(), outside.path().to_path_buf());
        ctx.approve = Box::new(move |_, _| {
            fs::rename(&sub, &backup).unwrap();
            std::os::unix::fs::symlink(&external, &sub).unwrap();
            true
        });

        let result = EditFileTool
            .execute(&json!({"file": "sub/notes.txt", "edits": [{"old_text": "before", "new_text": "after"}]}), &ctx);
        assert!(!result.ok);
        assert_eq!(fs::read_to_string(outside.path().join("notes.txt")).unwrap(), "before");
        assert_eq!(fs::read_to_string(parked.join("notes.txt")).unwrap(), "before");
    }

    #[cfg(unix)]
    #[test]
    fn it_refuses_a_renamed_project_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        let moved = dir.path().join("moved-project");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("notes.txt"), "before").unwrap();
        let mut ctx = write_ctx(&root, &Recorder::new(true));
        let (from, to) = (root.clone(), moved.clone());
        ctx.approve = Box::new(move |_, _| {
            fs::rename(&from, &to).unwrap();
            fs::create_dir(&from).unwrap();
            fs::write(from.join("notes.txt"), "before").unwrap();
            true
        });

        let result = EditFileTool
            .execute(&json!({"file": "notes.txt", "edits": [{"old_text": "before", "new_text": "after"}]}), &ctx);
        assert!(!result.ok);
        assert!(result.output.contains("project folder changed since the preview"), "{}", result.output);
        assert_eq!(fs::read_to_string(moved.join("notes.txt")).unwrap(), "before");
        assert_eq!(fs::read_to_string(root.join("notes.txt")).unwrap(), "before");
    }

    #[test]
    fn a_landed_write_can_be_undone_to_the_original_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.txt");
        fs::write(&file, "eerste regel\n").unwrap();
        let (result, _) = edit_with(
            true,
            dir.path(),
            json!({"file": "notes.txt", "edits": [{"old_text": "eerste", "new_text": "tweede"}]}),
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), "tweede regel\n");
        let undo = result.undo.expect("een gelande write draagt zijn handgreep mee");
        undo();
        assert_eq!(fs::read_to_string(&file).unwrap(), "eerste regel\n");
    }

    #[test]
    fn undo_refuses_a_newer_edit_or_a_replacement_file() {
        for replace_file in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("notes.txt");
            fs::write(&file, "original content").unwrap();
            let (result, _) = edit_with(
                true,
                dir.path(),
                json!({"file": "notes.txt", "edits": [{"old_text": "original content", "new_text": "updated"}]}),
            );
            assert_eq!(fs::read_to_string(&file).unwrap(), "updated");
            let undo = result.undo.unwrap();
            let expected = if replace_file {
                fs::rename(&file, dir.path().join("original.txt")).unwrap();
                "updated"
            } else {
                "newer user edit"
            };
            fs::write(&file, expected).unwrap();
            undo();
            assert_eq!(fs::read_to_string(&file).unwrap(), expected);
            if replace_file {
                assert_eq!(fs::read_to_string(dir.path().join("original.txt")).unwrap(), "updated");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_absolute_link_to_a_file_inside_the_project_remains_editable() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.txt");
        fs::write(&file, "before").unwrap();
        std::os::unix::fs::symlink(&file, dir.path().join("alias.txt")).unwrap();
        let (result, _) = edit_with(
            true,
            dir.path(),
            json!({"file": "alias.txt", "edits": [{"old_text": "before", "new_text": "after"}]}),
        );
        assert!(result.ok, "{}", result.output);
        assert_eq!(fs::read_to_string(&file).unwrap(), "after");
    }

    #[test]
    fn given_write_permission_without_read_permission_when_editing_then_the_preview_is_refused() {
        let project = tempfile::tempdir().unwrap();
        let path = project.path().join("notes.txt");
        std::fs::write(&path, "private note").unwrap();
        let context = ToolContext::for_folder(project.path(), false, true, false);
        let result = EditFileTool.execute(&json!({
            "file": "notes.txt",
            "edits": [{"old_text": "private note", "new_text": "updated"}]
        }), &context);
        assert!(!result.ok && result.diff.is_none());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "private note");
    }

    #[test]
    fn it_applies_several_edits_in_order_through_one_approval() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("list.txt"), "a\nb\nc\n").unwrap();
        let (result, recorder) = edit_with(
            true,
            dir.path(),
            json!({"file": "list.txt", "edits": [
                {"old_text": "a", "new_text": "A"},
                {"old_text": "b", "new_text": "B"}
            ]}),
        );
        assert!(result.ok);
        assert_eq!(fs::read_to_string(dir.path().join("list.txt")).unwrap(), "A\nB\nc\n");
        assert_eq!(recorder.asked.lock().unwrap().len(), 1, "twee edits zijn één vraag, niet twee");
    }

    #[test]
    fn a_refused_patch_reaches_the_model_without_asking_and_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("notes.txt"), "x x\n").unwrap();
        let (result, recorder) =
            edit_with(true, dir.path(), json!({"file": "notes.txt", "edits": [{"old_text": "x", "new_text": "y"}]}));
        assert_eq!(
            result.output,
            "edit_file: edit 1: old_text matches 2 times; add surrounding context or set replace_all"
        );
        assert!(recorder.asked.lock().unwrap().is_empty());
        assert_eq!(fs::read_to_string(dir.path().join("notes.txt")).unwrap(), "x x\n");
    }

    #[test]
    fn a_junk_element_is_dropped_and_the_good_ones_still_land() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("notes.txt"), "eerste\ntweede\n").unwrap();
        let (result, _) = edit_with(
            true,
            dir.path(),
            json!({"file": "notes.txt", "edits": [
                {"old_text": "tweede"},
                {"old_text": "eerste", "new_text": "EERSTE"}
            ]}),
        );
        assert!(result.ok, "{}", result.output);
        assert_eq!(fs::read_to_string(dir.path().join("notes.txt")).unwrap(), "EERSTE\ntweede\n");
    }

    #[test]
    fn it_declares_the_card_of_a_file_change_and_stays_serial() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = write_ctx(dir.path(), &Recorder::new(true));
        assert_eq!(EditFileTool.item_kind(), ItemKind::FileChange);
        assert!(
            !EditFileTool.can_run_parallel(&json!({"file": "notes.txt"}), &ctx),
            "schrijven mag nooit naast zijn buren racen"
        );
    }

    #[test]
    fn its_schema_requires_both_arguments_and_names_both_edit_texts() {
        let schema = EditFileTool.parameters();
        assert_eq!(schema["required"], json!(["file", "edits"]));
        assert_eq!(schema["properties"]["edits"]["items"]["required"], json!(["old_text", "new_text"]));
        assert_eq!(schema["properties"]["edits"]["items"]["properties"]["replace_all"]["type"], "boolean");
    }
}
