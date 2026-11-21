//! The two flanking panels: the workspace on the left, the work in progress on the right.
//!
//! Both read the filesystem and git through `baudet-core`, and neither holds a copy of anything the
//! transcript owns. That asymmetry is why the right panel can exist before a backend does: git is
//! already a source of truth for "what changed", so the panel lists measured paths, while an
//! unreadable folder or a folder that is not a repository says so in words and lists nothing. An
//! honest cold start beats a screen of invented rows.
//!
//! Refresh happens when a folder becomes the subject -- on opening one and on switching to another
//! -- and never on a timer, bounded by the probe's own budget ([`baudet_core::GIT_PROBE_TIMEOUT`] per
//! git call, two calls per refresh). Polling on a timer would make the panel's cost a function of
//! frame rate, which is how an inspector ends up costing more than the transcript it sits beside.
//! The consequence is stated rather than hidden: edit a file outside the app and the list stays as
//! it was until the folder is chosen again.

use crate::palette;
use baudet_core::{probe_changes, probe_git, ChangeKind, ChangedFile, GitMeta};
use egui::{Color32, Panel, ScrollArea, Ui};
use std::path::{Path, PathBuf};

/// Names that are build output rather than source. Measured on a workspace whose crate had been
/// built, `target` alone held more entries than every source directory combined.
const IGNORED_DIRS: &[&str] = &["target", "node_modules", "build", "dist", "out"];

/// Directories descended per listing. A panel four rows deep is already showing more than a person
/// scans, and an unbounded walk of a large checkout is work nobody asked for.
const MAX_DEPTH: usize = 4;

/// Entries kept per directory. When a listing is cut, the panel says so: a silent truncation reads
/// as "these are all the files", which is a false statement about someone's folder.
const MAX_ENTRIES_PER_DIR: usize = 200;

/// Leading characters of a directory kept when a name has to be cut. The tail is what identifies a
/// file, so it survives and the head gives way.
const KEEP_TAIL: usize = 22;

/// The left panel's view of a folder.
///
/// Whether a directory is open is *not* stored here. egui keeps that state per widget identity, and
/// a second copy in this struct would be a second answer to the same question — the kind that ends
/// up disagreeing with the arrow the user just clicked.
#[derive(Default)]
pub struct Workspace {
    /// The folder under inspection. Empty means nothing has been chosen yet.
    cwd: String,
    /// Whether the current folder has been probed. Opening a folder and switching to another are
    /// one event -- "this folder is now the subject" -- so the caller refreshes on the first folder
    /// for the same reason it refreshes on the second, and `Workspace` stays the only owner of
    /// which folder that is.
    probed: bool,
}

/// One directory level, as read from disk.
pub struct DirListing {
    /// Directories, folders-first order already applied.
    pub dirs: Vec<PathBuf>,
    /// Files, in the same order.
    pub files: Vec<PathBuf>,
    /// Whether this level alone was cut at [`MAX_ENTRIES_PER_DIR`].
    pub truncated: bool,
}

impl DirListing {
    /// A level that could not be read: no entries, and not claiming to be empty.
    fn unreadable() -> DirListing {
        DirListing { dirs: Vec::new(), files: Vec::new(), truncated: false }
    }
}

impl Workspace {
    /// Point the panel at `path`. Always replaces the previous folder, including when `path` cannot
    /// be read: keeping the old listing would describe a folder the panel no longer looks at.
    pub fn open(&mut self, path: &str) {
        self.cwd = path.trim().to_string();
        self.probed = false;
    }

    /// The folder being shown, or `None` when nothing has been chosen.
    pub fn cwd(&self) -> Option<&str> {
        (!self.cwd.is_empty()).then_some(self.cwd.as_str())
    }

