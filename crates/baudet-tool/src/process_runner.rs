
use std::collections::VecDeque;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const HEAD_BYTES: usize = 8 * 1024;
const TAIL_BYTES: usize = 8 * 1024;
const CAP_BYTES: usize = HEAD_BYTES + TAIL_BYTES;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Captured {
    pub text: String,
    pub truncated: bool,
    pub length: usize,
}

impl Captured {
    pub fn section(&self, label: &str) -> String {
        if self.length == 0 {
            return String::new();
        }
        let marker = if self.truncated { format!(" (truncated, {} bytes read)", self.length) } else { String::new() };
        format!("\n--- {label}{marker} ---\n{}", self.text)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProcessOutcome {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: Captured,
    pub stderr: Captured,
    pub duration_ms: u64,
}

impl ProcessOutcome {
    pub fn body(&self) -> String {
        let mut out = self.stdout.section("stdout");
        out.push_str(&self.stderr.section("stderr"));
        if self.stdout.length == 0 && self.stderr.length == 0 {
            out.push_str("\n(no output)");
        }
        out
    }
}

const JOIN_MILLIS: u64 = 1_000;

#[derive(Debug, thiserror::Error)]
#[error("could not start {program}: {source}")]
pub struct StartError {
    program: String,
    #[source]
    source: std::io::Error,
}

pub struct ProcessRunner;

impl ProcessRunner {
    pub fn run(argv: &[&str], dir: &Path, timeout: Duration) -> Result<ProcessOutcome, StartError> {
        Self::run_with(argv, dir, timeout, |_| {})
    }

    pub fn run_with(
        argv: &[&str],
        dir: &Path,
        timeout: Duration,
        configure: impl FnOnce(&mut Command),
    ) -> Result<ProcessOutcome, StartError> {
        let started_at = Instant::now();
        let program = argv.first().copied().unwrap_or_default();
        let mut command = Command::new(program);
        command.args(&argv[1..]).current_dir(dir).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        configure(&mut command);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn().map_err(|source| StartError { program: program.to_owned(), source })?;

        let out_thread = drain_thread(child.stdout.take(), "stdout");
        let err_thread = drain_thread(child.stderr.take(), "stderr");

        let timeout_ms = timeout.as_millis() as u64;
        let outcome = wait_with_timeout(&mut child, timeout_ms);
        let timed_out = outcome.is_none();
        let exit_code = match outcome {
            Some(status) => status.code(),
            None => {
                kill_timed_out_process(&mut child);
                let _ = child.wait();
                None
            }
        };

        let stdout = collect_drain(out_thread);
        let stderr = collect_drain(err_thread);

        Ok(ProcessOutcome {
            exit_code,
            timed_out,
            stdout,
            stderr,
            duration_ms: started_at.elapsed().as_millis() as u64,
        })
    }
}

fn kill_timed_out_process(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        const SIGKILL: std::ffi::c_int = 9;
        unsafe extern "C" {
            fn kill(pid: std::ffi::c_int, sig: std::ffi::c_int) -> std::ffi::c_int;
        }

        if let Ok(pid) = std::ffi::c_int::try_from(child.id()) {
            // The unreaped child leads the group created at spawn.
            if pid > 0 && unsafe { kill(-pid, SIGKILL) } == 0 {
                return;
            }
        }
    }
    let _ = child.kill();
}

fn wait_with_timeout(child: &mut std::process::Child, timeout_ms: u64) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    return None;
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return Some(std::process::ExitStatus::default()),
        }
    }
}

fn drain_thread<R: Read + Send + 'static>(reader: Option<R>, _label: &str) -> thread::JoinHandle<Captured> {
    thread::spawn(move || match reader {
        Some(reader) => read_capped(Box::new(reader)),
        None => Captured::default(),
    })
}

