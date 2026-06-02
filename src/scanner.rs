use std::collections::HashSet;
use std::fs::File as StdFile;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use async_stream::try_stream;
use futures::{Stream, StreamExt, TryStreamExt};
use reqwest::{
    Client, Method, Proxy, RequestBuilder, Response, StatusCode,
    header::{
        AUTHORIZATION, COOKIE, HeaderName, HeaderValue, LOCATION, PROXY_AUTHORIZATION,
        WWW_AUTHENTICATE,
    },
    redirect::Policy,
};
use serde::Serialize;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use url::{Position, Url};

use crate::cli::{Args, HttpMethod, OutputFormat};

const MAX_REDIRECTS: usize = 10;

#[derive(Debug)]
struct Candidate {
    word: String,
    url: Url,
}

#[derive(Debug, Serialize)]
struct ProbeResult {
    word: String,
    url: String,
    status: Option<u16>,
    size: Option<u64>,
    elapsed_ms: u128,
    error: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct RequestPacing {
    jitter: Duration,
}

impl RequestPacing {
    fn new(jitter_ms: u64) -> Self {
        Self {
            jitter: Duration::from_millis(jitter_ms),
        }
    }

    fn delay_for(&self, word: &str, url: &Url, request_index: usize) -> Duration {
        if self.jitter.is_zero() {
            return Duration::ZERO;
        }

        let max_ms = self.jitter.as_millis().min(u64::MAX as u128) as u64;
        let hash = jitter_hash(word, url.as_str(), request_index);
        let delay_ms = if max_ms == u64::MAX {
            hash
        } else {
            hash % (max_ms + 1)
        };
        Duration::from_millis(delay_ms)
    }

