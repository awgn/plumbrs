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
    let mut uri = uri_str
        .parse::<hyper::Uri>()
        .unwrap_or_else(|e| fatal!(1, "invalid uri: {e}"));

    let (mut host, mut port) =
        get_conn_address(&opts, &uri).unwrap_or_else(|| fatal!(1, "no host specified in uri"));
    let mut endpoint = build_conn_endpoint(&host, port);

    if opts.sse {
        let (new_uri, _task) = sse_handshake::<B>(endpoint, &uri, &opts, rt_stats).await;
        uri = new_uri;

        if opts.host.is_none() {
            if let Some(h) = uri.host() {
                host = h.to_owned();
            }
            if let Some(p) = uri.port_u16() {
                port = p;
            }
            endpoint = build_conn_endpoint(&host, port);
        }
    }

    let headers = build_headers(&uri, opts.as_ref())
        .unwrap_or_else(|e| fatal!(2, "could not build headers: {e}"));

    let trailers = build_trailers(opts.as_ref())
        .unwrap_or_else(|e| fatal!(2, "could not build trailers: {e}"));

    let bodies: Vec<Full<Bytes>> = opts.bodies().map_or_else(
        |e| fatal!(2, "could not read body: {e}"),
        |v| v.into_iter().map(Full::new).collect(),
    );

    let start = Instant::now();
    'connection: loop {
        if should_stop(total, start, &opts) {
            break 'connection;
        }

        if cid < opts.uri.len() && !banner.contains(uri_str) {
            banner.insert(uri_str.to_owned());
            println!(
                "hyper [{tid:>2}] -> connecting to {}:{}, method = {} uri = {} {}...",
                host,
                port,
                opts.method.as_ref().unwrap_or(&http::Method::GET),
                uri,
                B::SCHEME
            );
        }

        let (mut sender, mut conn_task) =
            match B::build_connection(endpoint, &mut statistics, rt_stats, &opts).await {
                Some(s) => s,
                None => {
                    total += 1;
                    continue 'connection;
                }
            };

        statistics.inc_conn();

        loop {
            let body = bodies
                .get(total as usize)
                .or(bodies.last())
                .cloned()
                .unwrap_or_else(|| Full::new(Bytes::from("")));

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

            let start_lat = opts.latency.then_some(Instant::now());

            match sender.send_request(req).await {
                Ok(res) => match discard_body(res).await {
                    Ok(StatusCode::OK) => statistics.inc_ok(rt_stats),
                    Ok(code) => statistics.set_http_status(code, rt_stats),
                    Err(ref err) => {
                        statistics.set_error(err.as_ref(), rt_stats);
                        total += 1;
                        continue 'connection;
                    }
                },
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
                conn_task.abort();
                continue 'connection;
            } else {
                tokio::select! {
                    res = sender.ready() => {
                        if let Err(ref err) = res {
                            statistics.set_error(err, rt_stats);
                            continue 'connection;
                        }
                    }
                    _ = &mut conn_task => {
                        continue 'connection;
                    }
                }
            }
        }
    }

    statistics
}
