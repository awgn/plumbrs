use crate::fatal;
use crate::stats::Statistics;

use crate::Options;

use bytes::Bytes;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use http::Request;
use http::StatusCode;
use tokio::net::TcpStream;

use crate::client::utils::*;

use h2;

pub async fn http_hyper_h2(tid: usize, cid: usize, opts: Arc<Options>) -> Statistics {
    let mut stats = Statistics {
        ok: 0,
        err: HashMap::new(),
        http_status: HashMap::new(),
        idle: 0.0,
    };
    let mut total: u32 = 0;
    let mut banner = HashSet::new();
    let uri_str = opts.uri[cid % opts.uri.len()].as_str();
    let (host, port) = get_host_port(&opts, uri_str);
    let endpoint = build_endpoint(&host, port);
    let uri = uri_str
        .parse::<hyper::Uri>()
        .unwrap_or_else(|e| fatal!(1, "invalid uri: {e}"));

    let body = if let Some(body) = &opts.body {
        body.clone().into()
    } else {
        Bytes::new()
    };

    // http/2 use :authority: instead of Host header...
    let headers = build_headers(None, opts.as_ref())
        .unwrap_or_else(|e| fatal!(2, "could not build headers: {e}"));

    let start = Instant::now();
    'connection: loop {
        if should_stop(total, start, &opts) {
            break 'connection;
        }

        if cid < opts.uri.len() && !banner.contains(uri_str) {
            banner.insert(uri_str.to_owned());
            println!(
                "[{tid:>3}] hyper-h2 -> connecting to {}:{}, uri= {} HTTP/2...",
                host, port, uri
            );
        }

        let stream_res = TcpStream::connect(endpoint)
            .await
            .and_then(|s| s.set_nodelay(true).map(|_| s));
        let mut stream = match stream_res {
            Ok(s) => s,
            Err(ref err) => {
                stats.err(format!("{err:?}"));
                total += 1;
                continue 'connection;
            }
        };

        let conn = h2::client::Builder::new()
            .handshake::<_, bytes::Bytes>(stream)
            .await;
        let (mut h2_client, mut connection) = match conn {
            Ok(h2_conn) => h2_conn,
            Err(ref err) => {
                stats.err(format!("{err:?}"));
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
            *req.method_mut() = opts.method.clone();
            *req.uri_mut() = uri.clone();
            *req.headers_mut() = headers.clone();

            h2_client = match h2_client.ready().await {
                Ok(h2) => h2,
                Err(ref err) => {
                    stats.err(format!("{err:?}"));
                    continue 'connection;
                }
            };

            let (response, mut send_stream) = match h2_client.send_request(req, false) {
                Ok(r) => r,
                Err(ref err) => {
                    stats.err(err.to_string());
                    total += 1;
                    continue 'connection;
                }
            };

            if !body.is_empty() {
                send_stream.send_data(body.clone(), false).unwrap();
            }

            let res = match response.await {
                Ok(res) => res,
                Err(ref err) => {
                    stats.err(format!("{err:?}"));
                    total += 1;
                    continue 'connection;
                }
            };

            match res.status() {
                StatusCode::OK => {
                    stats.ok += 1;
                    let (_head, mut body) = res.into_parts();
                    while let Some(chunk_res) = body.data().await {
                        let chunk_len = match chunk_res {
                            Ok(ref c) => c.len(),
                            Err(ref err) => {
                                stats.err(format!("{err:?}"));
                                total += 1;
                                continue 'connection;
                            }
                        };

                        let _ = body.flow_control().release_capacity(chunk_len);
                    }
                }
                code => stats.http_status(code),
            }

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
                        stats.err(format!("{err:?}"));
                        total += 1;
                        continue 'connection;
                    }
                };
                let conn = h2::client::Builder::new()
                    .handshake::<_, bytes::Bytes>(stream)
                    .await;
                (h2_client, connection) = match conn {
                    Ok(h2_conn) => h2_conn,
                    Err(ref err) => {
                        stats.err(format!("{err:?}"));
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

    stats
}
