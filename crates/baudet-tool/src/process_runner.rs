//! Een vast argv in een map draaien, met timeout en een begrensde, gelijktijdige lezing van beide streams.
//!
//! Poort van `tool/ProcessRunner.kt`. De reden van bestaan is één hard feit: een subproces dat meer schrijft
//! dan een pipe-puffer vat, blokkeert zolang er niemand leest, en een tool die op de exit code wacht terwijl
//! de pipe vol is, hangt de hele beurt. Daarom leest élke stream op een eigen draad. De cap begrenst naast de
//! pipe ook het context-venster, want een build-log van een megabyte is voor het model geen informatie maar
//! een overstroming.
//!
//! De runner start zelf géén shell en past géén beleid toe: aanroepers kiezen hun argv, schrijven hun eigen
//! kopregel, en zetten een start-fout om in hun eigen reden. Zo blijven de cap en de drain één keer
//! gedefinieerd in plaats van twee keer.

use std::collections::VecDeque;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Bytes bewaard van het begin van een stream: de commando-echo en vroege diagnose.
const HEAD_BYTES: usize = 8 * 1024;
/// Bytes bewaard van het eind van een stream. Een build- of test-faal rapporteert aan het eind, dus een
/// head-only cap zou de boilerplate houden en de ene regel weggooien die het model nodig heeft. De cap is de
/// twee samen.
const TAIL_BYTES: usize = 8 * 1024;
const CAP_BYTES: usize = HEAD_BYTES + TAIL_BYTES;

/// Een gevangen uitvoer-stream: de bewaarde tekst (tot de cap), of er meer viel, en de ware aantal bytes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Captured {
    pub text: String,
    pub truncated: bool,
    pub length: usize,
}

impl Captured {
    /// Een gelabelde sectie voor deze stream, of leeg wanneer hij niets ving.
    pub fn section(&self, label: &str) -> String {
        if self.length == 0 {
            return String::new();
        }
        let marker = if self.truncated { format!(" (truncated, {} bytes read)", self.length) } else { String::new() };
        format!("\n--- {label}{marker} ---\n{}", self.text)
    }
}

/// De uitkomst van een run: de exit code (`None` bij timeout), de timeout-vlag, beide streams, en de
/// verstreken tijd. `duration_ms` wordt hier gemeten omdat dit de enige plek is die weet wanneer het proces
/// werkelijk startte — een aanroeper die de aanroep zelf timt, rekent ook het staging van het script mee.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProcessOutcome {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: Captured,
    pub stderr: Captured,
    pub duration_ms: u64,
}

impl ProcessOutcome {
    /// De twee stream-secties, met een no-output-markering wanneer beide leeg zijn.
    pub fn body(&self) -> String {
        let mut out = self.stdout.section("stdout");
        out.push_str(&self.stderr.section("stderr"));
        if self.stdout.length == 0 && self.stderr.length == 0 {
            out.push_str("\n(no output)");
        }
        out
    }
}

/// Hoe lang op een lees-draad gewacht wordt nadat het proces eindigde of werd gedoofd. Een draad die dan nog
/// leeft, houdt een pipe open die niet meer closed wordt; één seconde is de origineler zijn gekozen grens en
/// deze poort behoudt hem, want langer wachten kost de beurt tijd zonder iets op te leveren.
const JOIN_MILLIS: u64 = 1_000;

/// Een run die niet eens kon starten. De boodschap is technisch en naar de aanroeper, niet naar het model:
/// elke tool zet er zijn eigen reden van bovenop.
#[derive(Debug, thiserror::Error)]
#[error("could not start {program}: {source}")]
pub struct StartError {
    program: String,
    #[source]
    source: std::io::Error,
}

/// De runner zelf.
pub struct ProcessRunner;

impl ProcessRunner {
    /// Start en leegt het proces dat `argv` beschrijft, draaiend in `dir`, begrensd aan `timeout`.
    ///
    /// Een timeout dooft het proces forcijd, want een kind dat op iets wacht zou anders eeuwig blijven leven.
    pub fn run(argv: &[&str], dir: &Path, timeout: Duration) -> Result<ProcessOutcome, StartError> {
        Self::run_with(argv, dir, timeout, |_| {})
    }

    /// Als [`Self::run`], maar met een environment-haak die de aanroeper ná onze eigen stdio-instellingen mag
    /// aanvullen — precies de `configure: (ProcessBuilder) -> Unit` van de origineler (`ExecuteTool.kt:109`),
    /// die er `pb.environment()["TMPDIR"]` doorheen zet. De haak krijgt alleen een `&mut Command` mee, dus
    /// alleen toevoegingen: wie onze drain met `null()` zou overschrijven, verliest zijn uitvoer, en dat past
    /// niet bij de afspraak dat cap en drain hier één keer gedefinieerd zijn. Wij zetten zelf géén environment
    /// — het kind erft de onze — en een aanroeper zet zijn variabele op het kind in plaats van in dit proces,
    /// want dit proces leeft door in elke volgende tool en in elke parallelle draad.
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
        let mut child = command.spawn().map_err(|source| StartError { program: program.to_owned(), source })?;

        // Beide drains op een eigen draad; de kanaal-stuurder is de enige eigenaar van de bytes.
        let out_thread = drain_thread(child.stdout.take(), "stdout");
        let err_thread = drain_thread(child.stderr.take(), "stderr");

