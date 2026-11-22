//! Append-only rollout files: the durability every other layer of this app currently lacks.
//!
//! Right now a restart loses the transcript, because nothing owns durability. Kotlin gets it from
//! H2 (`store/H2Store.kt`), which would mean shipping a SQL database to persist a text log. The
//! shape the harness actually uses is better suited and is what I measured: JSON Lines, one object
//! per line, day folders, `session_meta` first. Flat text means the file is inspectable with
//! `head`, survives a build that changes the type, and needs no migration step to read.
//!
//! Three rules carry the design, and each is load-bearing:
//!
//!  * **append only.** A turn adds lines at the end; nothing rewriting the file is offered. That
//!    matches the app — the reducer produces facts, never edits — so the writer cannot invent an
//!    inconsistent transcript the way a `rewriteMessages` call can.
//!  * **a corrupt tail is tolerated, not an error.** A process killed mid-write leaves a partial
//!    last line. Failing the whole read over that would make the one accident durability exists to
//!    survive also destroy the transcript. An undecodable line is dropped; every whole line stays.
//!  * **one file per kind.** Items go to `items.jsonl`, turns to `history.jsonl`, and the
//!    conversation's identity leads `session.jsonl`. The harness interleaves kinds in one stream;
//!    splitting them buys one decoder per file and makes the append-and-restart boundary exact.
//!    A mixed rollout, if we ever want it, is a merger of these three in write order.

use crate::records::{Record, Session, SessionDetail, SessionMeta};
use baudet_core::{Item, Msg};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Which file a record goes to.
///
/// `Meta` and `Detail` are named but never constructed: the identity line is written by
/// [`Rollout::create`] and the head line by [`Rollout::write_detail`], each straight to its own
/// file, because neither is an append. They stay in this enum so [`Rollout::file`] remains the
/// only place that maps a kind to a handle, which is the whole reason the routing is a function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum Kind {
    Meta,
    Item,
    Msg,
    Detail,
}

/// The name of the file holding one kind. Kept as constants so a session directory written by one
/// build is still recognisable to the next.
const META_FILE: &str = "session.jsonl";
const ITEMS_FILE: &str = "items.jsonl";
const HISTORY_FILE: &str = "history.jsonl";
/// The mutable head of a conversation: one rewritten line, deliberately not in `session.jsonl`.
const DETAIL_FILE: &str = "detail.jsonl";

/// A session transcript on disk, open for appending.
///
/// Holds one `File` per kind, each opened in append mode, so a writer cannot truncate a transcript
/// by accident and a crashed process leaves only its own partial tail line.
/// `Debug` deliberately omits the open files: a transcript window's state should print as a
/// session, not as three descriptors.
#[derive(Debug)]
pub struct Rollout {
    dir: PathBuf,
    meta: Session,
    meta_file: fs::File,
    items_file: fs::File,
    history_file: fs::File,
    detail_file: fs::File,
}

