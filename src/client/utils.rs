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
use std::{path::Path, str::FromStr, time::Instant};
use tokio::net::TcpStream;

use crate::client::tls::{self, MaybeTlsStream};

/// This macro prints a formatted message to stderr and then exits the process
/// with the given exit code.
/// It restores the terminal cursor first, as `std::process::exit` bypasses
/// destructors (including the `HiddenCursor` guard in `main`).
#[macro_export]
macro_rules! fatal {
    ($exit_code:expr, $fmt:literal $(, $($arg:tt)*)?) => {
        {
            eprintln!($fmt $(, $($arg)*)?);
            $crate::restore_cursor();
            std::process::exit($exit_code as i32);
        }
    };
}

#[inline]
pub fn should_stop(total: u32, start: Instant, opts: &Options) -> bool {
    opts.requests.is_some_and(|m| total >= m) || opts.duration.is_some_and(|d| start.elapsed() > d)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    pub domain: String,
    pub include_subdomains: bool,
    pub path: String,
    pub secure: bool,
    pub expires: i64,
    pub name: String,
    pub value: String,
    pub http_only: bool,
}

pub fn parse_cookie_str(content: &str) -> Vec<Cookie> {
    let mut cookies = Vec::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        let (http_only, line_data) = if line.to_ascii_lowercase().starts_with("#httponly_") {
            (true, &line[10..])
        } else if line.starts_with('#') {
            continue;
        } else {
            (false, line)
        };

        let parts: Vec<&str> = if line_data.contains('\t') {
            line_data.split('\t').collect()
        } else {
            line_data.split_whitespace().collect()
        };

        if parts.len() < 6 {
            continue;
        }

        let domain = parts[0].trim();
        let include_subdomains = parts[1].trim().eq_ignore_ascii_case("TRUE");
        let path = parts[2].trim();
        let secure = parts[3].trim().eq_ignore_ascii_case("TRUE");
        let expires = parts[4].trim().parse::<i64>().unwrap_or(0);
        let name = parts[5].trim();
        let value = if parts.len() >= 7 {
            parts[6].trim()
        } else {
            ""
        };

        if name.is_empty() || domain.is_empty() {
            continue;
        }

        cookies.push(Cookie {
            domain: domain.to_string(),
            include_subdomains,
            path: if path.is_empty() {
                "/".to_string()
            } else {
                path.to_string()
            },
            secure,
            expires,
            name: name.to_string(),
            value: value.to_string(),
            http_only,
        });
    }

    cookies
}

pub fn parse_cookie_file(path: impl AsRef<Path>) -> std::io::Result<Vec<Cookie>> {
    let content = std::fs::read_to_string(path)?;
    Ok(parse_cookie_str(&content))
}

pub fn path_matches(cookie_path: &str, req_path: &str) -> bool {
    if cookie_path == req_path {
        return true;
    }
    if let Some(stripped) = req_path.strip_prefix(cookie_path)
        && (cookie_path.ends_with('/') || stripped.starts_with('/'))
    {
        return true;
    }
    false
}

pub fn match_cookies<'a>(
    cookies: &'a [Cookie],
    uri: &http::Uri,
    host_override: Option<&str>,
) -> Vec<&'a Cookie> {
    let host = host_override.or_else(|| uri.host());
    let Some(host) = host else {
        return Vec::new();
    };

    let req_host = if let Some(stripped) = host.strip_prefix('[') {
        if let Some(end) = stripped.find(']') {
            &stripped[..end]
        } else {
            host
        }
    } else if let Some((h, _)) = host.split_once(':') {
        h
    } else {
        host
    };
    let req_host_lower = req_host.to_ascii_lowercase();

    let is_secure = uri
        .scheme_str()
        .map(|s| s.eq_ignore_ascii_case("https"))
        .unwrap_or(false);

    let req_path = uri.path();
    let req_path = if req_path.is_empty() { "/" } else { req_path };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let mut matching: Vec<&'a Cookie> = cookies
        .iter()
        .filter(|cookie| {
            if cookie.expires > 0 && cookie.expires < now {
                return false;
            }

            if cookie.secure && !is_secure {
                return false;
            }

            let cookie_path = if cookie.path.is_empty() {
                "/"
            } else {
                &cookie.path
            };
            if !path_matches(cookie_path, req_path) {
                return false;
            }

            let cookie_domain = cookie.domain.to_ascii_lowercase();
            let clean_cookie_domain = cookie_domain.strip_prefix('.').unwrap_or(&cookie_domain);

            if req_host_lower == clean_cookie_domain {
                return true;
            }

            if cookie.include_subdomains {
                let is_subdomain = req_host_lower.len() > clean_cookie_domain.len()
                    && req_host_lower.ends_with(clean_cookie_domain)
                    && req_host_lower.as_bytes()
                        [req_host_lower.len() - clean_cookie_domain.len() - 1]
                        == b'.';
                if is_subdomain {
                    return true;
                }
            }

            false
        })
        .collect();

    matching.sort_by_key(|a| std::cmp::Reverse(a.path.len()));

    matching
}