        let timeout_ms = timeout.as_millis() as u64;
        let outcome = wait_with_timeout(&mut child, timeout_ms);
        let timed_out = outcome.is_none();
        let exit_code = match outcome {
            Some(status) => status.code(),
            None => {
                let _ = child.kill();
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

/// Wachten met een cap zonder een wachtdraad te lenen: polls in plaats van een blokering, zodat er geen
/// second draad nodig is en de timeout geen race wordt met de drains.
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
            // Een wait die faalt is een proces dat wegviel; alsof het eindigde zonder code is het eerlijkst.
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

/// Wacht hooguit [`JOIN_MILLIS`] op de drain, precies zoals de origineler `join(JOIN_MILLIS)` doet.
///
/// Een onbegrensde `join` zou hier stil een ander gedrag zijn: een kind dat een kleindkind achterlaat met de
/// pipe open, houdt de drain in leven, en de tool zou dan de beurt van de persoon overleveren aan een proces
/// dat nooit stopt. Wat de drain tot die cap ving, is de waarheid over die run; wat daarna kwam, was er voor
/// niemand.
///
/// De sluiting kan een `JoinHandle` niet met cap opeten, dus de handtekening van de origineler wordt met een
/// eigen wachtdraad gehaald: de drain stuurt naar een kanaal en de aanroeper wacht op dat kanaal. De draad
/// zelf wordt met rust gelaten als hij te laat is — hij houdt alleen een gesloten pipe vast, en het proces is
/// dan al gedoofd.
fn collect_drain(handle: thread::JoinHandle<Captured>) -> Captured {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        // `unwrap_or_default` is hier geen zwijgen over een fout: een paniek in de drain mag de run niet
        // kosten, en wat er gelezen is, is wat er gelezen is — een gestoorde drain telt dan als niets.
        let captured = handle.join().unwrap_or_default();
        let _ = sender.send(captured);
    });
    receiver.recv_timeout(Duration::from_millis(JOIN_MILLIS)).unwrap_or_default()
}

/// Leegt `input` volledig terwijl het begin en het eind bewaard blijven, zodat een klets proces noch de pipe
/// kan laten hangen noch het context-venster kan overstromen, en een faal die aan het eind rapporteert toch
/// gezien wordt. [`HEAD_BYTES`] blijven van vooraan en [`TAIL_BYTES`] van achteren; het weggelaten midden wordt
/// vervangen door een markering die noemt hoeveel bytes het hield. Past de hele stream binnen de cap, dan valt
/// er niets en verschijnt de markering nooit.
fn read_capped(input: Box<dyn Read>) -> Captured {
    let mut buffer = vec![0u8; 8192];
    let mut front: Vec<u8> = Vec::new(); // de eerste CAP_BYTES, dus de bewaarde kop is compleet
    let mut tail: VecDeque<Vec<u8>> = VecDeque::new(); // een rollend venster van de laatste TAIL_BYTES
    let mut tail_size = 0usize;
    let mut total = 0usize;

    let mut reader = input;
    loop {
        // Eén buffer die hergebruikt wordt; een read die faalt (de dooide pipe na een timeout) telt als
        // einde, niet als fout: wat er voor ving, is de waarheid over die run.
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

/// De bewaarde tekst: de hele stream wanneer hij past, anders kop + weglating-markering + staart.
fn render(front: &[u8], tail: &VecDeque<Vec<u8>>, total: usize, truncated: bool) -> String {
    let all = String::from_utf8_lossy(front).into_owned();
    if !truncated {
        return all;
    }
    let tail_text: String = tail.iter().map(|chunk| String::from_utf8_lossy(chunk).into_owned()).collect();
    let head: String = all.chars().take(HEAD_BYTES).collect();
    // De markering noemt de werkelijk weggelaten aantallen, niet een schatting: het model moet kunnen weten
    // hoeveel er ontbreekt om te beslissen of het de uitvoer opnieuw moet vragen, smaller afgebakend.
    let omitted = total.saturating_sub(head.len()).saturating_sub(tail_text.len());
    // Model-gerichte markering, dus Engels: hij wordt teruggelezen als onderdeel van de observatie.
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
        // Een sleep die twee seconden duurt tegen een timeout van 0,2 s: de test bewijst daarmee ook dat de
        // runner niet doorloopt tot het kind klaar is, want dat zou twee seconden kosten.
        let started = Instant::now();
        let result = sh("sleep 2", Duration::from_millis(200));
        assert!(result.timed_out);
        assert_eq!(result.exit_code, None, "een gedoofd proces heeft geen exit code");
        assert!(started.elapsed() < Duration::from_secs(2), "de timeout moet echt doven, niet wachten");
    }

    #[test]
    fn a_chatty_run_neither_hangs_the_pipe_nor_floods_the_text() {
        // 400 KB door een pipe van enkele tientallen KB: zonder gelijktijdige drain zou dit hier blokkeren tot
        // de timeout, en mét een dompe cap zou het model 400 KB Proza krijgen.
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
        // Geen sh-call hier, want het punt is de handtekening van de drain zelf: een drain die langer leeft
        // dan de cap mag de aanroeper niet langer dan die cap + marge ophouden. Twee seconden slapen tegen
        // één seconde cap bewijst dat, en houdt het bedrag klein genoeg voor een testsuite.
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
        // De cap is de som van kop en staart; een stream van precies die maat past en is dus geen knipping.
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