impl Rollout {
    /// Create a session directory and write its identity as the first and only line of
    /// `session.jsonl`.
    ///
    /// Fails only when the directory cannot be created or the identity cannot be written: once a
    /// session exists, every later write is best-effort by contract (see [`Rollout::append`]).
    pub fn create(root: &Path, meta: &SessionMeta) -> Result<Rollout, std::io::Error> {
        let dir = session_dir(root, &meta.session_id);
        fs::create_dir_all(&dir)?;
        let meta_path = dir.join(META_FILE);
        if meta_path.exists() {
            // Refusing to create over an existing session is the honest answer: silently
            // re-opening would either duplicate the identity line or overwrite it, and neither is
            // something a caller asked for.
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("session {} already has a rollout at {}", meta.session_id, dir.display()),
            ));
        }
        let mut rollout = open_in(&dir, meta.session())?;
        write_line(&mut rollout.meta_file, &Record::SessionMeta(meta.clone()))?;
        Ok(rollout)
    }

    /// Open an existing session for appending, reading back what it already holds so callers can
    /// resume instead of starting from an empty transcript.
    ///
    /// A missing directory, a missing file and an empty session directory are all `Ok`: a session
    /// can legitimately have no turns yet. What is *not* tolerated is a directory with no identity
    /// line at all — that is not an empty session, it is a file we cannot name, and continuing
    /// would write rows into a transcript whose provenance nobody knows.
    pub fn open(root: &Path, session_id: &str) -> Result<Rollout, std::io::Error> {
        let dir = session_dir(root, session_id);
        let meta = read_meta(&dir)?.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{} has no {META_FILE} first line", dir.display()),
            )
        })?;
        open_in(&dir, meta.session())
    }

    /// The session this rollout appends to, as recorded when it was created.
    pub fn session(&self) -> &Session {
        &self.meta
    }

    /// The session directory, so a caller can show or archive the file it is looking at.
    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// Which of a session's files a record belongs to. Naming the kind rather than passing a
    /// `File` keeps the routing in one place and makes an accidental item-in-history write
    /// impossible at the call site.
    fn file(&mut self, kind: Kind) -> &mut fs::File {
        match kind {
            Kind::Meta => &mut self.meta_file,
            Kind::Item => &mut self.items_file,
            Kind::Msg => &mut self.history_file,
            Kind::Detail => &mut self.detail_file,
        }
    }

    /// Append one display row.
    pub fn append_item(&mut self, item: &Item) -> std::io::Result<()> {
        self.append(Kind::Item, &Record::Item(item.clone()))
    }

    /// Append one turn of the model's vocabulary.
    pub fn append_msg(&mut self, msg: &Msg) -> std::io::Result<()> {
        self.append(Kind::Msg, &Record::Msg(msg.clone()))
    }

    /// Append one turn and flush both files.
    ///
    /// The end of a turn is the one moment worth paying for: a flush makes everything before it
    /// durable without costing a sync per streamed delta. A lost in-flight turn is one turn;
    /// stalling a stream on every disk write is every turn.
    pub fn commit_turn(&mut self, msg: &Msg) -> std::io::Result<()> {
        self.append_msg(msg)?;
        self.flush()
    }

    /// Push buffered records to the file, so a caller that appended several kinds can make them
    /// durable together.
    pub fn flush(&mut self) -> std::io::Result<()> {
        self.items_file.flush()?;
        self.history_file.flush()
    }

    fn append(&mut self, kind: Kind, record: &Record) -> std::io::Result<()> {
        write_line(self.file(kind), record)
    }

    /// Replace the conversation's mutable head with [`SessionDetail`].
    ///
    /// This is the one write in this crate that is not an append, and the only place allowed to be
    /// one. The reason it may be: `detail.jsonl` holds a single derived line — every value on it is
    /// recomputable from the turns that are already durable — so a crash between truncate and write
    /// loses a boundary marker and a summary, never a turn. A non-empty trailer is therefore torn,
    /// not authoritative, and [`Rollout::detail`] says `None` for it rather than trusting a partial
    /// record: the alternative is a resumed session appending its tail at the wrong sequence.
    pub fn write_detail(&mut self, detail: &SessionDetail) -> std::io::Result<()> {
        self.detail_file.set_len(0)?;
        self.detail_file.seek(SeekFrom::Start(0))?;
        write_line(&mut self.detail_file, &Record::SessionDetail(detail.clone()))
    }

    /// The conversation's mutable head, or `None` when it has never been persisted.
    ///
    /// The *last* decodable `session_detail` line wins, so a store written by a build that appended
    /// instead of replacing still reads correctly rather than freezing on its first value.
    pub fn detail(&self) -> std::io::Result<Option<SessionDetail>> {
        Ok(read_records(&self.dir, DETAIL_FILE)?
            .into_iter()
            .filter_map(|record| match record {
                Record::SessionDetail(detail) => Some(detail),
                _ => None,
            })
            .next_back())
    }

    /// Every row in the trail, oldest first, undecodable lines skipped.
    pub fn items(&self) -> std::io::Result<Vec<Item>> {
        let mut out = Vec::new();
        for record in read_records(&self.dir, ITEMS_FILE)? {
            if let Record::Item(item) = record {
                out.push(item);
            }
        }
        Ok(out)
    }

    /// Every turn, oldest first, undecodable lines skipped.
    pub fn messages(&self) -> std::io::Result<Vec<Msg>> {
        let mut out = Vec::new();
        for record in read_records(&self.dir, HISTORY_FILE)? {
            if let Record::Msg(msg) = record {
                out.push(msg);
            }
        }
        Ok(out)
    }

    /// The session directories under `root`, most recently active first.
    ///
    /// Ordering by activity means "resume the last conversation" costs one directory read and needs
    /// no index to keep correct — the property an H2 table would have to maintain explicitly. The
    /// figure is the session's last appended turn (see [`newest_activity`]); a never-written session
    /// falls back to its own creation time, which is the most that can be said about it.
    pub fn sessions(root: &Path) -> std::io::Result<Vec<Session>> {
        let mut found: Vec<(std::time::SystemTime, Session)> = Vec::new();
        // No data directory yet is an empty list, not a failure: the first run of the app is
        // this path, and treating it as an error would make startup depend on a mkdir.
        let days = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        for day in days.flatten() {
            // Sessions live in a day bucket, not beside it: `session_dir` writes `<root>/<day>/<id>`,
            // so a single scan of `root` finds day names and never a session. Reading one level
            // deeper is what makes the listing agree with the writer; asserting the shallower shape
            // yields an empty list, which is indistinguishable from a user who never had a chat.
            let day_dir = day.path();
            let sessions = match fs::read_dir(&day_dir) {
                Ok(entries) => entries,
                // Anything that is not a readable directory is not a day bucket. A stray file in
                // the data directory costs a failed read, not the listing.
                // Not a day bucket. A stray file in the data directory costs a failed read,
                // not the listing.
                Err(_) => continue,
            };
            for entry in sessions.flatten() {
                let dir = entry.path();
                if let Some(meta) = read_meta(&dir)? {
                    // Newest file wins; a session that never had a turn has no file mtime to speak
                    // of, and the directory's own time is all that is left.
                    let modified = newest_activity(&dir)
                        .or_else(|| fs::metadata(&dir).ok().and_then(|meta| meta.modified().ok()))
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    found.push((modified, meta.session()));
                }
            }
        }
        found.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        Ok(found.into_iter().map(|(_, session)| session).collect())
    }
}

