use crate::palette;
use eframe::egui::{self, text::LayoutJob, Label, TextFormat, TextStyle, Ui};

pub fn show(ui: &mut Ui, text: &str, wrap: bool) {
    let (body, mono) = (TextStyle::Body.resolve(ui.style()), TextStyle::Monospace.resolve(ui.style()));
    let format = TextFormat { font_id: body, color: ui.visuals().text_color(), ..Default::default() };
    let code = TextFormat { font_id: mono, background: palette::CARD, ..format.clone() };
    let job = layout(text, &format, &code, ui.visuals().strong_text_color());
    let label = Label::new(job).selectable(true);
    ui.add(if wrap { label.wrap() } else { label.extend() });
}

fn layout(text: &str, body: &TextFormat, code: &TextFormat, strong: egui::Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    let mut fence = None;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let width = trimmed.bytes().take_while(|&byte| byte == b'`').count();
        if width >= 3 {
            match fence {
                None => {
                    fence = Some(width);
                    continue;
                }
                Some(opening) if width >= opening && trimmed[width..].trim().is_empty() => {
                    fence = None;
                    continue;
                }
                _ => {}
            }
        }
        if fence.is_some() {
            job.append(line, 0.0, code.clone());
            continue;
        }
        let heading = trimmed.bytes().take_while(|&byte| byte == b'#').count();
        let mut format = body.clone();
        let mut content = line;
        if (1..=6).contains(&heading) {
            if let Some(title) = trimmed[heading..].strip_prefix(' ') {
                content = title;
                format.font_id.size += (7 - heading) as f32;
                format.color = strong;
            }
        } else if let Some(rest) =
            trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")).or_else(|| trimmed.strip_prefix("+ "))
        {
            job.append(&line[..line.len() - trimmed.len()], 0.0, format.clone());
            job.append("• ", 0.0, TextFormat { color: strong, ..format.clone() });
            content = rest;
        }
        inline(&mut job, content, &format, code, strong);
    }
    job
}

fn inline(job: &mut LayoutJob, mut text: &str, body: &TextFormat, code: &TextFormat, strong: egui::Color32) {
    while let Some(start) = text.find(['*', '`']) {
        job.append(&text[..start], 0.0, body.clone());
        text = &text[start..];
        let (marker, format) = if text.starts_with('`') {
            ("`", code.clone())
        } else if text.starts_with("**") {
            ("**", TextFormat { color: strong, extra_letter_spacing: 0.2, ..body.clone() })
        } else {
            ("*", TextFormat { italics: true, ..body.clone() })
        };
        let tail = &text[marker.len()..];
        if let Some(end) = tail.find(marker) {
            let inner = &tail[..end];
            if !inner.is_empty() && (marker == "`" || inner.trim() == inner) {
                job.append(inner, 0.0, format);
                text = &tail[end + marker.len()..];
                continue;
            }
        }
        job.append(marker, 0.0, body.clone());
        text = tail;
    }
    job.append(text, 0.0, body.clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_transcript_styles_paired_markers_and_keeps_code_literal() {
        let body = TextFormat::default();
        let code = TextFormat { font_id: egui::FontId::monospace(12.0), ..body.clone() };
        let rendered = layout(
            "# Heading\n- **bold** and *italic*\n```rs\n*literal*\n```\n2 * 3 * 4\n",
            &body,
            &code,
            egui::Color32::WHITE,
        );
        assert_eq!(rendered.text, "Heading\n• bold and italic\n*literal*\n2 * 3 * 4\n");
        assert!(rendered.sections.iter().any(|section| section.format.italics));
    }
}
