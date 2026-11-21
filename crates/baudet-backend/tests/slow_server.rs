//! What a client owes a local server that stops talking.
//!
//! A reasoning model can be silent for minutes before its first token, and a machine that stalls
//! mid-generation does the same thing. A client must not treat those as a dead socket, and must not
//! hang forever on a stream that never resumes either. Measured here rather than assumed, because
//! the answer decides which HTTP stack this project can use at all.

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

/// How many deltas [`stream_slowly`] sends, and how long it pauses before each one.
///
/// The count is the instrument: how many deltas a reader reaches *is* the measurement, so it has to
/// sit far enough from `deadline / gap` to read as a bound rather than as a coincidence. Set from a
/// measured run (2026-11-21): the blocking stack read all twelve of these, the async stack one.
const DELTAS: usize = 12;
const GAP: Duration = Duration::from_millis(400);

/// Width of one delta: an SSE line and its blank line, so one delta is exactly one HTTP chunk and,
/// downstream, exactly one line for the framer to hand the accumulator.
const BLOCK_BYTES: usize = 64;

/// The one deadline both tests arm, chosen so the two stacks cannot be confused with each other:
/// above a single gap, well below the whole stream.
const READ_DEADLINE: Duration = Duration::from_millis(500);

/// The chunked sibling of [`hold_after_headers`]: headers, then one delta every [`GAP`].
///
/// Both fixtures answer their headers at once and then keep the connection open, which is the only
/// difference that matters to a streaming reader. Whether a delta survives a gap of a third of a
/// second is exactly what separates a per-read deadline from a whole-request one, so the two servers
/// are deliberately shaped the same apart from that gap.
///
/// [`DELTAS`] deltas at [`GAP`] is the fixture's own bound, and it is the counterhold of every
/// measurement below: a stream that ends early would look like a deadline that fired, so each test
/// asserts against these numbers rather than around them.
///
/// One measured detail the first version of this file got wrong, and which the reading helpers
/// therefore stop before it: the terminator is not free. Reading a chunked body all the way past its
/// `0\r\n\r\n` ends with a decoder error on *both* stacks (`Err("error decoding response body")`,
/// measured 2026-11-21), because the fixture writes the terminator and hangs up while the reader is
/// still inside that read. A transport must therefore treat the end of stream as an ordinary ending
/// rather than as the last thing it reads, which is what the stop-before-the-terminator rules below
/// are for.
fn stream_slowly(mut socket: TcpStream) {
    let _ =
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n");
    let _ = socket.flush();
    for _ in 0..DELTAS {
        std::thread::sleep(GAP);
        if write_chunk(&mut socket, &delta(BLOCK_BYTES)).is_err() {
            return; // the reader hung up; that is a valid outcome of the measurement
        }
    }
    let _ = socket.write_all(b"0\r\n\r\n"); // a real end of stream
    let _ = socket.flush();
}

/// The delta the fixture sends: [`BLOCK_BYTES`] bytes, a Responses-shaped line that survives UTF-8
/// slicing because it is ASCII.
fn delta(width: usize) -> Vec<u8> {
    let prefix = b"data: {\"type\":\"response.output_text.delta\",\"padding\":\"";
    let mut payload = prefix.to_vec();
    payload.resize(width - 3, b'x'); // -3 for the closing `"}`
    payload.extend_from_slice(b"\"}\n\n");
    payload
}

/// Frame `payload` as one HTTP chunk, the way a gateway streams: without that framing the response
/// has no end, the reader would be right to keep waiting, and the fixture -- not the deadline --
/// would decide how a measurement ends.
fn write_chunk(socket: &mut TcpStream, payload: &[u8]) -> std::io::Result<()> {
    let header = format!("{:X}\r\n", payload.len());
    socket.write_all(header.as_bytes())?;
    socket.write_all(payload)?;
    socket.write_all(b"\r\n")?;
    socket.flush()
}

/// Reply with SSE headers and then stop answering, holding the socket open.
fn hold_after_headers(mut socket: TcpStream) {
    let _ =
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n");
    let _ = socket.flush();
    // Silent for longer than either deadline in this file, and longer than a reader should tolerate,
    // so whichever way the read ends is the read's own doing.
    std::thread::sleep(Duration::from_secs(4));
}

