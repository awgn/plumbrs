use crate::stats::Statistics;
use crate::{Options, fatal};

use bytes::Bytes;
use http::Request;
use http_body_util::Full;
use hyper::StatusCode;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use crate::client::utils::{
    build_headers, build_http_connection_legacy, discard_body, should_stop,
};

pub async fn http_hyper_legacy(tid: usize, cid: usize, opts: Arc<Options>) -> Statistics {
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
                "[{tid:>3}] hyper-legacy -> connecting to {} {}...",
                uri,
                if opts.http2 { "HTTP/2" } else { "HTTP/1.1" }
            );
        }

        let mut client = build_http_connection_legacy(&opts);

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

            if opts.cps {
                client = build_http_connection_legacy(&opts);
            }
        }
    }

    stats
}