    async fn wait_before_request(&self, word: &str, url: &Url, request_index: usize) {
        let delay = self.delay_for(word, url, request_index);
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
    }
}

fn jitter_hash(word: &str, url: &str, request_index: usize) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in word
        .as_bytes()
        .iter()
        .chain(url.as_bytes())
        .copied()
        .chain((request_index as u64).to_le_bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[derive(Debug)]
enum UrlGenerator {
    Append(Url),
    Fixed(Url),
    Replace { template: String, token: String },
}

impl UrlGenerator {
    fn new(target: String, replace: Option<String>, header_has_token: bool) -> Result<Self> {
        let parsed_target = parse_http_url(&target)?;

        match replace {
            Some(token) => {
                if token.is_empty() {
                    bail!("替换占位符不能为空");
                }
                if target.contains(&token) {
                    Ok(Self::Replace {
                        template: target,
                        token,
                    })
                } else if header_has_token {
                    Ok(Self::Fixed(parsed_target))
                } else {
                    bail!("目标URL或HTTP Header中未找到替换占位符 `{token}`");
                }
            }
            None => Ok(Self::Append(parsed_target)),
        }
    }

    fn build(&self, word: &str) -> Result<Url> {
        match self {
            Self::Append(base) => {
                let word = word.trim_start_matches('/');
                append_path(base, word)
            }
            Self::Fixed(target) => Ok(target.clone()),
            Self::Replace { template, token } => {
                let value = template.replace(token, word);
                parse_http_url(&value).with_context(|| format!("字典项 `{word}` 生成了无效URL"))
            }
        }
    }
}

fn append_path(base: &Url, word: &str) -> Result<Url> {
    let before_suffix = &base[..Position::AfterPath];
    let suffix = &base[Position::AfterPath..];
    let before_suffix = before_suffix.strip_suffix('/').unwrap_or(before_suffix);
    let mut value = String::with_capacity(base.as_str().len() + word.len() + 1);
    value.push_str(before_suffix);
    value.push('/');
    for character in word.chars() {
        match character {
            '?' => value.push_str("%3F"),
            '#' => value.push_str("%23"),
            _ => value.push(character),
        }
    }
    value.push_str(suffix);
    parse_http_url(&value)
}

#[derive(Debug)]
struct Filters {
    accepted_status: HashSet<u16>,
    blocked_status: HashSet<u16>,
    blocked_size: BlockedSizes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SizeRange {
    start: u64,
    end: u64,
}

#[derive(Debug, Default)]
struct BlockedSizes(Vec<SizeRange>);

impl BlockedSizes {
    fn parse(values: &[String]) -> Result<Self> {
        let mut ranges = Vec::with_capacity(values.len());
        for value in values {
            ranges.push(parse_size_range(value)?);
        }
        ranges.sort_unstable_by_key(|range| range.start);

        let mut merged: Vec<SizeRange> = Vec::with_capacity(ranges.len());
        for range in ranges {
            if let Some(previous) = merged.last_mut()
                && range.start <= previous.end.saturating_add(1)
            {
                previous.end = previous.end.max(range.end);
                continue;
            }
            merged.push(range);
        }
        Ok(Self(merged))
    }

    fn contains(&self, size: u64) -> bool {
        let index = self.0.partition_point(|range| range.start <= size);
        index > 0 && self.0[index - 1].end >= size
    }
}

impl Filters {
    fn new(args: &Args) -> Result<Self> {
        Ok(Self {
            accepted_status: args.filter_http_code.iter().copied().collect(),
            blocked_status: args.black_http_code.iter().copied().collect(),
            blocked_size: BlockedSizes::parse(&args.black_size)?,
        })
    }

    fn accepts_status(&self, status: u16) -> bool {
        (self.accepted_status.is_empty() || self.accepted_status.contains(&status))
            && !self.blocked_status.contains(&status)
    }

    fn accepts_failure(&self) -> bool {
        self.accepted_status.is_empty()
    }

    fn accepts_size(&self, size: u64) -> bool {
        !self.blocked_size.contains(size)
    }
}

fn parse_size_range(value: &str) -> Result<SizeRange> {
    let value = value.trim();
    if value.is_empty() {
        bail!("排除响应大小的值不能为空");
    }

    if let Some((start, end)) = value.split_once('-') {
        if start.trim().is_empty() || end.trim().is_empty() || end.contains('-') {
            bail!("无效响应大小范围 `{value}`, 格式应为 `START-END`");
        }
        let start = parse_size_value(start, value)?;
        let end = parse_size_value(end, value)?;
        if start > end {
            bail!("无效响应大小范围 `{value}`, 起始值不能大于结束值");
        }
        return Ok(SizeRange { start, end });
    }

    let size = parse_size_value(value, value)?;
    Ok(SizeRange {
        start: size,
        end: size,
    })
}

fn parse_size_value(value: &str, input: &str) -> Result<u64> {
    value
        .trim()
        .parse()
        .with_context(|| format!("无效响应大小 `{input}`, 应为非负整数或 `START-END`"))
}

#[derive(Debug)]
enum RequestHeaderName {
    Literal(HeaderName),
    Replace { template: String, token: String },
}

impl RequestHeaderName {
    fn render(&self, word: &str) -> Result<HeaderName> {
        match self {
            Self::Literal(name) => Ok(name.clone()),
            Self::Replace { template, token } => {
                let value = template.replace(token, word);
                HeaderName::from_bytes(value.as_bytes())
                    .with_context(|| format!("字典项 `{word}` 生成了无效HTTP Header名称 `{value}`"))
            }
        }
    }
}

#[derive(Debug)]
enum RequestHeaderValue {
    Literal(HeaderValue),
    Replace { template: String, token: String },
}

impl RequestHeaderValue {
    fn render(&self, word: &str) -> Result<HeaderValue> {
        match self {
            Self::Literal(value) => Ok(value.clone()),
            Self::Replace { template, token } => {
                HeaderValue::from_str(&template.replace(token, word))
                    .with_context(|| format!("字典项 `{word}` 生成了无效HTTP Header值"))
            }
        }
    }
}

#[derive(Debug)]
struct RequestHeader {
    name: RequestHeaderName,
    value: RequestHeaderValue,
}

#[derive(Debug, Default)]
struct RequestHeaders {
    values: Vec<RequestHeader>,
    has_token: bool,
}

impl RequestHeaders {
    fn parse(headers: &[String], replacement: Option<&str>) -> Result<Self> {
        let mut parsed = Vec::with_capacity(headers.len());
        let mut has_token = false;
        for header in headers {
            let (name, value) = header
                .split_once(':')
                .with_context(|| format!("无效HTTP Header `{header}`, 格式应为 `Name: value`"))?;
            let name = name.trim();
            if name.is_empty() {
                bail!("无效HTTP Header `{header}`, Header名称不能为空");
            }
            let value = value.trim();
            let name = if let Some(token) = replacement.filter(|token| name.contains(token)) {
                has_token = true;
                RequestHeaderName::Replace {
                    template: name.to_owned(),
                    token: token.to_owned(),
                }
            } else {
                RequestHeaderName::Literal(
                    HeaderName::from_bytes(name.as_bytes())
                        .with_context(|| format!("无效HTTP Header名称 `{name}`"))?,
                )
            };
            let value = if let Some(token) = replacement.filter(|token| value.contains(token)) {
                has_token = true;
                RequestHeaderValue::Replace {
                    template: value.to_owned(),
                    token: token.to_owned(),
                }
            } else {
                RequestHeaderValue::Literal(
                    HeaderValue::from_str(value)
                        .with_context(|| format!("无效HTTP Header值 `{header}`"))?,
                )
            };
            parsed.push(RequestHeader { name, value });
        }
        Ok(Self {
            values: parsed,
            has_token,
        })
    }

    fn has_token(&self) -> bool {
        self.has_token
    }

    fn apply(
        &self,
        mut request: RequestBuilder,
        include_sensitive: bool,
        word: &str,
    ) -> Result<RequestBuilder> {
        for header in &self.values {
            let name = header.name.render(word)?;
            if !include_sensitive && is_sensitive_redirect_header(&name) {
                continue;
            }
            let value = header
                .value
                .render(word)
                .with_context(|| format!("字典项 `{word}` 生成了无效HTTP Header值 `{name}`"))?;
            request = request.header(name, value);
        }
        Ok(request)
    }
}

enum ResultWriter {
    Csv(Box<csv::Writer<Box<dyn Write>>>),
    Jsonl(BufWriter<Box<dyn Write>>),
}

impl ResultWriter {
    fn new(format: OutputFormat, output: Option<&str>) -> Result<Self> {
        let destination: Box<dyn Write> = match output {
            Some(path) => Box::new(
                StdFile::create(path).with_context(|| format!("无法创建输出文件 `{path}`"))?,
            ),
            None => Box::new(io::stdout()),
        };

        Ok(match format {
            OutputFormat::Csv => {
                Self::Csv(Box::new(csv::WriterBuilder::new().from_writer(destination)))
            }
            OutputFormat::Jsonl => Self::Jsonl(BufWriter::new(destination)),
        })
    }

    fn write(&mut self, result: &ProbeResult) -> Result<()> {
        match self {
            Self::Csv(writer) => writer.serialize(result).context("写入CSV结果失败"),
            Self::Jsonl(writer) => {
                serde_json::to_writer(&mut *writer, result).context("写入JSONL结果失败")?;
                writeln!(writer).context("写入JSONL换行失败")
            }
        }
    }

    fn flush(&mut self) -> Result<()> {
        match self {
            Self::Csv(writer) => writer.flush().context("刷新CSV输出失败"),
            Self::Jsonl(writer) => writer.flush().context("刷新JSONL输出失败"),
        }
    }
}

pub async fn run(args: Args) -> Result<()> {
    if args.concurrency == 0 {
        bail!("并发数必须大于0");
    }
    if args.timeout == 0 {
        bail!("超时时间必须大于0秒");
    }

    let filters = Arc::new(Filters::new(&args)?);
    let target = read_target(&args.target).await?;
    let headers = Arc::new(RequestHeaders::parse(
        &args.headers,
        args.replace.as_deref(),
    )?);
    let generator = Arc::new(UrlGenerator::new(
        target,
        args.replace.clone(),
        headers.has_token(),
    )?);
    let extensions = normalize_extensions(&args.extensions);
    let client = build_client(&args)?;
    let method = method_for(args.method);
    let pacing = RequestPacing::new(args.request_jitter_ms);
    let dict = open_dictionary(Path::new(&args.dict), args.output.as_deref()).await?;
    let mut writer = ResultWriter::new(args.format, args.output.as_deref())?;

    let candidates = candidate_stream(dict, generator, extensions);
    let requests = candidates.map_ok(|candidate| {
        probe(
            client.clone(),
            method.clone(),
            candidate,
            Arc::clone(&filters),
            Arc::clone(&headers),
            args.follow_redirect,
            pacing,
        )
    });
    let results = requests.try_buffer_unordered(args.concurrency);
    futures::pin_mut!(results);

    while let Some(results) = results.try_next().await? {
        for result in results {
            writer.write(&result)?;
        }
    }
    writer.flush()?;
    Ok(())
}

fn candidate_stream(
    file: File,
    generator: Arc<UrlGenerator>,
    extensions: Vec<String>,
) -> impl Stream<Item = Result<Candidate>> {
    try_stream! {
        let mut lines = BufReader::new(file).lines();
        let mut seen = HashSet::new();

        while let Some(line) = lines.next_line().await.context("读取字典失败")? {
            let word = line.trim();
            if word.is_empty() {
                continue;
            }
            for candidate_word in expanded_words(word, &extensions) {
                if seen.insert(candidate_word.clone()) {
                    let url = generator.build(&candidate_word)?;
                    yield Candidate {
                        word: candidate_word,
                        url,
                    };
                }
            }
        }
    }
}

async fn open_dictionary(path: &Path, output: Option<&str>) -> Result<File> {
    let file = File::open(path)
        .await
        .with_context(|| format!("无法读取字典文件 `{}`", path.display()))?;

    if let Some(output) = output {
        match same_file::is_same_file(path, output) {
            Ok(true) => bail!("输出文件不能与字典文件相同，以免覆盖输入数据"),
            Ok(false) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("无法校验输出文件 `{output}` 是否覆盖字典"));
            }
        }
    }

    Ok(file)
}

async fn read_target(value: &str) -> Result<String> {
    if value != "-" {
        return Ok(value.to_owned());
    }

    let mut input = String::new();
    tokio::io::stdin()
        .read_to_string(&mut input)
        .await
        .context("从stdin读取目标URL失败")?;
    let targets: Vec<&str> = input
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    match targets.as_slice() {
        [target] => Ok((*target).to_owned()),
        [] => bail!("stdin中未提供目标URL"),
        _ => bail!("stdin中只能提供一条目标URL"),
    }
}

fn normalize_extensions(extensions: &[String]) -> Vec<String> {
    let mut seen = HashSet::with_capacity(extensions.len());
    let mut normalized = Vec::with_capacity(extensions.len());
    for extension in extensions {
        let extension = extension.trim().trim_start_matches('.').to_owned();
        if !extension.is_empty() && seen.insert(extension.clone()) {
            normalized.push(extension);
        }
    }
    normalized
}

fn expanded_words<'a>(
    word: &'a str,
    extensions: &'a [String],
) -> impl Iterator<Item = String> + 'a {
    std::iter::once(word.to_owned()).chain(extensions.iter().map(move |extension| {
        let mut candidate = String::with_capacity(word.len() + extension.len() + 1);
        candidate.push_str(word);
        candidate.push('.');
        candidate.push_str(extension);
        candidate
    }))
}

