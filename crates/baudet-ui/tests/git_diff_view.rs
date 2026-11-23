use baudet_i18n::{Key, Texts};
use baudet_ui::{palette, SidebarModel, TranscriptApp};
use eframe::egui;
use std::path::Path;
use std::time::{Duration, Instant};

fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
}

fn paint(ctx: &egui::Context, app: &mut TranscriptApp, time: f64, events: Vec<egui::Event>) -> egui::FullOutput {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0))),
        time: Some(time),
        events,
        ..Default::default()
    };
    ctx.run_ui(input, |ui| app.paint(ui))
}

fn click(ctx: &egui::Context, app: &mut TranscriptApp, pos: egui::Pos2, time: &mut f64) {
    for pressed in [true, false] {
        *time += 0.04;
        let events = vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ];
        paint(ctx, app, *time, events).drop_without_applying_deltas();
    }
}

fn wait_for_patch(ctx: &egui::Context, app: &mut TranscriptApp, time: &mut f64, expected: &str) -> egui::FullOutput {
    let start = Instant::now();
    loop {
        *time += 0.02;
        let out = paint(ctx, app, *time, Vec::new());
        let colored = out.shapes.iter().any(|shape| {
            let egui::Shape::Text(text) = &shape.shape else { return false };
            text.galley.text().contains(expected)
                && text.galley.job.sections.iter().any(|part| part.format.color == palette::DIFF_ADDED)
        });
        if colored {
            return out;
        }
        out.drop_without_applying_deltas();
        assert!(start.elapsed() < Duration::from_secs(4), "Expected a visible colored patch containing {expected}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_double_click_in_either_list_opens_its_own_colored_patch() {
    let folder = tempfile::tempdir().unwrap();
    let repo = folder.path();
    let project = repo.join("project");
    std::fs::create_dir(&project).unwrap();
    for name in ["first.txt", "second.txt"] {
        std::fs::write(project.join(name), "before\n").unwrap();
    }
    git(repo, &["init", "-q"]);
    git(repo, &["add", "."]);
    git(repo, &["-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "base"]);
    std::fs::write(project.join("first.txt"), "from the tree\n").unwrap();
    std::fs::write(project.join("second.txt"), "from the changes list\n").unwrap();
    std::fs::write(project.join("new.txt"), "from an untracked file\n").unwrap();

    let ctx = egui::Context::default();
    palette::install(&ctx);
    let catalog = tempfile::tempdir().unwrap();
    let mut sidebar = SidebarModel::new(baudet_projects::Catalog::open(catalog.path()).unwrap());
    let linked = sidebar.add_project("Patch project").unwrap();
    sidebar.select_project(Some(&linked.id));
    sidebar.link_folder(&linked.id, project.to_str()).unwrap();
    sidebar.set_permissions(true, false, false).unwrap();
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend()).with_sidebar_model(sidebar);
    let mut time = 0.0;
    for (id, expected) in [
        (egui::Id::new(("file-row", project.join("first.txt").as_path())), "+from the tree"),
        (egui::Id::new(("change-row", "second.txt")), "+from the changes list"),
        (egui::Id::new(("change-row", "new.txt")), "+from an untracked file"),
    ] {
        // Git metadata and the open folder settle before measuring the click target.
        for _ in 0..5 {
            time += 0.2;
            paint(&ctx, &mut app, time, Vec::new()).drop_without_applying_deltas();
        }
        let row = ctx.read_response(id).expect("the row is addressable");
        assert!(row.interact_rect.height() > 0.0 && row.interact_rect.height() < 35.0);
        let pos = row.interact_rect.center();
        click(&ctx, &mut app, pos, &mut time);
        click(&ctx, &mut app, pos, &mut time);

        let out = wait_for_patch(&ctx, &mut app, &mut time, expected);
        let toggle = Texts::get(Key::DiffShowFile);
        let toggle = out
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == toggle => Some(text.visual_bounding_rect().center()),
                _ => None,
            })
            .expect("the patch offers a file view");
        out.drop_without_applying_deltas();
        click(&ctx, &mut app, toggle, &mut time);
        time += 0.1;
        let out = paint(&ctx, &mut app, time, Vec::new());
        assert!(out.shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text().trim_end() == &expected[1..])
        }));
        out.drop_without_applying_deltas();

        time += 0.02;
        let escape = egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        paint(&ctx, &mut app, time, vec![escape]).drop_without_applying_deltas();
    }
}
