<img src="logo/plumbrs.png" alt="plumbrs-logo" style="zoom: 60%;" />

# Plumbrs — A benchmarking tool for HTTP servers and client libraries in Rust

Plumbrs is a benchmarking tool for measuring and comparing HTTP server performance using a variety of asynchronous HTTP client libraries in Rust. It focuses on realistic workloads across servers and client implementations built on Tokio, helping you identify bottlenecks and optimize performance.

## Overview

Plumbrs provides ready-to-use benchmarking tasks for several popular HTTP clients, allowing you to test throughput, latency, and runtime efficiency under various configurations.

### Built-in clients

1. Auto (`auto`) — Automatically select the best client (default).
2. Hyper (`hyper`) — Hyper-based HTTP client (one per connection).
3. Hyper (`hyper-multichunk`) — Hyper-based HTTP client with multi-chunked body (one per connection).
4. Hyper (legacy) (`hyper-legacy`) — Legacy Hyper HTTP client (one per connection).
5. Hyper (legacy, one per runtime) (`hyper-rt1`) — Legacy Hyper HTTP client shared across a runtime.
6. Hyper + h2 (`hyper-h2`) — HTTP/2 client using Hyper with the h2 library (one per connection).
7. Reqwest (`reqwest`) — Popular Reqwest HTTP client (one per runtime).
8. Help (`help`) — Print available client types and exit.

## Key features

- Benchmark-oriented design — Compare HTTP client performance under load.
- Multi-runtime support — Run benchmarks using multiple Tokio runtimes (single-threaded or multi-threaded).
- Connection configuration — Control the total number of connections or rely on client-specific pooling.
- Runtime tuning — Customize thread counts, scheduling, and runtime parameters.
- Optional Tokio metrics — Collect fine-grained runtime data with `--metrics` option.

## Basic options

- URI (positional)
  HTTP URI for the request (for example, `http://192.168.0.1:80`).

- `-t, --threads <NUMBER>` (default: `1`)
  Number of worker threads to run.

- `-m, --multi-threaded <NUMBER>`
  Number of threads per Tokio runtime. If omitted, each runtime uses a single-threaded executor.

- `-v, --verbose`
  Enable verbose output.

- `-c, --concurrency <NUMBER>` (default: `1`)
  Number of concurrent connections or HTTP/2 streams.

- `-d, --duration <SECONDS>`
  Duration of the test in seconds.

- `-r, --requests <NUMBER>`
  Maximum number of requests per worker. If omitted, runs indefinitely or until the duration elapses.

## HTTP options

- `-M, --method <METHOD>` (default: `GET`)
  HTTP method to use (for example, `GET`, `POST`, `PUT`, `DELETE`).

- `-H, --header <KEY:VALUE>` (repeatable)
  Add an HTTP header to the request. Can be specified multiple times.
  Format: `KEY:VALUE` (for example, `-H "Accept:application/json" -H "X-Token:abc"`).

- `-T, --trailer <KEY:VALUE>` (repeatable)
  Add an HTTP trailer to the request. Can be specified multiple times.
  Format: `KEY:VALUE` (for example, `-T "Trailer-Name:value"`).
  Note: Not available with `reqwest` client.

- `-B, --body <BODY>`
  Body content for the HTTP request. If repeated multiple times, chunked transfer encoding
  is used.

- `--host <HOST>`
  Set the host to benchmark (for example, `http://192.168.0.1:8080`).
  Note: Not available with `hyper-legacy` or `hyper-rt1`.

## Client options

- `-C, --client <TYPE>` (default: `auto`)
  Client type to use for benchmarking. Options:
  - `auto` — Automatically select the best client (default).
  - `hyper` — Hyper client; one per connection. Supports HTTP/1 and HTTP/2.
  - `hyper-multichunk` — Hyper client; one per connection, with multi-chunked body. Supports HTTP/1 and HTTP/2.
  - `hyper-h2` — Hyper client using the h2 library; one per connection. HTTP/2 only.
  - `hyper-legacy` — Legacy Hyper client; one per connection. Supports HTTP/1 and HTTP/2.
  - `hyper-rt1` — Legacy Hyper client; one per runtime. Supports HTTP/1 and HTTP/2.
  - `reqwest` — Reqwest client; one per runtime. Supports HTTP/1 and HTTP/2.
  - `help` — Show available client types and exit.

