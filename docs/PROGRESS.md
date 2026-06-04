# url_enum 开发进度

最后更新：2026-06-04

## 项目目标

`url_enum` 是一个面向单个目标 URL 或目标列表文件的并发字典探测工具。目前支持两种枚举方式：

- 路径追加：将字典项追加到目标 URL 的现有路径之后。
- 模板替换：使用 `--replace TOKEN` 将目标 URL 或 HTTP Header 名称和值中的占位符替换为字典项，例如 `--replace ENUM`。

请仅对已授权测试的目标使用本工具。

## 已完成能力

### 命令行接口

- `-t, --target` 接受一条目标 URL、已存在的目标列表文件，或使用 `-` 从标准输入读取唯一目标。
- `-d, --dict` 接受按行组织的路径或替换值字典。
- `-r, --replace <TOKEN>` 启用 URL 与 HTTP Header 的模板替换模式；占位符仅出现在 Header 中时保持目标 URL 不变。
- `--concurrency`、`--request-jitter-ms`、`--timeout`、`--method get|head` 控制请求行为。
- `--random-sequence` 随机打乱完整展开后的 `target × dict` 请求组合顺序，而不是只随机目标顺序。
- `--user-agent`、`--insecure` 控制 HTTP 客户端；`--follow-redirect true` 跟随并记录跳转链。
- `-H, --header 'Name: value'` 按参数顺序添加自定义 HTTP Header，支持重复同名值，也可通过 `-H 'Cookie: name=value'` 提供 Cookie。
- 通过 `-H` 指定 `User-Agent` 时，请求级 Header 优先于客户端默认 User-Agent。
- `--proxy` 支持 HTTP(S) 与 SOCKS5/SOCKS5H 代理；代理认证通过 URL 用户信息提供。
- `--filter-http-code`、`--black-http-code` 支持状态过滤；`--black-size` 支持按实际响应大小的单值或闭区间排除。
- `--extensions` 自动展开扩展名候选，例如 `admin.php`、`admin.bak`。
- `-o, --output` 和 `--format csv|jsonl` 支持结果持久化。
- 输出目标与字典为同一文件或其硬链接时拒绝运行，避免扫描前截断字典。
- 命令帮助与双语 README 顶部显著提示仅对已授权目标执行扫描。
- 命令行 `--help` 的简介、安全提示与选项说明均使用英文。

### 扫描流程

1. 读取目标 URL 或目标列表文件，并校验每个目标只接受 `http` 与 `https`。
2. 默认顺序扫描时逐行异步读取字典，跳过空行并去除当前目标下的重复候选；目标列表模式会按 target-major 顺序为每个目标重新顺序读取字典。
3. 启用 `--random-sequence` 时，先读取去重后的字典候选，并按目标与字典组合生成随机请求序列。
4. 按追加模式或替换模式生成待访问 URL。
5. 使用共享 HTTP 客户端并发发送请求。
6. 配置请求抖动时，在每次发送 HTTP 请求前加入确定性的 0..N 毫秒等待。
7. 开启重定向时，记录每个 3xx 响应并继续访问跳转地址，最多跟随 10 次。
8. 根据状态码与响应大小对每条实际响应应用过滤条件。
9. 按请求完成顺序输出 CSV 或 JSONL；成功执行时不产生统计或进度文本。

### 输出字段

| 字段 | 含义 |
| --- | --- |
| `word` | 生成该请求的字典项 |
| `url` | 实际访问 URL |
| `status` | HTTP 状态码；连接失败时为空 |
| `size` | 响应体大小；无法确定时为空 |
| `elapsed_ms` | 请求处理耗时，单位毫秒 |
| `error` | 请求或响应读取错误信息 |

成功执行的输出通道只包含上述结果记录。指定 `-o/--output` 后，结果只写入
文件；标准错误保留给无法继续执行的错误诊断。

## 性能设计

- 全部请求复用同一个 `reqwest::Client` 与连接池。
- 显式代理配置一次性绑定在共享客户端上，避免为每条候选重复建立代理规则。
- 使用异步 `try_buffer_unordered` 将同时在途的请求限制为 `--concurrency` 指定数量。
- 使用确定性请求抖动分散请求发起时间，降低短时突发压力；严格 RPS 限制仍作为后续能力。
- 默认顺序扫描时，字典以流方式读取，避免预先生成完整的 URL 请求集合。
- 多目标顺序扫描按 target-major 顺序枚举：先流式枚举第一个目标的所有字典项，再为下一个目标重新顺序读取字典并枚举。
- `--random-sequence` 仅保存完整展开后的 `(target_index, word_index)` 索引序列并洗牌，避免一次性构建完整 URL 字符串矩阵。
- 扩展名按用户输入顺序惰性生成，避免每个字典项创建临时候选列表。
- 追加模式保留字典路径中的百分号编码，包括原始字节路径，避免访问到重复编码的路径。
- 追加路径中的 `.` 与 `..` 按 URL 解析规则规范化，输出 URL 与实际请求目标一致。
- GET 响应以流方式计算大小，避免将整个响应体一次性载入内存。
- 状态码已经被过滤的响应不继续读取正文。
- 大小屏蔽范围在扫描开始前排序合并，并以二分定位匹配实际响应大小。
- 重定向链在同一个候选任务内处理，并在跨主机或端口时停止转发敏感 Header。
- release 配置启用 thin LTO 和单代码生成单元。
- `benches/throughput.rs` 使用本地 HTTP 服务比较不同并发值与 CSV/JSONL 输出格式吞吐。

