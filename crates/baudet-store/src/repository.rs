//! Opening a stored conversation as a [`Session`], and persisting one again.
//!
//! Kotlin keeps this in `H2Store` behind the [`TranscriptStore`](dev.baudet.store) interface, and
//! `AppState` splits it in two on purpose: `buildSession` reads without touching a shared field, so
//! it may run on a worker, while `adoptSession` swaps it in on the UI thread. This module is the
//! read half plus the write half, and neither one holds state — the [`Session`] does.
//!
//! What the flat-file shape buys over the interface, and what it costs:
//!
//!  * **`seq` is a line position within its file**, not a column. Reading the whole file is how a
//!    turn's tail gets appended and how a page gets cut, so no counter has to be kept correct by a
//!    second writer.
//!  * **the item cursor is a line position too**, which is why [`Repository::items_window`] reads a
//!    whole file before it slices it. That is honest paging, not index paging: correct at the sizes
//!    this app reaches, with a real cap on what it does. A real index is a later decision, and the
//!    signature below is already the shape an index would serve, so the caller will not change.
//!  * **the trail's decode tolerance is re-applied per window** rather than assumed from a
//!    `LIMIT` in a query. A dropped row still costs a slot and still proves that more precedes it.

use crate::paging::ItemWindow;
use crate::records::SessionDetail;
use crate::rollout::Rollout;
use crate::session::Session;
use baudet_core::Item;
use std::path::Path;

/// The page sizes carried over from `AppState`, because they are two different intentions.
pub mod page {
    /// What opening a conversation reads: enough to show a tail without a second thought.
    pub const INITIAL: usize = 10;
    /// What one scroll-up pull reads.
    pub const SCROLL: usize = 20;
}

/// Read and write one session's transcript on disk.
///
/// Stateless by design: every mutation it makes is reflected in the [`Session`] it is handed, so
/// two conversations can be persisted from the same repository without one learning about the
/// other.
#[derive(Debug, Default, Clone, Copy)]
pub struct Repository;

impl Repository {
    /// Read a conversation out of the store into a fresh state, touching nothing else.
    ///
    /// `fallback_model` is what a session with no recorded model gets — the configured model, which
    /// is the answer Kotlin gives too (`store.model(id) ?: config.model`). The page is the initial
    /// one: reading two hundred rows to draw ten is the thing this function exists to avoid.
    ///
    /// The boundary fields are set from what was read, so the very next persist appends only the
    /// tail a turn adds instead of rewriting every row.
    pub fn build(
        &self,
        root: &Path,
        session_id: &str,
        fallback_model: &str,
    ) -> std::io::Result<Session> {
        let rollout = Rollout::open(root, session_id)?;
        let detail = rollout.detail()?;
        let mut session = Session::new(
            session_id,
            detail
                .as_ref()
                .and_then(|d| d.model.clone())
                .or_else(|| rollout.session().model.clone())
                .unwrap_or_else(|| fallback_model.to_string()),
        );
        session.previous_response_id = detail.as_ref().and_then(|d| d.prev_response_id.clone());
        session.messages = rollout.messages()?;
        session.summary = detail.and_then(|d| d.summary);
        let window = self.items_window(root, session_id, page::INITIAL, None)?;
        session.items = window.items;
        // Two spellings of the same fact, and the distinction matters: a window that reaches the
        // start has no cursor to hand back, so `None` here is what stops the UI offering "older".
        session.oldest_loaded_seq = if window.has_more_older { window.older_seq } else { None };
        session.persisted_messages = session.messages.len();
        session.persisted_items_tail_id = session.items.last().map(Item::item_id).map(String::from);
        Ok(session)
    }

    /// Persist one conversation, writing only what is new.
    ///
    /// Turns grow at the tail between persistences — compaction is the lone head-cut and it
    /// rewrites and re-anchors before calling here — so the newer tail is appended. The item trail
    /// is append-only for the same reason: find the newest stored row from the back and write what
    /// came after it, which lands on the genuine tail even after a page of older rows was prepended.
    ///
    /// The head line is written last and always, carrying the summary: the older turns it replaces
    /// are already gone, so losing only the summary would drop that context for good.
    pub fn persist(&self, root: &Path, session: &mut Session) -> std::io::Result<()> {
        let mut rollout = Rollout::open(root, &session.session_id)?;
        if session.messages.len() > session.persisted_messages {
            let tail = session.messages[session.persisted_messages..].to_vec();
            for msg in &tail {
                rollout.append_msg(msg)?;
            }
            session.persisted_messages = session.messages.len();
        }
        let start = session.items.iter().rposition(|row| {
            session.persisted_items_tail_id.as_deref() == Some(row.item_id())
        });
        let new_items: &[Item] = match start {
            Some(index) => &session.items[index + 1..],
            // Nothing stored yet, so everything this view holds is new.
            None => &session.items[..],
        };
        if !new_items.is_empty() {
            for item in new_items {
                rollout.append_item(item)?;
            }
            session.persisted_items_tail_id =
                session.items.last().map(Item::item_id).map(String::from);
        }
        rollout.write_detail(&SessionDetail {
            session_id: session.session_id.clone(),
            model: Some(session.model.clone()),
            prev_response_id: session.previous_response_id.clone(),
            seq: session.messages.len() as u64,
            summary: session.summary.clone(),
        })?;
        // One flush at the end of a persist is the moment worth paying for: everything before it
        // becomes durable, and a streamed delta costs no disk round-trip.
        rollout.flush()
    }

