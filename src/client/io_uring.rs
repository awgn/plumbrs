use std::{collections::HashSet, sync::Arc, time::Instant};

use bytes::Bytes;
use http::{Request, StatusCode};
use http_body_util::{BodyExt, Either, Full};
use http_wire::ToWire;
use tokio_uring::net::TcpStream;

use crate::{
    client::utils::{build_endpoint, build_headers, build_trailers, get_host_port, should_stop},
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
    let (host, port) = get_host_port(&opts, uri_str);
    let _endpoint = build_endpoint(&host, port);
    let uri = uri_str
        .parse::<hyper::Uri>()
        .unwrap_or_else(|e| fatal!(1, "invalid uri: {e}"));

    let headers = build_headers(uri.host(), opts.as_ref())
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
        let addr = format!("{host}:{port}")
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
pub fn find_complete_response(buf: &[u8]) -> Option<(usize, StatusCode)> {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut res = httparse::Response::new(&mut headers);

    match res.parse(buf) {
        Ok(httparse::Status::Complete(headers_len)) => {
            let status_code = StatusCode::from_u16(res.code.unwrap_or(0)).unwrap_or(StatusCode::OK);

            let mut content_length: Option<usize> = None;
            let mut is_chunked = false;

            // Iterate through the parsed headers
            for header in res.headers.iter() {
                if header.name.eq_ignore_ascii_case("Content-Length") {
                    if let Ok(s) = std::str::from_utf8(header.value)
                        && let Ok(val) = s.trim().parse::<usize>()
                    {
                        content_length = Some(val);
                    }
                } else if header.name.eq_ignore_ascii_case("Transfer-Encoding")
                    && let Ok(s) = std::str::from_utf8(header.value)
                    && s.to_lowercase().contains("chunked")
                {
                    is_chunked = true;
                }
            }

            if is_chunked {
                // For chunked encoding, find the terminating "0\r\n\r\n"
                find_chunked_end(&buf[headers_len..])
                    .map(|body_len| (headers_len + body_len, status_code))
            } else if let Some(cl) = content_length {
                let total_len = headers_len + cl;
                if buf.len() >= total_len {
                    Some((total_len, status_code))
                } else {
                    None
                }
            } else {
                // No Content-Length and not chunked - assume headers only (like 204 No Content)
                // or wait for connection close
                Some((headers_len, status_code))
            }
        }
        _ => None,
    }
}

/// Find the end of chunked transfer encoding body
fn find_chunked_end(buf: &[u8]) -> Option<usize> {
    let mut pos = 0;

    loop {
        // Find the end of chunk size line
        let chunk_size_end = find_crlf(&buf[pos..])?;
        let chunk_size_str = std::str::from_utf8(&buf[pos..pos + chunk_size_end]).ok()?;

        // Parse chunk size (may include extensions after semicolon)
        let chunk_size_hex = chunk_size_str.split(';').next()?;
        let chunk_size = usize::from_str_radix(chunk_size_hex.trim(), 16).ok()?;

        // Move past chunk size line (including \r\n)
        pos += chunk_size_end + 2;

        if chunk_size == 0 {
            // Last chunk - expect trailing \r\n (and possibly trailers)
            if buf.len() >= pos + 2 && &buf[pos..pos + 2] == b"\r\n" {
                return Some(pos + 2);
            }
            // Check for trailers (simplified - just look for \r\n\r\n)
            if let Some(trailer_end) = find_double_crlf(&buf[pos..]) {
                return Some(pos + trailer_end);
            }
            return None;
        }

        // Move past chunk data and trailing \r\n
        let chunk_end = pos + chunk_size + 2;
        if buf.len() < chunk_end {
            return None;
        }
        pos = chunk_end;
    }
}

/// Find \r\n in buffer, returns position of \r
#[inline]
fn find_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\r\n")
}

/// Find \r\n\r\n in buffer, returns position after the sequence
#[inline]
fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}
