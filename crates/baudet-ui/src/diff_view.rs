use crate::palette::{self, DiffLine};
use baudet_core::context::{probe_patch, FilePatch};
use baudet_i18n::{Key, Texts};
use baudet_tool::{diff::DiffLineKind, folder_boundary::FolderBoundary};
use eframe::egui::{self, text::LayoutJob, Color32, FontId, Id, Label, ScrollArea, TextFormat, Window};
use std::io::Read;
use std::path::Path;
use std::sync::{mpsc, Arc};

const MAX_FILE_BYTES: u64 = 1_000_000;
const MAX_FILE_CHARS: usize = 60_000;

pub(crate) struct Request {
    pub root: String,
    pub relative: String,
    pub in_repo: bool,
    pub old_path: Option<String>,
}

struct Document {
    patch: Arc<LayoutJob>,
    file: Option<String>,
    file_first: bool,
}

enum State {
    Loading(mpsc::Receiver<Document>),
    Loaded(Document),
}

pub(crate) struct DiffView {
    relative: String,
    state: State,
    shows_file: bool,
}

impl DiffView {
    // Keep Git and file reads off the paint thread.
    pub fn open(ctx: &egui::Context, request: Request) -> Self {
        let relative = request.relative.clone();
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(load(request));
            ctx.request_repaint();
        });
        Self { relative, state: State::Loading(rx), shows_file: false }
    }

    pub fn show(&mut self, ctx: &egui::Context, saved_size: Option<[f32; 2]>) -> (bool, Option<[f32; 2]>) {
        if let State::Loading(rx) = &self.state {
            let document = match rx.try_recv() {
                Ok(document) => Some(document),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(document(Texts::get(Key::DiffUnavailable), None, false, false))
                }
            };
            if let Some(document) = document {
                self.shows_file = document.file_first;
                self.state = State::Loaded(document);
            }
        }

        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            return (false, None);
        }
        let title = if self.shows_file { Key::FileTitle } else { Key::DiffTitle };
        let title = Texts::fmt(title, &[&self.relative]);
        let mut open = true;
        let saved_size = saved_size.filter(|size| size.iter().all(|n| n.is_finite() && *n > 0.0));
        let window = Window::new(title)
            .id(Id::new("diff-view"))
            .open(&mut open)
            .order(egui::Order::Foreground)
            .collapsible(false)
            .default_size(saved_size.unwrap_or([720.0, 460.0]))
            .min_size([260.0, 150.0])
            .show(ctx, |ui| {
                let layer = ui.layer_id();
                ctx.memory_mut(|mem| mem.set_modal_layer(layer));
                match &self.state {
                    State::Loading(_) => {
                        ui.set_min_size(ui.available_size());
                        ui.spinner().on_hover_text(Texts::get(Key::DiffLoading));
                        ui.label(Texts::get(Key::DiffLoading));
                    }
                    State::Loaded(document) => {
                        if document.file.is_some() {
                            let key = if self.shows_file { Key::DiffShowPatch } else { Key::DiffShowFile };
                            if ui.button(Texts::get(key)).clicked() {
                                self.shows_file = !self.shows_file;
                                ctx.request_repaint();
                            }
                        }
                        let content = match (self.shows_file, document.file.as_deref()) {
                            (true, Some(file)) => egui::WidgetText::from(egui::RichText::new(file).monospace()),
                            _ => egui::WidgetText::from(Arc::clone(&document.patch)),
                        };
                        ScrollArea::both()
                            .id_salt(("diff-content", &self.relative, self.shows_file))
                            .auto_shrink(false)
                            .show(ui, |ui| ui.add(Label::new(content).selectable(true).extend()));
                    }
                }
            });
        let size = window.map(|window| {
            let size = window.response.rect.size();
            [size.x, size.y]
        });
        (open, size)
    }
}

fn load(request: Request) -> Document {
    let (mut file, truncated) = read_text(Path::new(&request.root), &request.relative)
        .map(|(text, truncated)| (Some(text), truncated))
        .unwrap_or_default();
    let (patch, file_first, append_plain) = if !request.in_repo {
        (Texts::get(Key::DiffNoGit).to_owned(), true, false)
    } else {
        match probe_patch(&request.root, &request.relative, request.old_path.as_deref()) {
            Some(FilePatch::Diff(patch)) => (patch, false, false),
            Some(FilePatch::Untracked) => {
                let intro = Texts::fmt(Key::DiffUntracked, &[&request.relative]);
                (new_file_patch(intro, &request.relative, file.as_deref(), truncated), false, false)
            }
            Some(FilePatch::Unchanged) => (Texts::fmt(Key::DiffNoChanges, &[&request.relative]), false, true),
            None => (Texts::get(Key::DiffUnavailable).to_owned(), false, false),
        }
    };
    if truncated {
        if let Some(text) = &mut file {
            text.push_str("\n\n");
            text.push_str(Texts::get(Key::DiffContentTruncated));
        }
    }
    document(&patch, file, file_first, append_plain)
}