fn build_client(args: &Args) -> Result<Client> {
    let mut builder = Client::builder()
        // Redirects are handled by `probe` so each received response can be emitted.
        .redirect(Policy::none())
        .danger_accept_invalid_certs(args.insecure)
        .user_agent(&args.user_agent)
        .timeout(Duration::from_secs(args.timeout))
        .connect_timeout(Duration::from_secs(args.timeout))
        .pool_max_idle_per_host(args.concurrency)
        .tcp_keepalive(Duration::from_secs(30));
    if let Some(proxy) = build_proxy(args)? {
        builder = builder.proxy(proxy);
    }
    builder.build().context("创建HTTP客户端失败")
}

fn build_proxy(args: &Args) -> Result<Option<Proxy>> {
    let Some(value) = args.proxy.as_deref() else {
        return Ok(None);
    };
    let url = Url::parse(value).context("无法解析代理URL")?;
    if !matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h") {
        bail!("代理URL只支持http、https、socks5或socks5h协议");
    }
    if url.host_str().is_none() {
        bail!("代理URL必须包含主机名");
    }

    let proxy = Proxy::all(url).context("创建代理配置失败")?;
    Ok(Some(proxy))
}

async fn probe(
    client: Client,
    method: Method,
    candidate: Candidate,
    filters: Arc<Filters>,
    headers: Arc<RequestHeaders>,
    follow_redirect: bool,
    pacing: RequestPacing,
) -> Result<Vec<ProbeResult>> {
    let mut results = Vec::new();
    let mut url = candidate.url;
    let mut redirects_followed = 0_usize;
    let mut include_sensitive_headers = true;
    let mut request_index = 0_usize;

    loop {
        pacing
            .wait_before_request(&candidate.word, &url, request_index)
            .await;
        request_index += 1;
        let started = Instant::now();
        let response = headers
            .apply(
                client.request(method.clone(), url.clone()),
                include_sensitive_headers,
                &candidate.word,
            )?
            .send()
            .await;

        match response {
            Ok(response) => {
                let redirect = follow_redirect
                    .then(|| redirect_target(&response, &url))
                    .flatten();
                if let Some(result) =
                    response_result(&candidate.word, &url, &method, response, &filters, started)
                        .await?
                {
                    results.push(result);
                }

                let Some(next_url) = redirect else {
                    return Ok(results);
                };
                if redirects_followed >= MAX_REDIRECTS {
                    return Ok(results);
                }
                if !same_authority(&url, &next_url) {
                    include_sensitive_headers = false;
                }
                redirects_followed += 1;
                url = next_url;
            }
            Err(error) => {
                if filters.accepts_failure() {
                    results.push(ProbeResult {
                        word: candidate.word,
                        url: url.to_string(),
                        status: None,
                        size: None,
                        elapsed_ms: started.elapsed().as_millis(),
                        error: Some(error.to_string()),
                    });
                }
                return Ok(results);
            }
        }
    }
}

