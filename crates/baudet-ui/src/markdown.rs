use eframe::egui::{self, text::LayoutJob, Label, TextFormat, TextStyle, Ui};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

pub fn show(ui: &mut Ui, text: &str, wrap: bool) {
    let (body, mono) = (TextStyle::Body.resolve(ui.style()), TextStyle::Monospace.resolve(ui.style()));
    let format = TextFormat { font_id: body, color: ui.visuals().text_color(), ..Default::default() };
    let code = TextFormat { font_id: mono, background: ui.visuals().code_bg_color, ..format.clone() };
    let strong = ui.visuals().strong_text_color();
    let mut remaining = 0;
    if text.contains('|') || text.contains("<svg") {
        let mut events = Parser::new_ext(text, Options::ENABLE_TABLES).into_offset_iter();
        while let Some((event, range)) = events.next() {
            if range.start < remaining {
                continue;
            }
            match event {
                Event::Start(Tag::Table(columns)) => {
                    show_prose(ui, &text[remaining..range.start], &format, &code, strong, wrap);
                    show_table(ui, columns.len(), &format, &code, events.by_ref()
                        .map(|(event, _)| event)
                        .take_while(|event| !matches!(event, Event::End(TagEnd::Table))));
                    remaining = range.end;
                }
                Event::Start(Tag::CodeBlock(_)) if crate::svg_preview::block(&text[range.clone()]).is_some() => {
                    let code_text: String = events.by_ref()
                        .take_while(|(event, _)| !matches!(event, Event::End(TagEnd::CodeBlock)))
                        .fold(String::new(), |mut code, (event, _)| {
                            if let Event::Text(text) = event {
                                code.push_str(&text);
                            }
                            code
                        });
                    show_prose(ui, &text[remaining..range.start], &format, &code, strong, wrap);
                    show_code(ui, &code_text, wrap);
                    remaining = range.end;
                }
                Event::Start(Tag::HtmlBlock) if crate::svg_preview::block(&text[range.clone()]).is_some() => {
                    show_prose(ui, &text[remaining..range.start], &format, &code, strong, wrap);
                    show_code(ui, &text[range.clone()], wrap);
                    remaining = range.end;
                }
                Event::InlineHtml(html) if html.starts_with("<svg")
                    && text[..range.start].rsplit('\n').next().is_some_and(|line| line.trim().is_empty()) =>
                {
                    if let Some(("", svg, _)) = crate::svg_preview::block(&text[range.start..]) {
                        show_prose(ui, &text[remaining..range.start], &format, &code, strong, wrap);
                        show_code(ui, svg, wrap);
                        remaining = range.start + svg.len();
                    }
                }
                _ => {}
            }
        }
    }
    show_prose(ui, &text[remaining..], &format, &code, strong, wrap);
}

pub(crate) fn show_code(ui: &mut Ui, mut text: &str, wrap: bool) {
    while let Some((before, svg, after)) = crate::svg_preview::block(text) {
        code_label(ui, before.trim_end_matches('\n'), wrap);
        crate::svg_preview::show(ui, svg, |ui, source| code_label(ui, source, wrap));
        text = after.trim_start_matches('\n');
    }
    code_label(ui, text, wrap);
}

fn code_label(ui: &mut Ui, text: &str, wrap: bool) {
    if !text.is_empty() {
        let label = Label::new(egui::RichText::new(text).monospace().size(12.0)).selectable(true);
        ui.add(if wrap { label.wrap() } else { label.extend() });
    }
}

fn show_prose(ui: &mut Ui, text: &str, body: &TextFormat, code: &TextFormat, strong: egui::Color32, wrap: bool) {
    let job = layout(text, body, code, strong);
    if !job.text.is_empty() {
        let label = Label::new(job).selectable(true);
        ui.add(if wrap { label.wrap() } else { label.extend() });
    }
}