/// Format a host (or override) with an optional port from the URI or `--port`,
/// unless the host already includes an explicit `:port` suffix.
#[inline]
pub fn format_host_authority(uri: &http::Uri, opts: &Options, host: &str) -> String {
    let has_explicit_port = if let Some(rest) = host.strip_prefix('[') {
        rest.split_once(']')
            .is_some_and(|(_, after)| after.starts_with(':'))
    } else {
        host.contains(':')
    };

    if has_explicit_port {
        host.to_string()
    } else {
        let port = uri
            .port()
            .map(|p| p.as_str().to_string())
            .or_else(|| opts.port.map(|p| p.to_string()));
        match port {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        }
    }
}

pub fn build_headers(
    uri: &http::Uri,
    opts: &Options,
) -> Result<HeaderMap, http::header::InvalidHeaderValue> {
    let cookies = opts.cookies.as_ref().map(|cookie_path| {
        parse_cookie_file(cookie_path).unwrap_or_else(|e| {
            fatal!(
                2,
                "could not read cookie file '{}': {e}",
                cookie_path.display()
            )
        })
    });
    build_headers_with_cookies(
        uri,
        opts,
        opts.host.first().map(String::as_str),
        cookies.as_deref(),
    )
}

fn build_headers_with_cookies(
    uri: &http::Uri,
    opts: &Options,
    host_override: Option<&str>,
    cookies: Option<&[Cookie]>,
) -> Result<HeaderMap, http::header::InvalidHeaderValue> {
    let mut headers = HeaderMap::new();

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
                .unwrap_or_else(|e| fatal!(2, "invalid header name: {e}")),
            HeaderValue::from_str(v).unwrap_or_else(|e| fatal!(2, "invalid header value: {e}")),
        );
    }

    if let Some(cookies) = cookies {
        let matching = match_cookies(cookies, uri, host_override);
        if !matching.is_empty() {
            let cookie_str = matching
                .iter()
                .map(|c| format!("{}={}", c.name, c.value))
                .collect::<Vec<_>>()
                .join("; ");
            if let Some(existing) = headers.get(header::COOKIE) {
                if let Ok(existing_str) = existing.to_str() {
                    let combined = format!("{existing_str}; {cookie_str}");
                    headers.insert(header::COOKIE, HeaderValue::from_str(&combined)?);
                } else {
                    headers.append(header::COOKIE, HeaderValue::from_str(&cookie_str)?);
                }
            } else {
                headers.insert(header::COOKIE, HeaderValue::from_str(&cookie_str)?);
            }
        }
    }

    if !opts.http2 && !headers.contains_key(header::HOST) {
        let host = host_override.or_else(|| uri.host());
        if let Some(host) = host {
            let formatted = format_host_authority(uri, opts, host);
            headers.append(header::HOST, HeaderValue::from_str(&formatted)?);
        }
    }

    Ok(headers)
}

/// Pre-built request target (`Uri` + `HeaderMap`) for zero-overhead per-request host rotation.
#[derive(Clone, Debug)]
pub struct RequestTarget {
    pub uri: http::Uri,
    pub headers: HeaderMap,
}