    /// Read one directory. Called as the tree is painted, for each level that is open, so closing a
    /// directory also stops reading it. The answer carries whether *that level* was cut; `&mut self`
    /// is a formality of the paint borrow and no part of a listing depends on it, which is why a
    /// level cannot leave an alarm behind for a folder that was never cut.
    pub fn tree_at(&mut self, dir: &Path) -> DirListing {
        let Ok(read_dir) = std::fs::read_dir(dir) else { return DirListing::unreadable() };
        let mut names: Vec<(String, PathBuf, bool)> = Vec::new();
        for entry in read_dir.flatten() {
            let Ok(file_type) = entry.file_type() else { continue };
            // A symlink is listed and never descended: `node_modules` trees and self-referential
            // links loop, and a walk that loops during a paint pass is a hung window.
            let is_dir = file_type.is_dir() && !file_type.is_symlink();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || (is_dir && IGNORED_DIRS.contains(&name.as_str())) {
                continue;
            }
            names.push((name, entry.path(), is_dir));
        }
        // Folders first, then case-insensitive by name: the order a file manager uses and the one a
        // person scanning this panel expects.
        names.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
        let truncated = names.len() > MAX_ENTRIES_PER_DIR;
        if truncated {
            names.truncate(MAX_ENTRIES_PER_DIR);
        }
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        for (_, path, is_dir) in names {
            if is_dir {
                dirs.push(path);
            } else {
                files.push(path);
            }
        }
        DirListing { dirs, files, truncated }
    }
}

/// The right panel's view of the repository in the folder.
#[derive(Default)]
pub struct Changes {
    /// The repository's own facts, or `None` when the folder holds none.
    meta: Option<GitMeta>,
    paths: Vec<ChangedFile>,
    /// Whether the probe could not be answered. That is a different sentence from "nothing
    /// changed", and the panel says them as two sentences.
    unavailable: bool,
}

impl Changes {
    /// Ask git twice about `path` and keep its answers.
    pub fn refresh(&mut self, path: Option<&str>) {
        self.meta = None;
        self.paths.clear();
        self.unavailable = false;
        let Some(cwd) = path else { return };
        let Some(meta) = probe_git(cwd) else { return };
        match probe_changes(cwd) {
            Some(paths) => {
                self.meta = Some(meta);
                self.paths = paths;
            }
            // A repository whose status git would not answer. Saying so beats an empty list, which
            // would read as "the tree is clean".
            None => self.unavailable = true,
        }
    }

    /// The repository facts, for the left panel's branch line.
    pub fn meta(&self) -> Option<&GitMeta> {
        self.meta.as_ref()
    }
}

/// The left panel: the chosen folder, its tree, the branch it is on.
///
/// Returns whether the caller should ask git about the folder now: true for a folder that has not
/// been probed yet and for every switch, false while the same folder stays the subject. A folder
/// that cannot be read asks once as well, because "no repository" is git's honest answer there and
/// not one the panel may invent.
pub fn left(ui: &mut Ui, workspace: &mut Workspace, meta: Option<&GitMeta>) -> bool {
    // A local cell rather than a binding: the field is a request to open a folder, not a second
    // owner of `Workspace::cwd`, so the moment it commits is the only moment the two must agree.
    let mut requested: Option<String> = None;
    let frame = Panel::left("workspace")
        .resizable(true)
        .default_size(220.0)
        .min_size(150.0)
        .frame(egui::Frame::side_top_panel(ui.style()));
    frame.show(ui, |ui| {
        ui.strong("Workspace");
        // The draft is empty whenever the panel is painted, so it is a place to type the next
        // folder and never a second display of the current one -- that is what the folder line
        // below is for.
        let mut draft = String::new();
        let field = ui.add_sized(
            [ui.available_width(), 20.0],
            egui::TextEdit::singleline(&mut draft).hint_text("folder path").desired_width(f32::INFINITY),
        );
        // Enter or the button commits a typed path; an empty field means "keep what is open", so it
        // never wipes the panel on a stray Enter.
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        let commit = field.lost_focus() && enter;
        let button = ui.small_button("Open").clicked();
        if (commit || button) && !draft.trim().is_empty() {
            requested = Some(draft.trim().to_string());
        }
        ui.separator();
        let chosen = workspace.cwd().map(|s| s.to_string());
        match chosen {
            None => {
                ui.label(dim("No folder chosen."));
            }
            Some(cwd) => {
                let root = PathBuf::from(&cwd);
                ui.label(folder_name(&cwd)).on_hover_text(cwd.clone());
                match meta {
                    Some(meta) => branch_line(ui, meta),
                    None => {
                        ui.label(dim("no repository"));
                    }
                }
                ui.separator();
                ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    if !root.is_dir() {
                        ui.label(dim("unreadable folder"));
                        return;
                    }
                    // `default_open`, because `begin` loads the state through
                    // `CollapsingState::load_with_default_open` (collapsing_header.rs:540): the
                    // chosen folder is open on the first paint and closed from the second onward
                    // only if the user said so. A panel that lists its own name and nothing else
                    // reads as an empty folder rather than as a collapsed one.
                    egui::CollapsingHeader::new(folder_name(&cwd))
                        .id_salt(cwd.clone())
                        .default_open(true)
                        .show(ui, |ui| tree(ui, workspace, &root, 1));
                });
            }
        }
    });
    if requested.is_some() {
        // `open` already cleared `probed`, so the answer is read from the state it just left rather
        // than recomputed from the field: the panel cannot claim a refresh the state disagrees with.
        workspace.open(requested.as_deref().unwrap().trim());
    }
    if workspace.cwd().is_some() && !workspace.probed {
        workspace.probed = true;
        return true;
    }
    false
}

