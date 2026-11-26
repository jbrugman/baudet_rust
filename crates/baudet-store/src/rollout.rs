
use crate::records::{Record, Session, SessionDetail, SessionMeta};
use baudet_core::{Item, Msg};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum Kind {
    Meta,
    Item,
    Msg,
    Detail,
}

const META_FILE: &str = "session.jsonl";
const ITEMS_FILE: &str = "items.jsonl";
const HISTORY_FILE: &str = "history.jsonl";
const DETAIL_FILE: &str = "detail.jsonl";

#[derive(Debug)]
pub struct Rollout {
    dir: PathBuf,
    meta: Session,
    meta_file: fs::File,
    items_file: fs::File,
    history_file: fs::File,
    detail_file: fs::File,
}

pub struct StagedRolloutDeletion {
    original: PathBuf,
    staging: PathBuf,
}

impl StagedRolloutDeletion {
    pub fn restore(self) -> std::io::Result<()> {
        fs::rename(self.staging.join("rollout"), &self.original)?;
        let _ = fs::remove_dir(self.staging);
        Ok(())
    }

    pub fn finish(self) -> std::io::Result<()> {
        fs::remove_dir_all(self.staging)
    }
}

impl Rollout {
    pub fn stage_deletion(root: &Path, id: &str) -> std::io::Result<Option<StagedRolloutDeletion>> {
        use std::io::{Error, ErrorKind};

        if Path::new(id).file_name() != Some(std::ffi::OsStr::new(id)) {
            return Err(Error::new(ErrorKind::InvalidInput, "invalid conversation ID"));
        }
        let day = root.join(sessions_root(id));
        let original = day.join(id);
        for dir in [&day, &original] {
            match fs::symlink_metadata(dir) {
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => return Err(Error::new(ErrorKind::InvalidData, "conversation path is not a directory")),
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
            }
        }
        if !read_meta(&original)?.is_some_and(|meta| meta.session_id == id) {
            return Err(Error::new(ErrorKind::InvalidData, "conversation metadata does not match its ID"));
        }
        let staging = day.join(format!(".{id}.deleting"));
        fs::create_dir(&staging)?;
        if let Err(error) = fs::rename(&original, staging.join("rollout")) {
            let _ = fs::remove_dir(&staging);
            return Err(error);
        }
        Ok(Some(StagedRolloutDeletion { original, staging }))
    }