    /// Read one older page of a conversation for the scroll path, stopping just before
    /// `before_seq`.
    ///
    /// Read-only, so a worker can run it while the UI thread keeps showing the rows it already
    /// holds; the caller prepends the page and moves the cursor. Writes append only the new tail,
    /// so a partially loaded view never has to pull the older pages it has not shown.
    pub fn items_window(
        &self,
        root: &Path,
        session_id: &str,
        limit: usize,
        before_seq: Option<u64>,
    ) -> std::io::Result<ItemWindow> {
        let rollout = Rollout::open(root, session_id)?;
        let items = rollout.items()?;
        // Newest first, as the origineler's `ORDER BY seq DESC` hands them over, cut at the cursor.
        // Position in the file is the sequence, so a `before_seq` of n keeps the first n rows.
        let cut = before_seq.unwrap_or(items.len() as u64).min(items.len() as u64) as usize;
        let rows: Vec<(u64, Item)> = items
            .into_iter()
            .take(cut)
            .enumerate()
            .rev()
            .map(|(index, item)| (index as u64, item))
            .collect();
        // The rows are already decoded, which is the tolerance the whole-file read applied. One
        // deliberate difference: an *encoded* row that cannot be decoded is dropped there, so the
        // cursor names a line that may hold such a row. Paging below it still cannot skip or repeat
        // a readable row, which is the only promise the cursor makes.
        Ok(ItemWindow::from_newest_first(rows, limit, |_slot, row| Some(row.1.clone())))
    }

