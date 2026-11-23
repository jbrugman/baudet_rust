
pub fn display_path(root: Option<&std::path::Path>, path: Option<&std::path::Path>) -> String {
    let Some(path) = path else {
        return String::new();
    };
    match root {
        Some(root) if path == root => ".".to_string(),
        Some(root) => path
            .strip_prefix(root)
            .map(|rel| rel.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string_lossy().into_owned()),
        None => path.to_string_lossy().into_owned(),
    }
}

pub fn copyable(path: Option<&std::path::Path>) -> bool {
    path.is_some()
}

pub const COPY_PATH: &str = "Copy path";

pub const COPY_PATH_NL: &str = "Kopieer pad";

pub fn copy(text: &str) -> Result<(), String> {
    arboard::Clipboard::new()
        .and_then(|mut board| board.set_text(text.to_string()))
        .map_err(|problem| format!("clipboard: {problem}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn een_projectmap_relatief_en_de_map_zelf_is_een_punt() {
        let root = Path::new("/projects/baudet");
        assert_eq!(display_path(Some(root), Some(root)), ".");
        assert_eq!(display_path(Some(root), Some(Path::new("/projects/baudet/src/main.rs"))), "src/main.rs");
        assert_eq!(display_path(Some(root), Some(Path::new("/other/thing.txt"))), "/other/thing.txt");
    }

    #[test]
    fn zonder_projectmap_is_het_pad_zijn_eigen_tekst() {
        assert_eq!(display_path(None, Some(Path::new("/tmp/x/y.rs"))), "/tmp/x/y.rs");
        assert_eq!(display_path(None, None), "");
        assert!(!copyable(None), "wel blanco kopiëren is wat de bron weigert");
        assert!(copyable(Some(Path::new("/tmp"))));
    }

    #[test]
    fn het_woord_staat_in_key_dot_kt() {
        assert_eq!(COPY_PATH, "Copy path");
        assert_eq!(COPY_PATH_NL, "Kopieer pad");
    }
}

const MAX_EDGE: u32 = 4096;
const MAX_IMAGE_BYTES: usize = 12 * 1024 * 1024;

pub(crate) fn image_data_url() -> Option<String> {
    let pixels = arboard::Clipboard::new().ok()?.get_image().ok()?;
    let width = u32::try_from(pixels.width).ok()?;
    let height = u32::try_from(pixels.height).ok()?;
    if width == 0 || height == 0 || width > MAX_EDGE || height > MAX_EDGE {
        return None;
    }
    let rgba = image::RgbaImage::from_raw(width, height, pixels.bytes.into_owned())?;
    png_data_url(&rgba)
}

fn png_data_url(rgba: &image::RgbaImage) -> Option<String> {
    use base64::Engine;
    use image::ImageEncoder;

    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(rgba.as_raw(), rgba.width(), rgba.height(), image::ExtendedColorType::Rgba8)
        .ok()?;
    if png.len() > MAX_IMAGE_BYTES {
        return None;
    }
    Some(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(&png)))
}

#[cfg(test)]
#[test]
fn clipboard_encoding_preserves_rgba_pixels_in_a_png_data_url() {
    use base64::Engine;
    let input = image::RgbaImage::from_pixel(2, 3, image::Rgba([25, 50, 75, 127]));
    let url = png_data_url(&input).unwrap();
    let bytes = base64::engine::general_purpose::STANDARD.decode(url.strip_prefix("data:image/png;base64,").unwrap()).unwrap();
    let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).unwrap().into_rgba8();
    assert_eq!(decoded, input);
}

pub(crate) fn show_image(ui: &mut eframe::egui::Ui, image_id: eframe::egui::Id, item: &baudet_core::ImageItem) {
    use baudet_i18n::{Key, Texts};
    use base64::Engine;
    use eframe::egui::{self, TextureHandle};
    use std::sync::Arc;

    type Loaded = Option<(TextureHandle, Arc<Vec<u8>>, &'static str)>;
    let key = ui.id().with(("tool-image", image_id));
    let cached: Option<Loaded> = ui.ctx().data_mut(|data| data.get_temp(key));
    let decoded = cached.unwrap_or_else(|| {
        let loaded = (|| {
            if item.base64.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 {
                return None;
            }
            let bytes = base64::engine::general_purpose::STANDARD.decode(&item.base64).ok()?;
            let (format, extension) = match item.mime_type.as_str() {
                "image/png" => (image::ImageFormat::Png, "png"),
                "image/jpeg" => (image::ImageFormat::Jpeg, "jpg"),
                _ => return None,
            };
            let mut reader = image::ImageReader::with_format(std::io::Cursor::new(&bytes), format);
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(MAX_EDGE);
            limits.max_image_height = Some(MAX_EDGE);
            limits.max_alloc = Some(80 * 1024 * 1024);
            reader.limits(limits);
            let rgba = reader.decode().ok()?.into_rgba8();
            let size = [rgba.width() as usize, rgba.height() as usize];
            let image = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
            let texture = ui.ctx().load_texture(format!("{key:?}"), image, egui::TextureOptions::LINEAR);
            Some((texture, Arc::new(bytes), extension))
        })();
        ui.ctx().data_mut(|data| data.insert_temp(key, loaded.clone()));
        loaded
    });
    let Some((image, bytes, extension)) = decoded else {
        ui.label(Texts::get(Key::ImageUnreadable));
        return;
    };
    let width = ui.available_width().max(1.0);
    let size = image.size_vec2();
    let scale = (width / size.x).min(360.0 / size.y).min(1.0);
    ui.add(egui::Image::new((image.id(), size * scale)));
    let status = key.with("save-status");
    if ui.button(Texts::get(Key::SaveImage)).clicked() {
        if let Some(path) = rfd::FileDialog::new().set_file_name(format!("image.{extension}")).save_file() {
            let message = match std::fs::write(&path, bytes.as_slice()) {
                Ok(()) => Texts::fmt(Key::ImageSavedTo, &[&path.display()]),
                Err(error) => Texts::fmt(Key::ImageSaveFailed, &[&error]),
            };
            ui.ctx().data_mut(|data| data.insert_temp(status, message));
        }
    }
    if let Some(message) = ui.ctx().data_mut(|data| data.get_temp::<String>(status)) {
        ui.label(message);
    }
}