- `--cps`
  Open a new connection for every request, measuring Connections Per Second (CPS).

- `--latency`
  Enable latency estimation using Gil Tene's coordinated omission correction algorithm.

## HTTP/1 options

- `--http1-max-buf-size <NUMBER>`
  Set the maximum buffer size for HTTP/1.
  Default is ~400kb.

- `--http1-read-buf-exact-size <NUMBER>`
  Set the exact size of the read buffer to always use for HTTP/1.
  Note: Setting this option unsets the `http1-max-buf-size` option.

- `--http1-writev <true|false>`
  Set whether HTTP/1 connections should try to use vectored writes.
  Default is `auto` (hyper will try to guess which mode to use).

- `--http1-title-case-headers`
  Set whether HTTP/1 connections will write header names as title case at the socket level.

- `--http1-preserve-header-case`
  Set whether to support preserving original header cases for HTTP/1.
  Note: Not available with `hyper-legacy` or `hyper-rt1` clients.

- `--http1-max-headers <NUMBER>`
  Set the maximum number of headers for HTTP/1.
  Default is 100.

- `--http1-allow-spaces-after-header-name-in-responses`
  Set whether HTTP/1 connections will accept spaces between header names and the colon that follows them in responses.

- `--http1-allow-obsolete-multiline-headers-in-responses`
  Set whether HTTP/1 connections will accept obsolete line folding for header values.

- `--http1-ignore-invalid-headers-in-responses`
  Set whether HTTP/1 connections will silently ignore malformed header lines.

- `--http09-responses`
  Set whether HTTP/0.9 responses should be tolerated.

### HTTP/1 options compatibility table

| Option | hyper | hyper-legacy | hyper-rt1 | reqwest |
|--------|-------|--------------|-----------|---------|
| `--http1-max-buf-size` | ✅ | ✅ | ✅ | ❌ |
| `--http1-read-buf-exact-size` | ✅ | ✅ | ✅ | ❌ |
| `--http1-writev` | ✅ | ✅ | ✅ | ❌ |
| `--http1-title-case-headers` | ✅ | ✅ | ✅ | ✅ |
| `--http1-preserve-header-case` | ✅ | ✅ | ✅ | ❌ |
| `--http1-max-headers` | ✅ | ✅ | ✅ | ❌ |
| `--http1-allow-spaces-after-header-name-in-responses` | ✅ | ✅ | ✅ | ✅ |
| `--http1-allow-obsolete-multiline-headers-in-responses` | ✅ | ✅ | ✅ | ✅ |
| `--http1-ignore-invalid-headers-in-responses` | ✅ | ✅ | ✅ | ✅ |
| `--http09-responses` | ✅ | ✅ | ✅ | ✅ |

## HTTP/2 options

- `--http2`
  Use HTTP/2 only.

- `--http2-adaptive-window <true|false>`
  Enable or disable adaptive flow control for HTTP/2.
  Note: Not available with `hyper-h2` client.

- `--http2-initial-max-send-streams <NUMBER>`
  Set the initial maximum number of locally initiated (send) streams.
  Note: Not available with `reqwest` client.

- `--http2-max-concurrent-reset-streams <NUMBER>`
  Set the initial maximum number of concurrently reset streams.
  Note: Not available with `reqwest` client.

- `--http2-initial-stream-window-size <NUMBER>`
  Set the initial window size for HTTP/2 stream-level flow control.

- `--http2-initial-connection-window-size <NUMBER>`
  Set the initial window size for HTTP/2 connection-level flow control.

- `--http2-max-frame-size <NUMBER>`
  Set the maximum frame size for HTTP/2.

- `--http2-max-header-list-size <NUMBER>`
  Set the maximum header list size for HTTP/2.

- `--http2-max-send-buffer-size <NUMBER>`
  Set the maximum send buffer size for HTTP/2.
  Note: Not available with `reqwest` client.

