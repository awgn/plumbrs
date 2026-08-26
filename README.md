<img src="pics/plumbrs.png" alt="plumbrs-logo" style="zoom: 60%;" />

# Plumbrs — HTTP/HTTP2 load generator for benchmarking

Plumbrs is a high-performance HTTP/HTTP2 request generator designed for benchmarking servers and comparing Rust HTTP client libraries. Built on Tokio, it helps you measure throughput, latency, and identify bottlenecks.

## Built-in clients

- **Auto** (`auto`) — Automatically select the best client (default). Uses `hyper` for `https://` URIs.
- **Hyper** (`hyper`) — Hyper-based HTTP client (one per connection). Supports HTTPS.
- **Hyper MCP** (`hyper-mcp`) — Hyper client for MCP (Model Context Protocol) servers (requires `mcp` feature). Supports HTTPS.
- **Hyper chunked** (`hyper-chunked`) — Hyper client with multi-chunked body (one per connection). Both HTTP/1 and HTTP/2. Supports HTTPS.
- **Hyper legacy** (`hyper-legacy`) — Legacy Hyper HTTP client (one per connection). HTTP only.
- **Hyper RT1** (`hyper-rt1`) — Legacy Hyper HTTP client shared across a runtime. HTTP only.
- **Hyper H2** (`hyper-h2`) — HTTP/2 client using Hyper with the h2 library (one per connection). Supports HTTPS.
- **Reqwest** (`reqwest`) — Popular Reqwest HTTP client (one per runtime). Supports HTTPS.
- **TokioUring** (`tokio-uring`) — HTTP client using tokio-uring for high-performance I/O (Linux only, requires `tokio_uring` feature). HTTP only.
- **Monoio** (`monoio`) — HTTP client using monoio for high-performance I/O (Linux only, requires `monoio` feature). HTTP only.
- **Compio** (`compio`) — HTTP client using compio for high-performance I/O (requires `compio` feature). HTTP only.
- **Help** (`help`) — Print available client types and exit.

## Basic options

- `<URI>` — HTTP/HTTPS URI(s) for the request (e.g., `http://192.168.0.1:80` or `https://example.com`). Required for most clients. HTTPS is supported with `auto`, `hyper`, `hyper-chunked`, `hyper-h2`, `hyper-mcp`, and `reqwest`. Forcing an incompatible client (e.g. `tokio-uring`, `hyper-legacy`) exits with an error.

- `-t, --threads <NUMBER>` (default: `1`) — Number of worker threads.

- `-m, --multi-threaded <NUMBER>` — Threads per Tokio runtime. If omitted, uses single-threaded executor. When specified, `--threads` must be an exact multiple of this value.

- `-c, --concurrency <NUMBER>` (default: `1`) — Concurrent connections or HTTP/2 streams.

- `-d, --duration <SECONDS>` — Test duration in seconds.

- `-r, --requests <NUMBER>` — Maximum requests per worker. If omitted, runs until duration elapses.

- `-C, --client <TYPE>` (default: `auto`) — Client type: `auto`, `hyper`, `hyper-mcp`, `hyper-chunked`, `hyper-h2`, `hyper-legacy`, `hyper-rt1`, `reqwest`, `tokio-uring`, `monoio`, `compio`, or `help`.

- `--rpc <NUMBER>` — Requests per connection. After every N requests the connection is closed (`Connection: close` is sent on the last request) and a new one is opened. Use `--rpc 1` to measure Connections Per Second. By default, connections are reused indefinitely.

- `--latency` — Enable latency estimation using Gil Tene's coordinated omission correction algorithm.



- `--host <HOST>` — Override the host to connect to. Not available with `hyper-legacy` or `hyper-rt1`. TLS SNI still uses the URI hostname unless `--sni` is set.

- `--port <PORT>` — Override the port to connect to.

- `--sni <NAME>` — Override the TLS SNI server name. Defaults to the URI hostname. Not available with `reqwest`.

- `-k, --insecure` — Skip TLS certificate verification.

- `-v, --verbose` — Enable verbose output.

- `--metrics` — Display Tokio runtime metrics at the end.

- `--stats-csv` — Print complete end-of-run statistics as a CSV row (no header).

- `--stats-csv-header` — Same as `--stats-csv`, but also print the CSV header.

## HTTP options

- `-M, --method <METHOD>` — HTTP method (e.g., `GET`, `POST`, `PUT`, `DELETE`). If omitted, defaults to `GET` when no body is provided, or `POST` when a body is specified. Note: `TRACE` method cannot have a body.

- `-H, --header <KEY:VALUE>` — Add HTTP header (repeatable).

- `-T, --trailer <KEY:VALUE>` — Add HTTP trailer (repeatable). Only supported with `hyper-chunked` or `hyper-h2` clients.

- `-b, --body <BODY>` — Request body content. Can be specified multiple times for multi-chunk encoding, but multi-chunk is only supported with `hyper-chunked` client. Use `@path` to read the body from a file (streamed).

- `--http2` — Use HTTP/2 only. Not available with `tokio-uring`, `monoio`, or `compio` clients.

## MCP options (requires `mcp` feature)

