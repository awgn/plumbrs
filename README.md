# Plumbrs — A benchmarking tool for HTTP servers and client libraries in Rust

Plumbrs is a benchmarking tool for measuring and comparing HTTP server performance using a variety of asynchronous HTTP client libraries in Rust. It focuses on realistic workloads across servers and client implementations built on Tokio, helping you identify bottlenecks and optimize performance.

## Overview

Plumbrs provides ready-to-use benchmarking tasks for several popular HTTP clients, allowing you to test throughput, latency, and runtime efficiency under various configurations.

### Built-in clients

1. Hyper (`hyper`) — Hyper-based HTTP client (one per connection).
2. Hyper (legacy) (`hyper-legacy`) — Legacy Hyper HTTP client (one per connection).
3. Hyper (legacy, one per runtime) (`hyper-1rt`) — Legacy Hyper HTTP client shared across a runtime.
4. Hyper + h2 (`hyper-h2`) — HTTP/2 client using Hyper with the h2 library (one per connection).
5. Reqwest (`reqwest`) — Popular Reqwest HTTP client (one per runtime).
6. Help (`help`) — Print available client types and exit.

## Key features

- Benchmark-oriented design — Compare HTTP client performance under load.
- Multi-runtime support — Run benchmarks using multiple Tokio runtimes (single-threaded or multi-threaded).
- Connection configuration — Control the total number of connections or rely on client-specific pooling.
- Runtime tuning — Customize thread counts, scheduling, and runtime parameters.
- Optional Tokio metrics — Collect fine-grained runtime data with `tokio-metrics`.

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

- `-H, --header <KEY=VALUE>` (repeatable)
  Add an HTTP header to the request. Can be specified multiple times.
  Format: `KEY=VALUE` (for example, `-H "Accept=application/json" -H "X-Token=abc"`).

- `-B, --body <BODY>`
  Body content for the HTTP request.

- `--host <HOST>`
  Set the host to benchmark (for example, `http://192.168.0.1:8080`).
  Note: Not available with `hyper-legacy` or `hyper-1rt`.

## Client options

- `-T, --client-type <TYPE>` (default: `hyper`)
  Client type to use for benchmarking. Options:
  - `hyper` — Hyper client; one per connection. Supports HTTP/1 and HTTP/2.
  - `hyper-h2` — Hyper client using the h2 library; one per connection. HTTP/2 only.
  - `hyper-legacy` — Legacy Hyper client; one per connection. Supports HTTP/1 and HTTP/2.
  - `hyper-1rt` — Legacy Hyper client; one per runtime. Supports HTTP/1 and HTTP/2.
  - `reqwest` — Reqwest client; one per runtime. Supports HTTP/1 and HTTP/2.
  - `help` — Show available client types and exit.

- `--cps`
  Open a new connection for every request, measuring Connections Per Second (CPS).
  Note: Available only with the `hyper` client.

## HTTP/2 options

- `--http2`
  Use HTTP/2 only.

- `--http2-adaptive-window <true|false>`
  Enable or disable adaptive flow control for HTTP/2.

- `--http2-initial-max-send-streams <NUMBER>`
  Set the initial maximum number of locally initiated (send) streams.

- `--http2-max-concurrent-streams <NUMBER>`
  Set the initial maximum number of concurrent streams.

- `--http2-max-concurrent-reset-streams <NUMBER>`
  Set the initial maximum number of concurrently reset streams.

- `--http2-can-share` (feature-gated; see Feature flags)
  Allow HTTP/2 connection sharing.
  Notes:
  - Requires `--http2`.
  - Available only with `hyper-legacy` or `hyper-1rt`.
  - Available only when built with the `orion_client` feature flag.

## Tokio runtime options

- `--global-queue-interval <TICKS>`
  Tokio global queue interval (in ticks).

- `--event-interval <TICKS>`
  Tokio event interval (in ticks).

- `--max-io-events-per-tick <NUMBER>`
  Maximum number of I/O events processed per tick.

- `--multi-thread-alt` (requires `tokio_unstable`)
  Use Tokio’s alternate multi-thread scheduler.

- `--disable-lifo-slot` (requires `tokio_unstable`)
  Disable Tokio’s LIFO slot heuristic.

## Validation rules

The following CLI constraints are enforced at runtime:

- URI required for most clients:
  - For `hyper`, `hyper-legacy`, `hyper-1rt`, and `hyper-h2`, a URI (positional argument) must be provided.
- Host option restrictions:
  - `--host` is not available with `hyper-legacy` or `hyper-1rt`.
- CPS restriction:
  - `--cps` can only be used with `--client-type hyper`.
- HTTP/2 connection sharing (`--http2-can-share`) restrictions:
  - Requires `--http2`.
  - Only valid with `--client-type hyper-legacy` or `--client-type hyper-1rt`.
  - Only available when compiled with the `orion_client` feature flag.
- Threading consistency:
  - When `--multi-threaded <N>` is provided, `--threads` must be an exact multiple of `<N>`.

## Feature flags

- `tokio_metrics`
  - Enables Tokio runtime metrics collection.
  - Requires building with `tokio_unstable`. If enabled without `tokio_unstable`, the build will fail.
- `orion_client`
  - Enables the `--http2-can-share` option (HTTP/2 connection sharing).
  - Without this feature, `--http2-can-share` is not available.

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

HTTP/2 benchmark with connection sharing (requires `--features orion_client` and `--http2`):
```bash
plumbrs -T hyper-legacy --http2 --http2-can-share -c 50 -d 60 http://localhost:8080
```

Connections Per Second (CPS) test:
```bash
plumbrs --cps -c 10 -r 1000 http://localhost:8080
```

List available client types:
```bash
plumbrs -T help
```

## Enabling Tokio unstable APIs

Some runtime options require Tokio’s unstable APIs. Build with:
```bash
RUSTFLAGS="--cfg tokio_unstable" cargo build --release
```

## Enabling Tokio metrics

To enable Tokio metrics (and print aggregated runtime metrics), build with:
```bash
RUSTFLAGS="--cfg tokio_unstable" cargo build --release --features tokio_metrics
```

If `tokio_metrics` is enabled without `tokio_unstable`, the build will fail.

## Feature-gated HTTP/2 connection sharing

The `--http2-can-share` option is available only when compiled with the `orion_client` feature:
```bash
cargo build --release --features orion_client
```
It also requires `--http2` and is only valid with `hyper-legacy` or `hyper-1rt`.