fn new_file_patch(mut patch: String, relative: &str, text: Option<&str>, truncated: bool) -> String {
    let Some(text) = text else { return patch };
    patch.push_str("\n\n--- /dev/null\n+++ b/");
    patch.push_str(relative);
    patch.push('\n');
    for line in text.lines() {
        patch.push('+');
        patch.push_str(line);
        patch.push('\n');
    }
    if truncated {
        patch.push('\n');
        patch.push_str(Texts::get(Key::DiffContentTruncated));
    }
    patch
}

fn read_text(root: &Path, relative: &str) -> Option<(String, bool)> {
    let path = FolderBoundary::real_within(root, &root.join(relative))?;
    let meta = path.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_FILE_BYTES || bytes.contains(&0) {
        return None;
    }
    let mut text = String::from_utf8(bytes).ok()?;
    let end = text.char_indices().nth(MAX_FILE_CHARS).map(|(end, _)| end);
    if let Some(end) = end {
        text.truncate(end);
    }
    Some((text, end.is_some()))
}

fn document(patch: &str, file: Option<String>, file_first: bool, append_plain: bool) -> Document {
    let mut job = LayoutJob::default();
    let format = |color| TextFormat { font_id: FontId::monospace(13.0), color, ..Default::default() };
    for line in patch.split_inclusive('\n') {
        job.append(line, 0.0, format(line_color(line)));
    }
    if append_plain {
        if let Some(file) = &file {
            job.append("\n\n", 0.0, format(palette::DIM));
            job.append(file, 0.0, format(palette::DIM));
        }
    }
    Document { patch: Arc::new(job), file_first: file_first && file.is_some(), file }
}

fn line_color(line: &str) -> Color32 {
    let kind = match DiffLineKind::of(line) {
        DiffLineKind::Added => DiffLine::Added,
        DiffLineKind::Removed => DiffLine::Removed,
        DiffLineKind::FileHeader => DiffLine::Header,
        DiffLineKind::HunkHeader => DiffLine::Hunk,
        DiffLineKind::Context => DiffLine::Context,
        DiffLineKind::Meta => return palette::DIM,
    };
    palette::diff_ink(kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_previews_keep_text_inside_the_project_and_the_size_limit() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path();
        std::fs::write(root.join("binary"), [0, 1, 2]).unwrap();
        std::fs::write(root.join("big"), vec![b'x'; MAX_FILE_BYTES as usize + 1]).unwrap();
        std::fs::write(root.join("text"), "é".repeat(MAX_FILE_CHARS + 1)).unwrap();
        assert_eq!(read_text(root, "binary"), None);
        assert_eq!(read_text(root, "big"), None);
        let (text, truncated) = read_text(root, "text").unwrap();
        assert_eq!(text.chars().count(), MAX_FILE_CHARS);
        assert!(truncated);
        #[cfg(unix)]
        {
            let outside = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(outside.path(), "outside the project").unwrap();
            std::os::unix::fs::symlink(outside.path(), root.join("link")).unwrap();
            assert_eq!(read_text(root, "link"), None);
        }
    }

    #[test]
    fn a_saved_dialog_size_reopens_at_the_same_dimensions() {
        let size_after_opening = |size| {
            let ctx = egui::Context::default();
            let mut view = DiffView {
                relative: "example.txt".to_owned(),
                state: State::Loaded(document("+example\n", Some("example\n".to_owned()), false, false)),
                shows_file: false,
            };
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0))),
                ..Default::default()
            };
            let mut actual = None;
            for _ in 0..3 {
                ctx.run_ui(input.clone(), |ui| actual = view.show(ui.ctx(), Some(size)).1)
                    .drop_without_applying_deltas();
            }
            actual.unwrap()
        };
        let saved = size_after_opening([640.0, 360.0]);
        assert_eq!(size_after_opening(saved), saved);
    }
}
