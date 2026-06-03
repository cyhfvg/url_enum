use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::Deserialize;
use tempfile::TempDir;
use url_enum::cli::{Args, HttpMethod, OutputFormat};

#[derive(Debug, Clone, PartialEq, Eq)]
struct RequestLog {
    method: String,
    path: String,
}

struct LocalHttpServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<RequestLog>>>,
    handle: Option<JoinHandle<()>>,
}

impl LocalHttpServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local HTTP server");
        let address = listener.local_addr().expect("read local HTTP address");
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let worker_stop = Arc::clone(&stop);
        let worker_requests = Arc::clone(&requests);
        let handle = thread::spawn(move || {
            for stream in listener.incoming() {
                if worker_stop.load(Ordering::Relaxed) {
                    break;
                }
                match stream {
                    Ok(stream) => {
                        let requests = Arc::clone(&worker_requests);
                        thread::spawn(move || handle_connection(stream, requests));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            address,
            stop,
            requests,
            handle: Some(handle),
        }
    }

    fn target(&self) -> String {
        format!("http://{}", self.address)
    }

    fn requests(&self) -> Vec<RequestLog> {
        self.requests.lock().expect("request log lock").to_owned()
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

#[derive(Debug, Deserialize)]
struct TestResult {
    word: String,
    status: Option<u16>,
    size: Option<u64>,
    error: Option<String>,
}

struct ResponseSpec {
    status: &'static str,
    content_length: usize,
    body: Vec<u8>,
    delay: Duration,
}

fn handle_connection(mut stream: TcpStream, requests: Arc<Mutex<Vec<RequestLog>>>) {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set read timeout");
    let mut pending = Vec::new();
    let mut buffer = [0_u8; 4096];

    while header_end_position(&pending).is_none() {
        let read = match stream.read(&mut buffer) {
            Ok(0) => return,
            Ok(read) => read,
            Err(_) => return,
        };
        pending.extend_from_slice(&buffer[..read]);
    }

    let request = String::from_utf8_lossy(&pending);
    let mut parts = request
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    requests.lock().expect("request log lock").push(RequestLog {
        method: method.clone(),
        path: path.clone(),
    });

    let response = response_for(&method, &path);
    if !response.delay.is_zero() {
        thread::sleep(response.delay);
    }

    let header = format!(
        "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status, response.content_length
    );
    if stream.write_all(header.as_bytes()).is_err() {
        return;
    }
    let _ = stream.write_all(&response.body);
}

fn header_end_position(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
}

fn response_for(method: &str, path: &str) -> ResponseSpec {
    match (method, path) {
        (_, "/status-ok") => response("200 OK", b"status-ok".to_vec()),
        (_, "/status-forbidden") => response("403 Forbidden", b"forbidden".to_vec()),
        (_, "/status-missing") => response("404 Not Found", b"missing".to_vec()),
        (_, "/small") => response("200 OK", b"tiny".to_vec()),
        (_, "/exact-blocked") => response("200 OK", b"12345".to_vec()),
        (_, "/range-blocked") => response("200 OK", b"123456789".to_vec()),
        (_, "/large") => response("200 OK", b"123456789012".to_vec()),
        (_, "/slow") => ResponseSpec {
            status: "200 OK",
            content_length: 4,
            body: b"slow".to_vec(),
            delay: Duration::from_secs(2),
        },
        (_, "/fast") => response("200 OK", b"fast".to_vec()),
        ("HEAD", "/head-target") => ResponseSpec {
            status: "200 OK",
            content_length: 123,
            body: Vec::new(),
            delay: Duration::ZERO,
        },
        (_, "/head-target") => response("200 OK", b"get".to_vec()),
        _ => response("404 Not Found", b"unknown".to_vec()),
    }
}

fn response(status: &'static str, body: Vec<u8>) -> ResponseSpec {
    ResponseSpec {
        status,
        content_length: body.len(),
        body,
        delay: Duration::ZERO,
    }
}

fn args_for(
    target: String,
    dictionary: &Path,
    output: &Path,
    method: HttpMethod,
    timeout: u64,
) -> Args {
    Args {
        target,
        dict: dictionary.display().to_string(),
        replace: None,
        concurrency: 4,
        request_jitter_ms: 0,
        timeout,
        method,
        user_agent: "url_enum-integration-test".to_owned(),
        headers: Vec::new(),
        proxy: None,
        follow_redirect: false,
        insecure: true,
        filter_http_code: Vec::new(),
        black_http_code: Vec::new(),
        black_size: Vec::new(),
        output: Some(output.display().to_string()),
        format: OutputFormat::Jsonl,
        extensions: Vec::new(),
    }
}

fn run_with_words<F>(words: &[&str], configure: F) -> (LocalHttpServer, Vec<TestResult>)
where
    F: FnOnce(&mut Args),
{
    let server = LocalHttpServer::start();
    let temp = TempDir::new().expect("create temporary directory");
    let dictionary = temp.path().join("dict.txt");
    let output = temp.path().join("results.jsonl");
    std::fs::write(&dictionary, words.join("\n") + "\n").expect("write dictionary");

    let mut args = args_for(server.target(), &dictionary, &output, HttpMethod::Get, 10);
    configure(&mut args);

    let runtime = tokio::runtime::Runtime::new().expect("create Tokio runtime");
    runtime
        .block_on(url_enum::scanner::run(args))
        .expect("run scanner");

    let results = read_results(&output);
    (server, results)
}

fn read_results(path: &Path) -> Vec<TestResult> {
    let contents = std::fs::read_to_string(path).expect("read JSONL output");
    contents
        .lines()
        .map(|line| serde_json::from_str(line).expect("parse JSONL result"))
        .collect()
}

fn result_for<'a>(results: &'a [TestResult], word: &str) -> &'a TestResult {
    results
        .iter()
        .find(|result| result.word == word)
        .unwrap_or_else(|| panic!("missing result for word `{word}`"))
}

#[test]
fn filters_status_codes_with_allowlist_and_blocklist() {
    let (_server, results) = run_with_words(
        &["status-ok", "status-forbidden", "status-missing"],
        |args| {
            args.filter_http_code = vec![200, 403];
            args.black_http_code = vec![403];
        },
    );

    assert_eq!(results.len(), 1);
    let result = result_for(&results, "status-ok");
    assert_eq!(result.status, Some(200));
    assert_eq!(result.error, None);
}

#[test]
fn filters_response_sizes_with_exact_values_and_ranges() {
    let (_server, results) = run_with_words(
        &["small", "exact-blocked", "range-blocked", "large"],
        |args| {
            args.black_size = vec!["5".to_owned(), "9-10".to_owned()];
        },
    );

    assert_eq!(results.len(), 2);
    assert_eq!(result_for(&results, "small").size, Some(4));
    assert_eq!(result_for(&results, "large").size, Some(12));
    assert!(results.iter().all(|result| result.error.is_none()));
}

#[test]
fn records_request_timeout_as_a_failed_probe() {
    let (_server, results) = run_with_words(&["slow", "fast"], |args| {
        args.timeout = 1;
    });

    let slow = result_for(&results, "slow");
    let fast = result_for(&results, "fast");
    assert_eq!(slow.status, None);
    assert_eq!(slow.size, None);
    assert!(slow.error.as_deref().is_some_and(|error| !error.is_empty()));
    assert_eq!(fast.status, Some(200));
    assert_eq!(fast.size, Some(4));
}

#[test]
fn sends_head_requests_and_uses_content_length_as_size() {
    let (server, results) = run_with_words(&["head-target"], |args| {
        args.method = HttpMethod::Head;
    });

    let result = result_for(&results, "head-target");
    assert_eq!(result.status, Some(200));
    assert_eq!(result.size, Some(123));
    assert_eq!(result.error, None);
    assert!(
        server
            .requests()
            .iter()
            .any(|request| { request.method == "HEAD" && request.path == "/head-target" })
    );
}