fn collect_drain(handle: thread::JoinHandle<Captured>) -> Captured {
    // A descendant may still hold the pipe after the direct child exits.
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let captured = handle.join().unwrap_or_default();
        let _ = sender.send(captured);
    });
    receiver.recv_timeout(Duration::from_millis(JOIN_MILLIS)).unwrap_or_default()
}

fn read_capped(input: Box<dyn Read>) -> Captured {
    // Continue draining after the capture limit to avoid blocking the child.
    let mut buffer = vec![0u8; 8192];
    let mut front: Vec<u8> = Vec::new();
    let mut tail: VecDeque<Vec<u8>> = VecDeque::new();
    let mut tail_size = 0usize;
    let mut total = 0usize;

    let mut reader = input;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                total += n;
                if front.len() < CAP_BYTES {
                    let room = CAP_BYTES - front.len();
                    front.extend_from_slice(&buffer[..n.min(room)]);
                }
                tail_size += n;
                tail.push_back(buffer[..n].to_vec());
                while tail_size > TAIL_BYTES && tail.len() > 1 {
                    if let Some(dropped) = tail.pop_front() {
                        tail_size -= dropped.len();
                    }
                }
            }
        }
    }
    let truncated = total > CAP_BYTES;
    Captured { text: render(&front, &tail, total, truncated), truncated, length: total }
}