Plumbrs supports benchmarking [Model Context Protocol (MCP)](https://modelcontextprotocol.io/) servers. MCP is a protocol for communication between AI applications and tool servers, using JSON-RPC over HTTP.

When MCP mode is enabled, Plumbrs will:
1. Perform the MCP handshake (initialize, initialized notification)
2. Discover available tools via `tools/list`
3. Benchmark the server by repeatedly calling the discovered tools

Two transport modes are supported:

- `--mcp` — Enable MCP mode with **Streamable HTTP** transport (recommended). This is the newer transport where JSON-RPC requests and responses flow over standard HTTP POST requests. The server returns a session ID via the `Mcp-Session-Id` header.

- `--mcp-sse` — Enable MCP mode with legacy **Server-Sent Events (SSE)** transport (implies `--mcp`). This older transport uses a persistent SSE connection for receiving responses while sending requests via separate HTTP POST calls.

- `--mcp-rand-string-len <NUMBER>` — Fix the length of random strings generated for `tools/call` arguments. If omitted, a random length between 5 and 20 is used each time.

Both options are only available with `auto` or `hyper-mcp` client types.

**Example — Benchmark an MCP server with Streamable HTTP:**
```
plumbrs -c 10 -d 30 http://localhost:3001/mcp --mcp
```

**Example — Benchmark an MCP server with SSE transport:**
```
plumbrs -c 10 -d 30 http://localhost:3001/sse --mcp-sse
```

## HTTP/1 tuning options

- `--http1-max-buf-size <NUMBER>` — Maximum buffer size (default: ~400kb).
- `--http1-read-buf-exact-size <NUMBER>` — Exact read buffer size (unsets max-buf-size).
- `--http1-writev <true|false>` — Use vectored writes (default: auto).
- `--http1-title-case-headers` — Write header names as title case.
- `--http1-preserve-header-case` — Preserve original header case.
- `--http1-max-headers <NUMBER>` — Maximum number of headers (default: 100).
- `--http1-allow-spaces-after-header-name-in-responses` — Accept spaces after header names.
- `--http1-allow-obsolete-multiline-headers-in-responses` — Accept obsolete line folding.
- `--http1-ignore-invalid-headers-in-responses` — Silently ignore malformed headers.
- `--http09-responses` — Tolerate HTTP/0.9 responses.

## HTTP/2 tuning options

- `--http2-adaptive-window <true|false>` — Enable adaptive flow control. Not available with `hyper-h2`.
- `--http2-initial-max-send-streams <NUMBER>` — Initial max locally initiated streams. Not available with `reqwest`.
- `--http2-max-concurrent-reset-streams <NUMBER>` — Max concurrently reset streams. Not available with `reqwest`.
- `--http2-initial-stream-window-size <NUMBER>` — Initial stream-level flow control window.
- `--http2-initial-connection-window-size <NUMBER>` — Initial connection-level flow control window.
- `--http2-max-frame-size <NUMBER>` — Maximum frame size.
- `--http2-max-header-list-size <NUMBER>` — Maximum header list size.
- `--http2-max-send-buffer-size <NUMBER>` — Maximum send buffer size. Not available with `reqwest`.
- `--http2-keep-alive-while-idle` — Enable keep-alive while idle. Not available with `hyper-h2`.

## Tokio runtime options

- `--global-queue-interval <TICKS>` — Global queue interval.
- `--event-interval <TICKS>` — Event interval.
- `--max-io-events-per-tick <NUMBER>` — Maximum I/O events per tick.
- `--disable-lifo-slot` — Disable LIFO slot heuristic (requires `tokio_unstable`).

## io_uring options (Linux only)

The `tokio-uring`, `monoio`, and `compio` clients support HTTP/1 only and do not support multi-threaded runtimes (`-m`).

- `--uring-entries <NUMBER>` (default: `4096`) — Size of the io_uring Submission Queue.
- `--uring-sqpoll <MILLISECONDS>` — Enable kernel-side submission polling with idle timeout in milliseconds.

## Examples

Basic GET request with 10 concurrent connections for 30 seconds:
```
plumbrs -c 10 -d 30 http://localhost:8080
```

POST request with headers and body:
```
plumbrs -t 4 -c 100 -M POST \
  -H "Content-Type:application/json" \
  -b '{"key":"value"}' http://localhost:8080/api
```

POST with body from file:
```
plumbrs -M POST -b @./payload.json http://localhost:8080/api
```

HTTP/2 with flow control tuning:
```
plumbrs -C hyper --http2 \
  --http2-initial-stream-window-size 1048576 \
  --http2-initial-connection-window-size 2097152 \
  -c 100 -d 30 http://localhost:8080
```

Connections Per Second test:
```
plumbrs -C hyper --rpc 1 -c 10 -r 1000 http://localhost:8080
```

Latency-corrected benchmarking:
```
plumbrs --latency -c 100 -d 30 http://localhost:8080
```

## Performance

### HTTP/1.1

![HTTP/1.1 Performance](pics/http1_perf.png)

The `tokio-uring` client delivers **382K RPS on a single thread** and scales to **+1.1M RPS with 4 threads**. The `hyper` client also performs exceptionally (198K → 724K RPS), surpassing `wrk`. The `reqwest` client maintains competitive throughput (109K → 397K RPS).

### HTTP/2

![HTTP/2 Performance](pics/http2_perf.png)

The `hyper-h2` client achieves **187K RPS on a single thread** and **689K RPS with 4 threads**. The standard `hyper` client with HTTP/2 follows closely (135K → 580K RPS). All Plumbrs HTTP/2 clients outperform `rewrk` in this benchmark.

## Allocator features

Plumbrs uses [mimalloc](https://github.com/microsoft/mimalloc) by default for improved memory allocation performance.

| Build command | Allocator |
|---|---|
| `cargo build --release` | mimalloc (default) |
| `cargo build --release --no-default-features` | system allocator |

## Enabling MCP support

To enable MCP support, build Plumbrs with the `mcp` feature:
```
cargo build --release --features mcp
```

## Enabling Tokio unstable APIs

Some options require Tokio's unstable APIs:
```
RUSTFLAGS="--cfg tokio_unstable" cargo build --release
```
