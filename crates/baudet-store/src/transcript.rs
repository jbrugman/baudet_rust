
use crate::paging::ItemWindow;
use crate::records::{SessionDetail, SessionMeta};
use crate::repository::page;
use crate::rollout::Rollout;
use crate::session::Session;
use baudet_core::Item;
use std::path::Path;

#[derive(Debug)]
pub struct Transcript {
    rollout: Rollout,
}

impl Transcript {
    pub fn open_or_create(
        root: &Path,
        session_id: &str,
        model: Option<&str>,
        cwd: Option<&str>,
    ) -> std::io::Result<Transcript> {
        match Rollout::open(root, session_id) {
            Ok(rollout) => Ok(Transcript { rollout }),
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                let mut meta = SessionMeta::new(session_id);
                meta.model = model.map(str::to_owned);
                meta.cwd = cwd.map(str::to_owned);
                Ok(Transcript { rollout: Rollout::create(root, &meta)? })
            }
            Err(error) => Err(error),
        }
    }

    pub fn build(&self, fallback_model: &str) -> std::io::Result<Session> {
        let detail = self.rollout.detail()?;
        let mut session = Session::new(
            self.rollout.session().session_id.clone(),
            detail
                .as_ref()
                .and_then(|d| d.model.clone())
                .or_else(|| self.rollout.session().model.clone())
                .unwrap_or_else(|| fallback_model.to_owned()),
        );
        session.previous_response_id = detail.as_ref().and_then(|d| d.prev_response_id.clone());
        let (messages, checkpoint) = self.rollout.context()?;
        session.messages = messages;
        // The old server-side chain may still contain the turns replaced by the checkpoint.
        if checkpoint.as_ref().is_some_and(|summary| {
            !detail.as_ref().is_some_and(|detail| detail.summary.as_ref() == Some(summary) && detail.seq == session.messages.len() as u64)
        }) {
            session.previous_response_id = None;
        }
        session.summary = checkpoint.or_else(|| detail.as_ref().and_then(|d| d.summary.clone()));
        session.project_id = detail.as_ref().and_then(|d| d.project_id.clone());
        let window = self.items_window(page::INITIAL, None)?;
        session.items = window.items;
        session.oldest_loaded_seq = if window.has_more_older { window.older_seq } else { None };
        session.persisted_messages = session.messages.len();
        session.persisted_items_tail_id = session.items.last().map(Item::item_id).map(String::from);
        Ok(session)
    }

    pub fn persist(&mut self, session: &mut Session) -> std::io::Result<()> {
        if session.messages.len() > session.persisted_messages {
            for msg in &session.messages[session.persisted_messages..].to_vec() {
                self.rollout.append_msg(msg)?;
            }
            session.persisted_messages = session.messages.len();
        }
        // An older page adds rows before this boundary. Only append rows after it.
        let start = session.items.iter().rposition(|row| {
            session.persisted_items_tail_id.as_deref() == Some(row.item_id())
        });
        let new_items: &[Item] = match start {
            Some(index) => &session.items[index + 1..],
            None => &session.items[..],
        };
        if !new_items.is_empty() {
            for item in new_items {
                self.rollout.append_item(item)?;
            }
            session.persisted_items_tail_id =
                session.items.last().map(Item::item_id).map(String::from);
        }
        self.rollout.write_detail(&SessionDetail {
            session_id: session.session_id.clone(),
            model: Some(session.model.clone()),
            prev_response_id: session.previous_response_id.clone(),
            seq: session.messages.len() as u64,
            summary: session.summary.clone(),
            project_id: session.project_id.clone(),
        })?;
        self.rollout.flush()
    }

    pub fn item_count(&self) -> std::io::Result<usize> {
        self.rollout.item_count()
    }

    pub fn compact_context(&mut self, messages: &[baudet_core::Msg], summary: &str) -> std::io::Result<()> {
        self.rollout.compact_context(messages, summary)
    }

    pub fn items_window(&self, limit: usize, before_seq: Option<u64>) -> std::io::Result<ItemWindow> {
        let rows = self.rollout.item_rows(Some(limit), before_seq)?;
        Ok(ItemWindow::from_newest_first(rows, limit, |_slot, row| Some(row.1.clone())))
    }

    pub fn message_count(&self) -> std::io::Result<usize> {
        Ok(self.rollout.messages()?.len())
    }
}
