use std::{collections::HashSet, sync::Arc, time::Instant};

use bytes::Bytes;
use http::{Request, StatusCode};
use http_body_util::{BodyExt, Either, Full};
use http_wire::{WireDecode, WireEncode, response::ResponseStatusCode};
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
        .encode()
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
                if let Some((status_code, response_end)) =
                    ResponseStatusCode::decode(&connection_buffer)
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
