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
    /// Signature: `fn start() -> Self`
    ///
    /// Purpose: Starts a lightweight local HTTP server for integration tests.
    ///
    /// Parameters: None.
    ///
    /// Returns: A [`LocalHttpServer`] handle that owns the listener thread and
    /// request log.
    ///
    /// Notes: The server binds to an ephemeral loopback port to avoid collisions.
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

    /// Signature: `fn target(&self) -> String`
    ///
    /// Purpose: Builds the base HTTP target URL for the test server.
    ///
    /// Parameters: None.
    ///
    /// Returns: A string in `http://host:port` form.
    ///
    /// Notes: Tests append paths or tokens to this base target.
    fn target(&self) -> String {
        format!("http://{}", self.address)
    }

    /// Signature: `fn requests(&self) -> Vec<RequestLog>`
    ///
    /// Purpose: Returns the requests observed by the local test server.
    ///
    /// Parameters: None.
    ///
    /// Returns: A cloned vector of recorded request method/path pairs.
    ///
    /// Notes: Cloning avoids holding the mutex while assertions inspect data.
    fn requests(&self) -> Vec<RequestLog> {
        self.requests.lock().expect("request log lock").to_owned()
    }
}

impl Drop for LocalHttpServer {
    /// Signature: `fn drop(&mut self)`
    ///
    /// Purpose: Stops the local HTTP server and joins its listener thread.
    ///
    /// Parameters: None beyond the mutable server handle being dropped.
    ///
    /// Returns: Nothing.
    ///
    /// Notes: A loopback connection wakes the blocking `incoming` iterator so
    /// shutdown does not hang the test process.
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
    url: String,
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

/// Signature: `fn handle_connection(stream, requests)`
///
/// Purpose: Reads one HTTP request, records it, and writes a deterministic test
/// response.
///
/// Parameters:
/// - `stream`: Accepted TCP connection from the local server.
/// - `requests`: Shared request log for assertions.
///
/// Returns: Nothing.
///
/// Notes: The parser is intentionally minimal because tests only need method
/// and path from simple HTTP/1.1 requests.
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

/// Signature: `fn header_end_position(bytes) -> Option<usize>`
///
/// Purpose: Locates the end of an HTTP header block in a byte buffer.
///
/// Parameters:
/// - `bytes`: Pending bytes read from a TCP stream.
///
/// Returns: The byte index immediately after `\r\n\r\n`, if present.
///
/// Notes: The body is not parsed because test requests do not send one.
fn header_end_position(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
}

/// Signature: `fn response_for(method, path) -> ResponseSpec`
///
/// Purpose: Maps a test request to the response scenario needed by assertions.
///
/// Parameters:
/// - `method`: Request method from the HTTP request line.
/// - `path`: Request path from the HTTP request line.
///
/// Returns: A [`ResponseSpec`] containing status, content length, body, and
/// optional delay.
///
/// Notes: Unknown paths deliberately return `404 Not Found`.
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

/// Signature: `fn response(status, body) -> ResponseSpec`
///
/// Purpose: Builds a simple immediate response specification.
///
/// Parameters:
/// - `status`: HTTP status text, for example `200 OK`.
/// - `body`: Response body bytes.
///
/// Returns: A [`ResponseSpec`] with content length derived from `body`.
///
/// Notes: Delayed or bodyless responses are constructed inline by tests that
/// need those details.
fn response(status: &'static str, body: Vec<u8>) -> ResponseSpec {
    ResponseSpec {
        status,
        content_length: body.len(),
        body,
        delay: Duration::ZERO,
    }
}

/// Signature: `fn args_for(target, dictionary, output, method, timeout) -> Args`
///
/// Purpose: Creates scanner arguments with integration-test defaults.
///
/// Parameters:
/// - `target`: Target URL or target-list path for the run.
/// - `dictionary`: Path to the temporary dictionary file.
/// - `output`: Path where scanner results should be written.
/// - `method`: HTTP method to configure.
/// - `timeout`: Request timeout in seconds.
///
/// Returns: A complete [`Args`] value ready for test-specific mutation.
///
/// Notes: Defaults favor deterministic output and low concurrency for stable
/// assertions unless a test overrides them.
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
        random_sequence: false,
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

/// Signature: `fn run_with_words(words, configure) -> (LocalHttpServer, Vec<TestResult>)`
///
/// Purpose: Runs the scanner against the local server using a temporary
/// dictionary.
///
/// Parameters:
/// - `words`: Dictionary entries to write.
/// - `configure`: Callback that mutates default scanner arguments.
///
/// Returns: The running server handle and parsed JSONL results.
///
/// Notes: Returning the server keeps it alive long enough for callers to inspect
/// request logs.
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

/// Signature: `fn read_results(path) -> Vec<TestResult>`
///
/// Purpose: Reads scanner JSONL output into strongly typed test results.
///
/// Parameters:
/// - `path`: Output file produced by the scanner.
///
/// Returns: Parsed [`TestResult`] values in file order.
///
/// Notes: Integration tests use JSONL output because it is easy to parse line by
/// line.
fn read_results(path: &Path) -> Vec<TestResult> {
    let contents = std::fs::read_to_string(path).expect("read JSONL output");
    contents
        .lines()
        .map(|line| serde_json::from_str(line).expect("parse JSONL result"))
        .collect()
}

/// Signature: `fn result_for(results, word) -> &TestResult`
///
/// Purpose: Finds the result associated with a dictionary word.
///
/// Parameters:
/// - `results`: Parsed scanner results.
/// - `word`: Dictionary word to search for.
///
/// Returns: A borrowed [`TestResult`] for the requested word.
///
/// Panics: Panics when the expected word is missing from results.
///
/// Notes: The panic message includes the missing word to simplify failed test
/// diagnosis.
fn result_for<'a>(results: &'a [TestResult], word: &str) -> &'a TestResult {
    results
        .iter()
        .find(|result| result.word == word)
        .unwrap_or_else(|| panic!("missing result for word `{word}`"))
}

