use crate::fatal;
use crate::stats::Statistics;

use crate::Options;
use crate::stats::RealtimeStats;

use bytes::Bytes;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use http::Request;
use http::StatusCode;
use tokio::net::TcpStream;

use crate::client::utils::*;

use h2;

pub async fn http_hyper_h2(
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
    let endpoint = build_endpoint(&host, port);
    let uri = uri_str
        .parse::<hyper::Uri>()
        .unwrap_or_else(|e| fatal!(1, "invalid uri: {e}"));

    let body : Bytes = opts.body.iter().next().map(|b| b.clone().into()).unwrap_or_else(|| Bytes::new());

    // http/2 use :authority: instead of Host header...
    let headers = build_headers(None, opts.as_ref())
        .unwrap_or_else(|e| fatal!(2, "could not build headers: {e}"));

    let trailers = build_trailers(opts.as_ref())
        .unwrap_or_else(|e| fatal!(2, "could not build trailers: {e}"));

    let start = Instant::now();
    'connection: loop {
        if should_stop(total, start, &opts) {
            break 'connection;
        }

        if cid < opts.uri.len() && !banner.contains(uri_str) {
            banner.insert(uri_str.to_owned());
            println!(
                "hyper-h2 [{tid:>2}] -> connecting to {}:{}, method = {} uri = {} HTTP2...",
                host, port, opts.method.as_ref().unwrap_or(&http::Method::GET), uri
            );
        }

        let stream_res = TcpStream::connect(endpoint)
            .await
            .and_then(|s| s.set_nodelay(true).map(|_| s));
        let mut stream = match stream_res {
            Ok(s) => s,
            Err(ref err) => {
                statistics.err(format!("{err:?}"), rt_stats);
                total += 1;
                continue 'connection;
            }
        };

        let mut h2_builder = h2::client::Builder::new();
        
        // Configure HTTP/2 options
        // Note: h2 doesn't have adaptive_window option, only hyper does
        if let Some(v) = opts.http2_initial_max_send_streams {
            h2_builder.initial_max_send_streams(v);
        }
        if let Some(v) = opts.http2_max_concurrent_reset_streams {
            h2_builder.max_concurrent_reset_streams(v);
        }
        if let Some(v) = opts.http2_initial_stream_window_size {
            h2_builder.initial_window_size(v);
        }
        if let Some(v) = opts.http2_initial_connection_window_size {
            h2_builder.initial_connection_window_size(v);
        }
        if let Some(v) = opts.http2_max_frame_size {
            h2_builder.max_frame_size(v);
        }
        if let Some(v) = opts.http2_max_header_list_size {
            h2_builder.max_header_list_size(v);
        }
        if let Some(v) = opts.http2_max_send_buffer_size {
            h2_builder.max_send_buffer_size(v);
        }
        
        let conn = h2_builder
            .handshake::<_, bytes::Bytes>(stream)
            .await;
        let (mut h2_client, mut connection) = match conn {
            Ok(h2_conn) => h2_conn,
            Err(ref err) => {
                statistics.err(format!("{err:?}"), rt_stats);
                total += 1;
                continue 'connection;
            }
        };

        tokio::task::spawn(async move {
            if let Err(err) = connection.await {
                eprintln!("error in connection: {}", err)
            }
        });

        loop {
            let mut req = Request::new(());
            *req.method_mut() = opts.method.clone().unwrap_or(http::Method::GET);
            *req.uri_mut() = uri.clone();
            *req.headers_mut() = headers.clone();

            h2_client = match h2_client.ready().await {
                Ok(h2) => h2,
                Err(ref err) => {
                    statistics.err(format!("{err:?}"), rt_stats);
                    continue 'connection;
                }
            };

            let end_of_stream = body.is_empty() && trailers.is_none();

            let start_lat = opts.latency.then_some(Instant::now());

            let (response, mut send_stream) = match h2_client.send_request(req, end_of_stream) {
                Ok(r) => r,
                Err(ref err) => {
                    statistics.err(err.to_string(), rt_stats);
                    total += 1;
                    continue 'connection;
                }
            };

            if !body.is_empty() {
                let end_of_stream = trailers.is_none();
                send_stream.send_data(body.clone(), end_of_stream).unwrap();
            }

            if let Some(ref tr) = trailers {
                send_stream.send_trailers(tr.clone()).unwrap();
            }

            let res = match response.await {
                Ok(res) => res,
                Err(ref err) => {
                    statistics.err(format!("{err:?}"), rt_stats);
                    total += 1;
                    continue 'connection;
                }
            };

            match res.status() {
                StatusCode::OK => {
                    statistics.ok(rt_stats);
                    let (_head, mut body) = res.into_parts();
                    while let Some(chunk_res) = body.data().await {
                        let chunk_len = match chunk_res {
                            Ok(ref c) => c.len(),
                            Err(ref err) => {
                                statistics.err(format!("{err:?}"), rt_stats);
                                total += 1;
                                continue 'connection;
                            }
                        };

                        let _ = body.flow_control().release_capacity(chunk_len);
                    }
                }
                code => statistics.http_status(code, rt_stats),
            }

            if let Some(start_lat) = start_lat
                && let Some(hist) = &mut statistics.latency
            {
                hist.record(start_lat.elapsed().as_micros() as u64).ok();
            };

            total += 1;

            if should_stop(total, start, &opts) {
                break 'connection;
            }

            if opts.cps {
                let stream_res = TcpStream::connect(endpoint)
                    .await
                    .and_then(|s| s.set_nodelay(true).map(|_| s));
                stream = match stream_res {
                    Ok(s) => s,
                    Err(ref err) => {
                        statistics.err(format!("{err:?}"), rt_stats);
                        total += 1;
                        continue 'connection;
                    }
                };
                let mut h2_builder = h2::client::Builder::new();
                
                // Configure HTTP/2 options
                // Note: h2 doesn't have adaptive_window option, only hyper does
                if let Some(v) = opts.http2_initial_max_send_streams {
                    h2_builder.initial_max_send_streams(v);
                }
                if let Some(v) = opts.http2_max_concurrent_reset_streams {
                    h2_builder.max_concurrent_reset_streams(v);
                }
                if let Some(v) = opts.http2_initial_stream_window_size {
                    h2_builder.initial_window_size(v);
                }
                if let Some(v) = opts.http2_initial_connection_window_size {
                    h2_builder.initial_connection_window_size(v);
                }
                if let Some(v) = opts.http2_max_frame_size {
                    h2_builder.max_frame_size(v);
                }
                if let Some(v) = opts.http2_max_header_list_size {
                    h2_builder.max_header_list_size(v);
                }
                if let Some(v) = opts.http2_max_send_buffer_size {
                    h2_builder.max_send_buffer_size(v);
                }
                
                let conn = h2_builder
                    .handshake::<_, bytes::Bytes>(stream)
                    .await;
                (h2_client, connection) = match conn {
                    Ok(h2_conn) => h2_conn,
                    Err(ref err) => {
                        statistics.err(format!("{err:?}"), rt_stats);
                        total += 1;
                        continue 'connection;
                    }
                };

                tokio::task::spawn(async move {
                    if let Err(err) = connection.await {
                        eprintln!("error in connection: {}", err)
                    }
                });
            }
        }
    }

    statistics
}
