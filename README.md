# Plumbrs – A Benchmark Tool for HTTP Server and HTTP Libraries in Rust

**Plumbrs** is a benchmarking tool built to measure and compare the performance of HTTP servers by means of a variety of **asynchronous HTTP client libraries** in Rust.
It focuses on evaluating real-world HTTP workloads across servers and different client implementations built on **Tokio**, helping developers identify bottlenecks and optimize performance.

## Overview

Plumbers provides ready-to-use benchmarking tasks for several popular HTTP clients, allowing you to test throughput, latency, and runtime efficiency under different configurations.

### Built-in Clients

1. **hyper** – HTTP client built with the popular *Hyper* library.
2. **hyper-legacy** – HTTP client built with the legacy *Hyper* HTTP client.
3. **hyper-1rt** – Legacy *Hyper* HTTP client, shared across a runtime.
4. **hyper-h2** – HTTP2 client with *Hyper* and the *h2* library.
4. **reqwest** – Popular reqwest HTTP client.

## Key Features

- **Benchmark-oriented design** – Focused on comparing HTTP client performance under load.
- **Multi-runtime support** – Run benchmarks using multiple Tokio runtimes (single-threaded or multi-threaded).
- **Connection configuration** – Control the total number of connections or rely on client-specific pooling mechanisms.
- **Runtime tuning** – Customize thread counts, scheduling, and runtime parameters.
- **Optional Tokio metrics** – Collect fine-grained runtime data with `tokio-metrics`.

### Basic Options

- **URI** (positional argument)
  HTTP URI used in the request (e.g., `http://192.168.0.1:80`)

- `-t, --threads <NUMBER>` (default: `1`)
  Number of threads to use for running the benchmark.

- `-m, --multi-threaded <NUMBER>`
  Number of threads per Tokio runtime. If not specified, each runtime uses a single-threaded executor.

- `-v, --verbose`
  Enable verbose mode for detailed output.

- `-c, --concurrency <NUMBER>` (default: `1`)
  Concurrent number of connections or HTTP/2 streams.

- `-d, --duration <SECONDS>`
  Duration of the test in seconds.

- `-r, --requests <NUMBER>`
  Maximum number of requests per worker. If not specified, runs indefinitely or until duration expires.

### HTTP Options

- `-M, --method <METHOD>` (default: `GET`)
  HTTP method to use (e.g., `GET`, `POST`, `PUT`, `DELETE`).

- `-B, --body <BODY>`
  Body content for the HTTP request.

- `--host <HOST>`
  Set the host to benchmark (e.g., `http://192.168.0.1:8080`).
  **Note:** Not available with `hyper-legacy` or `hyper-1rt` clients.

### Client Options

- `-T, --client-type <TYPE>` (default: `hyper`)
  Client type to use for benchmarking. Available options:
  - `hyper` – Hyper client, one per connection. Supports both HTTP/1 and HTTP/2.
  - `hyper-h2` – Hyper client using the h2 package, one per connection. HTTP/2 only.
  - `hyper-legacy` – Legacy Hyper client, one per connection. Supports both HTTP/1 and HTTP/2.
  - `hyper-1rt` – Legacy Hyper client, one per runtime. Supports both HTTP/1 and HTTP/2.
  - `reqwest` – Reqwest client, one per runtime. Supports both HTTP/1 and HTTP/2.
  - `help` – Display available client types and exit.

- `--cps`
  Open a new connection for every request, computing Connections Per Second (CPS).
  **Note:** Only available with the `hyper` client.

### HTTP/2 Options

- `--http2`
  Use HTTP/2 only.

- `--http2-adaptive-window <true|false>`
  Enable or disable adaptive flow control for HTTP/2.

- `--http2-initial-max-send-streams <NUMBER>`
  Sets the initial maximum number of locally initiated (send) streams.

- `--http2-max-concurrent-streams <NUMBER>`
  Sets the initial maximum number of concurrent streams.

- `--http2-max-concurrent-reset-streams <NUMBER>`
  Sets the initial maximum number of concurrently reset streams.

- `--http2-can-share`
  Allow HTTP/2 connection sharing.
  **Note:** Requires `--http2` to be enabled. Only available with `hyper-legacy` or `hyper-1rt` clients.

### Tokio Runtime Options

- `--global-queue-interval <TICKS>`
  Tokio global queue interval in ticks.

- `--event-interval <TICKS>`
  Tokio event interval in ticks.

- `--max-io-events-per-tick <NUMBER>`
  Maximum number of I/O events processed per tick.

- `--multi-thread-alt` *(requires `tokio_unstable`)*
  Use Tokio's alternate multi-thread scheduler.

- `--disable-lifo-slot` *(requires `tokio_unstable`)*
  Disable Tokio's LIFO slot heuristic.

### Examples

Basic GET request with 10 concurrent connections for 30 seconds:
```bash
plumbrs -c 10 -d 30 http://localhost:8080
```

POST request with a body using 4 threads and 100 concurrent connections:
```bash
plumbrs -t 4 -c 100 -M POST -B '{"key":"value"}' http://localhost:8080/api
```

HTTP/2 benchmark with connection sharing:
```bash
plumbrs -T hyper-legacy --http2 --http2-can-share -c 50 -d 60 http://localhost:8080
```

Connections Per Second (CPS) test:
```bash
plumbrs --cps -c 10 -r 1000 http://localhost:8080
```

## Enabling Tokio Unstable

To enable features that rely on Tokio’s unstable API, build with:

```bash
RUSTFLAGS="--cfg tokio_unstable" cargo build --release
```

## Enabling Tokio Metrics

Optionally, enable Tokio Metrics for detailed runtime statistics:

```bash
RUSTFLAGS="--cfg tokio_unstable" cargo build --release --features tokio_metrics
```