async fn response_result(
    word: &str,
    url: &Url,
    method: &Method,
    response: Response,
    filters: &Filters,
    started: Instant,
) -> Result<Option<ProbeResult>> {
    let status = response.status().as_u16();
    if !filters.accepts_status(status) {
        return Ok(None);
    }

    let size = if *method == Method::HEAD {
        response.content_length()
    } else {
        let mut bytes = response.bytes_stream();
        let mut total = 0_u64;
        while let Some(chunk) = bytes.next().await {
            match chunk {
                Ok(chunk) => total = total.saturating_add(chunk.len() as u64),
                Err(error) => {
                    return Ok(Some(ProbeResult {
                        word: word.to_owned(),
                        url: url.to_string(),
                        status: Some(status),
                        size: None,
                        elapsed_ms: started.elapsed().as_millis(),
                        error: Some(error.to_string()),
                    }));
                }
            }
        }
        Some(total)
    };
    if size.is_some_and(|size| !filters.accepts_size(size)) {
        return Ok(None);
    }

    Ok(Some(ProbeResult {
        word: word.to_owned(),
        url: url.to_string(),
        status: Some(status),
        size,
        elapsed_ms: started.elapsed().as_millis(),
        error: None,
    }))
}

fn redirect_target(response: &Response, current: &Url) -> Option<Url> {
    if !matches!(
        response.status(),
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    ) {
        return None;
    }
    let location = response.headers().get(LOCATION)?.to_str().ok()?;
    let next = current.join(location).ok()?;
    matches!(next.scheme(), "http" | "https").then_some(next)
}