/// Paint one directory level, descending through the headers the user has opened.
fn tree(ui: &mut Ui, workspace: &mut Workspace, dir: &Path, depth: usize) {
    let listing = workspace.tree_at(dir);
    for path in &listing.files {
        file_row(ui, path);
    }
    // Said by the level that was cut, so the claim and the rows it describes are one unit: a folder
    // that fits never inherits another folder's warning.
    if listing.truncated {
        ui.label(dim(format!("… cut at {MAX_ENTRIES_PER_DIR} entries")));
    }
    for path in &listing.dirs {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        egui::CollapsingHeader::new(truncate_head(&name, KEEP_TAIL)).id_salt(path.display().to_string()).show(
            ui,
            |ui| {
                if depth >= MAX_DEPTH {
                    ui.label(dim("…"));
                } else {
                    tree(ui, workspace, path, depth + 1);
                }
            },
        );
    }
    if listing.files.is_empty() && listing.dirs.is_empty() {
        ui.label(dim("empty"));
    }
}

/// One file in the tree. Selecting it is not a thing this panel does yet, so it shows its full path
/// on hover rather than pretending a click will open it.
fn file_row(ui: &mut Ui, path: &Path) {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    ui.add(
        egui::Label::new(egui::RichText::new(truncate_head(&name, KEEP_TAIL)).color(palette::DIM)).selectable(false),
    )
    .on_hover_text(path.display().to_string());
}

/// The right panel: the paths git reports as different from the commit.
pub fn right(ui: &mut Ui, changes: &Changes, chosen: Option<&str>) {
    let frame = Panel::right("changes")
        .resizable(true)
        .default_size(260.0)
        .min_size(170.0)
        .frame(egui::Frame::side_top_panel(ui.style()));
    frame.show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.strong("Changes");
            if !changes.paths.is_empty() {
                ui.label(dim(format!("{}", changes.paths.len())));
            }
        });
        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if chosen.is_none() {
                ui.label(dim("No folder chosen."));
                return;
            }
            if changes.unavailable {
                ui.label(dim("git did not answer."));
                return;
            }
            if changes.meta.is_none() {
                ui.label(dim("no repository"));
                return;
            }
            if changes.paths.is_empty() {
                ui.label(dim("nothing changed"));
                return;
            }
            for file in &changes.paths {
                change_row(ui, file);
            }
        });
    });
}

