//! Temporary micro-benchmark: OLD vs NEW code paths from hyper_mcp.rs optimization.
//! Run with: cargo run --release --example mcp_opt_bench --features mcp
//! (deleted after measurement)
use bytes::Bytes;
use http::{HeaderMap, header};
use http_body_util::Full;
use rand::RngExt;
use std::hint::black_box;
use std::time::Instant;

fn bench(name: &str, iters: u64, mut f: impl FnMut()) {
    for _ in 0..1000 {
        f();
    }
    let start = Instant::now();
    for _ in 0..iters {
        f();
    }
    let ns = start.elapsed().as_nanos() as f64 / iters as f64;
    println!("{name:44} {ns:10.1} ns/op");
}

fn main() {
    // ---- A: per-request body selection (8 tools, ~200B bodies) ----
    let tool_bodies: Vec<Bytes> = (0..8)
        .map(|i| Bytes::from(format!("{{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"tools/call\",\"params\":{{\"padding\":\"{}\"}}}}", 100 + i, "x".repeat(150))))
        .collect();
    let bodies_old: Vec<Full<Bytes>> = tool_bodies.iter().cloned().map(Full::new).collect();

    let mut total_old: usize = 0;
    bench("A OLD body: get(total % len).cloned()", 2_000_000, || {
        let b = bodies_old
            .get(total_old % bodies_old.len())
            .cloned()
            .unwrap_or_else(|| Full::new(Bytes::from("")));
        total_old = total_old.wrapping_add(1);
        black_box(b);
    });
    let mut body_idx: usize = 0;
    bench("A NEW body: wrapping cursor, no modulo", 2_000_000, || {
        if body_idx >= tool_bodies.len() {
            body_idx = 0;
        }
        let b = Full::new(tool_bodies[body_idx].clone());
        body_idx += 1;
        black_box(b);
    });

    // ---- B: per-request headers (5 headers, like the MCP hot loop) ----
    let mut headers = HeaderMap::new();
    headers.insert(
        header::HOST,
        header::HeaderValue::from_static("localhost:8080"),
    );
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    headers.insert(
        header::ACCEPT,
        header::HeaderValue::from_static("application/json, text/event-stream"),
    );
    headers.insert(
        header::HeaderName::from_static("mcp-session-id"),
        header::HeaderValue::from_static("abc123"),
    );
    headers.insert(
        header::USER_AGENT,
        header::HeaderValue::from_static("plumbrs"),
    );
    let mut headers_close = headers.clone();
    headers_close.insert(
        header::CONNECTION,
        header::HeaderValue::from_static("close"),
    );

    bench("B OLD headers: clone + insert(close)", 1_000_000, || {
        let mut h = headers.clone();
        h.insert(
            header::CONNECTION,
            header::HeaderValue::from_static("close"),
        );
        black_box(h);
    });
    bench("B OLD headers: clone only (common path)", 1_000_000, || {
        black_box(headers.clone());
    });
    bench("B NEW headers: clone prebuilt set", 1_000_000, || {
        black_box(headers_close.clone());
    });

    // ---- C: SSE line processing, 100 data lines ----
    let mut src = String::new();
    for i in 0..100 {
        src.push_str(&format!(
            "data: {{\"jsonrpc\":\"2.0\",\"id\":{i},\"result\":{{}}}}\n"
        ));
        if i % 10 == 9 {
            src.push('\n');
        }
    }
    bench("C OLD sse line: drain+collect per line", 20_000, || {
        let mut buffer = src.clone();
        let mut event_data = String::new();
        loop {
            if let Some(idx) = buffer.find('\n') {
                let line_full: String = buffer.drain(..=idx).collect();
                let line = line_full.trim_end();
                if line.is_empty() {
                    if !event_data.is_empty() {
                        break;
                    }
                    continue;
                }
                if let Some(rest) = line.strip_prefix("data:") {
                    let content = rest.strip_prefix(' ').unwrap_or(rest);
                    event_data.push_str(content);
                    event_data.push('\n');
                }
                continue;
            } else {
                break;
            }
        }
        black_box(event_data);
    });
    bench("C NEW sse line: borrow, copy once, drain", 20_000, || {
        let mut buffer = src.clone();
        let mut event_data = String::new();
        loop {
            if let Some(idx) = buffer.find('\n') {
                let empty_line = {
                    let line = buffer[..idx].trim_end();
                    if line.is_empty() {
                        true
                    } else {
                        if let Some(rest) = line.strip_prefix("data:") {
                            event_data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
                            event_data.push('\n');
                        }
                        false
                    }
                };
                buffer.drain(..=idx);
                if empty_line && !event_data.is_empty() {
                    break;
                }
                continue;
            } else {
                break;
            }
        }
        black_box(event_data);
    });

    // ---- D: chunk append, 1KB valid UTF-8 ----
    let chunk: &[u8] = &[b'a'; 1024];
    bench("D OLD chunk: push lossy (allocs temp)", 200_000, || {
        let mut buffer = String::new();
        buffer.push_str(&String::from_utf8_lossy(black_box(chunk)));
        black_box(buffer);
    });
    bench("D NEW chunk: from_utf8 fast path", 200_000, || {
        let mut buffer = String::new();
        match std::str::from_utf8(black_box(chunk)) {
            Ok(s) => buffer.push_str(s),
            Err(_) => buffer.push_str(&String::from_utf8_lossy(chunk)),
        }
        black_box(buffer);
    });

    // ---- E: init response parse, ~3KB tools/list JSON ----
    let tool_json = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{{\"tools\":[{}]}}}}",
        (0..8)
            .map(|i| format!("{{\"name\":\"tool{i}\",\"inputSchema\":{{\"type\":\"object\",\"properties\":{{\"q\":{{\"type\":\"string\"}}}},\"required\":[\"q\"]}}}}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let json_bytes = Bytes::from(tool_json);
    bench("E OLD parse: lossy + to_string + from_str", 20_000, || {
        let s = String::from_utf8_lossy(&json_bytes).to_string();
        let v: rmcp::serde_json::Value = rmcp::serde_json::from_str(&s).unwrap();
        black_box(v);
    });
    bench("E NEW parse: from_slice, zero copy", 20_000, || {
        let v: rmcp::serde_json::Value = rmcp::serde_json::from_slice(&json_bytes).unwrap();
        black_box(v);
    });

    // ---- F: fuzz string gen, len 12 ----
    bench("F OLD fuzz string: collect chars", 500_000, || {
        let mut rng = rand::rng();
        let s: String = (0..12)
            .map(|_| rng.sample(rand::distr::Alphanumeric) as char)
            .collect();
        black_box(s);
    });
    bench("F NEW fuzz string: with_capacity loop", 500_000, || {
        let mut rng = rand::rng();
        let mut s = String::with_capacity(12);
        for _ in 0..12 {
            s.push(rng.sample(rand::distr::Alphanumeric) as char);
        }
        black_box(s);
    });
}