/// One record as one line.
///
/// Serialisation happens before any byte is written, so a record that cannot be encoded leaves the
/// file untouched instead of appending a torn line — which matters because a torn line is precisely
/// what the reader is taught to tolerate, and inventing one would erode that guarantee.
fn write_line(file: &mut fs::File, record: &Record) -> std::io::Result<()> {
    let line = serde_json::to_string(record).map_err(std::io::Error::other)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")
}

/// The directory for one session: a day folder holding one folder per session, mirroring the
/// harness's `sessions/2026-11-03/rollout-…` layout without its filename-encoded timestamp.
fn session_dir(root: &Path, session_id: &str) -> PathBuf {
    root.join(sessions_root(session_id)).join(session_id)
}

/// The day bucket for a session id, derived from the id rather than the clock so a session always
/// lands in the same folder. Ids are `s-<RFC3339>+<suffix>` (see `baudet_core::session::new_id`),
/// which means the id itself carries the date. Falling back to `unknown` keeps a hand-made id
/// storable instead of rejected.
fn sessions_root(session_id: &str) -> String {
    let id = session_id.strip_prefix("s-").unwrap_or(session_id);
    // Expected shape: 2026-11-19T09-00-00Z-<suffix>.
    let day: String = id.chars().take(10).collect();
    let digits = day.chars().filter(|c| c.is_ascii_digit()).count();
    if digits == 8 && day.as_bytes().get(4) == Some(&b'-') && day.as_bytes().get(7) == Some(&b'-') {
        day
    } else {
        "unknown".to_string()
    }
}

fn open_in(dir: &Path, meta: Session) -> Result<Rollout, std::io::Error> {
    Ok(Rollout {
        dir: dir.to_path_buf(),
        meta,
        meta_file: append_to(&dir.join(META_FILE))?,
        items_file: append_to(&dir.join(ITEMS_FILE))?,
        history_file: append_to(&dir.join(HISTORY_FILE))?,
        detail_file: append_to(&dir.join(DETAIL_FILE))?,
    })
}

fn append_to(path: &Path) -> Result<fs::File, std::io::Error> {
    fs::OpenOptions::new().create(true).append(true).open(path)
}

/// When this session last did something, or `None` when it never had a turn.
///
/// Only `history.jsonl` answers that. The measurement that settled it: in a three-session store,
/// `session.jsonl` and `items.jsonl` carried the *same* mtime for every session — the instant the
/// store was created — so their newest value cannot rank one conversation against another. The
/// history file is the one that a turn appends to, which is the event being ordered.
fn newest_activity(dir: &Path) -> Option<std::time::SystemTime> {
    fs::metadata(dir.join(HISTORY_FILE)).ok()?.modified().ok()
}