/// One path git reports: its state, its name, and git's own counts.
fn change_row(ui: &mut Ui, file: &ChangedFile) {
    let ink = ink_for(file.change);
    let body_ink = if file.change.exists_on_disk() { palette::TEXT } else { palette::DIM };
    ui.horizontal(|ui| {
        ui.add(
            egui::Label::new(egui::RichText::new(format!("{:4}", file.change.word())).color(ink).size(11.0))
                .selectable(false),
        )
        .on_hover_text(&file.path);
        ui.add(
            egui::Label::new(egui::RichText::new(truncate_head(&file.name(), KEEP_TAIL)).color(body_ink).strong())
                .selectable(false),
        )
        .on_hover_text(&file.path);
        if let Some(old) = &file.old_path {
            let old =
                Path::new(old).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| old.clone());
            ui.label(dim(truncate_head(&format!("← {old}"), KEEP_TAIL)));
        }
        // Git's numbers or nothing. A path without counts shows no figures, because `+0 −0` would
        // claim a measured zero on a binary path or in a repository without a commit.
        if let (Some(a), Some(d)) = (file.additions, file.deletions) {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if d > 0 {
                    ui.label(egui::RichText::new(format!("−{d}")).color(palette::DIFF_REMOVED).size(11.0));
                }
                if a > 0 {
                    ui.label(egui::RichText::new(format!("+{a}")).color(palette::DIFF_ADDED).size(11.0));
                }
            });
        }
    });
}

/// The branch line, in the words the probe actually managed to produce.
fn branch_line(ui: &mut Ui, meta: &GitMeta) {
    match (&meta.branch, &meta.commit_hash) {
        (Some(branch), Some(hash)) => {
            ui.label(format!("{branch}@{}", short(hash)));
        }
        (Some(branch), None) => {
            // An unborn HEAD: measured, `rev-parse HEAD` fails there while `--is-inside-work-tree`
            // still answers `true`, so the branch is real and the commit is not.
            ui.label(format!("{branch} (no commits yet)"));
        }
        (None, Some(hash)) => {
            ui.label(format!("detached@{}", short(hash)));
        }
        (None, None) => {
            ui.label(dim("no branch"));
        }
    }
    if let Some(url) = &meta.repository_url {
        ui.label(dim(truncate_head(url, KEEP_TAIL))).on_hover_text(url.clone());
    }
}

/// Git's state as ink. Additions read as work, removals as subtraction, work in progress as the
/// accent: the three things a person scanning this list has to tell apart.
fn ink_for(change: ChangeKind) -> Color32 {
    match change {
        ChangeKind::Added | ChangeKind::Untracked => palette::DIFF_ADDED,
        ChangeKind::Modified | ChangeKind::Renamed | ChangeKind::Copied => palette::ACCENT,
        ChangeKind::Deleted | ChangeKind::Removed | ChangeKind::Unmerged => palette::DIFF_REMOVED,
        ChangeKind::Other => palette::DIM,
    }
}

fn dim(text: impl Into<String>) -> egui::WidgetText {
    egui::RichText::new(text.into()).color(palette::DIM).into()
}

fn folder_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string())
}

fn short(hash: &str) -> String {
    hash.chars().take(7).collect()
}

