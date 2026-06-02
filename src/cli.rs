use clap::{ArgAction, Parser, ValueEnum};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum HttpMethod {
    Get,
    Head,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum OutputFormat {
    Csv,
    Jsonl,
}

#[derive(Debug, Parser)]
#[command(
    name = "url_enum",
    version,
    about = "Enumerate paths for a single target URL (authorized targets only)",
    long_about = "Enumerate paths for a single target URL.\n\nSecurity notice: Run this tool only against systems you own or are explicitly authorized to test."
)]
pub struct Args {
    /// Target URL; provide one URL or `-` to read from stdin
    #[arg(short = 't', long = "target")]
    pub target: String,

    /// Wordlist file; one path per line, for example: admin, login, api/v1
    #[arg(short = 'd', long = "dict")]
    pub dict: String,

    /// Replace TOKEN wherever it occurs in the target URL or HTTP headers
    #[arg(short = 'r', long = "replace", value_name = "TOKEN")]
    pub replace: Option<String>,

    /// Maximum number of concurrent requests; must be at least 1
    #[arg(long = "concurrency", default_value_t = 50)]
    pub concurrency: usize,

    /// Add deterministic per-request jitter before sending, from 0 up to this many milliseconds
    #[arg(long = "request-jitter-ms", default_value_t = 0)]
    pub request_jitter_ms: u64,

    /// Request timeout in seconds
    #[arg(long = "timeout", default_value_t = 10)]
    pub timeout: u64,

    /// HTTP request method
    #[arg(long = "method", value_enum, default_value_t = HttpMethod::Get)]
    pub method: HttpMethod,

    /// User-Agent string
    #[arg(
        long = "user-agent",
        default_value = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
    )]
    pub user_agent: String,

    /// Custom HTTP header in "Name: value" format; may be repeated
    #[arg(short = 'H', long = "header", value_name = "HEADER", action = ArgAction::Append)]
    pub headers: Vec<String>,

    /// Proxy URL; supports http(s)://, socks5://, and socks5h:// with optional credentials
    #[arg(long = "proxy", value_name = "PROXY_URL")]
    pub proxy: Option<String>,

    /// Follow redirects and emit responses from the redirect chain
    #[arg(long = "follow-redirect", default_value_t = false, action = ArgAction::Set)]
    pub follow_redirect: bool,

    /// Allow invalid HTTPS certificates
    #[arg(long = "insecure", default_value_t = true, action = ArgAction::Set)]
    pub insecure: bool,

    /// Include only specified HTTP status codes, comma-separated
    #[arg(long = "filter-http-code", value_delimiter = ',')]
    pub filter_http_code: Vec<u16>,

    /// Exclude specified HTTP status codes, comma-separated
    #[arg(long = "black-http-code", value_delimiter = ',')]
    pub black_http_code: Vec<u16>,

    /// Exclude response sizes; supports values or inclusive ranges, for example: 612,612-614
    #[arg(long = "black-size", value_delimiter = ',')]
    pub black_size: Vec<String>,

    /// Write results to a file instead of stdout
    #[arg(short = 'o', long = "output")]
    pub output: Option<String>,

    /// Output format
    #[arg(long = "format", value_enum, default_value_t = OutputFormat::Csv)]
    pub format: OutputFormat,

    /// Add extension variants to wordlist entries, comma-separated, for example: php,bak,txt
    #[arg(long = "extensions", value_delimiter = ',')]
    pub extensions: Vec<String>,
}