- `--http2-keep-alive-while-idle`
  Enable HTTP/2 keep-alive while the connection is idle.
  Note: Not available with `hyper-h2` client.

### HTTP/2 options compatibility table

| Option | hyper | hyper-h2 | hyper-legacy | hyper-rt1 | reqwest |
|--------|-------|----------|--------------|-----------|---------|
| `--http2-adaptive-window` | ✅ | ❌ | ✅ | ✅ | ✅ |
| `--http2-initial-max-send-streams` | ✅ | ✅ | ✅ | ✅ | ❌ |
| `--http2-max-concurrent-reset-streams` | ✅ | ✅ | ✅ | ✅ | ❌ |
| `--http2-initial-stream-window-size` | ✅ | ✅ | ✅ | ✅ | ✅ |
| `--http2-initial-connection-window-size` | ✅ | ✅ | ✅ | ✅ | ✅ |
| `--http2-max-frame-size` | ✅ | ✅ | ✅ | ✅ | ✅ |
| `--http2-max-header-list-size` | ✅ | ✅ | ✅ | ✅ | ✅ |
| `--http2-max-send-buffer-size` | ✅ | ✅ | ✅ | ✅ | ❌ |
| `--http2-keep-alive-while-idle` | ✅ | ❌ | ✅ | ✅ | ✅ |

## Tokio runtime options

- `--global-queue-interval <TICKS>`
  Tokio global queue interval (in ticks).

- `--event-interval <TICKS>`
  Tokio event interval (in ticks).

- `--max-io-events-per-tick <NUMBER>`
  Maximum number of I/O events processed per tick.

- `--disable-lifo-slot` (requires `tokio_unstable`)
  Disable Tokio’s LIFO slot heuristic.

## Validation rules

The following CLI constraints are enforced at runtime:

- URI required for most clients:
  - For `hyper`, `hyper-legacy`, `hyper-rt1`, and `hyper-h2`, a URI (positional argument) must be provided.
- Host option restrictions:
  - `--host` is not available with `hyper-legacy` or `hyper-rt1`.
- CPS restriction:
  - `--cps` can only be used with `--client hyper`.
- Trailer restrictions:
  - `-T, --trailer` is not available with the `reqwest` client.
- Threading consistency:
  - When `--multi-threaded <N>` is provided, `--threads` must be an exact multiple of `<N>`.

## Examples

Basic GET request with 10 concurrent connections for 30 seconds:
```bash
plumbrs -c 10 -d 30 http://localhost:8080
```

POST request with headers and a body using 4 threads and 100 concurrent connections:
```bash
plumbrs -t 4 -c 100 -M POST \
  -H "Content-Type=application/json" -H "Accept=application/json" \
  -B '{"key":"value"}' http://localhost:8080/api
```

HTTP/2 with advanced flow control and tuning options:
```bash
plumbrs -C hyper --http2 \
  --http2-adaptive-window true \
  --http2-initial-stream-window-size 1048576 \
  --http2-initial-connection-window-size 2097152 \
  --http2-max-frame-size 32768 \
  --http2-keep-alive-while-idle \
  -c 100 -d 30 http://localhost:8080
```

Connections Per Second (CPS) test:
```bash
plumbrs --cps -c 10 -r 1000 http://localhost:8080
```

List available client types:
```bash
plumbrs -C help
```

HTTP/1 with custom buffer size and header options:
```bash
plumbrs -C hyper \
  --http1-max-buf-size 524288 \
  --http1-title-case-headers \
  --http1-max-headers 150 \
  -c 100 -d 30 http://localhost:8080
```

Latency-corrected benchmarking:
```bash
plumbrs --latency -c 100 -d 30 http://localhost:8080
```

Using HTTP trailers:
```bash
plumbrs -C hyper -T "X-Checksum:abc123" -T "X-Signature:xyz789" http://localhost:8080
```

## Enabling Tokio unstable APIs

Some runtime options require Tokio’s unstable APIs. Build with:
```bash
RUSTFLAGS="--cfg tokio_unstable" cargo build --release
```
