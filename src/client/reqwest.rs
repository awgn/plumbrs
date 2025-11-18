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
    let url = Url::parse(uri_str).unwrap_or_else(|e| fatal!(1, "invalid url: {e}"));
    let headers = build_headers(None, opts.as_ref())
        .unwrap_or_else(|e| fatal!(2, "could not build headers: {e}"));

    let body = opts.body.iter().next().map(|b| b.clone());

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

        loop {
            let mut req = Request::new(opts.method.clone().unwrap_or(http::Method::GET), url.clone());
            *req.headers_mut() = headers.clone();

            if let Some(ref body) = body {
                *req.body_mut() = Some(body.clone().into());
            }

            let start_lat = opts.latency.then_some(Instant::now());

            match client.execute(req).await {
                Ok(res) => {
                    let code = res.status();
                    if matches!(code, StatusCode::OK) {
                        statistics.ok(rt_stats);
                    } else {
                        statistics.http_status(code, rt_stats);
                    }
                }
                Err(ref err) => {
                    statistics.err(format!("{err:?}"), rt_stats);
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
        if !opts.http2 {
            builder = builder.http1_only();
        }
        builder = builder.http2_adaptive_window(opts.http2_adaptive_window.unwrap_or(false));
    }
    builder.build()
}
