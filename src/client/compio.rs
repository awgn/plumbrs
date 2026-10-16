use std::{collections::HashSet, sync::Arc, time::Instant};

use bytes::Bytes;
use compio::io::{AsyncRead, AsyncWriteExt};
use compio::net::{TcpSocket, TcpStream, ToSocketAddrsAsync};
use http::{Request, StatusCode};
use http_body_util::{BodyExt, Either, Full};
use http_wire::{WireDecode, WireEncode, response::FullResponse};

use crate::{
    client::utils::{
        build_conn_endpoint, build_request_targets, build_trailers, ensure_content_length,
        get_conn_address, should_stop,
    },
    fatal,
    options::Options,
    stats::{RealtimeStats, Statistics},
};

/// Open a compio TCP connection, optionally binding a source address first.
///
/// With an empty `locals` the kernel picks the source IP and an ephemeral
/// port (previous behavior). Otherwise each candidate is tried in order until
/// one connects; `AddrInUse`/`AddrNotAvailable` moves on to the next
/// candidate so a busy port does not fail the connection.
async fn connect_compio(
    endpoint: &str,
    locals: &[std::net::SocketAddr],
) -> std::io::Result<TcpStream> {
    if locals.is_empty() {
        let stream = TcpStream::connect(endpoint).await?;
        stream.set_nodelay(true)?;
        return Ok(stream);
    }

    let mut remotes: Vec<std::net::SocketAddr> = endpoint.to_socket_addrs_async().await?.collect();
    if remotes.is_empty() {
        return Err(std::io::Error::other(format!(
            "cannot resolve '{endpoint}'"
        )));
    }
    // Prefer a remote matching the requested local family.
    if let Some(local) = locals.first()
        && let Some(pos) = remotes.iter().position(|r| r.is_ipv4() == local.is_ipv4())
    {
        remotes.swap(0, pos);
    }
    let remote = remotes[0];

    let mut last_err = None;
    for local in locals {
        // A wildcard local follows the remote family (e.g. 0.0.0.0 vs [::]).
        let mut bind = *local;
        if bind.ip().is_unspecified() && bind.is_ipv4() != remote.is_ipv4() {
            bind = std::net::SocketAddr::new(
                if remote.is_ipv4() {
                    std::net::IpAddr::from([0, 0, 0, 0])
                } else {
                    std::net::IpAddr::from([0u16; 8])
                },
                bind.port(),
            );
        }
        let socket = match bind {
            std::net::SocketAddr::V4(_) => TcpSocket::new_v4().await,
            std::net::SocketAddr::V6(_) => TcpSocket::new_v6().await,
        };
        let socket = match socket {
            Ok(s) => s,
            Err(e) => {
                last_err = Some(e);
                continue;
            }
        };
        // Allow quick reuse of ports stuck in TIME_WAIT (matters with --rpc).
        let _ = socket.set_reuseaddr(true);
        if let Err(e) = socket.bind(bind).await {
            last_err = Some(e);
            continue;
        }
        match socket.connect(remote).await {
            Ok(stream) => {
                stream.set_nodelay(true)?;
                return Ok(stream);
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::AddrInUse | std::io::ErrorKind::AddrNotAvailable
                ) =>
            {
                last_err = Some(e);
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_err.unwrap_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::AddrInUse, "no usable source port")
    }))
}