/// Build the pre-compiled list of [`RequestTarget`]s for all configured `--host` values
/// (or a single target derived from `uri` when `--host` is not specified).
pub fn build_request_targets(
    uri: &http::Uri,
    opts: &Options,
    absolute: bool,
) -> Result<Vec<RequestTarget>, http::header::InvalidHeaderValue> {
    let cookies = opts.cookies.as_ref().map(|cookie_path| {
        parse_cookie_file(cookie_path).unwrap_or_else(|e| {
            fatal!(
                2,
                "could not read cookie file '{}': {e}",
                cookie_path.display()
            )
        })
    });

    if opts.host.is_empty() {
        let req_uri = request_uri_for_host(uri, opts, absolute, None);
        let headers = build_headers_with_cookies(uri, opts, None, cookies.as_deref())?;
        return Ok(vec![RequestTarget {
            uri: req_uri,
            headers,
        }]);
    }

    let mut targets = Vec::with_capacity(opts.host.len());
    for host in &opts.host {
        let req_uri = request_uri_for_host(uri, opts, absolute, Some(host.as_str()));
        let headers =
            build_headers_with_cookies(uri, opts, Some(host.as_str()), cookies.as_deref())?;
        targets.push(RequestTarget {
            uri: req_uri,
            headers,
        });
    }
    Ok(targets)
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
                .unwrap_or_else(|e| fatal!(2, "invalid trailer name: {e}")),
            HeaderValue::from_str(v).unwrap_or_else(|e| fatal!(2, "invalid trailer value: {e}")),
        );
    }

    Ok(Some(trailers))
}

/// Add an explicit `Content-Length` for the raw io_uring clients
/// (compio, monoio, tokio-uring).
///
/// Those clients serialize the request with `http_wire` and write the bytes
/// as-is, so framing must be explicit: without `Content-Length` the upstream
/// treats the request as bodyless and the body never reaches it (hyper-based
/// clients get framing for free from hyper). User-provided `Content-Length`
/// or `Transfer-Encoding` headers are left untouched.
pub fn ensure_content_length(mut headers: HeaderMap, body_len: usize) -> HeaderMap {
    if body_len > 0
        && !headers.contains_key(header::CONTENT_LENGTH)
        && !headers.contains_key(header::TRANSFER_ENCODING)
    {
        headers.insert(
            header::CONTENT_LENGTH,
            body_len
                .to_string()
                .parse()
                .expect("body length is a valid header value"),
        );
    }
    headers
}

/// URI placed on the HTTP request.
///
/// HTTP/1 origin servers expect origin-form (`/path`). Absolute-form
/// (`http://host/path`) is used when `absolute` is true, and must also be
/// kept for HTTP/2 so `:scheme` / `:authority` can be derived.
#[inline]
pub fn request_uri(uri: &http::Uri, absolute: bool) -> http::Uri {
    if absolute {
        uri.clone()
    } else {
        origin_form(uri)
    }
}

/// Build the request URI, rewriting its authority when `absolute` is true (HTTP/2 or `--absolute-uri`)
/// and a `host_override` is provided.
#[inline]
pub fn request_uri_for_host(
    uri: &http::Uri,
    opts: &Options,
    absolute: bool,
    host_override: Option<&str>,
) -> http::Uri {
    if !absolute {
        return origin_form(uri);
    }
    let Some(host) = host_override else {
        return uri.clone();
    };
    let authority_str = format_host_authority(uri, opts, host);
    let mut parts = uri.clone().into_parts();
    if parts.scheme.is_none() {
        parts.scheme = Some(http::uri::Scheme::HTTP);
    }
    parts.authority = Some(
        http::uri::Authority::from_str(&authority_str)
            .unwrap_or_else(|e| fatal!(2, "invalid host authority '{authority_str}': {e}")),
    );
    if parts.path_and_query.is_none() {
        parts.path_and_query = Some(http::uri::PathAndQuery::from_static("/"));
    }
    http::Uri::from_parts(parts)
        .unwrap_or_else(|e| fatal!(2, "invalid request uri for host '{authority_str}': {e}"))
}

#[inline]
pub fn origin_form(uri: &http::Uri) -> http::Uri {
    match uri.path_and_query() {
        Some(pq) if !pq.as_str().is_empty() => pq
            .as_str()
            .parse()
            .unwrap_or_else(|_| http::Uri::from_static("/")),
        _ => http::Uri::from_static("/"),
    }
}

#[inline]
pub fn get_conn_address(opts: &Options, uri: &hyper::Uri) -> Option<(String, u16)> {
    let default_port = match uri.scheme_str() {
        Some("https") => 443,
        _ if opts.http2 => 443,
        _ => 80,
    };

    let (host, fallback_port) = if let Some(h) = uri.host() {
        (h.to_string(), uri.port_u16())
    } else {
        let first_host = opts.host.first()?;
        if let Ok(auth) = http::uri::Authority::from_str(first_host) {
            (auth.host().to_string(), auth.port_u16())
        } else {
            (first_host.clone(), None)
        }
    };

    let mut port = fallback_port.unwrap_or(default_port);
    if let Some(ref p) = opts.port {
        port = *p;
    }

    Some((host, port))
}

