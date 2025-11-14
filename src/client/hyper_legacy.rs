use crate::stats::{RealtimeStats, Statistics};
use crate::{Options, fatal};

use bytes::Bytes;
use http::Request;
use http_body_util::{BodyExt, Either, Full};
use hyper::StatusCode;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use crate::client::utils::{
    build_headers, build_http_connection_legacy, build_trailers, discard_body, should_stop,
};

pub async fn http_hyper_legacy(
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
        .parse::<http::Uri>()
        .unwrap_or_else(|e| fatal!(1, "invalid uri: {e}"));
    let host = uri.host().unwrap_or_else(|| fatal!(3, "host not found"));

    let headers = build_headers(Some(host), opts.as_ref())
        .unwrap_or_else(|e| fatal!(2, "could not build headers: {e}"));

    let trailers = build_trailers(opts.as_ref())
        .unwrap_or_else(|e| fatal!(2, "could not build trailers: {e}"));

    let body = if let Some(body) = &opts.body {
        Full::new(body.clone().into())
    } else {
        Full::new(Bytes::new())
    };

    let start = Instant::now();
    'connection: loop {
        if should_stop(total, start, &opts) {
            break 'connection;
        }

        if cid < opts.uri.len() && !banner.contains(uri_str) {
            banner.insert(uri_str.to_owned());
            println!(
                "hyper-legacy [{tid:>2}] -> connecting to {} {}...",
                uri,
                if opts.http2 { "HTTP/2" } else { "HTTP/1.1" }
            );
        }

        let mut client = build_http_connection_legacy(&opts);

        loop {
            let body = match &trailers {
                None => Either::Left(body.clone()),
                tr => {
                    let trailers = tr.clone().map(Result::Ok);
                    Either::Right(body.clone().with_trailers(std::future::ready(trailers)))
                }
            };

            let mut req = Request::new(body);
            *req.method_mut() = opts.method.clone();
            *req.uri_mut() = uri.clone();
            *req.headers_mut() = headers.clone();

            let start_lat = opts.latency.then_some(Instant::now());

            match client.request(req).await {
                Ok(res) => match discard_body(res).await {
                    Ok(StatusCode::OK) => statistics.ok(rt_stats),
                    Ok(code) => statistics.http_status(code, rt_stats),
                    Err(err) => {
                        statistics.err(format!("{err:?}"), rt_stats);
                        total += 1;
                        continue 'connection;
                    }
                },
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
                client = build_http_connection_legacy(&opts);
            }
        }
    }

    statistics
}
