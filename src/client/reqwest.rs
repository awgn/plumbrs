use crate::stats::{RealtimeStats, Statistics};
use crate::{Options, fatal};

use http::{HeaderMap, StatusCode};

use crate::client::utils::{build_headers, should_stop};
use reqwest::{Client, ClientBuilder, Request, Result, Url};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

pub async fn http_reqwest(
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

    let url = Url::parse(uri_str).unwrap_or_else(|e| fatal!(1, "invalid url: {e}"));

    let headers = build_headers(&uri, opts.as_ref())
        .unwrap_or_else(|e| fatal!(2, "could not build headers: {e}"));

    let body: Option<String> = Some(opts.full_body().map_or_else(
        |e| fatal!(2, "could not read body: {e}"),
        |b| String::from_utf8_lossy(&b).to_string(),
    ));

    let start = Instant::now();
    'connection: loop {
        if should_stop(total, start, &opts) {
            break 'connection;
        }

        if cid < opts.uri.len() && !banner.contains(uri_str) {
            banner.insert(uri_str.to_owned());
            println!(
                "reqwest [{tid:>2}] -> connecting. {} {} {}...",
                opts.method.as_ref().unwrap_or(&http::Method::GET),
                url,
                if opts.http2 { "HTTP/2" } else { "HTTP/1.1" }
            );
        }

        let mut client = match build_http_client(opts.as_ref(), &headers) {
            Ok(client) => client,
            Err(e) => {
                fatal!(4, "could not build reqwest http client: {e}");
            }
        };

        statistics.inc_conn();

        loop {
            let mut req = Request::new(
                opts.method.clone().unwrap_or(http::Method::GET),
                url.clone(),
            );
            *req.headers_mut() = headers.clone();

            if let Some(ref body) = body {
                *req.body_mut() = Some(body.clone().into());
            }

            let start_lat = opts.latency.then_some(Instant::now());

            match client.execute(req).await {
                Ok(res) => {
                    let code = res.status();
                    if matches!(code, StatusCode::OK) {
                        statistics.inc_ok(rt_stats);
                    } else {
                        statistics.set_http_status(code, rt_stats);
                    }
                }
                Err(ref err) => {
                    statistics.set_error(err, rt_stats);
                    total += 1;
                    continue 'connection;
                }
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
                client = match build_http_client(opts.as_ref(), &headers) {
                    Ok(client) => client,
                    Err(e) => {
                        fatal!(4, "could not build reqwest http client: {e}");
                    }
                };
            }
        }
    }

    statistics
}

pub fn build_http_client(opts: &Options, headers: &HeaderMap) -> Result<Client> {
    let mut builder = ClientBuilder::new().default_headers(headers.clone());
    if opts.http2 {
        builder = builder.http2_adaptive_window(opts.http2_adaptive_window.unwrap_or(false));

        // Apply additional HTTP/2 options
        // Note: reqwest doesn't expose initial_max_send_streams, max_concurrent_reset_streams, or max_send_buffer_size
        builder = builder.http2_initial_stream_window_size(opts.http2_initial_stream_window_size);
        builder =
            builder.http2_initial_connection_window_size(opts.http2_initial_connection_window_size);
        builder = builder.http2_max_frame_size(opts.http2_max_frame_size);
        if let Some(v) = opts.http2_max_header_list_size {
            builder = builder.http2_max_header_list_size(v);
        }
        builder = builder.http2_keep_alive_while_idle(opts.http2_keep_alive_while_idle);
    } else {
        // Configure HTTP/1 options
        if opts.http1_title_case_headers {
            builder = builder.http1_title_case_headers();
        }
        if opts.http1_allow_obsolete_multiline_headers_in_responses {
            builder = builder.http1_allow_obsolete_multiline_headers_in_responses(true);
        }
        if opts.http1_ignore_invalid_headers_in_responses {
            builder = builder.http1_ignore_invalid_headers_in_responses(true);
        }
        if opts.http1_allow_spaces_after_header_name_in_responses {
            builder = builder.http1_allow_spaces_after_header_name_in_responses(true);
        }
        if opts.http09_responses {
            builder = builder.http09_responses();
        }
    }
    builder.build()
}