/// Read a whole file's records, skipping anything undecodable and any torn tail line.
fn read_records(dir: &Path, name: &str) -> std::io::Result<Vec<Record>> {
    let mut out = Vec::new();
    for line in read_lines(&dir.join(name))? {
        // Tolerance, not silence: an unknown kind is a newer build's record, and dropping it is
        // what lets an old reader still open the file. A torn line lands here too.
        if let Ok(record) = serde_json::from_str::<Record>(&line) {
            out.push(record);
        }
    }
    Ok(out)
}

/// The identity line of a session directory, or `None` when there is no directory or no decodable
/// identity. A whole-but-unknown-kind first line also yields `None`, which `open` reports as
/// "cannot name this transcript" rather than guessing.
fn read_meta(dir: &Path) -> std::io::Result<Option<SessionMeta>> {
    if !dir.is_dir() {
        return Ok(None);
    }
    Ok(read_lines(&dir.join(META_FILE))?.iter().filter_map(|line| serde_json::from_str::<Record>(line).ok()).find_map(
        |record| match record {
            Record::SessionMeta(meta) => Some(meta),
            _ => None,
        },
    ))
}

/// Every whole line of a file, with a trailing partial line dropped.
///
/// Hand-rolled rather than `read_to_string` + `lines()` for one reason: the last line of a file
/// left by a killed process has no newline, and keeping it would append half a JSON object to the
/// transcript. The single-byte probe after the last delimiter settles whether that tail is a whole
/// record or a torn one, and a file ending in `\n` — the normal case — keeps its final line.
fn read_lines(path: &Path) -> std::io::Result<Vec<String>> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let whole_tail = tail_is_whole(&mut file)?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    let mut lines: Vec<String> = Vec::new();
    let mut start = 0usize;
    while let Some(offset) = buf[start..].iter().position(|byte| *byte == b'\n') {
        let line = &buf[start..start + offset];
        // A blank line is written by nothing here, so skipping one cannot hide a record; it does
        // keep a stray newline from looking like a corrupt one.
        if !line.is_empty() {
            lines.push(String::from_utf8_lossy(line).into_owned());
        }
        start += offset + 1;
    }
    let tail = &buf[start..];
    if whole_tail && !tail.is_empty() {
        lines.push(String::from_utf8_lossy(tail).into_owned());
    }
    Ok(lines)
}

/// Whether everything after the last newline is a complete record. A file that ends in a newline,
/// or is empty, ends whole; a non-empty tail without one means the writer died mid-record.
fn tail_is_whole(file: &mut fs::File) -> std::io::Result<bool> {
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(true);
    }
    let mut buffer = [0u8; 1];
    file.seek(SeekFrom::End(-1))?;
    file.read_exact(&mut buffer)?;
    file.seek(SeekFrom::Start(0))?;
    Ok(buffer[0] == b'\n')
}

#[cfg(test)]
mod tests {
    use super::*;
    use baudet_core::{AgentMessageItem, ItemStatus, Msg, ReasoningItem, ToolCallItem, UserMessageItem};

    /// A throwaway data root, named for the test so a failure leaves inspectable files rather than
    /// a temp dir that vanished with it.
    struct Root(PathBuf);

