use std::hint::black_box;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use tempfile::TempDir;
use tokio::runtime::Runtime;
use url_enum::cli::{Args, HttpMethod, OutputFormat};
use url_enum::scanner;

const WORDS: usize = 500;
struct LocalHttpServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl LocalHttpServer {
    /// Signature: `fn start() -> Self`
    ///
    /// Purpose: Starts a local keep-alive HTTP server for throughput benchmarks.
    ///
    /// Parameters: None.
    ///
    /// Returns: A [`LocalHttpServer`] handle that stops the listener on drop.
    ///
    /// Notes: Binding to an ephemeral loopback port keeps benchmark runs
    /// isolated from fixed-port conflicts.
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local HTTP server");
        let address = listener.local_addr().expect("read local HTTP address");
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            for stream in listener.incoming() {
                if worker_stop.load(Ordering::Relaxed) {
                    break;
                }
                match stream {
                    Ok(stream) => {
                        thread::spawn(move || handle_connection(stream));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            address,
            stop,
            handle: Some(handle),
        }
    }

    /// Signature: `fn target(&self) -> String`
    ///
    /// Purpose: Builds the scanner target URL for the benchmark server.
    ///
    /// Parameters: None.
    ///
    /// Returns: A string in `http://host:port` form.
    ///
    /// Notes: The scanner appends dictionary paths to this base URL.
    fn target(&self) -> String {
        format!("http://{}", self.address)
    }
}

impl Drop for LocalHttpServer {
    /// Signature: `fn drop(&mut self)`
    ///
    /// Purpose: Stops the benchmark HTTP server and joins its listener thread.
    ///
    /// Parameters: None beyond the mutable server handle being dropped.
    ///
    /// Returns: Nothing.
    ///
    /// Notes: A loopback connection wakes the blocking listener so cleanup is
    /// deterministic.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.address);
        if let Some(handle) = self.handle.take() {
            handle.join().expect("join local HTTP server");
        }
    }
}

/// Signature: `fn handle_connection(stream)`
///
/// Purpose: Handles one benchmark TCP connection and responds to repeated
/// keep-alive requests.
///
/// Parameters:
/// - `stream`: Accepted TCP connection.
///
/// Returns: Nothing.
///
/// Notes: The loop drains complete request headers and writes a fixed small
/// response to minimize server-side benchmark noise.
fn handle_connection(mut stream: TcpStream) {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set read timeout");
    let mut pending = Vec::new();
    let mut buffer = [0_u8; 4096];

    loop {
        while !contains_header_end(&pending) {
            let read = match stream.read(&mut buffer) {
                Ok(0) => return,
                Ok(read) => read,
                Err(_) => return,
            };
            pending.extend_from_slice(&buffer[..read]);
        }

        if let Some(end) = header_end_position(&pending) {
            pending.drain(..end);
        }

        if stream.write_all(response_bytes()).is_err() {
            return;
        }
    }
}

/// Signature: `fn contains_header_end(bytes) -> bool`
///
/// Purpose: Checks whether a pending byte buffer contains a complete HTTP
/// header block.
///
/// Parameters:
/// - `bytes`: Pending bytes read from a TCP stream.
///
/// Returns: `true` when `\r\n\r\n` is present.
///
/// Notes: Delegates to `header_end_position` so the delimiter logic lives in one
/// helper.
fn contains_header_end(bytes: &[u8]) -> bool {
    header_end_position(bytes).is_some()
}

/// Signature: `fn header_end_position(bytes) -> Option<usize>`
///
/// Purpose: Locates the byte position immediately after the HTTP header
/// delimiter.
///
/// Parameters:
/// - `bytes`: Pending bytes read from a TCP stream.
///
/// Returns: `Some(index)` after `\r\n\r\n`, otherwise `None`.
///
/// Notes: Benchmark requests do not include bodies, so this is sufficient for
/// request framing.
fn header_end_position(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
}

/// Signature: `fn response_bytes() -> &'static [u8]`
///
/// Purpose: Provides the fixed HTTP response used by the benchmark server.
///
/// Parameters: None.
///
/// Returns: Static bytes for a small `200 OK` keep-alive response.
///
/// Notes: Static bytes avoid per-request allocation in the local server.
fn response_bytes() -> &'static [u8] {
    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok"
}