    /// Replace a conversation's turns wholesale.
    ///
    /// Not the per-turn path — that is [`Repository::persist`]. This is the primitive a caller uses
    /// when compaction renumbers the head, and it re-anchors the boundary afterwards so the next
    /// persist cannot append the discarded tail back onto the compacted head.
    pub fn rewrite_messages(
        &self,
        root: &Path,
        session: &mut Session,
        messages: Vec<baudet_core::Msg>,
    ) -> std::io::Result<()> {
        let rollout = Rollout::open(root, &session.session_id)?;
        let path = rollout.path().join("history.jsonl");
        // The one place a history file is replaced. It is not an append because the point is to
        // make rows that exist on disk stop existing; the summary that replaces them is written by
        // the caller before this returns, so nothing becomes unrecoverable.
        //
        // Written beside the original and renamed, so a crash leaves the old file readable instead
        // of a truncated one.
        let temp = path.with_extension("jsonl.tmp");
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&temp)?;
            for msg in &messages {
                let line = serde_json::to_string(&crate::records::Record::Msg(msg.clone()))
                    .map_err(std::io::Error::other)?;
                file.write_all(line.as_bytes())?;
                file.write_all(b"\n")?;
            }
            file.flush()?;
        }
        std::fs::rename(&temp, &path)?;
        session.messages = messages;
        session.persisted_messages = session.messages.len();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::SessionMeta;
    use baudet_core::{
        AgentMessageItem, ItemStatus, Msg, ToolCallItem, UserMessageItem,
    };

    struct Root(std::path::PathBuf);

    impl Root {
        fn new(name: &str) -> Root {
            let path = std::path::PathBuf::from("/tmp/baudet-repository-tests").join(name);
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Root(path)
        }
    }

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn session_id(name: &str) -> String {
        format!("s-2026-11-22T09-00-00Z-{name}")
    }

    fn create(root: &Path, id: &str, model: Option<&str>) -> Rollout {
        Rollout::create(
            root,
            &SessionMeta {
                session_id: id.into(),
                started_at: Some("2026-11-22T09:00:00Z".into()),
                cwd: None,
                git: None,
                model: model.map(String::from),
                thread_id: None,
            },
        )
        .unwrap()
    }

    fn user(text: &str) -> Item {
        Item::UserMessage(UserMessageItem {
            item_id: format!("u-{text}"),
            text: text.into(),
            status: ItemStatus::Completed,
        })
    }

    fn agent(id: &str, text: &str) -> Item {
        Item::AgentMessage(AgentMessageItem {
            item_id: id.into(),
            text: text.into(),
            status: ItemStatus::Completed,
        })
    }

    #[test]
    fn a_fresh_session_reads_empty_and_still_names_its_own_model() {
        let root = Root::new("build-empty");
        let id = session_id("empty");
        create(&root.0, &id, Some("gemma")).flush().unwrap();
        let session = Repository.build(&root.0, &id, "verkeerd-model").unwrap();
        assert_eq!(session.model, "gemma", "een vastgelegd model wint van de fallback");
        assert!(session.messages.is_empty() && session.items.is_empty());
        assert_eq!(session.persisted_messages, 0);
        assert_eq!(session.persisted_items_tail_id, None);
        assert!(!session.has_more_older());
    }

    #[test]
    fn a_session_without_a_recorded_model_falls_back_instead_of_inventing_one() {
        let root = Root::new("build-fallback");
        let id = session_id("fallback");
        create(&root.0, &id, None).flush().unwrap();
        let session = Repository.build(&root.0, &id, "ingesteld-model").unwrap();
        assert_eq!(session.model, "ingesteld-model");
    }

    #[test]
    fn persisting_appends_only_the_tail_and_reattaches_the_boundary() {
        let root = Root::new("persist-tail");
        let id = session_id("tail");
        create(&root.0, &id, Some("gemma")).flush().unwrap();
        let repo = Repository;
        let mut session = repo.build(&root.0, &id, "gemma").unwrap();
        session.messages.push(Msg::user("eerste"));
        session.items.push(user("eerste"));
        repo.persist(&root.0, &mut session).unwrap();
        assert_eq!(session.persisted_messages, 1);
        assert_eq!(session.persisted_items_tail_id.as_deref(), Some("u-eerste"));

        // Tweede beurt: de store groeit met één beurt en één rij, niet met alles.
        session.messages.push(Msg::assistant("antwoord"));
        session.items.push(agent("a-1", "antwoord"));
        session.previous_response_id = Some("resp-1".into());
        session.summary = Some("kort gesprek".into());
        repo.persist(&root.0, &mut session).unwrap();

        let rollout = Rollout::open(&root.0, &id).unwrap();
        assert_eq!(rollout.messages().unwrap().len(), 2, "één beurt schrijft één beurt weg");
        assert_eq!(rollout.items().unwrap().len(), 2);
        let detail = rollout.detail().unwrap().expect("een beurt persisteert ook de kop");
        assert_eq!(detail.prev_response_id.as_deref(), Some("resp-1"));
        assert_eq!(detail.summary.as_deref(), Some("kort gesprek"), "een samenvatting mag niet alleen na verdichting meegaan");
        assert_eq!(detail.seq, 2);
    }

    #[test]
    fn persisting_twice_writes_nothing_twice() {
        let root = Root::new("persist-idempotent");
        let id = session_id("idempotent");
        create(&root.0, &id, Some("gemma")).flush().unwrap();
        let repo = Repository;
        let mut session = repo.build(&root.0, &id, "gemma").unwrap();
        session.messages.push(Msg::user("één"));
        session.items.push(user("één"));
        repo.persist(&root.0, &mut session).unwrap();
        repo.persist(&root.0, &mut session).unwrap();
        repo.persist(&root.0, &mut session).unwrap();
        let rollout = Rollout::open(&root.0, &id).unwrap();
        assert_eq!(rollout.messages().unwrap().len(), 1, "iets persists is geen tweede write");
        assert_eq!(rollout.items().unwrap().len(), 1);
    }

    #[test]
    fn an_opened_session_round_trips_its_turns_and_cards() {
        let root = Root::new("round-trip");
        let id = session_id("round");
        create(&root.0, &id, Some("gemma")).flush().unwrap();
        let repo = Repository;
        let mut session = repo.build(&root.0, &id, "gemma").unwrap();
        session.messages.push(Msg::user("vraag"));
        session.messages.push(Msg::assistant("antwoord"));
        session.items.push(user("vraag"));
        session.items.push(agent("a-1", "antwoord"));
        session.items.push(Item::ToolCall(ToolCallItem {
            item_id: "t-1".into(),
            tool: "read_file".into(),
            detail: "src/main.rs".into(),
            status: ItemStatus::Completed,
            ok: true,
            output: "3 regels".into(),
        }));
        repo.persist(&root.0, &mut session).unwrap();

        let reopened = repo.build(&root.0, &id, "gemma").unwrap();
        assert_eq!(reopened.messages, session.messages);
        assert_eq!(reopened.items.len(), 3, "kaarten komen terug als kaarten, niet als proza");
        assert_eq!(reopened.items[2].kind(), session.items[2].kind());
        assert_eq!(reopened.persisted_messages, 2, "heropenen is geen legen transcript doen");
        assert_eq!(reopened.persisted_items_tail_id.as_deref(), Some("t-1"));
    }

    #[test]
    fn opening_reads_the_initial_page_and_leaves_the_rest_in_the_store() {
        let root = Root::new("paging-open");
        let id = session_id("paging");
        create(&root.0, &id, Some("gemma")).flush().unwrap();
        {
            let mut rollout = Rollout::open(&root.0, &id).unwrap();
            for seq in 0..25 {
                rollout.append_item(&agent(&format!("a-{seq}"), &format!("rij {seq}"))).unwrap();
            }
            rollout.flush().unwrap();
        }
        let reopened = Repository.build(&root.0, &id, "gemma").unwrap();
        assert_eq!(reopened.items.len(), page::INITIAL, "openen leest de staart, niet het bestand");
        assert_eq!(reopened.items.first().unwrap().item_id(), "a-15", "vijfentwintig rijen, de laatste tien");
        assert_eq!(reopened.items.last().unwrap().item_id(), "a-24");
        assert!(reopened.has_more_older());
        assert_eq!(reopened.oldest_loaded_seq, Some(15));
    }

    #[test]
    fn scrolling_up_walks_the_store_without_gaps_and_stops_at_the_start() {
        let root = Root::new("paging-scroll");
        let id = session_id("scroll");
        create(&root.0, &id, Some("gemma")).flush().unwrap();
        {
            let mut rollout = Rollout::open(&root.0, &id).unwrap();
            for seq in 0..25 {
                rollout.append_item(&agent(&format!("a-{seq}"), &format!("rij {seq}"))).unwrap();
            }
            rollout.flush().unwrap();
        }
        let repo = Repository;
        let mut seen: Vec<String> = Vec::new();
        let mut cursor = None;
        let mut pulls = 0;
        loop {
            let window = repo.items_window(&root.0, &id, page::SCROLL, cursor).unwrap();
            if window.items.is_empty() {
                break;
            }
            pulls += 1;
            for item in &window.items {
                seen.push(item.item_id().to_string());
            }
            match window.older_seq {
                Some(next) if window.has_more_older => cursor = Some(next),
                _ => break,
            }
            assert!(pulls < 10, "bladeren moet bij de start ophouden");
        }
        assert_eq!(pulls, 2, "vijfentwintig rijen in bladen van twintig zijn twee bladen");
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 25, "geen rij gemist en geen rij dubbel");
        assert_eq!(seen.first().unwrap(), "a-0", "de allereerste rij is bereikbaar");
    }

    #[test]
    fn rewriting_the_head_reattaches_so_the_next_persist_cannot_resurrect_it() {
        let root = Root::new("rewrite-head");
        let id = session_id("rewrite");
        create(&root.0, &id, Some("gemma")).flush().unwrap();
        let repo = Repository;
        let mut session = repo.build(&root.0, &id, "gemma").unwrap();
        for seq in 0..4 {
            session.messages.push(Msg::user(format!("beurt {seq}")));
        }
        repo.persist(&root.0, &mut session).unwrap();
        assert_eq!(session.persisted_messages, 4);

        // Verdichting: de kop wordt korter en de samenvatting vervangt hem.
        let compacted = vec![Msg::user("beurt 2"), Msg::assistant("beurt 3")];
        repo.rewrite_messages(&root.0, &mut session, compacted.clone()).unwrap();
        assert_eq!(session.persisted_messages, 2, "de anker wordt verzet, anders schrijft de volgende persist de weggegoonde staart terug");
        session.summary = Some("samenvatting".into());
        session.messages.push(Msg::user("na de verdichting"));
        repo.persist(&root.0, &mut session).unwrap();

        let rollout = Rollout::open(&root.0, &id).unwrap();
        let mut expected = compacted.clone();
        expected.push(Msg::user("na de verdichting"));
        assert_eq!(rollout.messages().unwrap(), expected, "wat verdwenen is, blijft verdwenen; wat daarna kwam, staat er wél bij");
        assert_eq!(rollout.detail().unwrap().unwrap().summary.as_deref(), Some("samenvatting"));
    }
}