fn same_authority(left: &Url, right: &Url) -> bool {
    left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn is_sensitive_redirect_header(name: &HeaderName) -> bool {
    name == AUTHORIZATION
        || name == COOKIE
        || name == PROXY_AUTHORIZATION
        || name == WWW_AUTHENTICATE
        || name.as_str().eq_ignore_ascii_case("cookie2")
}

fn parse_http_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).context("无法解析目标URL")?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("目标URL只支持http或https协议");
    }
    if url.host_str().is_none() {
        bail!("目标URL必须包含主机名");
    }
    Ok(url)
}

fn method_for(method: HttpMethod) -> Method {
    match method {
        HttpMethod::Get => Method::GET,
        HttpMethod::Head => Method::HEAD,
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn appends_dictionary_path_after_target_path() {
        let generator = UrlGenerator::new("https://example.test/root?x=1".to_owned(), None, false)
            .expect("valid generator");

        let url = generator.build("api/v1").expect("valid candidate");

        assert_eq!(url.as_str(), "https://example.test/root/api/v1?x=1");
    }

    #[test]
    fn appending_path_preserves_existing_url_escaping() {
        let generator = UrlGenerator::new("https://example.test/a%20b/".to_owned(), None, false)
            .expect("valid generator");

        let url = generator.build("admin").expect("valid candidate");

        assert_eq!(url.as_str(), "https://example.test/a%20b/admin");
    }

    #[test]
    fn appending_path_preserves_dictionary_percent_encoding() {
        let generator = UrlGenerator::new("https://example.test/root".to_owned(), None, false)
            .expect("valid target");

        let url = generator
            .build("a%20b/next%2Fpart")
            .expect("valid encoded candidate");

        assert_eq!(url.as_str(), "https://example.test/root/a%20b/next%2Fpart");
    }

    #[test]
    fn appending_path_preserves_non_utf8_percent_encoded_bytes() {
        let generator = UrlGenerator::new("https://example.test/root".to_owned(), None, false)
            .expect("valid target");

        let url = generator.build("binary%FFname").expect("valid raw path");

        assert_eq!(url.as_str(), "https://example.test/root/binary%FFname");
    }

    #[test]
    fn appending_parent_segment_follows_url_resolution_rules() {
        let generator = UrlGenerator::new("https://example.test/root/base".to_owned(), None, false)
            .expect("valid target");

        let url = generator.build("..").expect("valid parent segment");

        assert_eq!(url.as_str(), "https://example.test/root/");
    }

    #[test]
    fn replaces_enum_placeholder() {
        let generator = UrlGenerator::new(
            "https://example.test/ENUM/index?name=ENUM".to_owned(),
            Some("ENUM".to_owned()),
            false,
        )
        .expect("valid generator");

        let url = generator.build("admin").expect("valid candidate");

        assert_eq!(url.as_str(), "https://example.test/admin/index?name=admin");
    }

    #[test]
    fn expands_extensions_without_leading_dots() {
        let extensions = normalize_extensions(&[".php".to_owned(), "bak".to_owned()]);
        let words: Vec<String> = expanded_words("admin", &extensions).collect();

        assert!(words.contains(&"admin".to_owned()));
        assert!(words.contains(&"admin.php".to_owned()));
        assert!(words.contains(&"admin.bak".to_owned()));
    }

    #[test]
    fn extension_expansion_preserves_argument_order_and_discards_duplicates() {
        let extensions = normalize_extensions(&[
            "bak".to_owned(),
            ".php".to_owned(),
            "bak".to_owned(),
            " txt ".to_owned(),
        ]);

        assert_eq!(extensions, vec!["bak", "php", "txt"]);
    }

    #[test]
    fn parses_and_appends_repeated_headers_in_value_order() {
        let headers = RequestHeaders::parse(
            &[
                "X-Trace: first".to_owned(),
                "X-Trace: second".to_owned(),
                "Authorization: Bearer token:a".to_owned(),
            ],
            None,
        )
        .expect("valid headers");
        let request = headers
            .apply(Client::new().get("https://example.test"), true, "admin")
            .expect("valid rendered headers")
            .build()
            .expect("valid request");
        let trace_values: Vec<&str> = request
            .headers()
            .get_all("x-trace")
            .iter()
            .map(|value| value.to_str().expect("ASCII header value"))
            .collect();

        assert_eq!(trace_values, vec!["first", "second"]);
        assert_eq!(
            request
                .headers()
                .get("authorization")
                .expect("authorization"),
            "Bearer token:a"
        );
    }

    #[test]
    fn rejects_header_without_a_colon() {
        let error = RequestHeaders::parse(&["Authorization token".to_owned()], None)
            .expect_err("invalid header");

        assert!(error.to_string().contains("Name: value"));
    }

    #[test]
    fn omits_sensitive_headers_after_cross_authority_redirect() {
        let headers = RequestHeaders::parse(
            &[
                "Authorization: Bearer secret".to_owned(),
                "Cookie: session=secret".to_owned(),
                "X-Trace: safe".to_owned(),
            ],
            None,
        )
        .expect("valid headers");
        let request = headers
            .apply(
                Client::new().get("https://other.example.test"),
                false,
                "admin",
            )
            .expect("valid rendered headers")
            .build()
            .expect("valid request");

        assert!(!request.headers().contains_key(AUTHORIZATION));
        assert!(!request.headers().contains_key(COOKIE));
        assert_eq!(request.headers().get("x-trace").expect("trace"), "safe");
    }

    #[test]
    fn replaces_token_in_header_values_for_each_candidate() {
        let headers = RequestHeaders::parse(
            &[
                "Host: ENUM.example.test".to_owned(),
                "X-Path: /api/ENUM".to_owned(),
            ],
            Some("ENUM"),
        )
        .expect("valid headers");
        let request = headers
            .apply(Client::new().get("https://example.test"), true, "admin")
            .expect("valid replacement")
            .build()
            .expect("valid request");

        assert!(headers.has_token());
        assert_eq!(
            request.headers().get("host").expect("host header"),
            "admin.example.test"
        );
        assert_eq!(
            request.headers().get("x-path").expect("path header"),
            "/api/admin"
        );
    }

    #[test]
    fn replaces_same_candidate_in_url_and_header_name_and_value() {
        let headers =
            RequestHeaders::parse(&["X-ENUM-TRACE: ENUM.example.com".to_owned()], Some("ENUM"))
                .expect("valid headers");
        let generator = UrlGenerator::new(
            "http://example.com/ENUM/a".to_owned(),
            Some("ENUM".to_owned()),
            headers.has_token(),
        )
        .expect("valid replacement generator");
        let url = generator.build("word1").expect("valid candidate URL");
        let request = headers
            .apply(Client::new().get(url), true, "word1")
            .expect("valid rendered headers")
            .build()
            .expect("valid request");

        assert_eq!(request.url().as_str(), "http://example.com/word1/a");
        assert_eq!(
            request
                .headers()
                .get("x-word1-trace")
                .expect("replaced header"),
            "word1.example.com"
        );
    }

    #[test]
    fn permits_replace_token_only_in_header_value_without_appending_path() {
        let headers = RequestHeaders::parse(&["Host: ENUM.example.test".to_owned()], Some("ENUM"))
            .expect("valid headers");
        let generator = UrlGenerator::new(
            "https://example.test/base".to_owned(),
            Some("ENUM".to_owned()),
            headers.has_token(),
        )
        .expect("valid header-only replacement");

        let url = generator.build("admin").expect("unchanged URL");

        assert_eq!(url.as_str(), "https://example.test/base");
    }

    #[test]
    fn permits_replace_token_only_in_header_name_without_appending_path() {
        let headers = RequestHeaders::parse(&["X-ENUM-Trace: fixed".to_owned()], Some("ENUM"))
            .expect("valid headers");
        let generator = UrlGenerator::new(
            "https://example.test/base".to_owned(),
            Some("ENUM".to_owned()),
            headers.has_token(),
        )
        .expect("valid header-name-only replacement");
        let url = generator.build("admin").expect("unchanged URL");
        let request = headers
            .apply(Client::new().get(url), true, "admin")
            .expect("valid replacement")
            .build()
            .expect("valid request");

        assert_eq!(request.url().as_str(), "https://example.test/base");
        assert_eq!(
            request
                .headers()
                .get("x-admin-trace")
                .expect("replaced header"),
            "fixed"
        );
    }

    #[test]
    fn omits_dynamically_named_sensitive_headers_after_cross_authority_redirect() {
        let headers = RequestHeaders::parse(&["ENUM: secret".to_owned()], Some("ENUM"))
            .expect("valid headers");
        let request = headers
            .apply(
                Client::new().get("https://other.example.test"),
                false,
                "Authorization",
            )
            .expect("valid replacement")
            .build()
            .expect("valid request");

        assert!(!request.headers().contains_key(AUTHORIZATION));
    }

    #[test]
    fn rejects_replace_token_missing_from_url_and_headers() {
        let error = UrlGenerator::new(
            "https://example.test/base".to_owned(),
            Some("ENUM".to_owned()),
            false,
        )
        .expect_err("missing replacement token must fail");

        assert!(error.to_string().contains("HTTP Header"));
    }

    #[test]
    fn detects_changed_redirect_authority() {
        let initial = Url::parse("https://example.test:443/start").expect("valid URL");
        let same = Url::parse("https://example.test/next").expect("valid URL");
        let changed = Url::parse("https://example.test:8443/next").expect("valid URL");

        assert!(same_authority(&initial, &same));
        assert!(!same_authority(&initial, &changed));
    }

    #[test]
    fn parses_and_merges_blocked_size_values_and_ranges() {
        let args = Args::try_parse_from([
            "url_enum",
            "-t",
            "https://example.test",
            "-d",
            "dict.txt",
            "--black-size",
            "612",
            "--black-size",
            "613,614",
            "--black-size",
            "700-702",
        ])
        .expect("valid size arguments");
        let blocked = BlockedSizes::parse(&args.black_size).expect("valid ranges");

        assert_eq!(
            blocked.0,
            vec![
                SizeRange {
                    start: 612,
                    end: 614
                },
                SizeRange {
                    start: 700,
                    end: 702
                }
            ]
        );
        assert!(blocked.contains(613));
        assert!(blocked.contains(702));
        assert!(!blocked.contains(615));
    }

    #[test]
    fn rejects_descending_blocked_size_range() {
        let error =
            BlockedSizes::parse(&["614-612".to_owned()]).expect_err("descending range must fail");

        assert!(error.to_string().contains("起始值不能大于结束值"));
    }

    #[test]
    fn parses_request_jitter_milliseconds() {
        let args = Args::try_parse_from([
            "url_enum",
            "-t",
            "https://example.test",
            "-d",
            "dict.txt",
            "--request-jitter-ms",
            "250",
        ])
        .expect("valid jitter arguments");

        assert_eq!(args.request_jitter_ms, 250);
    }

    #[test]
    fn request_jitter_is_deterministic_and_bounded() {
        let pacing = RequestPacing::new(250);
        let url = Url::parse("https://example.test/admin").expect("valid URL");

        let first = pacing.delay_for("admin", &url, 0);
        let second = pacing.delay_for("admin", &url, 0);
        let next_request = pacing.delay_for("admin", &url, 1);

        assert_eq!(first, second);
        assert!(first <= Duration::from_millis(250));
        assert!(next_request <= Duration::from_millis(250));
    }

    #[test]
    fn zero_request_jitter_has_no_delay() {
        let pacing = RequestPacing::new(0);
        let url = Url::parse("https://example.test/admin").expect("valid URL");

        assert_eq!(pacing.delay_for("admin", &url, 0), Duration::ZERO);
    }

    #[test]
    fn accepts_supported_proxy_urls_with_embedded_credentials() {
        for proxy_url in [
            "http://127.0.0.1:8080",
            "https://proxy-user:secret@proxy.example.test:8443",
            "socks5://proxy-user:secret@127.0.0.1:1080",
            "socks5h://127.0.0.1:1080",
        ] {
            let args = Args::try_parse_from([
                "url_enum",
                "-t",
                "https://example.test",
                "-d",
                "dict.txt",
                "--proxy",
                proxy_url,
            ])
            .expect("valid proxy arguments");

            assert!(build_proxy(&args).expect("valid proxy").is_some());
        }
    }

    #[test]
    fn rejects_removed_proxy_short_and_credentials_options() {
        let short_proxy = Args::try_parse_from([
            "url_enum",
            "-t",
            "https://example.test",
            "-d",
            "dict.txt",
            "-x",
            "http://127.0.0.1:8080",
        ]);
        assert!(short_proxy.is_err());

        for credentials_option in ["-U", "--proxy-user"] {
            let separate_credentials = Args::try_parse_from([
                "url_enum",
                "-t",
                "https://example.test",
                "-d",
                "dict.txt",
                "--proxy",
                "http://127.0.0.1:8080",
                credentials_option,
                "analyst:password",
            ]);
            assert!(separate_credentials.is_err());
        }
    }

    #[test]
    fn rejects_unsupported_proxy_protocol() {
        let unsupported = Args::try_parse_from([
            "url_enum",
            "-t",
            "https://example.test",
            "-d",
            "dict.txt",
            "--proxy",
            "ftp://127.0.0.1:2121",
        ])
        .expect("CLI accepts proxy value for scanner validation");
        let error = build_proxy(&unsupported).expect_err("unsupported proxy must fail");
        assert!(error.to_string().contains("代理URL只支持"));
    }

    #[test]
    fn rejects_http_url_without_a_host() {
        let error = parse_http_url("http:/").expect_err("hostless target must fail");

        assert!(error.to_string().contains("主机名") || error.to_string().contains("解析"));
    }

    #[tokio::test]
    async fn refuses_to_overwrite_dictionary_through_a_hard_link() {
        let directory = std::env::temp_dir().join(format!(
            "url_enum_test_{}_{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        std::fs::create_dir_all(&directory).expect("create temporary test directory");
        let dictionary = directory.join("dict.txt");
        let output = directory.join("result.csv");
        std::fs::write(&dictionary, "admin\n").expect("write dictionary");
        std::fs::hard_link(&dictionary, &output).expect("create hard link");

        let error = open_dictionary(&dictionary, output.to_str())
            .await
            .expect_err("same output file must fail");

        assert!(error.to_string().contains("输出文件不能与字典文件相同"));
        std::fs::remove_dir_all(directory).expect("remove temporary test directory");
    }
}