pub async fn http_compio(
    tid: usize,
    cid: usize,
    opts: Arc<Options>,
    rt_stats: &RealtimeStats,
) -> Statistics {
    let mut statistics = Statistics::new(opts.latency);

    let mut total: u32 = 0;
    let mut conn_req_count: u32;
    let mut banner = HashSet::new();
    let uri_str = opts.uri[cid % opts.uri.len()].as_str();
    let uri = uri_str
        .parse::<hyper::Uri>()
        .unwrap_or_else(|e| fatal!(1, "invalid uri: {e}"));

    let (host, port) =
        get_conn_address(&opts, &uri).unwrap_or_else(|| fatal!(1, "no host specified in uri"));
    let endpoint = build_conn_endpoint(&host, port);

    let targets = build_request_targets(&uri, opts.as_ref(), opts.absolute_uri)
        .unwrap_or_else(|e| fatal!(2, "could not build request targets: {e}"));

    let trailers = build_trailers(opts.as_ref())
        .unwrap_or_else(|e| fatal!(2, "could not build trailers: {e}"));

    let raw_bodies = opts
        .bodies()
        .unwrap_or_else(|e| fatal!(2, "could not read body: {e}"));
    let body_len = raw_bodies.first().map(|b| b.len()).unwrap_or(0);
    let bodies: Vec<Full<Bytes>> = raw_bodies.into_iter().map(Full::new).collect::<Vec<_>>();

    let base_body = bodies
        .first()
        .cloned()
        .unwrap_or_else(|| Full::new(Bytes::new()));

    // Pre-serialize a request buffer for each target ONCE outside the loop for zero runtime overhead.
    let request_bytes_templates: Vec<Bytes> = targets
        .into_iter()
        .map(|target| {
            let headers = ensure_content_length(target.headers, body_len);
            let body = match &trailers {
                None => Either::Left(base_body.clone()),
                tr => {
                    let trailers = tr.clone().map(Ok);
                    Either::Right(
                        base_body
                            .clone()
                            .with_trailers(std::future::ready(trailers)),
                    )
                }
            };
            let mut req = Request::new(body);
            *req.method_mut() = opts.method.clone().unwrap_or(http::Method::GET);
            *req.uri_mut() = target.uri;
            *req.headers_mut() = headers;
            req.encode()
                .unwrap_or_else(|e| fatal!(2, "could not serialize request: {e}"))
        })
        .collect();
    let mut target_idx = (opts.conn_base + cid) % request_bytes_templates.len();

    let clock = quanta::Clock::new();
    let start = Instant::now();
    let mut generation: u64 = 0;
    'connection: loop {
        if should_stop(total, start, &opts) {
            break 'connection;
        }

        if cid < opts.uri.len() && !banner.contains(uri_str) {
            banner.insert(uri_str.to_owned());
            eprintln!(
                "compio [{tid:>2}] -> connecting to {}:{}, method = {} uri = {} ...",
                host,
                port,
                opts.method.as_ref().unwrap_or(&http::Method::GET),
                uri,
            );
        }

        // Connect to the endpoint, binding a source port when requested.
        let locals = crate::rss::source_candidates(&opts, cid, generation);
        generation = generation.wrapping_add(1);
        let mut stream = match connect_compio(endpoint, &locals).await {
            Ok(s) => s,
            Err(ref err) => {
                statistics.set_error(err, rt_stats);
                total += 1;
                continue 'connection;
            }
        };

        statistics.inc_conn();
        conn_req_count = 0;

        // Buffer for reading responses
        let mut connection_buffer = Vec::new();
        let mut read_buf = vec![0u8; 4096];
        let mut request_buffers: Vec<Vec<u8>> = request_bytes_templates
            .iter()
            .map(|b| b.clone().into())
            .collect();
        loop {
            let start_lat = opts.latency.then_some(clock.raw());
            let cur_idx = target_idx;
            target_idx += 1;
            if target_idx == request_buffers.len() {
                target_idx = 0;
            }
            let request = std::mem::take(&mut request_buffers[cur_idx]);

            // Write the pre-serialized request
            let write_result = stream.write_all(request).await;
            let result = write_result.0;
            request_buffers[cur_idx] = write_result.1; // Get buffer back for next iteration

            if let Err(ref err) = result {
                statistics.set_error(err, rt_stats);
                total += 1;
                continue 'connection;
            }

            // Read response from server
            loop {
                let read_result = stream.read(read_buf).await;
                let result = read_result.0;
                read_buf = read_result.1;

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

                let mut headers = [httparse::EMPTY_HEADER; 16];

                // Check if we have a complete response
                if let Ok((resp, response_end)) =
                    FullResponse::decode(&connection_buffer, &mut headers)
                {
                    // Record latency if enabled
                    if let Some(start_lat) = start_lat
                        && let Some(hist) = &mut statistics.latency
                    {
                        hist.record(clock.delta_as_nanos(start_lat, clock.raw()) / 1000)
                            .ok();
                    }

                    // Update statistics based on status code
                    match resp.head.code {
                        Some(200) => statistics.inc_ok(rt_stats),
                        Some(c) => {
                            let code = StatusCode::from_u16(c)
                                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                            statistics.set_http_status(code, rt_stats);
                        }
                        None => {
                            statistics.set_http_status(StatusCode::INTERNAL_SERVER_ERROR, rt_stats);
                        }
                    }

                    // Remove processed response from buffer
                    connection_buffer.drain(..response_end);

                    total += 1;
                    conn_req_count += 1;

                    if should_stop(total, start, &opts) {
                        break 'connection;
                    }

                    // If rpc limit reached, close connection and open a new one
                    if conn_req_count >= opts.rpc {
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