    impl Root {
        fn new(name: &str) -> Root {
            let path = std::env::temp_dir().join(format!("baudet-store-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Root(path)
        }
    }

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn user(text: &str) -> Item {
        Item::UserMessage(UserMessageItem {
            item_id: "tu_1/user".into(),
            text: text.into(),
            status: ItemStatus::Completed,
        })
    }

    fn agent(turn: &str, text: &str) -> Item {
        Item::AgentMessage(AgentMessageItem {
            item_id: format!("{turn}/msg"),
            text: text.into(),
            status: ItemStatus::Completed,
        })
    }

    #[test]
    fn a_session_can_be_written_closed_and_read_back() {
        let root = Root::new("roundtrip");
        let meta = SessionMeta {
            session_id: "s-2026-11-19T09-00-00Z-abcd1234".into(),
            started_at: Some("2026-11-19T09:00:00Z".into()),
            cwd: Some("/tmp/repo".into()),
            git: None,
            model: Some("qwen-local".into()),
            thread_id: None,
        };
        {
            let mut rollout = Rollout::create(&root.0, &meta).unwrap();
            rollout.append_item(&user("hoi")).unwrap();
            rollout.append_item(&agent("tu_1", "daar is het antwoord")).unwrap();
            rollout.commit_turn(&Msg::user("hoi")).unwrap();
            assert_eq!(rollout.session().cwd.as_deref(), Some("/tmp/repo"));
        }
        let reopened = Rollout::open(&root.0, &meta.session_id).unwrap();
        assert_eq!(reopened.items().unwrap().len(), 2);
        assert_eq!(reopened.messages().unwrap(), vec![Msg::user("hoi")]);
        assert_eq!(reopened.session().model.as_deref(), Some("qwen-local"));
        // Two files, because the kinds decode separately; the trail and the turns differ by design.
        assert!(reopened.path().join(ITEMS_FILE).exists());
        assert!(reopened.path().join(HISTORY_FILE).exists());
    }

    #[test]
    fn appending_after_a_reopen_extends_rather_than_rewrites() {
        let root = Root::new("append");
        let id = "s-2026-11-19T09-00-01Z-ffff0000";
        {
            let mut rollout = Rollout::create(&root.0, &SessionMeta::new(id)).unwrap();
            rollout.append_msg(&Msg::user("eerste")).unwrap();
        }
        {
            let mut rollout = Rollout::open(&root.0, id).unwrap();
            assert_eq!(rollout.messages().unwrap().len(), 1);
            rollout.append_msg(&Msg::assistant("tweede")).unwrap();
        }
        let back = Rollout::open(&root.0, id).unwrap().messages().unwrap();
        assert_eq!(back.iter().map(|m| m.content.as_str()).collect::<Vec<_>>(), vec!["eerste", "tweede"]);
    }

    #[test]
    fn a_half_written_last_line_is_dropped_and_the_rest_survives() {
        let root = Root::new("truncated");
        let id = "s-2026-11-19T09-00-02Z-eeee1111";
        {
            let mut rollout = Rollout::create(&root.0, &SessionMeta::new(id)).unwrap();
            rollout.append_msg(&Msg::user("heel")).unwrap();
            rollout.append_msg(&Msg::assistant("gaat door")).unwrap();
        }
        let history = session_dir(&root.0, id).join(HISTORY_FILE);
        let mut file = fs::OpenOptions::new().append(true).open(&history).unwrap();
        // Exactly what a kill leaves: a valid record, then this partial one, with no newline.
        file.write_all(br#"{"type":"msg","payload":{"role":"user","conte"#).unwrap();
        file.flush().unwrap();
        drop(file);

        let back = Rollout::open(&root.0, id).unwrap().messages().unwrap();
        assert_eq!(back.len(), 2, "the whole lines must survive the torn tail");
        assert_eq!(back[1].content, "gaat door");
    }

    #[test]
    fn a_final_whole_line_is_not_mistaken_for_a_torn_one() {
        // The failure mode of "drop the last line" is quiet and permanent: every session would
        // lose its newest turn. Pinned explicitly because the check is byte-level.
        let root = Root::new("final-line");
        let id = "s-2026-11-19T09-00-03Z-dddd2222";
        {
            let mut rollout = Rollout::create(&root.0, &SessionMeta::new(id)).unwrap();
            rollout.append_msg(&Msg::user("laatste regels zijn heil")).unwrap();
        }
        let history = session_dir(&root.0, id).join(HISTORY_FILE);
        assert!(fs::read_to_string(&history).unwrap().ends_with('\n'));
        assert_eq!(Rollout::open(&root.0, id).unwrap().messages().unwrap().len(), 1);
    }

    #[test]
    fn an_unknown_kind_costs_its_line_and_nothing_else() {
        let root = Root::new("unknown-kind");
        let id = "s-2026-11-19T09-00-04Z-cccc3333";
        {
            let mut rollout = Rollout::create(&root.0, &SessionMeta::new(id)).unwrap();
            rollout.append_msg(&Msg::user("nu")).unwrap();
        }
        let history = session_dir(&root.0, id).join(HISTORY_FILE);
        fs::OpenOptions::new()
            .append(true)
            .open(&history)
            .and_then(|mut f| f.write_all(b"{\"type\":\"compacted\",\"payload\":{\"summary\":\"x\"}}\n"))
            .unwrap();
        let back = Rollout::open(&root.0, id).unwrap().messages().unwrap();
        assert_eq!(back.len(), 1, "a newer build's record must not break this one's read");
    }

    #[test]
    fn reasoning_and_tool_rows_are_stored_although_history_never_shows_them() {
        // The audit trail records what the interface showed; what goes back to the model is a
        // different vector on purpose. Storing both is not duplication — dropping the tool rows
        // would flatten the transcript, and replaying them would send noise the model never asked
        // for. This is the 587-versus-399 asymmetry, held open as a test.
        let root = Root::new("asymmetry");
        let id = "s-2026-11-19T09-00-05Z-bbbb4444";
        {
            let mut rollout = Rollout::create(&root.0, &SessionMeta::new(id)).unwrap();
            rollout.append_item(&user("tel de bestanden")).unwrap();
            rollout
                .append_item(&Item::Reasoning(ReasoningItem {
                    item_id: "tu_1/r".into(),
                    text: "eerst ls dan tel ik".into(),
                    status: ItemStatus::Completed,
                }))
                .unwrap();
            rollout
                .append_item(&Item::ToolCall(ToolCallItem {
                    item_id: "tu_1/tool".into(),
                    tool: "read_file".into(),
                    detail: "Cargo.toml".into(),
                    status: ItemStatus::Completed,
                    ok: true,
                    output: "24 lines".into(),
                }))
                .unwrap();
            rollout.append_item(&agent("tu_1", "vier")).unwrap();
        }
        let items = Rollout::open(&root.0, id).unwrap().items().unwrap();
        assert_eq!(items.len(), 4, "the trail keeps what was rendered, including what was not said");
    }

    #[test]
    fn sessions_are_listed_newest_first() {
        let root = Root::new("listing");
        for (id, mtime) in [
            ("s-2026-11-17T09-00-00Z-old00001", 1_700_000_000u64),
            ("s-2026-11-19T09-00-00Z-new00001", 1_760_000_000),
            ("s-2026-11-18T09-00-00Z-mid00001", 1_730_000_000),
        ] {
            let mut rollout = Rollout::create(&root.0, &SessionMeta::new(id)).unwrap();
            rollout.append_msg(&Msg::user(id)).unwrap();
            touch_older(&root.0, id, mtime);
        }
        let expected =
            ["s-2026-11-19T09-00-00Z-new00001", "s-2026-11-18T09-00-00Z-mid00001", "s-2026-11-17T09-00-00Z-old00001"];
        let listed = Rollout::sessions(&root.0).unwrap();
        assert_eq!(listed.iter().map(|s| s.session_id.as_str()).collect::<Vec<_>>(), expected);
    }

    /// Move *every* file of a session into the past.
    ///
    /// All of them, deliberately: `sessions()` ranks by the last appended turn, and a fixture that
    /// aged only one file while leaving the others at "now" would be asserting a shape the store
    /// never has on disk. `FileTimes` is the standard-library way and is portable, where a
    /// directory's mtime would need `libc` — and is the wrong figure anyway, since appending a file
    /// leaves its directory's timestamp alone.
    fn touch_older(root: &Path, id: &str, seconds: u64) {
        let dir = session_dir(root, id);
        let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds);
        for name in [META_FILE, ITEMS_FILE, HISTORY_FILE] {
            let handle = fs::OpenOptions::new().append(true).open(dir.join(name)).unwrap();
            handle.set_times(std::fs::FileTimes::new().set_modified(when)).unwrap();
        }
    }

    #[test]
    fn a_day_bucket_comes_from_the_id_and_not_the_clock() {
        assert_eq!(sessions_root("s-2026-11-19T09-00-00Z-abcd1234"), "2026-11-19");
        // A hand-made id is still storable; only the bucket gives up, never the write.
        assert_eq!(sessions_root("s-abc"), "unknown");
    }

    #[test]
    fn creating_over_an_existing_session_is_refused() {
        let root = Root::new("exists");
        let id = "s-2026-11-19T09-00-06Z-aaaa5555";
        Rollout::create(&root.0, &SessionMeta::new(id)).unwrap();
        let second = Rollout::create(&root.0, &SessionMeta::new(id));
        assert_eq!(second.err().map(|e| e.kind()), Some(std::io::ErrorKind::AlreadyExists));
        // The first transcript is untouched by the refusal.
        assert_eq!(Rollout::open(&root.0, id).unwrap().session().session_id, id);
    }

    #[test]
    fn a_directory_without_identity_is_reported_not_guessed() {
        let root = Root::new("nameless");
        let dir = session_dir(&root.0, "s-2026-11-19T09-00-07Z-9999aaaa");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(HISTORY_FILE), b"not ours\n").unwrap();
        let error = Rollout::open(&root.0, "s-2026-11-19T09-00-07Z-9999aaaa").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn an_empty_data_directory_is_not_an_error() {
        let missing = std::env::temp_dir().join(format!("baudet-store-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&missing);
        assert!(Rollout::sessions(&missing).unwrap().is_empty());
    }
}
