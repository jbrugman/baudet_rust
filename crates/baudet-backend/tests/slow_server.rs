
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

const DELTAS: usize = 12;
const GAP: Duration = Duration::from_millis(400);

const BLOCK_BYTES: usize = 64;

const READ_DEADLINE: Duration = Duration::from_millis(500);
const POST_BODY: &[u8; 2] = b"{}";

fn read_post(socket: &mut TcpStream) {
    socket.set_read_timeout(Some(Duration::from_secs(2))).expect("bounded fixture reads");
    let mut reader = BufReader::new(socket);
    let mut line = String::new();
    loop {
        assert_ne!(reader.read_line(&mut line).expect("request headers"), 0, "request ended before its body");
        if line == "\r\n" {
            break;
        }
        line.clear();
    }
    let mut body = [0; POST_BODY.len()];
    reader.read_exact(&mut body).expect("complete fixture POST body");
    assert_eq!(&body, POST_BODY);
}

fn stream_slowly(mut socket: TcpStream) {
    let _ =
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n");
    let _ = socket.flush();
    for _ in 0..DELTAS {
        std::thread::sleep(GAP);
        if write_chunk(&mut socket, &delta(BLOCK_BYTES)).is_err() {
            return;
        }
    }
    let _ = socket.write_all(b"0\r\n\r\n");
    let _ = socket.flush();
}

fn delta(width: usize) -> Vec<u8> {
    let prefix = b"data: {\"type\":\"response.output_text.delta\",\"padding\":\"";
    let mut payload = prefix.to_vec();
    payload.resize(width - 3, b'x');
    payload.extend_from_slice(b"\"}\n\n");
    payload
}

fn write_chunk(socket: &mut TcpStream, payload: &[u8]) -> std::io::Result<()> {
    let header = format!("{:X}\r\n", payload.len());
    socket.write_all(header.as_bytes())?;
    socket.write_all(payload)?;
    socket.write_all(b"\r\n")?;
    socket.flush()
}

fn hold_after_headers(mut socket: TcpStream) {
    let _ =
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n");
    let _ = socket.flush();
    std::thread::sleep(Duration::from_secs(4));
}

struct FixtureServer {
    addr: std::net::SocketAddr,
    stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl FixtureServer {
    fn start(serve: fn(TcpStream)) -> FixtureServer {
        use std::sync::atomic::{AtomicBool, Ordering};
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
        let addr = listener.local_addr().expect("a bound listener reports its address");
        listener.set_nonblocking(true).expect("a listener that can be polled");
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let server_stop = stop.clone();
        let handle = std::thread::spawn(move || {
            while !server_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((socket, _)) => {
                        std::thread::spawn(move || {
                            let mut socket = socket;
                            read_post(&mut socket);
                            serve(socket);
                        });
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

fn measure_reads(client: &reqwest::blocking::Client, url: &str, stop_at: usize) -> Vec<Result<usize, String>> {
    let mut response = client.post(url).body(POST_BODY.as_slice()).send().expect("headers come back");
    let mut buf = vec![0u8; BLOCK_BYTES * 4];
    let mut reads = Vec::new();
    while reads.iter().filter(|r| matches!(r, Ok(n) if *n > 0)).count() < stop_at {
        match std::io::Read::read(&mut response, &mut buf) {
            Ok(0) => break,
            Ok(n) => reads.push(Ok(n)),
            Err(error) => {
                reads.push(Err(error.to_string()));
                break;
            }
        }
    }
    reads
}

#[test]
fn the_blocking_deadline_rearms_per_read_so_a_slow_stream_survives_it() {
    let server = FixtureServer::start(stream_slowly);
    let url = server.url();
    let client = reqwest::blocking::Client::builder().timeout(READ_DEADLINE).build().unwrap();
    let reads = measure_reads(&client, &url, DELTAS - 1);
    let ok = reads.iter().filter(|r| matches!(r, Ok(n) if *n > 0)).count();
    let failed = reads.iter().filter(|r| r.is_err()).count();
    assert_eq!(ok, DELTAS - 1, "the deadline re-arms per read, so the stream keeps arriving: {reads:?}");
    assert_eq!(failed, 0, "no read of a live stream is cut off: {reads:?}");
}

async fn measure_chunks(client: &reqwest::Client, url: &str, stop_at: usize) -> Vec<Result<usize, String>> {
    let response = client.post(url).body(POST_BODY.as_slice()).send().await.expect("headers come back");
    let mut body = response;
    let mut chunks = Vec::new();
    while chunks.iter().filter(|c| matches!(c, Ok(n) if *n > 0)).count() < stop_at {
        match body.chunk().await {
            Ok(Some(bytes)) => chunks.push(Ok(bytes.len())),
            Ok(None) => break,
            Err(error) => {
                chunks.push(Err(error.to_string()));
                break;
            }
        }
    }
    chunks
}

#[test]
fn the_async_read_timeout_also_rearms_so_a_slow_stream_survives_it() {
    let server = FixtureServer::start(stream_slowly);
    let url = server.url();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let client = reqwest::Client::builder().read_timeout(READ_DEADLINE).build().unwrap();
    let chunks = runtime.block_on(measure_chunks(&client, &url, DELTAS - 1));
    let ok = chunks.iter().filter(|c| matches!(c, Ok(n) if *n > 0)).count();
    let failed = chunks.iter().filter(|c| c.is_err()).count();
    assert_eq!(ok, DELTAS - 1, "the async deadline is rearmed per frame too: {chunks:?}");
    assert_eq!(failed, 0, "no frame of a live stream is cut off: {chunks:?}");
}

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

#[test]
fn een_uitgelezen_terminator_is_measured_not_assumed() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let addr = listener.local_addr().expect("a bound listener reports its address");
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("one client");
        read_post(&mut socket);
        let _ = socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n");
        let _ = socket.flush();
        let _ = write_chunk(&mut socket, &delta(BLOCK_BYTES));
        std::thread::sleep(Duration::from_millis(200));
        let _ = socket.write_all(b"0\r\n\r\n");
        let _ = socket.flush();
        std::thread::sleep(Duration::from_millis(200));
    });
    let url = format!("http://{addr}/responses");
    let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build().unwrap();
    let mut response = client.post(&url).body(POST_BODY.as_slice()).send().expect("headers come back");
    let mut buf = vec![0u8; BLOCK_BYTES * 4];
    let mut outcome = Vec::new();
    loop {
        match std::io::Read::read(&mut response, &mut buf) {
            Ok(0) => {
                outcome.push("EOF".to_string());
                break;
            }
            Ok(n) => outcome.push(format!("{n} bytes")),
            Err(error) => {
                outcome.push(format!("Err({error})"));
                break;
            }
        }
    }
    server.join().expect("the fixture ends");
    eprintln!("terminator outcome: {outcome:?}");
    assert_eq!(
        outcome.as_slice(),
        &["65 bytes".to_string(), "EOF".to_string()],
        "the delta arrives, then the stream ends cleanly: {outcome:?}"
    );
}
