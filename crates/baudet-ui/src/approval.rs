use baudet_core::{ApprovalDecision, ApprovalRequest};
use baudet_i18n::{Key, Texts};
use eframe::egui;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Mutex,
};
use std::time::Duration;

#[derive(Default)]
pub struct Approvals(Mutex<Option<(ApprovalRequest, mpsc::Sender<ApprovalDecision>)>>);

impl Approvals {
    pub fn request(&self, request: ApprovalRequest, cancelled: &AtomicBool) -> ApprovalDecision {
        let (tx, rx) = mpsc::channel();
        *self.0.lock().unwrap() = Some((request, tx));
        while !cancelled.load(Ordering::Relaxed) {
            match rx.recv_timeout(Duration::from_millis(30)) {
                Ok(decision) => return decision,
                Err(mpsc::RecvTimeoutError::Disconnected) => return ApprovalDecision::Cancel,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        self.0.lock().unwrap().take();
        ApprovalDecision::Cancel
    }

    pub fn show(&self, ctx: &egui::Context) -> Option<String> {
        let request = self.0.lock().unwrap().as_ref()?.0.clone();
        let (title, detail, server) = match &request {
            ApprovalRequest::McpTool { server_id, tool, arguments } => {
                (Texts::fmt(Key::ApprovalMcpHeading, &[server_id, tool]), arguments.clone(), Some(server_id.clone()))
            }
            ApprovalRequest::RunCommand { command, cwd, timeout_seconds } => (
                Texts::get(Key::RunReviewTitle).to_owned(),
                format!(
                    "{}\n{}\n{}",
                    cwd.as_deref().unwrap_or_default(),
                    command,
                    timeout_seconds.map_or_else(String::new, |s| format!("{s}s"))
                ),
                None,
            ),
            ApprovalRequest::ApplyPatch { path, diff } => (
                Texts::get(Key::EditReviewTitle).to_owned(),
                format!("{path}\n\n{}", diff.as_deref().unwrap_or_default()),
                None,
            ),
        };
        let response = egui::Modal::new(egui::Id::new("tool-approval")).show(ctx, |ui| {
            ui.set_max_width(600.0);
            ui.heading(title);
            egui::ScrollArea::both().max_height(290.0).show(ui, |ui| {
                ui.add(egui::Label::new(egui::RichText::new(detail).monospace()).selectable(true));
            });
            ui.horizontal_wrapped(|ui| {
                if ui.button(Texts::get(Key::DenyButton)).clicked() {
                    Some(ApprovalDecision::Decline)
                } else if ui.button(Texts::get(Key::AllowOnce)).clicked() {
                    Some(ApprovalDecision::Accept)
                } else if server.is_some() && ui.button(Texts::get(Key::AlwaysAllow)).clicked() {
                    Some(ApprovalDecision::AcceptForSession)
                } else {
                    None
                }
            })
            .inner
        });
        let decision = response.inner.or_else(|| response.should_close().then_some(ApprovalDecision::Decline))?;
        let pending = self.0.lock().unwrap().take()?;
        let _ = pending.1.send(decision);
        (decision == ApprovalDecision::AcceptForSession).then_some(server).flatten()
    }
}