/// A server that answers every connection with `serve` until the test drops it.
///
/// Two rules the first measurement earned. The listener is held here rather than dropped when this
/// returns, because a dropped listener unbinds the port and the client then reports a transport
/// error instead of the behaviour being measured; and it accepts in a loop for the same reason -- a
/// client that reconnects must still find somebody. Dropping the fixture stops the loop.
struct FixtureServer {
    addr: std::net::SocketAddr,
    stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl FixtureServer {
    fn start(serve: fn(TcpStream)) -> FixtureServer {
        use std::sync::atomic::{AtomicBool, Ordering};
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
        // The address is read before the listener moves into the server, so a caller never has to
        // reach through the thread that owns it.
        let addr = listener.local_addr().expect("a bound listener reports its address");
        listener.set_nonblocking(true).expect("a listener that can be polled");
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let server_stop = stop.clone();
        let handle = std::thread::spawn(move || {
            while !server_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    // Each connection gets its own thread. Serving inline would serialise a client's
                    // retries behind the stream still being measured -- a reconnect would wait out
                    // the whole of `DELTAS` before anybody answered, and the deadline under test
                    // would fire on the queue rather than on the silence.
                    Ok((socket, _)) => {
                        std::thread::spawn(move || serve(socket));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => return,
                }
            }
        });
        FixtureServer { addr, stop: Some(stop), handle: Some(handle) }
    }

