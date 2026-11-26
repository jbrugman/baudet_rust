use eframe::egui::{self, Ui};
use quick_xml::{events::Event, Reader};
use resvg::{tiny_skia, usvg};
use std::sync::{Arc, Mutex, OnceLock, mpsc};

const MAX_SOURCE_BYTES: usize = 256 * 1024;
const MAX_PREVIEW_EDGE: f32 = 2048.0;

pub(crate) fn block(text: &str) -> Option<(&str, &str, &str)> {
    for (start, _) in text.match_indices("<svg") {
        let rest = &text[start + 4..];
        if !rest.starts_with(|c: char| c.is_whitespace() || c == '>' || c == '/') {
            continue;
        }
        let prefix = &text[..start];
        if !prefix.rsplit('\n').next()?.trim().is_empty() {
            continue;
        }
        let mut xml = Reader::from_str(&text[start..]);
        match xml.read_event().ok()? {
            Event::Start(element) => {
                xml.read_to_end(element.name()).ok()?;
            }
            Event::Empty(_) => {}
            _ => continue,
        }
        let xml_end = start + xml.buffer_position() as usize;
        return Some((prefix, &text[start..xml_end], &text[xml_end..]));
    }
    None
}

#[derive(Clone)]
enum Preview {
    Loading(Arc<Mutex<mpsc::Receiver<Result<egui::ColorImage, String>>>>),
    Ready(Result<egui::TextureHandle, String>),
}

pub(crate) fn show(ui: &mut Ui, source: &str, show_text: impl FnOnce(&mut Ui, &str)) {
    let id = ui.next_auto_id().with("svg-mode");
    let mut show_image = ui.ctx().data_mut(|data| data.get_temp::<bool>(id)).unwrap_or_default();
    ui.horizontal(|ui| {
        ui.selectable_value(&mut show_image, false, "Text");
        ui.selectable_value(&mut show_image, true, "Image");
    });
    ui.ctx().data_mut(|data| data.insert_temp(id, show_image));
    ui.separator();
    if !show_image {
        show_text(ui, source);
        return;
    }

    let key = egui::Id::new(("svg-preview", source));
    let cached = ui.ctx().data_mut(|data| data.get_temp::<Preview>(key));
    let state = cached.unwrap_or_else(|| {
        let (sender, receiver) = mpsc::channel();
        let ctx = ui.ctx().clone();
        let source = source.to_owned();
        std::thread::spawn(move || {
            let _ = sender.send(render(&source));
            ctx.request_repaint();
        });
        let state = Preview::Loading(Arc::new(Mutex::new(receiver)));
        ui.ctx().data_mut(|data| data.insert_temp(key, state.clone()));
        state
    });
    let texture = match state {
        Preview::Loading(receiver) => {
            let result = match receiver.lock().unwrap().try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Disconnected) => Err("Could not render the SVG preview.".into()),
                Err(mpsc::TryRecvError::Empty) => {
                    ui.spinner();
                    return;
                }
            };
            let texture = result.map(|image| ui.ctx().load_texture("svg-preview", image, egui::TextureOptions::LINEAR));
            ui.ctx().data_mut(|data| data.insert_temp(key, Preview::Ready(texture.clone())));
            texture
        }
        Preview::Ready(texture) => texture,
    };
    match texture {
        Ok(texture) => {
            let size = texture.size_vec2();
            let scale = (ui.available_width().max(1.0) / size.x).min(1.0);
            ui.image((texture.id(), size * scale));
        }
        Err(reason) => {
            ui.label(reason);
        }
    }
}

fn render(source: &str) -> Result<egui::ColorImage, String> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err("SVG is too large to preview.".into());
    }
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    let options = usvg::Options {
        fontdb: Arc::clone(FONTS.get_or_init(|| {
            let mut fonts = usvg::fontdb::Database::new();
            fonts.load_system_fonts();
            Arc::new(fonts)
        })),
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_string: Box::new(|_, _| None),
            resolve_data: Box::new(|_, _, _| None),
        },
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(source, &options).map_err(|error| format!("Could not preview SVG: {error}"))?;
    let size = tree.size();
    let scale = (MAX_PREVIEW_EDGE / size.width().max(size.height())).min(1.0);
    let width = (size.width() * scale).ceil().max(1.0) as u32;
    let height = (size.height() * scale).ceil().max(1.0) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(width, height).ok_or("Could not allocate the SVG preview.")?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    Ok(egui::ColorImage::from_rgba_premultiplied([width as usize, height as usize], pixmap.data()))
}
