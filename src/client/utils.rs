use crate::Options;
use crate::stats::{RealtimeStats, Statistics};
use http::header::HeaderValue;
use http::{HeaderMap, Request, Response, StatusCode, header};
use http_body_util::BodyExt;
use hyper::body::Body;
use hyper::body::Incoming;
use hyper::client::conn::http1 as conn1;
use hyper::client::conn::http2 as conn2;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo};
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

    if !opts.trailers.is_empty() {
        let trailers = opts
            .trailers
            .iter()
            .map(|(k, _)| k.as_str())
            .collect::<Vec<&str>>()
            .join(", ");

        headers.append("Trailer", http::HeaderValue::from_str(&trailers)?);
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

pub fn build_trailers(
    opts: &Options,
) -> Result<Option<HeaderMap>, http::header::InvalidHeaderValue> {
    if opts.trailers.is_empty() {
        return Ok(None);
    }

    let mut trailers = HeaderMap::with_capacity(opts.trailers.len());

    for (k, v) in &opts.trailers {
        trailers.append(
            http::header::HeaderName::from_str(k)
                .unwrap_or_else(|e| fatal!(3, "invalid trailer name: {e}")),
            HeaderValue::from_str(v).unwrap_or_else(|e| fatal!(3, "invalid trailer value: {e}")),
        );
    }

    Ok(Some(trailers))
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

pub struct Http1;
pub struct Http2;

pub trait RequestSender<B: Body> {
    fn send_request(
        &mut self,
        req: Request<B>,
    ) -> impl Future<Output = hyper::Result<Response<Incoming>>>;
    fn ready(&mut self) -> impl Future<Output = hyper::Result<()>>;
}

impl<B> RequestSender<B> for conn1::SendRequest<B>
where
    B: Body + 'static,
{
    async fn send_request(&mut self, req: Request<B>) -> hyper::Result<Response<Incoming>> {
        self.send_request(req).await
    }

    async fn ready(&mut self) -> hyper::Result<()> {
        self.ready().await
    }
}

impl<B> RequestSender<B> for conn2::SendRequest<B>
where
    B: Body + 'static,
{
    async fn send_request(&mut self, req: Request<B>) -> hyper::Result<Response<Incoming>> {
        self.send_request(req).await
    }

    async fn ready(&mut self) -> hyper::Result<()> {
        self.ready().await
    }
}

pub trait HttpConnectionBuilder {
    type Sender<B>: RequestSender<B>
    where
        B: Body + Send + Unpin + 'static,
        B::Data: Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>;

    const SCHEME: &'static str;

    fn build_connection<B>(
        endpoint: &'static str,
        stats: &mut Statistics,
        rt_stats: &RealtimeStats,
        _opts: &Options,
    ) -> impl Future<Output = Option<(Self::Sender<B>, tokio::task::JoinHandle<()>)>>
    where
        B: Body + Send + Unpin + 'static,
        B::Data: Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>;
}

impl HttpConnectionBuilder for Http1 {
    type Sender<B>
        = conn1::SendRequest<B>
    where
        B: Body + Send + Unpin + 'static,
        B::Data: Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>;

    const SCHEME: &'static str = "HTTP/1.1";

    async fn build_connection<B>(
        endpoint: &'static str,
        stats: &mut Statistics,
        rt_stats: &RealtimeStats,
        opts: &Options,
    ) -> Option<(Self::Sender<B>, tokio::task::JoinHandle<()>)>
    where
        B: Body + Send + Unpin + 'static,
        B::Data: Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let stream_res = TcpStream::connect(endpoint)
            .await
            .and_then(|s| s.set_nodelay(true).map(|_| s));
        let stream = match stream_res {
            Ok(s) => s,
            Err(ref err) => {
                stats.err(format!("{err:?}"), rt_stats);
                return None;
            }
        };
        let stream = TokioIo::new(stream);
        let mut builder = conn1::Builder::new();
        
        // Configure HTTP/1 options
        if let Some(v) = opts.http1_max_buf_size {
            builder.max_buf_size(v);
        }
        if let Some(v) = opts.http1_read_buf_exact_size {
            builder.read_buf_exact_size(Some(v));
        }
        if let Some(v) = opts.http1_writev {
            builder.writev(v);
        }
        if opts.http1_title_case_headers {
            builder.title_case_headers(true);
        }
        if opts.http1_preserve_header_case {
            builder.preserve_header_case(true);
        }
        if let Some(v) = opts.http1_max_headers {
            builder.max_headers(v);
        }
        if opts.http1_allow_spaces_after_header_name_in_responses {
            builder.allow_spaces_after_header_name_in_responses(true);
        }
        if opts.http1_allow_obsolete_multiline_headers_in_responses {
            builder.allow_obsolete_multiline_headers_in_responses(true);
        }
        if opts.http1_ignore_invalid_headers_in_responses {
            builder.ignore_invalid_headers_in_responses(true);
        }
        if opts.http09_responses {
            builder.http09_responses(true);
        }
        
        let conn_res = builder.handshake(stream).await;
        let (sender, connection) = match conn_res {
            Ok(p) => p,
            Err(ref err) => {
                stats.err(format!("{err:?}"), rt_stats);
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
}

impl HttpConnectionBuilder for Http2 {
    type Sender<B>
        = conn2::SendRequest<B>
    where
        B: Body + Send + 'static + Unpin,
        B::Data: Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>;

    const SCHEME: &'static str = "HTTP/2";

    async fn build_connection<B>(
        endpoint: &'static str,
        stats: &mut Statistics,
        rt_stats: &RealtimeStats,
        opts: &Options,
    ) -> Option<(Self::Sender<B>, tokio::task::JoinHandle<()>)>
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
                stats.err(format!("{err:?}"), rt_stats);
                return None;
            }
        };
        let stream = TokioIo::new(stream);
        let mut builder = conn2::Builder::new(TokioExecutor::new());

        // set http2 connection options...
        builder.adaptive_window(opts.http2_adaptive_window.unwrap_or(false));
        builder.initial_max_send_streams(opts.http2_initial_max_send_streams);
        if let Some(v) = opts.http2_max_concurrent_reset_streams {
            builder.max_concurrent_reset_streams(v);
        }
        builder.initial_stream_window_size(opts.http2_initial_stream_window_size);
        builder.initial_connection_window_size(opts.http2_initial_connection_window_size);
        builder.max_frame_size(opts.http2_max_frame_size);
        if let Some(v) = opts.http2_max_header_list_size {
            builder.max_header_list_size(v);
        }
        if let Some(v) = opts.http2_max_send_buffer_size {
            builder.max_send_buf_size(v);
        }
        builder.keep_alive_while_idle(opts.http2_keep_alive_while_idle);

        let conn_res = builder.handshake(stream).await;
        let (sender, connection) = match conn_res {
            Ok(p) => p,
            Err(ref err) => {
                stats.err(format!("{err:?}"), rt_stats);
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
        if let Some(v) = opts.http2_max_concurrent_reset_streams {
            builder.http2_max_concurrent_reset_streams(v);
        }
        builder.http2_initial_stream_window_size(opts.http2_initial_stream_window_size);
        builder.http2_initial_connection_window_size(opts.http2_initial_connection_window_size);
        builder.http2_max_frame_size(opts.http2_max_frame_size);
        if let Some(v) = opts.http2_max_header_list_size {
            builder.http2_max_header_list_size(v);
        }
        if let Some(v) = opts.http2_max_send_buffer_size {
            builder.http2_max_send_buf_size(v);
        }
        builder.http2_keep_alive_while_idle(opts.http2_keep_alive_while_idle);
    } else {
        // Configure HTTP/1 options
        if let Some(v) = opts.http1_max_buf_size {
            builder.http1_max_buf_size(v);
        }
        if let Some(v) = opts.http1_read_buf_exact_size {
            builder.http1_read_buf_exact_size(v);
        }
        if let Some(v) = opts.http1_writev {
            builder.http1_writev(v);
        }
        if opts.http1_title_case_headers {
            builder.http1_title_case_headers(true);
        }
        if opts.http1_preserve_header_case {
            builder.http1_preserve_header_case(true);
        }
        if let Some(v) = opts.http1_max_headers {
            builder.http1_max_headers(v);
        }
        if opts.http1_allow_spaces_after_header_name_in_responses {
            builder.http1_allow_spaces_after_header_name_in_responses(true);
        }
        if opts.http1_allow_obsolete_multiline_headers_in_responses {
            builder.http1_allow_obsolete_multiline_headers_in_responses(true);
        }
        if opts.http1_ignore_invalid_headers_in_responses {
            builder.http1_ignore_invalid_headers_in_responses(true);
        }
        if opts.http09_responses {
            builder.http09_responses(true);
        }
    }
    builder.build_http()
}