fn show_table<'a>(
    ui: &mut Ui,
    columns: usize,
    body: &TextFormat,
    code: &TextFormat,
    events: impl Iterator<Item = Event<'a>>,
) {
    let spacing = ui.spacing().item_spacing;
    let cell_width = ((ui.available_width() + spacing.x) / columns as f32 - spacing.x).max(1.0);
    let strong = ui.visuals().strong_text_color();
    egui::Grid::new(ui.next_auto_id()).num_columns(columns)
        .min_col_width(cell_width).max_col_width(cell_width).striped(true).show(ui, |ui| {
            let mut cell = LayoutJob::default();
            let mut header = false;
            let mut bold = false;
            let mut italic = false;
            for event in events {
                match event {
                    Event::Start(Tag::TableHead) => header = true,
                    Event::End(TagEnd::TableHead) => {
                        header = false;
                        ui.end_row();
                    }
                    Event::End(TagEnd::TableRow) => ui.end_row(),
                    Event::End(TagEnd::TableCell) => {
                        ui.add(Label::new(std::mem::take(&mut cell)).wrap().selectable(true));
                    }
                    Event::Start(Tag::Strong) => bold = true,
                    Event::End(TagEnd::Strong) => bold = false,
                    Event::Start(Tag::Emphasis) => italic = true,
                    Event::End(TagEnd::Emphasis) => italic = false,
                    Event::Text(text) | Event::InlineHtml(text) => {
                        let format = TextFormat {
                            color: if bold || header { strong } else { body.color },
                            extra_letter_spacing: if bold || header { 0.2 } else { body.extra_letter_spacing },
                            italics: italic,
                            ..body.clone()
                        };
                        cell.append(&text, 0.0, format);
                    }
                    Event::Code(text) => cell.append(&text, 0.0, code.clone()),
                    Event::SoftBreak | Event::HardBreak => cell.append("\n", 0.0, body.clone()),
                    _ => {}
                }
            }
        });
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
    fn given_a_markdown_table_when_rendered_then_its_cells_wrap_in_the_chat() {
        let content = concat!(
            "Before the table\n\n",
            "| Aspect | First | Second |\n|---|---|---|\n",
            "| Structure | `tool_gate` and events | The details in this cell are long enough to wrap within its own column. |\n",
            "| Boundaries | app \\| model | External services |\n\n",
            "After the table",
        );
        let ctx = egui::Context::default();
        let paint = || {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(390.0, 450.0))),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| show(ui, content, true));
            output.textures_delta.clear();
            output
        };
        paint();
        let output = paint();
        let labels: Vec<_> = output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some(text),
            _ => None,
        }).collect();
        for expected in ["Aspect", "First", "Second", "Structure", "tool_gate and events", "app | model"] {
            assert!(labels.iter().any(|text| text.galley.text() == expected), "missing table cell {expected}");
        }
        let cell = labels.iter().find(|text| text.galley.text().starts_with("The details")).unwrap();
        assert!(cell.galley.rows.len() > 1);
        assert!(cell.visual_bounding_rect().right() <= 390.0);
        assert!(labels.iter().any(|text| text.galley.text().trim() == "After the table"));
        assert!(labels.iter().all(|text| !text.galley.text().contains("|---|")));
    }

    #[test]
    fn given_a_table_inside_a_code_fence_when_rendered_then_the_pipe_syntax_stays_visible() {
        let source = "| Aspect | Value |\n|---|---|\n| Style | Literal |";
        let message = format!("```text\n{source}\n```");
        let ctx = egui::Context::default();
        let output = ctx.run_ui(egui::RawInput::default(), |ui| show(ui, &message, true));
        let literal = output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.text().trim() == source
        ));
        output.drop_without_applying_deltas();
        assert!(literal);
    }

    #[test]
    fn given_raw_svg_when_rendered_then_it_has_an_image_toggle_and_visible_source() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg"><svg><rect width="5" height="5"/></svg></svg>"#;
        let ctx = egui::Context::default();
        let output = ctx.run_ui(egui::RawInput::default(), |ui| show(ui, svg, true));
        let labels: Vec<_> = output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some(text.galley.text()),
            _ => None,
        }).collect();
        assert!(labels.contains(&"Image"));
        assert!(labels.contains(&svg));
        output.drop_without_applying_deltas();
    }

    #[test]
    fn given_an_svg_code_block_when_image_is_selected_then_the_chat_shows_the_rendered_svg() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"><rect width="40" height="20" fill="red"/></svg>"#;
        let message = format!("```svg\n{svg}\n```");
        let ctx = egui::Context::default();
        let paint = |events| {
            let mut output = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| show(ui, &message, true));
            output.textures_delta.clear();
            output
        };
        let initial = paint(Vec::new());
        let shows_source = |output: &egui::FullOutput| output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.text() == svg
        ));
        assert!(shows_source(&initial));
        let image = initial.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == "Image" => Some(text.visual_bounding_rect().center()),
            _ => None,
        }).expect("Image control above the SVG");
        for pressed in [true, false] {
            paint(vec![egui::Event::PointerMoved(image), egui::Event::PointerButton {
                pos: image, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
            }]);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        loop {
            let preview = paint(Vec::new());
            assert!(!shows_source(&preview));
            let primitives = ctx.tessellate(preview.shapes, preview.pixels_per_point);
            if primitives.iter().any(|primitive| matches!(&primitive.primitive,
                egui::epaint::Primitive::Mesh(mesh) if mesh.texture_id != egui::TextureId::default()
            )) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "the SVG preview finished rendering");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

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