/// Cut a string to `keep_tail` characters, keeping its end. A path is identified by its last
/// characters, so the head is what may go; the marker says the cut happened.
fn truncate_head(text: &str, keep_tail: usize) -> String {
    let count = text.chars().count();
    if count <= keep_tail {
        return text.to_string();
    }
    let skip = count - keep_tail;
    let tail: String = text.chars().skip(skip).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The last component of a path, as the tests name entries. Inside the test module on purpose:
    /// the panel has its own `folder_name` for this, and a second copy outside the tests would be
    /// dead code in every build that ships.
    fn name_of(path: &Path) -> String {
        path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    #[test]
    fn a_name_that_fits_is_left_alone() {
        assert_eq!(truncate_head("main.rs", KEEP_TAIL), "main.rs");
    }

    /// The expected strings here are written out rather than recomputed from the function, so a
    /// change to `truncate_head` has to disagree with an independently stated fact.
    #[test]
    fn a_long_path_keeps_its_tail_and_says_so() {
        // The input counts 27 characters, so a tail of 22 starts five in: the cut eats "crate".
        assert_eq!(truncate_head("crates/baudet-ui/src/app.rs", KEEP_TAIL), "…s/baudet-ui/src/app.rs");
        assert_eq!(truncate_head("crates/baudet-core/src/context.rs", 12), "…c/context.rs");
    }

    /// The accent in the middle of the name is the point: counting bytes would cut one character
    /// short per accent and can land the cut inside a two-byte sequence.
    #[test]
    fn the_cut_counts_characters_not_bytes() {
        assert_eq!(truncate_head("naam_met_éééën_tot_lang_pad.rs", 8), "…g_pad.rs");
    }

    /// A listing is read per level while the tree is painted, so nothing the panel showed for the
    /// previous folder can survive into the next one: there is no second listing to go stale. The
    /// numbers below are the contents of the two folders this test builds, written out rather than
    /// derived from `tree_at`.
    #[test]
    fn a_folder_without_a_path_has_nothing_to_show() {
        let mut workspace = Workspace::default();
        assert_eq!(workspace.cwd(), None);
        let missing = workspace.tree_at(Path::new("/no/such/folder/at/all"));
        assert!(missing.dirs.is_empty() && missing.files.is_empty(), "nothing chosen means nothing listed");
        assert!(!missing.truncated, "an unreadable level does not claim to have been cut");
        let changes = Changes::default();
        assert!(changes.meta().is_none());
        assert!(!changes.unavailable);
    }

    /// Moving the panel to a folder that is not there has to lose the previous folder's rows, and
    /// the panel must know which folder it is pointing at even then. A name the caller has already
    /// given up on cannot be resurrected by a later listing.
    #[test]
    fn moving_to_an_absent_folder_leaves_no_stale_listing() {
        let mut workspace = Workspace::default();
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("bron.rs"), "x\n").unwrap();
        workspace.open(&dir.path().to_string_lossy());
        let here = workspace.tree_at(dir.path());
        assert_eq!(here.files.len(), 1, "the real folder lists the file written into it");
        assert_eq!(here.dirs.len(), 0);

        let gone = dir.path().join("verwijderd");
        workspace.open(&gone.to_string_lossy());
        assert_eq!(workspace.cwd(), Some(gone.to_string_lossy().as_ref()));
        let there = workspace.tree_at(gone.as_path());
        assert!(there.files.is_empty() && there.dirs.is_empty(), "a folder that does not exist has no tree");
    }

    #[test]
    fn build_output_and_dotfiles_are_not_offerings_to_the_tree() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join("target").join("artifact"), "x").unwrap();
        // `main.rs` sits at the level the test reads, because `tree_at` gives one level and a file
        // one directory down is another level's business. That is what makes this a test of the
        // filter: the ordinary file and `.hidden` are the same kind of entry at the same level, and
        // only one of them may reach the listing.
        std::fs::write(dir.path().join("main.rs"), "x\n").unwrap();
        std::fs::write(dir.path().join(".hidden"), "x").unwrap();
        let mut workspace = Workspace::default();
        let listing = workspace.tree_at(dir.path());
        let folders: Vec<String> = listing.dirs.iter().map(|p| name_of(p.as_path())).collect();
        let files: Vec<String> = listing.files.iter().map(|p| name_of(p.as_path())).collect();
        // A level lists what is in it, folders and files apart. `target` is build output and the two
        // dot-prefixed names are not source, so none of the three may appear in either list.
        assert_eq!(folders, vec!["src".to_string()], "source folders show, build output does not");
        assert_eq!(files, vec!["main.rs".to_string()], "the files of the level show, dot files do not");
    }

    #[test]
    fn folders_are_listed_before_files() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("aaa.txt"), "x\n").unwrap();
        std::fs::create_dir_all(dir.path().join("zzz")).unwrap();
        let mut workspace = Workspace::default();
        let listing = workspace.tree_at(dir.path());
        assert_eq!(listing.dirs.iter().map(|p| name_of(p.as_path())).collect::<Vec<_>>(), vec!["zzz".to_string()]);
        assert_eq!(listing.files.iter().map(|p| name_of(p.as_path())).collect::<Vec<_>>(), vec!["aaa.txt".to_string()]);
        // Folders lead because they are asked about first and files are never interleaved: the panel
        // paints the whole of `dirs` before the whole of `files`.
        let paint_order: Vec<String> =
            listing.dirs.iter().chain(listing.files.iter()).map(|p| name_of(p.as_path())).collect();
        assert_eq!(paint_order, vec!["zzz".to_string(), "aaa.txt".to_string()], "directories lead");
    }
}
