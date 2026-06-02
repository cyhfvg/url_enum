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

    fn target(&self) -> String {
        format!("http://{}", self.address)
    }
}

impl Drop for LocalHttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.address);
        if let Some(handle) = self.handle.take() {
            handle.join().expect("join local HTTP server");
        }
    }
}

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

fn contains_header_end(bytes: &[u8]) -> bool {
    header_end_position(bytes).is_some()
}

fn header_end_position(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
}

fn response_bytes() -> &'static [u8] {
    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok"
}

fn write_dictionary(path: &Path) {
    let mut words = String::new();
    for index in 0..WORDS {
        words.push_str("path-");
        words.push_str(&index.to_string());
        words.push('\n');
    }
    std::fs::write(path, words).expect("write benchmark dictionary");
}

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

fn format_name(format: OutputFormat) -> &'static str {
    match format {
        OutputFormat::Csv => "csv",
        OutputFormat::Jsonl => "jsonl",
    }
}

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