    fn url(&self) -> String {
        format!("http://{}/responses", self.addr)
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Read a streaming body to its end, handing back how much each read returned.
///
/// The buffer is deliberately large. A first version of this file read one byte at a time on the
/// theory that a read then returns one chunk, which would make a delta countable without an SSE
/// parser; measured, that theory is wrong (`blocking/response.rs:435-445` reads through
/// `futures_util`'s `AsyncRead`, which fills as much of the buffer as the chunk decoder has) -- a
/// 1-byte buffer returned the first 12 bytes of the first delta and nothing more, while the same
/// read with room for a whole delta returned exactly one delta per call. So the honest shape of a
/// blocking read is *however much has arrived*, and the count that matters is the number of reads
/// that carried data, not the number of bytes they carried.
fn measure_reads(client: &reqwest::blocking::Client, url: &str, stop_at: usize) -> Vec<Result<usize, String>> {
    let mut response = client.post(url).body("{}").send().expect("headers come back");
    let mut buf = vec![0u8; BLOCK_BYTES * 4];
    let mut reads = Vec::new();
    // Stop op `stop_at` delta's: de terminator erbovenop lezen zou het chunk-decoder-verloop raken
    // dat hierboven beschreven staat, en dan meet de test dat in plaats van de deadline.
    while reads.iter().filter(|r| matches!(r, Ok(n) if *n > 0)).count() < stop_at {
        match std::io::Read::read(&mut response, &mut buf) {
            Ok(0) => break, // a clean end of stream: nothing to record
            Ok(n) => reads.push(Ok(n)),
            Err(error) => {
                reads.push(Err(error.to_string()));
                break;
            }
        }
    }
    reads
}

/// Whether a deadline bounds one read or the whole answer is not a detail to assume: it decides
/// whether a reasoning model that thinks for two minutes keeps its turn. Measured here on the
/// blocking stack against the fixture this file shares with its async sibling -- `GAP` silence
/// between each of `DELTAS` deltas, a stream that is slow but perfectly alive.
///
/// The blocking client's `timeout` is spent per `read()` rather than across the body: `Read::read`
/// hands `self.timeout` to `wait::timeout`, which builds `Instant::now() + d` on every call
/// (`blocking/response.rs:435-445`, `blocking/wait.rs:15-18`). A 500 ms deadline therefore outlives
/// every 400 ms gap, so the stream keeps arriving: measured on 2026-11-21, eleven of twelve deltas
/// in eleven reads, none of them cut off.
///
/// That is the property the transport needs, and it is why the blocking stack won. What the knob is
/// *not* is a bound on a single silence -- shortening it to make it one would cut off every thinking
/// model -- so the transport sets it generously and treats it as a backstop for a hung turn, exactly
/// as the reference app sets 30 minutes for the same reason (`Http.kt:30`).
#[test]
fn the_blocking_deadline_rearms_per_read_so_a_slow_stream_survives_it() {
    let server = FixtureServer::start(stream_slowly);
    let url = server.url();
    let client = reqwest::blocking::Client::builder().timeout(READ_DEADLINE).build().unwrap();
    let reads = measure_reads(&client, &url, DELTAS - 1);
    let ok = reads.iter().filter(|r| matches!(r, Ok(n) if *n > 0)).count();
    let failed = reads.iter().filter(|r| r.is_err()).count();
    // The count a whole-request deadline could never produce. Every delta is separated from the next
    // by a silence longer than the deadline, so a bound drawn across the body would have stopped at
    // the first; eleven arriving means the bound is re-armed by every read.
    assert_eq!(ok, DELTAS - 1, "the deadline re-arms per read, so the stream keeps arriving: {reads:?}");
    assert_eq!(failed, 0, "no read of a live stream is cut off: {reads:?}");
}

/// The async sibling of [`measure_reads`]: hand back the size of every delta that arrived, in order,
/// stopping at the terminator. Chunk sizes are what a streaming reader is really measured on -- a
/// delta must arrive as its own readable piece, and a deadline must fire between pieces rather than
/// across the whole answer.
///
/// Reaching `DELTAS` ends the read here rather than waiting for the EOF on top of it. The reason is
/// in `async_impl/body.rs`: `ReadTimeoutBody` clears its `Sleep` only on `Ready(Some(chunk))`, so a
/// `Ready(None)` -- the end every clean stream has -- leaves an armed timer on the connection, and
/// that connection outlives the read inside the client's pool. Diagnostics here (2026-11-21) showed
/// what that costs: a client reused after a finished stream lost its *first* delta to the previous
/// stream's timer. A transport that reads to EOF would inherit exactly that, so it is recorded as
/// part of the contract rather than discovered later on a real server.
async fn measure_chunks(client: &reqwest::Client, url: &str, stop_at: usize) -> Vec<Result<usize, String>> {
    let response = client.post(url).body("{}").send().await.expect("headers come back");
    let mut body = response;
    let mut chunks = Vec::new();
    while chunks.iter().filter(|c| matches!(c, Ok(n) if *n > 0)).count() < stop_at {
        match body.chunk().await {
            Ok(Some(bytes)) => chunks.push(Ok(bytes.len())),
            Ok(None) => break, // the server ended before `stop_at`; whatever arrived is the answer
            Err(error) => {
                chunks.push(Err(error.to_string()));
                break;
            }
        }
    }
    chunks
}

/// The async sibling of the blocking measurement: same fixture, same instruction, different stack.
///
/// `read_timeout` behaves like the blocking `timeout` after all. `ReadTimeoutBody::poll_frame` arms
/// one `Sleep` and clears it whenever a frame arrives (`async_impl/body.rs:341-357`), so a live stream
/// keeps resetting it: measured, eleven deltas arrive, each after a silence longer than the deadline.
/// The earlier note in this file, written from a broken measurement, claimed the opposite -- that the
/// bound is the *sum* of the silences and the stream dies at the first -- and it is corrected here
/// rather than repeated.
///
/// So the two stacks do not differ in whether a slow stream survives; they differ in what the reader
/// has to do to notice silence. That difference is measured in
/// [`a_deadline_ends_a_stream_that_goes_quiet`] and it is what decides the transport.
#[test]
fn the_async_read_timeout_also_rearms_so_a_slow_stream_survives_it() {
    let server = FixtureServer::start(stream_slowly);
    let url = server.url();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    // No `timeout` at all: the async builder's `timeout` is a whole-request deadline, and leaving
    // it unset is what makes `read_timeout` the only bound in play.
    let client = reqwest::Client::builder().read_timeout(READ_DEADLINE).build().unwrap();
    let chunks = runtime.block_on(measure_chunks(&client, &url, DELTAS - 1));
    let ok = chunks.iter().filter(|c| matches!(c, Ok(n) if *n > 0)).count();
    let failed = chunks.iter().filter(|c| c.is_err()).count();
    // Measured (2026-11-21), and it contradicts the note this file used to carry: `read_timeout`
    // does *not* bound the sum of the silences. Eleven deltas arrive, each after a silence longer
    // than the deadline, so the timer is rearmed by every frame -- `poll_frame` resets the `Sleep`
    // when a frame arrives, which is the same behaviour the blocking stack has per read.
    assert_eq!(ok, DELTAS - 1, "the async deadline is rearmed per frame too: {chunks:?}");
    assert_eq!(failed, 0, "no frame of a live stream is cut off: {chunks:?}");
}

/// The one property that does decide the transport's backstop: a stream that goes quiet ends from
/// inside the reader, with no watchdog thread closing the socket behind its back -- which is what the
/// reference app has to do, because java.net.http gives it no per-read bound at all.
#[test]
fn a_deadline_ends_a_stream_that_goes_quiet() {
    let server = FixtureServer::start(hold_after_headers);
    let url = server.url();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let client = reqwest::Client::builder().read_timeout(Duration::from_millis(600)).build().unwrap();
    let started = Instant::now();
    let chunks = runtime.block_on(measure_chunks(&client, &url, DELTAS));
    let elapsed = started.elapsed();
    assert_eq!(chunks.len(), 1, "the stalled stream yields exactly one failed read: {chunks:?}");
    assert!(chunks[0].is_err(), "the silence is reported as an error, not as a hang: {chunks:?}");
    assert!(
        elapsed >= Duration::from_millis(500),
        "the read waited out its own deadline rather than failing at once: {elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(3), "the deadline ends the wait long before the server lets go: {elapsed:?}");
}