fn render(front: &[u8], tail: &VecDeque<Vec<u8>>, total: usize, truncated: bool) -> String {
    let all = String::from_utf8_lossy(front).into_owned();
    if !truncated {
        return all;
    }
    let tail_text: String = tail.iter().map(|chunk| String::from_utf8_lossy(chunk).into_owned()).collect();
    let head: String = all.chars().take(HEAD_BYTES).collect();
    let omitted = total.saturating_sub(head.len()).saturating_sub(tail_text.len());
    format!("{head}\n\n[... {omitted} bytes omitted ...]\n\n{tail_text}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str, timeout: Duration) -> ProcessOutcome {
        let dir = std::env::temp_dir();
        ProcessRunner::run(&["/bin/sh", "-c", script], &dir, timeout).expect("sh moet hier starten")
    }

    #[test]
    #[cfg(unix)]
    fn a_timeout_closes_pipes_held_by_a_background_child() {
        let result = sh("sleep 5 & echo stdout; echo stderr >&2; wait", Duration::from_millis(200));
        assert!(result.timed_out);
        assert_eq!(result.exit_code, None);
        assert_eq!(result.stdout.text, "stdout\n");
        assert_eq!(result.stderr.text, "stderr\n");
        assert!(result.duration_ms < JOIN_MILLIS, "timeout took {} ms", result.duration_ms);
    }

    #[test]
    fn it_keeps_the_exit_code_of_a_clean_run() {
        let result = sh("exit 3", Duration::from_secs(10));
        assert_eq!(result.exit_code, Some(3), "de code moet heel blijven, die is de feedback voor het model");
        assert!(!result.timed_out);
    }

    #[test]
    fn it_captures_both_streams_without_mixing_them() {
        let result = sh("echo naar-stdout; echo naar-stderr >&2", Duration::from_secs(10));
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.stdout.text, "naar-stdout\n");
        assert_eq!(result.stderr.text, "naar-stderr\n");
        assert_eq!(result.stdout.length, 12);
        assert_eq!(result.stderr.length, 12);
    }

    #[test]
    fn a_timeout_kills_the_run_and_reports_no_exit_code() {
        let started = Instant::now();
        let result = sh("sleep 2", Duration::from_millis(200));
        assert!(result.timed_out);
        assert_eq!(result.exit_code, None, "een gedoofd proces heeft geen exit code");
        assert!(started.elapsed() < Duration::from_secs(2), "de timeout moet echt doven, niet wachten");
    }

    #[test]
    fn a_chatty_run_neither_hangs_the_pipe_nor_floods_the_text() {
        let result = sh(
            "for i in $(seq 1 8000); do echo 'regel met tekst om wat breedte te vullen'; done",
            Duration::from_secs(30),
        );
        assert!(result.stdout.length > CAP_BYTES * 4, "de stream moet echt boven de cap uit komen");
        assert!(result.stdout.truncated);
        assert!(result.stdout.text.len() < result.stdout.length, "de bewaarde tekst blijft onder wat er was");
        assert!(result.stdout.text.contains("[... ") && result.stdout.text.contains("bytes omitted ...]"));
        assert_eq!(result.exit_code, Some(0), "de run liep gewoon af, ook bij knipping");
    }

    #[test]
    fn the_kept_tail_holds_the_last_lines_because_failures_report_there() {
        let result = sh("for i in $(seq 1 8000); do echo $i; done; echo LAATSTE-REGEL", Duration::from_secs(30));
        assert!(result.stdout.truncated);
        assert!(result.stdout.text.contains("LAATSTE-REGEL"), "zonder staart zou de faal-regel ontbreken");
    }

    #[test]
    fn a_small_output_is_kept_whole_without_any_marker() {
        let result = sh("echo klein", Duration::from_secs(10));
        assert!(!result.stdout.truncated);
        assert!(!result.stdout.text.contains("omitted"));
        assert_eq!(result.stdout.text, "klein\n");
    }

    #[test]
    fn the_body_labels_the_streams_and_says_so_when_there_is_nothing() {
        let quiet = sh("true", Duration::from_secs(10));
        assert_eq!(quiet.body(), "\n(no output)");

        let loud = sh("echo hallo", Duration::from_secs(10));
        assert_eq!(loud.body(), "\n--- stdout ---\nhallo\n");
    }

    #[test]
    fn a_truncated_stream_says_how_many_bytes_it_read() {
        let mut captured = Captured { text: "kop".into(), truncated: true, length: 99_999 };
        assert_eq!(captured.section("stdout"), "\n--- stdout (truncated, 99999 bytes read) ---\nkop");
        captured.truncated = false;
        assert_eq!(captured.section("stdout"), "\n--- stdout ---\nkop");
        captured.length = 0;
        assert_eq!(captured.section("stdout"), "", "niets gevangen is geen sectie");
    }

    #[test]
    fn a_drain_that_outlives_the_run_does_not_outrun_the_join_cap() {
        let handle = thread::spawn(|| {
            thread::sleep(Duration::from_secs(2));
            Captured { text: "te laat".into(), truncated: false, length: 7 }
        });
        let started = Instant::now();
        let captured = collect_drain(handle);
        let waited = started.elapsed();
        assert_eq!(captured, Captured::default(), "wat na de cap valt, is niet meer van deze run");
        assert!(waited >= Duration::from_millis(900), "de cap mag niet weggeslopen worden: {waited:?}");
        assert!(waited < Duration::from_millis(1_600), "de aanroeper moet bij de cap stoppen: {waited:?}");
    }

    #[test]
    fn a_program_that_cannot_start_is_an_error_and_not_a_panic() {
        let dir = std::env::temp_dir();
        let error =
            ProcessRunner::run(&["/bestaat/niet"], &dir, Duration::from_secs(5)).expect_err("dit kan niet starten");
        assert!(error.to_string().contains("/bestaat/niet"), "{error}");
    }

    #[test]
    fn the_render_drops_only_the_middle_and_names_the_size_of_it() {
        assert!(!render(&[b'a'; HEAD_BYTES], &VecDeque::new(), CAP_BYTES, false).contains("omitted"));
        let front: Vec<u8> = vec![b'h'; CAP_BYTES];
        let mut tail = VecDeque::new();
        tail.push_back(vec![b't'; TAIL_BYTES]);
        let text = render(&front, &tail, CAP_BYTES * 3, true);
        assert!(text.starts_with(&"h".repeat(HEAD_BYTES)));
        assert!(text.ends_with(&"t".repeat(TAIL_BYTES)));
        assert!(text.contains(&format!("... {} bytes omitted ...", CAP_BYTES * 3 - HEAD_BYTES - TAIL_BYTES)));
    }
}
