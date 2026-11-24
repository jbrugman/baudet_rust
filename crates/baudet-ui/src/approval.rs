use baudet_core::{ApprovalDecision, ApprovalRequest};
use baudet_i18n::{Key, Texts};
use eframe::egui;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::time::Duration;

#[derive(Default)]
pub struct Approvals(Mutex<Option<(Arc<ApprovalRequest>, mpsc::Sender<ApprovalDecision>)>>);

pub const CARD_HEIGHT: f32 = 136.0;

pub enum SavedGrant {
    Command(String),
    Mcp(String),
}

impl Approvals {
    pub fn request(&self, request: ApprovalRequest, cancelled: &AtomicBool) -> ApprovalDecision {
        let (tx, rx) = mpsc::channel();
        let request = Arc::new(request);
        *self.0.lock().unwrap() = Some((Arc::clone(&request), tx));
        while !cancelled.load(Ordering::Relaxed) {
            match rx.recv_timeout(Duration::from_millis(30)) {
                Ok(decision) => return decision,
                Err(mpsc::RecvTimeoutError::Disconnected) => return ApprovalDecision::Cancel,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        self.take_if_current(&request);
        ApprovalDecision::Cancel
    }

    pub fn is_pending(&self) -> bool {
        self.0.lock().unwrap().is_some()
    }

    fn take_if_current(&self, request: &Arc<ApprovalRequest>) -> Option<mpsc::Sender<ApprovalDecision>> {
        let mut pending = self.0.lock().unwrap();
        if !pending.as_ref().is_some_and(|(current, _)| Arc::ptr_eq(current, request)) {
            return None;
        }
        pending.take().map(|(_, reply)| reply)
    }

    pub fn show(&self, ui: &mut egui::Ui) -> Option<SavedGrant> {
        let request = self.0.lock().unwrap().as_ref()?.0.clone();
        let (title, detail, grant) = match request.as_ref() {
            ApprovalRequest::McpTool { server_id, tool, arguments } => {
                (Texts::fmt(Key::ApprovalMcpHeading, &[server_id, tool]), arguments.clone(), Some(SavedGrant::Mcp(server_id.clone())))
            }
            ApprovalRequest::RunCommand { command, cwd, timeout_seconds, always_key } => (
                Texts::get(Key::RunReviewTitle).to_owned(),
                format!(
                    "{}\n{}\n{}",
                    cwd.as_deref().unwrap_or_default(),
                    command,
                    timeout_seconds.map_or_else(String::new, |s| format!("{s}s"))
                ),
                always_key.clone().map(SavedGrant::Command),
            ),
            ApprovalRequest::ApplyPatch { path, diff } => (
                Texts::get(Key::EditReviewTitle).to_owned(),
                format!("{path}\n\n{}", diff.as_deref().unwrap_or_default()),
                None,
            ),
        };
        let decision = egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.strong(title);
            egui::ScrollArea::both().max_height(42.0).min_scrolled_height(0.0).show(ui, |ui| {
                ui.add(egui::Label::new(egui::RichText::new(detail).monospace()).selectable(true));
            });
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 4.0);
                let allow = if matches!(request.as_ref(), ApprovalRequest::ApplyPatch { .. }) {
                    Key::ApplyPatch
                } else {
                    Key::AllowOnce
                };
                let once = ui.button(Texts::get(allow)).clicked();
                let always = grant.is_some() && ui.button(Texts::get(Key::AlwaysAllow)).clicked();
                let deny = ui.button(Texts::get(Key::DenyButton)).clicked();
                if once {
                    Some(ApprovalDecision::Accept)
                } else if always {
                    Some(ApprovalDecision::AcceptForSession)
                } else {
                    deny.then_some(ApprovalDecision::Decline)
                }
            }).inner
        }).inner?;
        self.take_if_current(&request)?.send(decision).ok()?;
        (decision == ApprovalDecision::AcceptForSession).then_some(grant).flatten()
    }
}
