use crate::Options;
use crate::stats::Statistics;
use bytes::Bytes;
use http::header::HeaderValue;
use http::{HeaderMap, StatusCode, header};
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::body::Body;
use hyper::body::Incoming;
use hyper::client::conn::http1 as conn1;
use hyper::client::conn::http2 as conn2;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo};
use std::pin::Pin;
use std::{str::FromStr, time::Instant};
use tokio::net::TcpStream;

/// This macro prints a formatted message to stderr and then exits the process
/// with the given exit code.
#[macro_export]
macro_rules! fatal {
    ($exit_code:expr, $fmt:literal $(, $($arg:tt)*)?) => {
        {
            eprintln!($fmt $(, $($arg)*)?);
            std::process::exit($exit_code as i32);
        }
    };
}

#[inline]
pub fn parse_host_port(uri: &str) -> Result<(String, u16), <hyper::Uri as FromStr>::Err> {
    let uri = uri.parse::<hyper::Uri>()?;
    let host = String::from(uri.host().expect("no host in uri"));
    let port = uri.port_u16().unwrap_or(80);
    Ok((host, port))
}

#[inline]
pub fn should_stop(total: u32, start: Instant, opts: &Options) -> bool {
    opts.requests.is_some_and(|m| total >= m) || opts.duration.is_some_and(|d| start.elapsed() > d)
}

pub fn build_headers(
    host: Option<&str>,
    opts: &Options,
) -> Result<HeaderMap, http::header::InvalidHeaderValue> {
    let mut headers = HeaderMap::new();

    if opts.cps {
        headers.append(header::CONNECTION, HeaderValue::from_str("close")?);
    }

    for (k, v) in &opts.headers {
        headers.append(
            http::header::HeaderName::from_str(k)
                .unwrap_or_else(|e| fatal!(3, "invalid header name: {e}")),
            HeaderValue::from_str(v).unwrap_or_else(|e| fatal!(3, "invalid header value: {e}")),
        );
    }

    if let Some(h) = host
        && !opts.http2
    {
        headers.append(header::HOST, HeaderValue::from_str(h)?);
    }

    Ok(headers)
}

#[inline]
pub fn get_host_port(opts: &Options, uri: &str) -> (String, u16) {
    match &opts.host {
        None => parse_host_port(uri).unwrap_or_else(|e| fatal!(3, "parse host:port: {e}")),
        Some(hp) => parse_host_port(hp).unwrap_or_else(|e| fatal!(3, "parse host:port: {e}")),
    }
}

#[inline]
pub fn build_endpoint(host: &String, port: u16) -> &'static str {
    Box::leak(format!("{}:{}", host, port).into_boxed_str())
}

#[inline]
pub async fn discard_body(
    res: http::Response<Incoming>,
) -> Result<StatusCode, Box<dyn std::error::Error + Send + Sync>> {
    let status_code = res.status();
    let mut body = res.into_body();
    while let Some(frame) = body.frame().await {
        frame?;
    }
    Ok(status_code)
}

pub async fn build_http1_connection<B>(
    endpoint: &'static str,
    stats: &mut Statistics,
    _opts: &Options,
) -> Option<(conn1::SendRequest<B>, tokio::task::JoinHandle<()>)>
where
    B: Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let stream_res = TcpStream::connect(endpoint)
        .await
        .and_then(|s| s.set_nodelay(true).map(|_| s));
    let stream = match stream_res {
        Ok(s) => s,
        Err(ref err) => {
            stats.err(format!("{err:?}"));
            return None;
        }
    };
    let stream = TokioIo::new(stream);
    let builder = conn1::Builder::new();
    let conn_res = builder.handshake(stream).await;
    let (sender, connection) = match conn_res {
        Ok(p) => p,
        Err(ref err) => {
            stats.err(format!("{err:?}"));
            return None;
        }
    };
    let conn = tokio::task::spawn(async move {
        if let Err(err) = connection.await {
            eprintln!("Error in connection: {}", err)
        }
    });

    Some((sender, conn))
}

pub async fn build_http2_connection<B>(
    endpoint: &'static str,
    stats: &mut Statistics,
    opts: &Options,
) -> Option<(conn2::SendRequest<B>, tokio::task::JoinHandle<()>)>
where
    B: Body + Send + 'static + Unpin,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let stream_res = TcpStream::connect(endpoint)
        .await
        .and_then(|s| s.set_nodelay(true).map(|_| s));
    let stream = match stream_res {
        Ok(s) => s,
        Err(ref err) => {
            stats.err(format!("{err:?}"));
            return None;
        }
    };
    let stream = TokioIo::new(stream);
    let mut builder = conn2::Builder::new(TokioExecutor::new());

    // set http2 connection options...
    builder.adaptive_window(opts.http2_adaptive_window.unwrap_or(false));
    builder.initial_max_send_streams(opts.http2_initial_max_send_streams);
    if let Some(v) = opts.http2_max_concurrent_streams {
        builder.max_concurrent_streams(v);
    }
    if let Some(v) = opts.http2_max_concurrent_reset_streams {
        builder.max_concurrent_reset_streams(v);
    }

    let conn_res = builder.handshake(stream).await;
    let (sender, connection) = match conn_res {
        Ok(p) => p,
        Err(ref err) => {
            stats.err(format!("{err:?}"));
            return None;
        }
    };
    let conn = tokio::task::spawn(async move {
        if let Err(err) = connection.await {
            eprintln!("Error in connection: {}", err)
        }
    });

    Some((sender, conn))
}

pub fn build_http_connection_legacy<B>(opts: &Options) -> Client<HttpConnector, B>
    where
    B: Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let mut builder = Client::builder(TokioExecutor::new());
    if opts.http2 {
        builder.http2_only(opts.http2);
        builder.http2_adaptive_window(opts.http2_adaptive_window.unwrap_or(false));
        builder.http2_initial_max_send_streams(opts.http2_initial_max_send_streams);
        if let Some(v) = opts.http2_max_concurrent_streams {
            builder.http2_max_concurrent_streams(v);
        }
        if let Some(v) = opts.http2_max_concurrent_reset_streams {
            builder.http2_max_concurrent_reset_streams(v);
        }
        #[cfg(feature = "orion_client")]
        builder.http2_connection_sharing(opts.http2_can_share);
    }
    builder.build_http()
}
