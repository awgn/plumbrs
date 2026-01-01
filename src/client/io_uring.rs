use std::{collections::HashSet, sync::Arc, time::Instant};

use bytes::Bytes;
use http::{Request, StatusCode};
use http_body_util::{BodyExt, Either, Full};
use http_wire::ToWire;
use tokio_uring::net::TcpStream;

use crate::{
    client::utils::{
        build_conn_endpoint, build_headers, build_trailers, get_conn_address, should_stop,
    },
    fatal,
    options::Options,
    stats::{RealtimeStats, Statistics},
};

pub async fn http_io_uring(
    tid: usize,
    cid: usize,
    opts: Arc<Options>,
    rt_stats: &RealtimeStats,
) -> Statistics {
    let mut statistics = Statistics::new(opts.latency);

    let mut total: u32 = 0;
    let mut banner = HashSet::new();
    let uri_str = opts.uri[cid % opts.uri.len()].as_str();
    let uri = uri_str
        .parse::<hyper::Uri>()
        .unwrap_or_else(|e| fatal!(1, "invalid uri: {e}"));

    let (host, port) =
        get_conn_address(&opts, &uri).unwrap_or_else(|| fatal!(1, "no host specified in uri"));
    let endpoint = build_conn_endpoint(&host, port);

    let headers = build_headers(&uri, opts.as_ref())
        .unwrap_or_else(|e| fatal!(2, "could not build headers: {e}"));

    let trailers = build_trailers(opts.as_ref())
        .unwrap_or_else(|e| fatal!(2, "could not build trailers: {e}"));

    let body: Full<Bytes> = opts
        .full_body()
        .map_or_else(|e| fatal!(2, "could not read body: {e}"), Full::new);

    let body = match &trailers {
        None => Either::Left(body.clone()),
        tr => {
            let trailers = tr.clone().map(Result::Ok);
            Either::Right(body.clone().with_trailers(std::future::ready(trailers)))
        }
    };

    let mut req = Request::new(body);
    *req.method_mut() = opts.method.clone().unwrap_or(http::Method::GET);
    *req.uri_mut() = uri.clone();
    *req.headers_mut() = headers.clone();

    // Pre-serialize the request to bytes ONCE outside the loop for better performance
    let request_bytes = req
        .to_bytes()
        .await
        .unwrap_or_else(|e| fatal!(2, "could not serialize request: {e}"));

    let start = Instant::now();
    'connection: loop {
        if should_stop(total, start, &opts) {
            break 'connection;
        }

        if cid < opts.uri.len() && !banner.contains(uri_str) {
            banner.insert(uri_str.to_owned());
            println!(
                "io-uring [{tid:>2}] -> connecting to {}:{}, method = {} uri = {} ...",
                host,
                port,
                opts.method.as_ref().unwrap_or(&http::Method::GET),
                uri,
            );
        }

        // Connect to the endpoint...
        let addr = endpoint
            .parse()
            .unwrap_or_else(|e| fatal!(1, "invalid address: {e}"));

        let stream = match TcpStream::connect(addr).await {
            Ok(s) => s,
            Err(ref err) => {
                statistics.set_error(err, rt_stats);
                total += 1;
                continue 'connection;
            }
        };

        statistics.inc_conn();

        // Buffer for reading responses
        let mut connection_buffer = Vec::new();
        let mut read_buf = vec![0u8; 4096];
        let request = request_bytes.clone();
        let mut request: Vec<u8> = request.into();
        loop {
            let start_lat = opts.latency.then_some(Instant::now());

            // Write the pre-serialized request
            let (result, req_buf) = stream.write_all(request).await;
            request = req_buf; // Get buffer back for next iteration

            if let Err(ref err) = result {
                statistics.set_error(err, rt_stats);
                total += 1;
                continue 'connection;
            }

            // Read response from server
            loop {
                let (result, buf) = stream.read(read_buf).await;
                read_buf = buf;

                let bytes_read = match result {
                    Ok(0) => {
                        // Connection closed by server
                        total += 1;
                        continue 'connection;
                    }
                    Ok(n) => n,
                    Err(ref err) => {
                        statistics.set_error(err, rt_stats);
                        total += 1;
                        continue 'connection;
                    }
                };

                // Append new data to connection buffer
                connection_buffer.extend_from_slice(&read_buf[..bytes_read]);

                // Check if we have a complete response
                if let Some((response_end, status_code)) =
                    find_complete_response(&connection_buffer)
                {
                    // Record latency if enabled
                    if let Some(start_lat) = start_lat
                        && let Some(hist) = &mut statistics.latency
                    {
                        hist.record(start_lat.elapsed().as_micros() as u64).ok();
                    }

                    // Update statistics based on status code
                    match status_code {
                        StatusCode::OK => statistics.inc_ok(rt_stats),
                        code => statistics.set_http_status(code, rt_stats),
                    }

                    // Remove processed response from buffer
                    connection_buffer.drain(..response_end);

                    total += 1;

                    if should_stop(total, start, &opts) {
                        break 'connection;
                    }

                    // If cps mode, close connection after each request
                    if opts.cps {
                        continue 'connection;
                    }

                    // Otherwise, continue with next request on same connection
                    break;
                }
            }
        }
    }

    statistics
}

