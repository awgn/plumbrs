use crate::Options;
use crate::client::utils::discard_body;
use crate::stats::Statistics;

use crate::fatal;
use bytes::Bytes;
use http::Request;
use http_body_util::Full;
use hyper::StatusCode;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use crate::client::utils::{build_headers, should_stop};

pub async fn http_hyper_1rt(
    tid: usize,
    cid: usize,
    opts: Arc<Options>,
    client: Client<HttpConnector, Full<Bytes>>,
) -> Statistics {
    let mut stats = Statistics {
        ok: 0,
        err: HashMap::new(),
        http_status: HashMap::new(),
        idle: 0.0,
    };
    let mut total: u32 = 0;
    let mut banner = HashSet::new();
    let uri_str = opts.uri[cid % opts.uri.len()].as_str();
    let uri = uri_str
        .parse::<http::Uri>()
        .unwrap_or_else(|e| fatal!(1, "invalid uri: {e}"));
    let host = uri.host().unwrap_or_else(|| fatal!(3, "host not found"));
    let headers = build_headers(Some(host), opts.as_ref())
        .unwrap_or_else(|e| fatal!(2, "could not build headers: {e}"));

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
                "[{tid:>3}] hyper-legacy (1rt) -> connecting to {} {}...",
                uri,
                if opts.http2 { "HTTP/2" } else { "HTTP/1.1" }
            );
        }

        loop {
            let mut req = Request::new(body.clone());
            *req.method_mut() = opts.method.clone();
            *req.uri_mut() = uri.clone();
            *req.headers_mut() = headers.clone();

            match client.request(req).await {
                Ok(res) => match discard_body(res).await {
                    Ok(StatusCode::OK) => stats.ok += 1,
                    Ok(code) => stats.http_status(code),
                    Err(err) => {
                        stats.err(format!("{err:?}"));
                        total += 1;
                        continue 'connection;
                    }
                },
                Err(ref err) => {
                    stats.err(format!("{err:?}"));
                    total += 1;
                    continue 'connection;
                }
            }

            total += 1;
            if should_stop(total, start, &opts) {
                break 'connection;
            }
        }
    }

    stats
}