## 已完成验证

已执行并通过以下检查：

```bash
cargo fmt
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

单元测试目前覆盖：

- 追加字典路径并保留原目标查询参数。
- 追加路径时保留原 URL 已有的百分号转义。
- 追加路径时保留字典项已有的百分号转义，避免 `%20` 被写为 `%2520`。
- 追加 `..` 等相对路径段时生成规范化的真实请求 URL。
- 使用 `ENUM` 模板占位符替换 URL。
- 展开带点或不带点的扩展名参数。
- 保留扩展名参数的输入顺序并去除重复后缀。
- 拒绝将输出写入字典文件的硬链接，避免输入被截断。
- 拒绝没有主机名的 HTTP(S) URL。
- 解析多条自定义 HTTP Header，并保留重复同名值的追加顺序。
- 使用 `--replace ENUM` 时以同一候选同步替换 URL、HTTP Header 名称和值中的占位符，并支持固定 URL 的 Header-only 替换。
- 跨目标跳转时省略包含 `Cookie` 在内的敏感自定义 Header。
- 解析并合并 `--black-size` 的单值、列表和闭区间表达式，并拒绝反向区间。
- 解析 `--request-jitter-ms` 并验证请求抖动延迟确定且不超过配置上限。
- 解析 `--random-sequence`。
- 解析目标列表内容并忽略空行。
- 默认多目标候选流按 target-major 顺序生成。
- `--random-sequence` 的洗牌对象覆盖完整 `target × dict` 组合。
- 接受支持的代理协议和代理认证形式，并拒绝不支持的协议或无效认证参数。

此外，已使用本地 HTTP 服务进行端到端验证：

- 默认追加模式可以命中存在的资源并过滤不存在的资源。
- `--replace ENUM` 模式生成的 URL 请求可正确访问相同资源。
- `--filter-http-code 200` 可只输出成功结果。
- 重复 Header 顺序及自定义 `User-Agent` 覆盖行为正确。
- `--follow-redirect true` 输出原始 `302` 和最终 `200` 两条记录。
- 跨端口跳转不会转发 `Authorization` 请求头。
- `--black-size` 的单值列表与闭区间均会屏蔽命中的实际响应大小。
- HTTP 与 SOCKS5H 代理可通过代理 URL 用户信息发送认证。
- SOCKS5H 代理可完成用户名/密码认证，并由代理侧接收待解析目标域名。

新增自动化集成测试使用本地 HTTP 服务覆盖：

- 状态码 allowlist 与 blocklist 组合过滤。
- `--black-size` 单值与闭区间过滤。
- 单请求超时会记录为失败探测结果。
- `--method head` 会发送 HEAD 请求，并使用 `Content-Length` 作为大小。
- `-t` 指向已存在目标列表文件时，会对文件内每个目标枚举字典项。

## 当前代码结构

| 文件 | 职责 |
| --- | --- |
| `src/main.rs` | 解析参数并启动异步扫描流程 |
| `src/lib.rs` | 导出 CLI 与扫描模块，供基准测试复用 |
| `src/cli.rs` | CLI 参数与枚举定义 |
| `src/scanner/mod.rs` | 组织扫描流程、候选流、并发探测与输出 |
| `src/scanner/input.rs` | 字典读取、目标列表读取、扩展名展开与候选顺序生成 |
| `src/scanner/url_generator.rs` | 目标 URL 校验、路径追加与模板替换 URL 生成 |
| `src/scanner/probe.rs` | HTTP 探测、响应大小计算与重定向链处理 |
| `src/scanner/filters.rs` | 状态码与响应大小过滤 |
| `src/scanner/headers.rs` | 自定义 Header 解析、模板替换与敏感 Header 保护 |
| `src/scanner/client.rs` | HTTP 客户端、代理与请求方法构建 |
| `src/scanner/output.rs` | CSV/JSONL 输出 |
| `src/scanner/pacing.rs` | 请求抖动计算与等待 |
| `benches/throughput.rs` | 本地 HTTP 吞吐基准测试 |
| `README.md` | 面向用户的构建和使用说明 |
| `docs/TODO.md` | 后续开发事项与优先级 |

## 已知边界

- 模板替换模式当前按原始字符串进行替换；字典项包含 `?`、`#`、空格或 URL 特殊字符时，需要进一步定义编码语义。
- 去重集合会随唯一字典候选数量增长；对于极大字典，需要评估内存策略。
- 多目标顺序扫描为了保持低内存和 target-major 顺序，会为每个目标重新读取字典文件；目标数量很大时需要评估重复 I/O 成本。
- 启用 `--random-sequence` 时需要保存去重后的字典候选和完整组合的索引序列，目标数与字典候选数乘积过大时需要评估内存策略。
- 当前没有严格速率限制、重试或中断后的断点恢复机制。

## 已确认决策

- 不新增专用 Cookie 配置参数：现有 `-H 'Cookie: name=value'` 已覆盖发送 Cookie 的需求，且跨目标重定向时敏感 Header 保护会停止转发 `Cookie`。