/// Find the end of a complete HTTP/1.1 response using the `httparse` crate.
/// Returns the total length of the response and the status code if complete.
#[inline(always)]
pub fn find_complete_response(buf: &[u8]) -> Option<(usize, StatusCode)> {
    // Determine headers array size. 32 is standard, but keeping it on stack is fast.
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut res = httparse::Response::new(&mut headers);

    // 1. Parse Headers
    match res.parse(buf) {
        Ok(httparse::Status::Complete(headers_len)) => {
            let code = res.code.unwrap_or(200);
            let status_code = StatusCode::from_u16(code).unwrap_or(StatusCode::OK);

            // Fast path for responses that never have a body (1xx, 204, 304)
            // Note: 1xx responses usually continue, but for a single response frame logic:
            if code == 204 || code == 304 || (code >= 100 && code < 200) {
                return Some((headers_len, status_code));
            }

            let mut content_len: Option<usize> = None;
            let mut is_chunked = false;

            // 2. Scan headers (Optimized Loop)
            for header in res.headers.iter() {
                let name = header.name.as_bytes();
                if name.len() == 14 && name.eq_ignore_ascii_case(b"Content-Length") {
                    // Manual fast parse u64
                    content_len = parse_usize(header.value);
                } else if name.len() == 17 && name.eq_ignore_ascii_case(b"Transfer-Encoding") {
                    // Check if value contains "chunked" (case insensitive)
                    // We check the last 7 bytes usually, or just linear scan.
                    // Given specific HTTP formatting, it's often exactly "chunked".
                    is_chunked = is_chunked_slice(header.value);
                }
            }

            // 3. Calculate Body End
            if is_chunked {
                let body_len = parse_chunked_body(&buf[headers_len..])?;
                Some((headers_len + body_len, status_code))
            } else {
                // If content-length is missing, length is 0 (unless connection close,
                // but for parsing complete frames we assume 0).
                let len = content_len.unwrap_or(0);
                let total = headers_len + len;
                if buf.len() >= total {
                    Some((total, status_code))
                } else {
                    None
                }
            }
        }
        _ => None, // Partial or Error
    }
}

/// Highly optimized hex parser for chunk sizes.
/// Returns the total length of the chunked body (including the final 0\r\n\r\n).
#[inline]
fn parse_chunked_body(buf: &[u8]) -> Option<usize> {
    let mut pos = 0;
    let len = buf.len();

    loop {
        if pos >= len {
            return None;
        }

        // Find CRLF fast.
        // We only scan a limited window because chunk sizes are usually small strings.
        let mut i = pos;
        let mut found_crlf = false;

        // Scan for LF. The max chunk size line usually isn't massive.
        while i < len {
            if buf[i] == b'\n' {
                found_crlf = true;
                break;
            }
            i += 1;
        }

        if !found_crlf {
            return None;
        } // Incomplete chunk size line

        // Parse hex from pos to i-1 (ignoring \r)
        // i points to \n, i-1 should be \r.
        if i == 0 || buf[i - 1] != b'\r' {
            return None;
        } // Invalid format

        let hex_end = i - 1;
        // Parse hex digits until semicolon (extension) or end of line
        let mut chunk_size = 0usize;
        for &b in &buf[pos..hex_end] {
            if b == b';' {
                break;
            } // Ignore chunk extensions

            let val = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                b'A'..=b'F' => b - b'A' + 10,
                _ => continue, // Skip whitespace or invalid chars silently for speed
            };

            // Check overflow if necessary, but for HTTP chunks usize is usually enough
            chunk_size = (chunk_size << 4) | (val as usize);
        }

        // Move pos after the \n
        pos = i + 1;

        if chunk_size == 0 {
            // Last chunk. Need to skip trailers and find final empty line (\r\n).
            // Current pos is after "0\r\n".
            // The simplest end is immediately "\r\n".
            if pos + 2 <= len && &buf[pos..pos + 2] == b"\r\n" {
                return Some(pos + 2);
            }
            // If there are trailers, we need to scan for \r\n\r\n
            // Scanning for double CRLF
            let mut k = pos;
            while k + 3 < len {
                if buf[k] == b'\r'
                    && buf[k + 1] == b'\n'
                    && buf[k + 2] == b'\r'
                    && buf[k + 3] == b'\n'
                {
                    return Some(k + 4);
                }
                k += 1;
            }
            return None; // Incomplete trailers
        }

        // Check if full chunk is available: data (chunk_size) + CRLF (2)
        let next_start = pos + chunk_size + 2;
        if next_start > len {
            return None; // Incomplete chunk data
        }
        pos = next_start;
    }
}

/// Fast usize parser (decimal).
#[inline(always)]
fn parse_usize(buf: &[u8]) -> Option<usize> {
    let mut res: usize = 0;
    let mut found = false;

    for &b in buf {
        if b.is_ascii_digit() {
            // Check for overflow could be added here if needed,
            // but wrapping is standard for "fast" parsing logic.
            res = res.wrapping_mul(10).wrapping_add((b - b'0') as usize);
            found = true;
        } else if found {
            // We were parsing numbers, now we hit a non-digit: stop.
            break;
        } else if b == b' ' || b == b'\t' {
            // Skip leading whitespace
            continue;
        } else {
            // Found a non-digit before any digit (e.g., letters)
            return None;
        }
    }

    if found {
        Some(res)
    } else {
        None
    }
}

/// Check for "chunked" case-insensitive.
#[inline(always)]
fn is_chunked_slice(buf: &[u8]) -> bool {
    let mut start = 0;
    while start < buf.len() && matches!(buf[start], b' ' | b'\t') {
        start += 1;
    }

    let mut end = buf.len();
    while end > start && matches!(buf[end - 1], b' ' | b'\t' | b'\r' | b'\n') {
        end -= 1;
    }

    let sliced = &buf[start..end];
    if sliced.len() != 7 {
        return false;
    }

    (sliced[0] | 0x20) == b'c' &&
    (sliced[1] | 0x20) == b'h' &&
    (sliced[2] | 0x20) == b'u' &&
    (sliced[3] | 0x20) == b'n' &&
    (sliced[4] | 0x20) == b'k' &&
    (sliced[5] | 0x20) == b'e' &&
    (sliced[6] | 0x20) == b'd'
}
