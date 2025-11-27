use crate::Options;
use crate::client::utils::build_trailers;
use crate::client::utils::discard_body;
use crate::stats::Statistics;

use crate::fatal;
use crate::stats::RealtimeStats;
use bytes::Bytes;
use http::Request;
use http_body_util::BodyExt;
use http_body_util::Either;
use http_body_util::Full;
use hyper::StatusCode;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use std::collections::HashSet;
use std::future::Ready;
use std::sync::Arc;
use std::time::Instant;

use crate::client::utils::{build_headers, should_stop};

type WithTrailersBody = http_body_util::combinators::WithTrailers<
    Full<Bytes>,
    Ready<Option<Result<http::HeaderMap, std::convert::Infallible>>>,
>;

pub type RequestBody = Either<Full<Bytes>, WithTrailersBody>;

pub async fn http_hyper_rt1(
    tid: usize,
    cid: usize,
    opts: Arc<Options>,
    client: Client<HttpConnector, RequestBody>,
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

    let body_bytes : Bytes = opts.full_body().unwrap_or_else(|e| fatal!(2, "could not read body: {e}"));

    let start = Instant::now();
    'connection: loop {
        if should_stop(total, start, &opts) {
            break 'connection;
        }

        if cid < opts.uri.len() && !banner.contains(uri_str) {
            banner.insert(uri_str.to_owned());
            println!(
                "hyper-rt1 [{tid:>2}] -> connecting. {} {} {}...",
                opts.method.as_ref().unwrap_or(&http::Method::GET),
                uri,
                if opts.http2 { "HTTP/2" } else { "HTTP/1.1" }
            );
        }

        loop {
            let body: RequestBody = if let Some(tr) = trailers.clone() {
                Either::Right(
                    Full::new(body_bytes.clone()).with_trailers(std::future::ready(Some(Ok(tr)))),
                )
            } else {
                Either::Left(Full::new(body_bytes.clone()))
            };
            let mut req = Request::builder()
                .method(opts.method.clone().unwrap_or(http::Method::GET))
                .uri(uri.clone())
                .body(body)
                .unwrap();
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
        }
    }

    statistics
}