#[inline]
pub fn tls_server_name<'a>(opts: &'a Options, uri: &'a http::Uri) -> Option<&'a str> {
    tls_server_name_at(opts, uri, 0, 0)
}

#[inline]
pub fn tls_server_name_at<'a>(
    opts: &'a Options,
    uri: &'a http::Uri,
    cid: usize,
    generation: u64,
) -> Option<&'a str> {
    match uri.scheme_str() {
        Some("https") => {
            if !opts.sni.is_empty() {
                let idx = (opts.conn_base + cid).wrapping_add(generation as usize) % opts.sni.len();
                Some(opts.sni[idx].as_str())
            } else {
                uri.host()
            }
        }
        _ => None,
    }
}

#[inline]
pub fn build_conn_endpoint(host: &String, port: u16) -> &'static str {
    Box::leak(format!("{}:{}", host, port).into_boxed_str())
}

/// Open a TCP connection, optionally binding a source address first.
///
/// With an empty `locals` the kernel picks the source IP and an ephemeral
/// port (previous behavior). Otherwise each candidate is tried in order until
/// one connects; `AddrInUse`/`AddrNotAvailable` moves on to the next
/// candidate so a busy port does not fail the connection.
async fn connect_tcp(
    endpoint: &str,
    locals: &[std::net::SocketAddr],
) -> std::io::Result<TcpStream> {
    if locals.is_empty() {
        let stream = TcpStream::connect(endpoint).await?;
        stream.set_nodelay(true)?;
        return Ok(stream);
    }

    let mut remotes: Vec<std::net::SocketAddr> = tokio::net::lookup_host(endpoint).await?.collect();
    if remotes.is_empty() {
        return Err(std::io::Error::other(format!(
            "cannot resolve '{endpoint}'"
        )));
    }
    // Prefer a remote matching the requested local family.
    if let Some(local) = locals.first()
        && let Some(pos) = remotes.iter().position(|r| r.is_ipv4() == local.is_ipv4())
    {
        remotes.swap(0, pos);
    }
    let remote = remotes[0];

    let mut last_err = None;
    for local in locals {
        // A wildcard local follows the remote family (e.g. 0.0.0.0 vs [::]).
        let mut bind = *local;
        if bind.ip().is_unspecified() && bind.is_ipv4() != remote.is_ipv4() {
            bind = std::net::SocketAddr::new(
                if remote.is_ipv4() {
                    std::net::IpAddr::from([0, 0, 0, 0])
                } else {
                    std::net::IpAddr::from([0u16; 8])
                },
                bind.port(),
            );
        }
        let socket = match bind {
            std::net::SocketAddr::V4(_) => tokio::net::TcpSocket::new_v4(),
            std::net::SocketAddr::V6(_) => tokio::net::TcpSocket::new_v6(),
        };
        let socket = match socket {
            Ok(s) => s,
            Err(e) => {
                last_err = Some(e);
                continue;
            }
        };
        // Allow quick reuse of ports stuck in TIME_WAIT (matters with --rpc).
        let _ = socket.set_reuseaddr(true);
        if let Err(e) = socket.bind(bind) {
            last_err = Some(e);
            continue;
        }
        match socket.connect(remote).await {
            Ok(stream) => {
                stream.set_nodelay(true)?;
                return Ok(stream);
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::AddrInUse | std::io::ErrorKind::AddrNotAvailable
                ) =>
            {
                last_err = Some(e);
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_err.unwrap_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::AddrInUse, "no usable source port")
    }))
}

