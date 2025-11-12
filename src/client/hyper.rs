use crate::Options;
use crate::stats::{RealtimeStats, Statistics};

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use http::{Request, StatusCode};

use crate::client::utils::*;
use crate::fatal;
use http_body_util::{BodyExt, Either, Full};

pub async fn http_hyper(
    tid: usize,
    cid: usize,
    opts: Arc<Options>,
    rt_stats: &RealtimeStats,
) -> Statistics {
    if opts.http2 {
        http_hyper_client::<Http2>(tid, cid, opts, rt_stats).await
    } else {
        http_hyper_client::<Http1>(tid, cid, opts, rt_stats).await
    }
}

async fn http_hyper_client<B: HttpConnectionBuilder>(
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

    let headers = build_headers(uri.host(), opts.as_ref())
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
                "hyper [{tid:>2}] -> connecting to {}:{}, uri = {} HTTP/1.1...",
                host, port, uri
            );
        }

        let (mut sender, mut conn_task) =
            match B::build_connection(endpoint, &mut statistics, &rt_stats, &opts).await {
                Some(s) => s,
                None => {
                    total += 1;
                    continue 'connection;
                }
            };

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

            match sender.send_request(req).await {
                Ok(res) => match discard_body(res).await {
                    Ok(StatusCode::OK) => statistics.ok(&rt_stats),
                    Ok(code) => statistics.http_status(code, &rt_stats),
                    Err(err) => {
                        statistics.err(format!("{err:?}"), &rt_stats);
                        total += 1;
                        continue 'connection;
                    }
                },
                Err(ref err) => {
                    statistics.err(format!("{err:?}"), &rt_stats);
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
                conn_task.abort();
                (sender, conn_task) =
                    match B::build_connection(endpoint, &mut statistics, &rt_stats, &opts).await {
                        Some(s) => s,
                        None => {
                            total += 1;
                            continue 'connection;
                        }
                    };
            } else {
                let res = sender.ready().await;
                if let Err(ref err) = res {
                    statistics.err(format!("{err:?}"), &rt_stats);
                }
            }
        }
    }

    statistics
}