/// Signature: `fn scans_each_target_from_an_existing_target_list_file()`
///
/// Purpose: Verifies target-list files are scanned in target-major order.
///
/// Parameters: None.
///
/// Returns: Nothing; assertions define success.
///
/// Notes: Concurrency is set to one so output order remains deterministic.
#[test]
fn scans_each_target_from_an_existing_target_list_file() {
    let server = LocalHttpServer::start();
    let temp = TempDir::new().expect("create temporary directory");
    let targets = temp.path().join("targets.txt");
    let dictionary = temp.path().join("dict.txt");
    let output = temp.path().join("results.jsonl");
    let first = format!("{}/first", server.target());
    let second = format!("{}/second", server.target());

    std::fs::write(&targets, format!("{first}\n\n{second}\n")).expect("write target list");
    std::fs::write(&dictionary, "admin\nlogin\n").expect("write dictionary");

    let mut args = args_for(
        targets.display().to_string(),
        &dictionary,
        &output,
        HttpMethod::Get,
        10,
    );
    args.concurrency = 1;

    let runtime = tokio::runtime::Runtime::new().expect("create Tokio runtime");
    runtime
        .block_on(url_enum::scanner::run(args))
        .expect("run scanner");

    let urls: Vec<String> = read_results(&output)
        .into_iter()
        .map(|result| result.url)
        .collect();

    assert_eq!(
        urls,
        vec![
            format!("{first}/admin"),
            format!("{first}/login"),
            format!("{second}/admin"),
            format!("{second}/login"),
        ]
    );
}

/// Signature: `fn replaces_token_inside_targets_from_a_target_list_file()`
///
/// Purpose: Verifies token replacement is applied independently to each target
/// loaded from a target-list file.
///
/// Parameters: None.
///
/// Returns: Nothing; assertions define success.
///
/// Notes: Concurrency is set to one so replacement output order is deterministic.
#[test]
fn replaces_token_inside_targets_from_a_target_list_file() {
    let server = LocalHttpServer::start();
    let temp = TempDir::new().expect("create temporary directory");
    let targets = temp.path().join("targets.txt");
    let dictionary = temp.path().join("dict.txt");
    let output = temp.path().join("results.jsonl");
    let first = format!("{}/ENUM", server.target());
    let second = format!("{}/root/ENUM", server.target());

    std::fs::write(&targets, format!("{first}\n{second}\n")).expect("write target list");
    std::fs::write(&dictionary, "admin\nlogin\n").expect("write dictionary");

    let mut args = args_for(
        targets.display().to_string(),
        &dictionary,
        &output,
        HttpMethod::Get,
        10,
    );
    args.concurrency = 1;
    args.replace = Some("ENUM".to_owned());

    let runtime = tokio::runtime::Runtime::new().expect("create Tokio runtime");
    runtime
        .block_on(url_enum::scanner::run(args))
        .expect("run scanner");

    let urls: Vec<String> = read_results(&output)
        .into_iter()
        .map(|result| result.url)
        .collect();

    assert_eq!(
        urls,
        vec![
            format!("{}/admin", server.target()),
            format!("{}/login", server.target()),
            format!("{}/root/admin", server.target()),
            format!("{}/root/login", server.target()),
        ]
    );
}

/// Signature: `fn filters_status_codes_with_allowlist_and_blocklist()`
///
/// Purpose: Verifies status allowlist and blocklist filters compose correctly.
///
/// Parameters: None.
///
/// Returns: Nothing; assertions define success.
///
/// Notes: Blocklist entries take precedence after allowlist matching.
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

/// Signature: `fn filters_response_sizes_with_exact_values_and_ranges()`
///
/// Purpose: Verifies exact and ranged response-size filters remove matching
/// results.
///
/// Parameters: None.
///
/// Returns: Nothing; assertions define success.
///
/// Notes: Remaining responses should be successful and unfiltered.
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

/// Signature: `fn records_request_timeout_as_a_failed_probe()`
///
/// Purpose: Verifies request timeouts are emitted as failed probe results.
///
/// Parameters: None.
///
/// Returns: Nothing; assertions define success.
///
/// Notes: Failures are emitted because no status allowlist is configured.
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

/// Signature: `fn sends_head_requests_and_uses_content_length_as_size()`
///
/// Purpose: Verifies `HEAD` mode sends HEAD requests and reports size from
/// `Content-Length`.
///
/// Parameters: None.
///
/// Returns: Nothing; assertions define success.
///
/// Notes: The request log confirms the scanner did not fall back to `GET`.
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