pub async fn connect_stream(
    endpoint: &str,
    tls_server_name: Option<&str>,
    http2: bool,
    stats: &mut Statistics,
    rt_stats: &RealtimeStats,
    locals: &[std::net::SocketAddr],
) -> Option<MaybeTlsStream> {
    let tcp = match connect_tcp(endpoint, locals).await {
        Ok(s) => s,
        Err(ref err) => {
            stats.set_error(err, rt_stats);
            return None;
        }
    };

    if let Some(server_name) = tls_server_name {
        match tls::connect(tcp, server_name, http2).await {
            Ok(tls) => Some(MaybeTlsStream::Right(tls)),
            Err(ref err) => {
                stats.set_error(err, rt_stats);
                None
            }
        }
    } else {
        Some(MaybeTlsStream::Left(tcp))
    }
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
        tls_server_name: Option<&str>,
        stats: &mut Statistics,
        rt_stats: &RealtimeStats,
        _opts: &Options,
        locals: &[std::net::SocketAddr],
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
        tls_server_name: Option<&str>,
        stats: &mut Statistics,
        rt_stats: &RealtimeStats,
        opts: &Options,
        locals: &[std::net::SocketAddr],
    ) -> Option<(Self::Sender<B>, tokio::task::JoinHandle<()>)>
    where
        B: Body + Send + Unpin + 'static,
        B::Data: Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let stream =
            connect_stream(endpoint, tls_server_name, false, stats, rt_stats, locals).await?;
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
                stats.set_error(err, rt_stats);
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
        tls_server_name: Option<&str>,
        stats: &mut Statistics,
        rt_stats: &RealtimeStats,
        opts: &Options,
        locals: &[std::net::SocketAddr],
    ) -> Option<(Self::Sender<B>, tokio::task::JoinHandle<()>)>
    where
        B: Body + Send + 'static + Unpin,
        B::Data: Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let stream =
            connect_stream(endpoint, tls_server_name, true, stats, rt_stats, locals).await?;
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
                stats.set_error(err, rt_stats);
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
    // Note: HttpConnector can only pin the source *IP*, not the source port,
    // so --local-port-range/--rss-* are rejected for these clients in check_options.
    match opts.local_addr {
        Some(ip) => {
            let mut connector = HttpConnector::new();
            connector.set_local_address(Some(ip));
            builder.build(connector)
        }
        None => builder.build_http(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn origin_form_strips_scheme_and_authority() {
        let uri: http::Uri = "http://myhost/home".parse().unwrap();
        assert_eq!(origin_form(&uri).to_string(), "/home");
    }

    #[test]
    fn origin_form_keeps_query() {
        let uri: http::Uri = "https://myhost:8080/home?x=1".parse().unwrap();
        assert_eq!(origin_form(&uri).to_string(), "/home?x=1");
    }

    #[test]
    fn origin_form_empty_path() {
        let uri: http::Uri = "http://myhost".parse().unwrap();
        assert_eq!(origin_form(&uri).to_string(), "/");
    }

    #[test]
    fn request_uri_absolute_unchanged() {
        let uri: http::Uri = "http://myhost/home".parse().unwrap();
        assert_eq!(request_uri(&uri, true), uri);
    }

    #[test]
    fn request_uri_relative_is_origin_form() {
        let uri: http::Uri = "http://myhost/home".parse().unwrap();
        assert_eq!(request_uri(&uri, false).to_string(), "/home");
    }

    #[test]
    fn build_headers_http1_with_hostname() {
        let opts = Options::parse_from(["plumbrs", "http://example.com/api"]);
        let uri: http::Uri = "http://example.com/api".parse().unwrap();
        let headers = build_headers(&uri, &opts).unwrap();
        assert_eq!(headers.get(header::HOST).unwrap(), "example.com");
    }

    #[test]
    fn build_headers_http1_with_ip() {
        let opts = Options::parse_from(["plumbrs", "http://192.168.1.1:8080/api"]);
        let uri: http::Uri = "http://192.168.1.1:8080/api".parse().unwrap();
        let headers = build_headers(&uri, &opts).unwrap();
        assert_eq!(headers.get(header::HOST).unwrap(), "192.168.1.1:8080");
    }

    #[test]
    fn build_headers_http1_custom_host_header_preserved() {
        let opts = Options::parse_from([
            "plumbrs",
            "-H",
            "Host:custom.override.com",
            "http://192.168.1.1:8080/api",
        ]);
        let uri: http::Uri = "http://192.168.1.1:8080/api".parse().unwrap();
        let headers = build_headers(&uri, &opts).unwrap();
        let host_headers: Vec<_> = headers.get_all(header::HOST).iter().collect();
        assert_eq!(host_headers.len(), 1);
        assert_eq!(host_headers[0], "custom.override.com");
    }

    #[test]
    fn build_headers_http2_omits_host_header() {
        let opts = Options::parse_from(["plumbrs", "--http2", "http://example.com/api"]);
        let uri: http::Uri = "http://example.com/api".parse().unwrap();
        let headers = build_headers(&uri, &opts).unwrap();
        assert!(headers.get(header::HOST).is_none());
    }

    #[test]
    fn build_headers_http1_with_opts_host() {
        let opts = Options::parse_from([
            "plumbrs",
            "--host",
            "virtual.host.com",
            "http://192.168.1.1:8080/api",
        ]);
        let uri: http::Uri = "http://192.168.1.1:8080/api".parse().unwrap();
        let headers = build_headers(&uri, &opts).unwrap();
        assert_eq!(headers.get(header::HOST).unwrap(), "virtual.host.com:8080");

        // And verify get_conn_address still connects to the endpoint IP, not the virtual host!
        let (host, port) = get_conn_address(&opts, &uri).unwrap();
        assert_eq!(host, "192.168.1.1");
        assert_eq!(port, 8080);
    }

    #[test]
    fn get_conn_address_fallback_to_opts_host() {
        let opts = Options::parse_from(["plumbrs", "--host", "10.0.0.1", "/api"]);
        let uri: hyper::Uri = "/api".parse().unwrap();
        let (host, port) = get_conn_address(&opts, &uri).unwrap();
        assert_eq!(host, "10.0.0.1");
        assert_eq!(port, 80);

        let opts_with_port = Options::parse_from(["plumbrs", "--host", "10.0.0.1:9090", "/api"]);
        let (host, port) = get_conn_address(&opts_with_port, &uri).unwrap();
        assert_eq!(host, "10.0.0.1");
        assert_eq!(port, 9090);
    }

    #[test]
    fn parse_netscape_cookies() {
        let cookie_file_content = "# Netscape HTTP Cookie File\n# https://curl.se/docs/http-cookies.html\n# This file was generated by libcurl! Edit at your own risk.\n\n#HttpOnly_192.168.100.1\tFALSE\t/\tFALSE\t1791996223\tRefreshToken\tmock_refresh_token_e95ee2a2dd996de43f5274f9810e48576f321e03\n#HttpOnly_192.168.100.1\tFALSE\t/\tFALSE\t1791395023\tOauthHMAC\tpfNHqMcbIXg1E3EYm3N+6zyLku4zU6F9I/INH4BMMpg=\n#HttpOnly_192.168.100.1\tFALSE\t/\tFALSE\t1791395023\tOauthExpires\t1791395023\n#HttpOnly_192.168.100.1\tFALSE\t/\tFALSE\t1791395023\tBearerToken\tmock_bearer_token_eb761c2f7d857ef06e9599cfb7d5dc3da8b78ccf\n";
        let cookies = parse_cookie_str(cookie_file_content);
        assert_eq!(cookies.len(), 4);

        assert_eq!(cookies[0].domain, "192.168.100.1");
        assert!(!cookies[0].include_subdomains);
        assert_eq!(cookies[0].path, "/");
        assert!(!cookies[0].secure);
        assert_eq!(cookies[0].expires, 1791996223);
        assert_eq!(cookies[0].name, "RefreshToken");
        assert_eq!(
            cookies[0].value,
            "mock_refresh_token_e95ee2a2dd996de43f5274f9810e48576f321e03"
        );
        assert!(cookies[0].http_only);

        assert_eq!(cookies[3].name, "BearerToken");
        assert_eq!(
            cookies[3].value,
            "mock_bearer_token_eb761c2f7d857ef06e9599cfb7d5dc3da8b78ccf"
        );
    }

    #[test]
    fn parse_space_separated_cookies() {
        let cookie_file_content = "# Netscape HTTP Cookie File\n#HttpOnly_192.168.100.1 FALSE   /       FALSE   1791996223      RefreshToken    mock_token_123\n";
        let cookies = parse_cookie_str(cookie_file_content);
        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].domain, "192.168.100.1");
        assert_eq!(cookies[0].name, "RefreshToken");
        assert_eq!(cookies[0].value, "mock_token_123");
    }

    #[test]
    fn build_headers_with_cookies_file() {
        let temp_dir = std::env::temp_dir();
        let cookie_file = temp_dir.join(format!("plumbrs_test_cookies_{}.txt", std::process::id()));
        let content = "# Netscape HTTP Cookie File\n#HttpOnly_192.168.100.1\tFALSE\t/\tFALSE\t2000000000\tRefreshToken\tmock_refresh\n#HttpOnly_192.168.100.1\tFALSE\t/\tFALSE\t2000000000\tBearerToken\tmock_bearer\n";
        std::fs::write(&cookie_file, content).unwrap();

        let opts = Options::parse_from([
            "plumbrs",
            "--cookies",
            cookie_file.to_str().unwrap(),
            "http://192.168.100.1/api",
        ]);
        let uri: http::Uri = "http://192.168.100.1/api".parse().unwrap();
        let headers = build_headers(&uri, &opts).unwrap();
        let cookie_val = headers.get(header::COOKIE).unwrap().to_str().unwrap();
        assert_eq!(
            cookie_val,
            "RefreshToken=mock_refresh; BearerToken=mock_bearer"
        );

        let _ = std::fs::remove_file(cookie_file);
    }

    #[test]
    fn match_cookies_domain_rules() {
        let cookies = vec![
            Cookie {
                domain: ".example.com".to_string(),
                include_subdomains: true,
                path: "/".to_string(),
                secure: false,
                expires: 0,
                name: "c1".to_string(),
                value: "v1".to_string(),
                http_only: false,
            },
            Cookie {
                domain: "example.com".to_string(),
                include_subdomains: false,
                path: "/".to_string(),
                secure: false,
                expires: 0,
                name: "c2".to_string(),
                value: "v2".to_string(),
                http_only: false,
            },
        ];

        let uri_sub: http::Uri = "http://sub.example.com/test".parse().unwrap();
        let m_sub = match_cookies(&cookies, &uri_sub, None);
        assert_eq!(m_sub.len(), 1);
        assert_eq!(m_sub[0].name, "c1");

        let uri_exact: http::Uri = "http://example.com/test".parse().unwrap();
        let m_exact = match_cookies(&cookies, &uri_exact, None);
        assert_eq!(m_exact.len(), 2);

        let uri_other: http::Uri = "http://other.com/test".parse().unwrap();
        let m_other = match_cookies(&cookies, &uri_other, None);
        assert_eq!(m_other.len(), 0);
    }

    #[test]
    fn match_cookies_path_and_secure() {
        let cookies = vec![
            Cookie {
                domain: "example.com".to_string(),
                include_subdomains: false,
                path: "/api".to_string(),
                secure: false,
                expires: 0,
                name: "p1".to_string(),
                value: "v1".to_string(),
                http_only: false,
            },
            Cookie {
                domain: "example.com".to_string(),
                include_subdomains: false,
                path: "/".to_string(),
                secure: true,
                expires: 0,
                name: "sec".to_string(),
                value: "v2".to_string(),
                http_only: false,
            },
        ];

        let uri_http: http::Uri = "http://example.com/api/v1".parse().unwrap();
        let m_http = match_cookies(&cookies, &uri_http, None);
        assert_eq!(m_http.len(), 1);
        assert_eq!(m_http[0].name, "p1");

        let uri_https: http::Uri = "https://example.com/api/v1".parse().unwrap();
        let m_https = match_cookies(&cookies, &uri_https, None);
        assert_eq!(m_https.len(), 2);
        assert_eq!(m_https[0].name, "p1");
        assert_eq!(m_https[1].name, "sec");

        let uri_nomatch: http::Uri = "http://example.com/apicall".parse().unwrap();
        let m_nomatch = match_cookies(&cookies, &uri_nomatch, None);
        assert_eq!(m_nomatch.len(), 0);
    }

    #[test]
    fn match_cookies_expiration() {
        let cookies = vec![
            Cookie {
                domain: "example.com".to_string(),
                include_subdomains: false,
                path: "/".to_string(),
                secure: false,
                expires: 1000,
                name: "expired".to_string(),
                value: "val".to_string(),
                http_only: false,
            },
            Cookie {
                domain: "example.com".to_string(),
                include_subdomains: false,
                path: "/".to_string(),
                secure: false,
                expires: 0,
                name: "session".to_string(),
                value: "val".to_string(),
                http_only: false,
            },
        ];

        let uri: http::Uri = "http://example.com/".parse().unwrap();
        let matched = match_cookies(&cookies, &uri, None);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].name, "session");
    }

    #[test]
    fn expand_range_pattern_single_and_cartesian() {
        let single = crate::options::expand_range_pattern("host-[1-3].example.com").unwrap();
        assert_eq!(
            single,
            vec![
                "host-1.example.com",
                "host-2.example.com",
                "host-3.example.com"
            ]
        );

        let padded = crate::options::expand_range_pattern("vh-[008-010].local").unwrap();
        assert_eq!(padded, vec!["vh-008.local", "vh-009.local", "vh-010.local"]);

        let cartesian =
            crate::options::expand_range_pattern("svc-[1-2].tier-[10-11].local").unwrap();
        assert_eq!(
            cartesian,
            vec![
                "svc-1.tier-10.local",
                "svc-1.tier-11.local",
                "svc-2.tier-10.local",
                "svc-2.tier-11.local",
            ]
        );
    }

    #[test]
    fn expand_range_pattern_preserves_ipv6_and_rejects_inverted() {
        let ipv6 = crate::options::expand_range_pattern("[::1]:8080").unwrap();
        assert_eq!(ipv6, vec!["[::1]:8080"]);

        let ipv6_with_range =
            crate::options::expand_range_pattern("[2001:db8::1]:808[0-1]").unwrap();
        assert_eq!(
            ipv6_with_range,
            vec!["[2001:db8::1]:8080", "[2001:db8::1]:8081"]
        );

        assert!(crate::options::expand_range_pattern("host-[5-2].com").is_err());
    }

    #[test]
    fn format_host_authority_handles_ipv6_without_port() {
        let opts = Options::parse_from(["plumbrs", "http://127.0.0.1:8080/api"]);
        let uri: http::Uri = "http://127.0.0.1:8080/api".parse().unwrap();
        assert_eq!(
            format_host_authority(&uri, &opts, "[2001:db8::1]"),
            "[2001:db8::1]:8080"
        );
        assert_eq!(
            format_host_authority(&uri, &opts, "[2001:db8::1]:9090"),
            "[2001:db8::1]:9090"
        );
    }

    #[test]
    fn build_request_targets_http1_and_http2_authority() {
        let mut opts = Options::parse_from([
            "plumbrs",
            "--host",
            "vh-[1-2].example.com",
            "--host",
            "custom.org:9090",
            "http://127.0.0.1:8080/api/v1?q=1",
        ]);
        opts.host = crate::options::expand_range_patterns(&opts.host).unwrap();

        let uri: http::Uri = "http://127.0.0.1:8080/api/v1?q=1".parse().unwrap();

        // HTTP/1 origin-form: URI is `/api/v1?q=1`, Host header rotates
        let h1_targets = build_request_targets(&uri, &opts, false).unwrap();
        assert_eq!(h1_targets.len(), 3);
        assert_eq!(h1_targets[0].uri.to_string(), "/api/v1?q=1");
        assert_eq!(
            h1_targets[0].headers.get(header::HOST).unwrap(),
            "vh-1.example.com:8080"
        );
        assert_eq!(
            h1_targets[1].headers.get(header::HOST).unwrap(),
            "vh-2.example.com:8080"
        );
        assert_eq!(
            h1_targets[2].headers.get(header::HOST).unwrap(),
            "custom.org:9090"
        );

        // HTTP/2: Host header omitted, `:authority` in target.uri rotates!
        opts.http2 = true;
        let h2_targets = build_request_targets(&uri, &opts, true).unwrap();
        assert_eq!(h2_targets.len(), 3);
        assert!(h2_targets[0].headers.get(header::HOST).is_none());
        assert_eq!(
            h2_targets[0].uri.to_string(),
            "http://vh-1.example.com:8080/api/v1?q=1"
        );
        assert_eq!(
            h2_targets[1].uri.to_string(),
            "http://vh-2.example.com:8080/api/v1?q=1"
        );
        assert_eq!(
            h2_targets[2].uri.to_string(),
            "http://custom.org:9090/api/v1?q=1"
        );
    }

    #[test]
    fn tls_server_name_rotates_across_connections_and_generations() {
        let mut opts = Options::parse_from([
            "plumbrs",
            "--sni",
            "sni-[1-3].example.com",
            "https://127.0.0.1:8443/api",
        ]);
        opts.sni = crate::options::expand_range_patterns(&opts.sni).unwrap();
        let uri: http::Uri = "https://127.0.0.1:8443/api".parse().unwrap();

        assert_eq!(
            tls_server_name_at(&opts, &uri, 0, 0),
            Some("sni-1.example.com")
        );
        assert_eq!(
            tls_server_name_at(&opts, &uri, 1, 0),
            Some("sni-2.example.com")
        );
        assert_eq!(
            tls_server_name_at(&opts, &uri, 2, 0),
            Some("sni-3.example.com")
        );
        assert_eq!(
            tls_server_name_at(&opts, &uri, 0, 1),
            Some("sni-2.example.com")
        );
    }
}