/// Signature: `fn write_dictionary(path)`
///
/// Purpose: Writes the benchmark dictionary file.
///
/// Parameters:
/// - `path`: Destination path for generated dictionary entries.
///
/// Returns: Nothing.
///
/// Panics: Panics if the dictionary file cannot be written.
///
/// Notes: The number of generated words is controlled by the `WORDS` constant.
fn write_dictionary(path: &Path) {
    let mut words = String::new();
    for index in 0..WORDS {
        words.push_str("path-");
        words.push_str(&index.to_string());
        words.push('\n');
    }
    std::fs::write(path, words).expect("write benchmark dictionary");
}

/// Signature: `fn benchmark_args(target, dictionary, output, concurrency, format) -> Args`
///
/// Purpose: Builds scanner arguments for one benchmark case.
///
/// Parameters:
/// - `target`: Base URL of the local benchmark server.
/// - `dictionary`: Path to the generated dictionary file.
/// - `output`: Path where scanner output should be written.
/// - `concurrency`: Scanner concurrency level under test.
/// - `format`: Output format under test.
///
/// Returns: A complete [`Args`] value.
///
/// Notes: Defaults disable filters and redirects so the benchmark focuses on
/// request and serialization throughput.
fn benchmark_args(
    target: String,
    dictionary: &Path,
    output: &Path,
    concurrency: usize,
    format: OutputFormat,
) -> Args {
    Args {
        target,
        dict: dictionary.display().to_string(),
        replace: None,
        concurrency,
        request_jitter_ms: 0,
        random_sequence: false,
        timeout: 10,
        method: HttpMethod::Get,
        user_agent: "url_enum-benchmark".to_owned(),
        headers: Vec::new(),
        proxy: None,
        follow_redirect: false,
        insecure: true,
        filter_http_code: Vec::new(),
        black_http_code: Vec::new(),
        black_size: Vec::new(),
        output: Some(output.display().to_string()),
        format,
        extensions: Vec::new(),
    }
}

/// Signature: `fn format_name(format) -> &'static str`
///
/// Purpose: Produces stable benchmark labels for output formats.
///
/// Parameters:
/// - `format`: Output format being benchmarked.
///
/// Returns: A short lowercase format name.
///
/// Notes: Keep labels stable to make Criterion history comparable.
fn format_name(format: OutputFormat) -> &'static str {
    match format {
        OutputFormat::Csv => "csv",
        OutputFormat::Jsonl => "jsonl",
    }
}

/// Signature: `fn throughput(c)`
///
/// Purpose: Registers throughput benchmarks across output formats and
/// concurrency levels.
///
/// Parameters:
/// - `c`: Criterion benchmark context.
///
/// Returns: Nothing.
///
/// Notes: The benchmark reads output metadata through `black_box` so file output
/// is part of the measured path.
fn throughput(c: &mut Criterion) {
    let runtime = Runtime::new().expect("create Tokio runtime");
    let server = LocalHttpServer::start();
    let temp = TempDir::new().expect("create benchmark temp dir");
    let dictionary = temp.path().join("dict.txt");
    write_dictionary(&dictionary);

    let mut group = c.benchmark_group("throughput");
    group.throughput(Throughput::Elements(WORDS as u64));
    for format in [OutputFormat::Csv, OutputFormat::Jsonl] {
        for concurrency in [1_usize, 10, 50] {
            let id = BenchmarkId::new(format_name(format), concurrency);
            let output = temp
                .path()
                .join(format!("out-{}-{concurrency}.txt", format_name(format)));
            group.bench_with_input(
                id,
                &(format, concurrency),
                |bench, &(format, concurrency)| {
                    bench.iter(|| {
                        let args = benchmark_args(
                            server.target(),
                            &dictionary,
                            &output,
                            concurrency,
                            format,
                        );
                        runtime.block_on(scanner::run(args)).expect("run scanner");
                        black_box(
                            std::fs::metadata(&output)
                                .expect("read output metadata")
                                .len(),
                        );
                    });
                },
            );
        }
    }
    group.finish();
}

criterion_group!(benches, throughput);
criterion_main!(benches);
