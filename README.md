# url_enum

[Chinese (Simplified)](README.zh-CN.md)

> **Authorization Required:** Use `url_enum` only against systems that you own
> or are explicitly authorized to test. Unauthorized scanning may be unlawful
> and disruptive.

`url_enum` is a command-line tool for discovering URL paths or substituting
values into URL templates from a wordlist. It accepts one web target or a
target-list file and produces results that are easy to filter or save.

## Features

- Append wordlist entries to a target URL or replace a token in supported
  request locations.
- Read an existing target-list file with one target URL per line.
- Send `GET` or `HEAD` requests with custom headers, proxy settings, and
  configurable timeouts.
- Control concurrency, add request jitter, and optionally follow redirects.
- Randomize the fully expanded target and wordlist request sequence.
- Generate extension variants and filter results by status code or response
  size.
- Write scan results as CSV or JSON Lines.

## Installation

### Download a release

Download a binary archive for your platform from
[GitHub Releases](https://github.com/cyhfvg/url_enum/releases) when available,
extract it, and place `url_enum` (or `url_enum.exe` on Windows) on your
`PATH`.

Release builds target:

- Linux x86_64: `x86_64-unknown-linux-musl`
- Windows x86_64: `x86_64-pc-windows-gnu`

### Build from source

Install the stable Rust toolchain, then run:

```bash
git clone https://github.com/cyhfvg/url_enum.git
cd url_enum
cargo build --release
```

The compiled binary is available at `target/release/url_enum` (or
`target\release\url_enum.exe` on Windows).

## Quick Start

Create a wordlist with one entry per line:

```text
admin
login
api/v1
```

Probe those paths on an authorized target:

```bash
url_enum -t https://example.com -d paths.txt --filter-http-code 200,403
```

Results are printed as CSV by default. To save JSON Lines instead:

```bash
url_enum -t https://example.com -d paths.txt \
  --filter-http-code 200,403 \
  --format jsonl -o results.jsonl
```

## Usage Examples

### Append paths to an existing URL

```bash
url_enum -t https://example.com/base -d paths.txt --concurrency 100
```

With entries such as `admin` and `api/v1`, this probes URLs under
`https://example.com/base/`.

### Scan a target list

When the value passed to `-t/--target` names an existing file, it is read as a
target list with one target URL per line:

```text
https://one.example.com
https://two.example.com/base
```

```bash
url_enum -t targets.txt -d paths.txt --concurrency 50
```

By default, requests are generated in target-major order: every wordlist entry
for the first target, then every wordlist entry for the next target.

### Randomize request order

Use `--random-sequence` to shuffle the complete target and wordlist product:

```bash
url_enum -t targets.txt -d paths.txt --random-sequence
```

For two targets and three wordlist entries, the shuffled sequence is drawn from
all six pairs, not just from a shuffled target list.

### Reduce request bursts

By default, each request gets deterministic jitter from `0` to `100`
milliseconds to reduce accidental request bursts. Increase the bound to spread
request start times further while preserving the selected concurrency limit:

```bash
url_enum -t https://example.com -d paths.txt \
  --concurrency 20 \
  --request-jitter-ms 250
```

Each HTTP request waits between `0` and the configured number of milliseconds
before it is sent. This helps reduce short bursts, but it does not replace
authorization, conservative concurrency, or an agreed testing window.

To intentionally disable this guard in a controlled environment, pass
`--request-jitter-ms 0` explicitly.

### Replace a placeholder

`--replace TOKEN` supports placeholders in:

- URLs
- Header names
- Header values

Every occurrence of `TOKEN` in those locations is replaced with the current
wordlist entry. For example:

```bash
url_enum -t http://example.com/ENUM/a -d words.txt --replace ENUM \
  -H 'X-ENUM-TRACE: ENUM.example.com'
```

For a wordlist entry of `word1`, this sends a request equivalent to:

```bash
curl http://example.com/word1/a -H 'X-word1-TRACE: word1.example.com'
```

### Try common file extensions

```bash
url_enum -t https://example.com -d paths.txt --extensions php,bak,txt
```

Each word is tried as provided and with each requested extension.

### Add request headers

`-H/--header` may be specified more than once:

```bash
url_enum -t https://example.com -d paths.txt \
  -H 'Authorization: Bearer TOKEN' \
  -H 'X-Trace: scan'
```

Cookies can be supplied as a request header:

```bash
url_enum -t https://example.com -d paths.txt -H 'Cookie: session=VALUE'
```

### Use a proxy

```bash
url_enum -t https://example.com -d paths.txt --proxy http://127.0.0.1:8080
url_enum -t https://example.com -d paths.txt \
  --proxy 'socks5h://username:password@127.0.0.1:1080'
```

Supported proxy URL schemes are `http`, `https`, `socks5`, and `socks5h`.
Include credentials in the proxy URL when authentication is required.

### Read a target from standard input

```bash
printf '%s\n' 'https://example.com' | url_enum -t - -d paths.txt
```

Standard input must provide one target URL.

## Options

| Option | Description | Default |
| --- | --- | --- |
| `-t, --target <TARGET>` | Target URL, existing target-list file, or `-` to read one URL from standard input. | Required |
| `-d, --dict <DICT>` | Wordlist file with one entry per line. | Required |
| `-r, --replace <TOKEN>` | Replace `TOKEN` wherever it occurs in URLs, header names, or header values. | Append paths |
| `--concurrency <N>` | Maximum number of concurrent requests. | `50` |
| `--request-jitter-ms <MS>` | Add deterministic per-request jitter before sending; pass `0` explicitly to disable. | `100` |
| `--random-sequence` | Shuffle the fully expanded target and wordlist request sequence. | Disabled |
| `--timeout <SECONDS>` | Request timeout in seconds. | `10` |
| `--method <get\|head>` | HTTP method. | `get` |
| `--user-agent <VALUE>` | User-Agent value. | Browser-style value |
| `-H, --header <'NAME: VALUE'>` | Add a request header; repeat as needed. | None |
| `--proxy <PROXY_URL>` | Use an HTTP(S) or SOCKS5 proxy; credentials may be included in the URL. | None |
| `--follow-redirect <true\|false>` | Follow redirects and include returned responses. | `false` |
| `--insecure <true\|false>` | Allow invalid HTTPS certificates. | `true` |
| `--filter-http-code <CODES>` | Include only comma-separated HTTP status codes. | All |
| `--black-http-code <CODES>` | Exclude comma-separated HTTP status codes. | None |
| `--black-size <SIZES>` | Exclude response sizes, such as `612` or `612-614`. | None |
| `--extensions <EXTENSIONS>` | Add comma-separated extension variants. | None |
| `-o, --output <FILE>` | Write results to a file instead of standard output. | Standard output |
| `--format <csv\|jsonl>` | Output format. | `csv` |

Run `url_enum --help` for the command-line help available in your build.

## Benchmarks

The repository includes a repeatable local benchmark that compares throughput
across concurrency values and CSV/JSONL output formats. It uses a loopback HTTP
server and does not contact external targets:

```bash
cargo bench --bench throughput
```

## Output

Both CSV and JSON Lines outputs contain these fields:

| Field | Description |
| --- | --- |
| `word` | Wordlist entry used for the request. |
| `url` | URL reported for the result. |
| `status` | HTTP status code, when a response is received. |
| `size` | Response size, when available. |
| `elapsed_ms` | Elapsed time in milliseconds. |
| `error` | Error message, when a request fails. |

## Security Notes

- Authorization is required: scan only systems that you own or are explicitly
  authorized to test.
- Begin with a conservative `--concurrency` value and follow the agreed test
  boundaries. The default `--request-jitter-ms 100` helps reduce accidental
  short bursts; pass `--request-jitter-ms 0` only when zero delay is intentional.
- Invalid HTTPS certificates are accepted by default. Use `--insecure false`
  when certificate validation is required.
- Treat output files and wordlists as potentially sensitive data.

## License

This project is licensed under the [BSD 3-Clause License](LICENSE).