    pub fn create(root: &Path, meta: &SessionMeta) -> Result<Rollout, std::io::Error> {
        let dir = session_dir(root, &meta.session_id);
        fs::create_dir_all(&dir)?;
        let meta_path = dir.join(META_FILE);
        if meta_path.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("session {} already has a rollout at {}", meta.session_id, dir.display()),
            ));
        }
        let mut rollout = open_in(&dir, meta.session())?;
        write_line(&mut rollout.meta_file, &Record::SessionMeta(meta.clone()))?;
        Ok(rollout)
    }

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

    pub fn session(&self) -> &Session {
        &self.meta
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    fn file(&mut self, kind: Kind) -> &mut fs::File {
        match kind {
            Kind::Meta => &mut self.meta_file,
            Kind::Item => &mut self.items_file,
            Kind::Msg => &mut self.history_file,
            Kind::Detail => &mut self.detail_file,
        }
    }

    pub fn append_item(&mut self, item: &Item) -> std::io::Result<()> {
        self.append(Kind::Item, &Record::Item(item.clone()))
    }

    pub fn append_msg(&mut self, msg: &Msg) -> std::io::Result<()> {
        self.append(Kind::Msg, &Record::Msg(msg.clone()))
    }

    pub fn commit_turn(&mut self, msg: &Msg) -> std::io::Result<()> {
        self.append_msg(msg)?;
        self.flush()
    }

    pub fn flush(&mut self) -> std::io::Result<()> {
        self.items_file.flush()?;
        self.history_file.flush()
    }

    // Route records by kind so display items cannot enter model history.
    fn append(&mut self, kind: Kind, record: &Record) -> std::io::Result<()> {
        write_line(self.file(kind), record)
    }

    pub fn write_detail(&mut self, detail: &SessionDetail) -> std::io::Result<()> {
        self.detail_file.set_len(0)?;
        self.detail_file.seek(SeekFrom::Start(0))?;
        write_line(&mut self.detail_file, &Record::SessionDetail(detail.clone()))
    }

    pub fn detail(&self) -> std::io::Result<Option<SessionDetail>> {
        Ok(read_records(&self.dir, DETAIL_FILE)?
            .into_iter()
            .filter_map(|record| match record {
                Record::SessionDetail(detail) => Some(detail),
                _ => None,
            })
            .next_back())
    }

    pub fn items(&self) -> std::io::Result<Vec<Item>> {
        let mut out = Vec::new();
        for record in read_records(&self.dir, ITEMS_FILE)? {
            if let Record::Item(item) = record {
                out.push(item);
            }
        }
        Ok(out)
    }

    // Read from the tail so opening a large transcript only decodes the visible page.
    pub fn item_rows(
        &self,
        limit: Option<usize>,
        before_seq: Option<u64>,
    ) -> std::io::Result<Vec<(u64, Item)>> {
        let path = self.dir.join(ITEMS_FILE);
        let want = limit.map(|limit| limit + 1);
        let mut out: Vec<(u64, Item)> = Vec::new();
        for (row, line) in read_lines_backwards(&path, want, before_seq)? {
            if let Ok(Record::Item(item)) = serde_json::from_str::<Record>(&line) {
                out.push((row, item));
            }
        }
        Ok(out)
    }

    pub fn item_count(&self) -> std::io::Result<usize> {
        let path = self.dir.join(ITEMS_FILE);
        match tail_index(&path) {
            Ok(index) => Ok(index.rows() as usize),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error),
        }
    }

    pub fn messages(&self) -> std::io::Result<Vec<Msg>> {
        self.context().map(|(messages, _)| messages)
    }

    pub(crate) fn context(&self) -> std::io::Result<(Vec<Msg>, Option<String>)> {
        let mut messages = Vec::new();
        let mut summary = None;
        for record in read_records(&self.dir, HISTORY_FILE)? {
            match record {
                Record::Msg(msg) => messages.push(msg),
                Record::CompactionCheckpoint(text) => summary = Some(text),
                _ => {}
            }
        }
        Ok((messages, summary))
    }

    // Replace the retained context and summary together. The display trail is independent.
    pub(crate) fn compact_context(&mut self, messages: &[Msg], summary: &str) -> std::io::Result<()> {
        let path = self.dir.join(HISTORY_FILE);
        let temp = path.with_extension("compact.tmp");
        let mut file = fs::OpenOptions::new().create(true).write(true).truncate(true).open(&temp)?;
        write_line(&mut file, &Record::CompactionCheckpoint(summary.to_owned()))?;
        for msg in messages {
            write_line(&mut file, &Record::Msg(msg.clone()))?;
        }
        file.sync_all()?;
        let append = fs::OpenOptions::new().append(true).open(&temp)?;
        fs::rename(&temp, &path)?;
        self.history_file = append;
        Ok(())
    }

    pub fn sessions(root: &Path) -> std::io::Result<Vec<Session>> {
        let mut found: Vec<(std::time::SystemTime, Session)> = Vec::new();
        let days = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        for day in days.flatten() {
            let day_dir = day.path();
            let sessions = match fs::read_dir(&day_dir) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            for entry in sessions.flatten() {
                let dir = entry.path();
                if let Some(meta) = read_meta(&dir)? {
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

fn write_line(file: &mut fs::File, record: &Record) -> std::io::Result<()> {
    let line = serde_json::to_string(record).map_err(std::io::Error::other)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")
}

fn session_dir(root: &Path, session_id: &str) -> PathBuf {
    root.join(sessions_root(session_id)).join(session_id)
}

fn sessions_root(session_id: &str) -> String {
    let id = session_id.strip_prefix("s-").unwrap_or(session_id);
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

fn newest_activity(dir: &Path) -> Option<std::time::SystemTime> {
    fs::metadata(dir.join(HISTORY_FILE)).ok()?.modified().ok()
}

fn read_records(dir: &Path, name: &str) -> std::io::Result<Vec<Record>> {
    let mut out = Vec::new();
    for line in read_lines(&dir.join(name))? {
        if let Ok(record) = serde_json::from_str::<Record>(&line) {
            out.push(record);
        }
    }
    Ok(out)
}

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

struct TailIndex {
    newlines: Vec<u64>,
}

impl TailIndex {
    fn rows(&self) -> u64 {
        self.newlines.len() as u64
    }
}

fn tail_index(path: &Path) -> std::io::Result<TailIndex> {
    let mut file = fs::File::open(path)?;
    let size = file.metadata()?.len();
    let mut newlines: Vec<u64> = Vec::new();
    if size > 0 {
        let mut buffer = vec![0u8; BLOCK];
        let mut end = size;
        while end > 0 {
            let start = end.saturating_sub(BLOCK as u64) as usize;
            file.seek(SeekFrom::Start(start as u64))?;
            let span = (end as usize) - start;
            let mut filled = 0usize;
            while filled < span {
                let read = file.read(&mut buffer[filled..span])?;
                if read == 0 {
                    break;
                }
                filled += read;
            }
            for offset in (0..filled).rev() {
                if buffer[offset] == b'\n' {
                    newlines.push(start as u64 + offset as u64);
                }
            }
            end -= span as u64;
        }
        newlines.reverse();
    }
    Ok(TailIndex { newlines })
}

fn read_lines_backwards(
    path: &Path,
    limit: Option<usize>,
    before_seq: Option<u64>,
) -> std::io::Result<Vec<(u64, String)>> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let index = tail_index(path)?;
    let bound = before_seq.unwrap_or_else(|| index.rows()).min(index.rows());
    let mut out: Vec<(u64, String)> = Vec::new();
    let mut row = bound;
    while row > 0 {
        if let Some(limit) = limit {
            if out.len() >= limit {
                break;
            }
        }
        row -= 1;
        let stop = index.newlines[row as usize] as usize;
        let start = if row == 0 { 0usize } else { index.newlines[row as usize - 1] as usize + 1 };
        if stop == start {
            out.push((row, String::new()));
            continue;
        }
        let mut buffer = vec![0u8; stop - start];
        file.seek(SeekFrom::Start(start as u64))?;
        file.read_exact(&mut buffer)?;
        out.push((row, String::from_utf8_lossy(&buffer).into_owned()));
    }
    Ok(out)
}

const BLOCK: usize = 64 * 1024;

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
            thumbnail: None,
            created_at_ms: None,
            item_id: "tu_1/user".into(),
            text: text.into(),
            status: ItemStatus::Completed,
        })
    }

    fn agent(turn: &str, text: &str) -> Item {
        Item::AgentMessage(AgentMessageItem {
            created_at_ms: None,
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
        file.write_all(br#"{"type":"msg","payload":{"role":"user","conte"#).unwrap();
        file.flush().unwrap();
        drop(file);

        let back = Rollout::open(&root.0, id).unwrap().messages().unwrap();
        assert_eq!(back.len(), 2, "the whole lines must survive the torn tail");
        assert_eq!(back[1].content, "gaat door");
    }

    #[test]
    fn a_final_whole_line_is_not_mistaken_for_a_torn_one() {
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
        assert_eq!(sessions_root("s-abc"), "unknown");
    }

    #[test]
    fn creating_over_an_existing_session_is_refused() {
        let root = Root::new("exists");
        let id = "s-2026-11-19T09-00-06Z-aaaa5555";
        Rollout::create(&root.0, &SessionMeta::new(id)).unwrap();
        let second = Rollout::create(&root.0, &SessionMeta::new(id));
        assert_eq!(second.err().map(|e| e.kind()), Some(std::io::ErrorKind::AlreadyExists));
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
