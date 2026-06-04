# url_enum

[English](README.md)

> **必须获得授权：** 请仅对您拥有或已获得明确测试授权的系统使用 `url_enum`。
> 未经授权的扫描可能违法并影响目标系统运行。

`url_enum` 是一个命令行工具，用于基于字典发现 URL 路径，或将字典值替换到
URL 模板中。它支持单个 Web 目标或目标列表文件，并输出便于过滤或保存的结果。

## 功能概览

- 将字典项追加到目标 URL，或替换受支持位置中的占位符。
- 支持读取每行一个目标 URL 的目标列表文件。
- 支持 `GET` 或 `HEAD` 请求、自定义请求头、代理和超时设置。
- 支持控制并发数、添加请求抖动，并可选择跟随重定向。
- 支持对完整展开后的目标与字典组合请求顺序进行随机化。
- 支持生成扩展名变体，以及按状态码或响应大小过滤结果。
- 支持以 CSV 或 JSON Lines 格式输出扫描结果。

## 安装

### 下载发布版本

发布版本可用时，从 [GitHub Releases](https://github.com/cyhfvg/url_enum/releases)
下载适用于您平台的压缩包，解压后将 `url_enum`（Windows 为
`url_enum.exe`）放入 `PATH`。

发布构建目标包括：

- Linux x86_64：`x86_64-unknown-linux-musl`
- Windows x86_64：`x86_64-pc-windows-gnu`

### 从源码构建

安装稳定版 Rust 工具链，然后运行：

```bash
git clone https://github.com/cyhfvg/url_enum.git
cd url_enum
cargo build --release
```

编译后的二进制文件位于 `target/release/url_enum`（Windows 为
`target\release\url_enum.exe`）。

## 快速开始

创建每行包含一个条目的字典文件：

```text
admin
login
api/v1
```

对已授权目标探测这些路径：

```bash
url_enum -t https://example.com -d paths.txt --filter-http-code 200,403
```

默认以 CSV 格式打印结果。若要保存 JSON Lines 结果：

```bash
url_enum -t https://example.com -d paths.txt \
  --filter-http-code 200,403 \
  --format jsonl -o results.jsonl
```

## 使用示例

### 向已有 URL 追加路径

```bash
url_enum -t https://example.com/base -d paths.txt --concurrency 100
```

若字典中包含 `admin` 和 `api/v1`，工具会探测
`https://example.com/base/` 下对应的 URL。

### 扫描目标列表

当传给 `-t/--target` 的值是一个已存在的文件名时，该文件会按目标列表读取，
每行一个目标 URL：

```text
https://one.example.com
https://two.example.com/base
```

```bash
url_enum -t targets.txt -d paths.txt --concurrency 50
```

默认请求生成顺序为 target-major：先对第一个目标枚举所有字典项，再对下一个
目标枚举所有字典项。

### 随机化请求顺序

使用 `--random-sequence` 可随机打乱完整展开后的目标与字典组合：

```bash
url_enum -t targets.txt -d paths.txt --random-sequence
```

例如两个目标、三个字典项时，会在全部六个 `target:dict` 组合上随机，而不是
只随机目标顺序。

### 降低请求突发

可以添加确定性的请求抖动，在保留并发上限的同时分散请求发起时间：

```bash
url_enum -t https://example.com -d paths.txt \
  --concurrency 20 \
  --request-jitter-ms 250
```

每个 HTTP 请求发送前会等待 `0` 到指定毫秒数之间的时间。它有助于降低短时
突发压力，但不能替代授权、保守的并发设置和约定好的测试窗口。

### 替换占位符

`--replace TOKEN` 支持的占位符位置包含：

- URL
- Header name
- Header value

在上述位置出现的每一个 `TOKEN` 均会替换为当前字典项。例如：

```bash
url_enum -t http://example.com/ENUM/a -d words.txt --replace ENUM \
  -H 'X-ENUM-TRACE: ENUM.example.com'
```

对于字典项 `word1`，该命令会发送等效于以下命令的请求：

```bash
curl http://example.com/word1/a -H 'X-word1-TRACE: word1.example.com'
```

### 尝试常见文件扩展名

```bash
url_enum -t https://example.com -d paths.txt --extensions php,bak,txt
```

每个字典项会按原值及指定的扩展名形式进行探测。

### 添加请求头

可重复指定 `-H/--header`：

```bash
url_enum -t https://example.com -d paths.txt \
  -H 'Authorization: Bearer TOKEN' \
  -H 'X-Trace: scan'
```

Cookie 可作为请求头提供：

```bash
url_enum -t https://example.com -d paths.txt -H 'Cookie: session=VALUE'
```

### 使用代理

```bash
url_enum -t https://example.com -d paths.txt --proxy http://127.0.0.1:8080
url_enum -t https://example.com -d paths.txt \
  --proxy 'socks5h://username:password@127.0.0.1:1080'
```

支持的代理 URL 协议为 `http`、`https`、`socks5` 和 `socks5h`。
如需认证，请将凭据包含在代理 URL 中。

### 从标准输入读取目标

```bash
printf '%s\n' 'https://example.com' | url_enum -t - -d paths.txt
```

标准输入必须提供一条目标 URL。

## 参数参考

| 参数 | 说明 | 默认值 |
| --- | --- | --- |
| `-t, --target <TARGET>` | 目标 URL、已存在的目标列表文件，或使用 `-` 从标准输入读取一条 URL。 | 必填 |
| `-d, --dict <DICT>` | 每行一个条目的字典文件。 | 必填 |
| `-r, --replace <TOKEN>` | 替换 URL、header name 或 header value 中出现的 `TOKEN`。 | 追加路径 |
| `--concurrency <N>` | 最大并发请求数。 | `50` |
| `--request-jitter-ms <MS>` | 发送前添加确定性的单请求抖动。 | `0` |
| `--random-sequence` | 随机打乱完整展开后的目标与字典组合请求顺序。 | 禁用 |
| `--timeout <SECONDS>` | 单请求超时时间，单位为秒。 | `10` |
| `--method <get\|head>` | HTTP 请求方法。 | `get` |
| `--user-agent <VALUE>` | User-Agent 值。 | 浏览器风格值 |
| `-H, --header <'NAME: VALUE'>` | 添加请求头；可重复指定。 | 无 |
| `--proxy <PROXY_URL>` | 使用 HTTP(S) 或 SOCKS5 代理；认证凭据可包含在 URL 中。 | 无 |
| `--follow-redirect <true\|false>` | 跟随重定向并包含获得的响应。 | `false` |
| `--insecure <true\|false>` | 允许无效 HTTPS 证书。 | `true` |
| `--filter-http-code <CODES>` | 仅包含以逗号分隔的 HTTP 状态码。 | 全部 |
| `--black-http-code <CODES>` | 排除以逗号分隔的 HTTP 状态码。 | 无 |
| `--black-size <SIZES>` | 排除响应大小，如 `612` 或 `612-614`。 | 无 |
| `--extensions <EXTENSIONS>` | 添加以逗号分隔的扩展名变体。 | 无 |
| `-o, --output <FILE>` | 将结果写入文件，而非标准输出。 | 标准输出 |
| `--format <csv\|jsonl>` | 输出格式。 | `csv` |

运行 `url_enum --help` 可查看当前构建提供的命令行帮助。

## 基准测试

仓库包含一个可重复运行的本地基准测试，用于比较不同并发值和 CSV/JSONL
输出格式下的吞吐差异。它使用 loopback HTTP 服务，不会访问外部目标：

```bash
cargo bench --bench throughput
```

## 输出结果

CSV 与 JSON Lines 输出均包含以下字段：

| 字段 | 说明 |
| --- | --- |
| `word` | 该请求使用的字典项。 |
| `url` | 结果所对应的 URL。 |
| `status` | 收到响应时的 HTTP 状态码。 |
| `size` | 可用时的响应大小。 |
| `elapsed_ms` | 耗时，单位为毫秒。 |
| `error` | 请求失败时的错误信息。 |

## 安全提示

- 必须获得授权：仅扫描您拥有或已获得明确测试授权的系统。
- 使用较为保守的 `--concurrency` 值开始测试，并遵守约定的测试边界；需要降低
  短时请求突发时可使用 `--request-jitter-ms`。
- 默认允许无效 HTTPS 证书；需要验证证书时请使用 `--insecure false`。
- 请将输出文件和字典视为可能包含敏感信息的数据。

## 许可证

本项目基于 [BSD 3-Clause License](LICENSE) 发布。
