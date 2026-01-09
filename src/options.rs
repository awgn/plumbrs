use std::{convert::Infallible, time::Duration};

use crate::client::ClientType;
use anyhow::Result;
use bytes::Bytes;
use clap::Parser;
use futures_util::StreamExt;
use http::{Method, method::InvalidMethod};
use tokio_util::either::Either;

#[derive(Parser, Debug, Clone)]
#[command(version)]
pub struct Options {
    #[arg(
        help = "Number of total threads",
        short = 't',
        long = "threads",
        default_value_t = 1
    )]
    pub threads: usize,
    #[arg(
        help = "Number of threads per Tokio runtime (not specified means single threaded)",
        short = 'm',
        long = "multi-threaded"
    )]
    pub multithreaded: Option<usize>,
    #[arg(
        help = "Verbose mode",
        short = 'v',
        long = "verbose",
        default_value_t = false
    )]
    pub verbose: bool,
    #[arg(
        help = "Display runtime metrics at the end",
        long = "metrics",
        default_value_t = false
    )]
    pub metrics: bool,
    #[arg(
        help = "Concurrent number of connections or HTTP2 streams",
        short = 'c',
        long = "concurrency",
        default_value_t = 1
    )]
    pub connections: usize,
    #[arg(help = "Duration of test (sec)", short='d', long = "duration", value_parser = parse_secs)]
    pub duration: Option<Duration>,
    #[arg(
        help = "Max number of requests per worker",
        short = 'r',
        long = "requests"
    )]
    pub requests: Option<u32>,
    #[arg(help = "Http Client", short = 'C', long = "client", default_value_t = ClientType::Auto)]
    pub client_type: ClientType,
    #[arg(
        help = "Tokio global queue interval (ticks)",
        long = "global-queue-interval"
    )]
    pub global_queue_interval: Option<u32>,
    #[arg(help = "Tokio event interval (ticks)", long = "event-interval")]
    pub event_interval: Option<u32>,
    #[arg(
        help = "Tokio max. io events per ticks",
        long = "max-io-events-per-tick"
    )]
    pub max_io_events_per_tick: Option<usize>,
    #[cfg(tokio_unstable)]
    #[arg(help = "Disable Tokio lifo slot heuristic", long = "disable-lifo-slot")]
    pub disable_lifo_slot: bool,
    #[arg(help = "HTTP method", short = 'M', long = "method", value_parser = parse_http_method)]
    pub method: Option<Method>,
    #[arg(help = "HTTP headers", short = 'H', long = "header", value_parser = parse_key_val)]
    pub headers: Vec<(String, String)>,
    #[arg(help = "HTTP trailers", short = 'T', long = "trailer", value_parser = parse_key_val)]
    pub trailers: Vec<(String, String)>,
    #[arg(
        help = "Body of the request; can be specified multiple times. @path read body from file",
        short = 'b',
        long = "body"
    )]
    pub body: Vec<String>,
    #[arg(
        help = "Open a new connection for every request, computing Connections Per Second",
        long = "cps",
        default_value_t = false
    )]
    pub cps: bool,
    #[arg(help = "Use http2 only", long = "http2")]
    pub http2: bool,
    #[arg(
        help = "Enable latency estimation (Gil Tene's algorithm)",
        long = "latency"
    )]
    pub latency: bool,
    #[arg(help = "Sets whether to use an adaptive flow control.", long)]
    pub http2_adaptive_window: Option<bool>,
    #[arg(
        help = "Sets the initial maximum of locally initiated (send) streams.",
        long
    )]
    pub http2_initial_max_send_streams: Option<usize>,
    #[arg(help = "Sets the initial maximum of concurrently reset streams.", long)]
    pub http2_max_concurrent_reset_streams: Option<usize>,
    #[arg(
        help = "Sets the initial window size for HTTP/2 stream-level flow control.",
        long
    )]
    pub http2_initial_stream_window_size: Option<u32>,
    #[arg(
        help = "Sets the initial window size for HTTP/2 connection-level flow control.",
        long
    )]
    pub http2_initial_connection_window_size: Option<u32>,
    #[arg(help = "Sets the maximum frame size for HTTP/2.", long)]
    pub http2_max_frame_size: Option<u32>,
    #[arg(help = "Sets the maximum header list size for HTTP/2.", long)]
    pub http2_max_header_list_size: Option<u32>,
    #[arg(help = "Sets the maximum send buffer size for HTTP/2.", long)]
    pub http2_max_send_buffer_size: Option<usize>,
    #[arg(help = "Enables HTTP/2 keep-alive while idle.", long)]
    pub http2_keep_alive_while_idle: bool,
    #[arg(help = "Sets the maximum buffer size for HTTP/1.", long)]
    pub http1_max_buf_size: Option<usize>,
    #[arg(
        help = "Sets the exact size of the read buffer to always use for HTTP/1.",
        long
    )]
    pub http1_read_buf_exact_size: Option<usize>,
    #[arg(
        help = "Set whether HTTP/1 connections should try to use vectored writes.",
        long
    )]
    pub http1_writev: Option<bool>,
    #[arg(
        help = "Set whether HTTP/1 connections will write header names as title case.",
        long
    )]
    pub http1_title_case_headers: bool,
    #[arg(
        help = "Set whether to support preserving original header cases for HTTP/1.",
        long
    )]
    pub http1_preserve_header_case: bool,
    #[arg(help = "Set the maximum number of headers for HTTP/1.", long)]
    pub http1_max_headers: Option<usize>,
    #[arg(
        help = "Set whether HTTP/1 connections will accept spaces after header name in responses.",
        long
    )]
    pub http1_allow_spaces_after_header_name_in_responses: bool,
    #[arg(
        help = "Set whether HTTP/1 connections will accept obsolete line folding for header values.",
        long
    )]
    pub http1_allow_obsolete_multiline_headers_in_responses: bool,
    #[arg(
        help = "Set whether HTTP/1 connections will silently ignore malformed header lines.",
        long
    )]
    pub http1_ignore_invalid_headers_in_responses: bool,
    #[arg(help = "Set whether HTTP/0.9 responses should be tolerated.", long)]
    pub http09_responses: bool,
    #[arg(
        help = "Enable Server-Sent Events (SSE) handshake.",
        long = "sse",
        default_value_t = false
    )]
    pub sse: bool,

    #[cfg(all(target_os = "linux", feature = "io_uring"))]
    #[arg(
        long,
        help = "Size of the io_uring Submission Queue (SQ)",
        default_value_t = 4096
    )]
    pub uring_entries: u32,

    #[cfg(all(target_os = "linux", feature = "io_uring"))]
    #[arg(
        long,
        help = "Enable kernel-side submission polling with idle timeout in milliseconds."
    )]
    pub uring_sqpoll: Option<u32>,

    #[arg(help = "Set the host to connect to (e.g. 192.168.0.1)", long = "host")]
    pub host: Option<String>,
    #[arg(help = "Set the port to connect to (e.g. 8080)", long = "port")]
    pub port: Option<u16>,

    #[arg(help = "HTTP URIs used in the request (e.g. http://192.168.0.1:80)")]
    pub uri: Vec<String>,
}

impl Options {
    pub fn bodies(&self) -> Result<Vec<Bytes>> {
        let mut bodies = Vec::new();
        for body in self.body.iter() {
            bodies.push(load_body_content(body)?);
        }
        Ok(bodies)
    }

    pub async fn stream_bodies(
        &self,
    ) -> Result<impl futures_util::Stream<Item = Result<Bytes, Infallible>> + Send + use<>> {
        // Constraint: @file cannot be mixed with other body chunks
        let has_file = self.body.iter().any(|b| b.starts_with('@'));
        if has_file && self.body.len() > 1 {
            anyhow::bail!(
                "file body (@path) cannot be mixed with other body chunks; use either a single @file or multiple in-memory chunks"
            );
        }

        // If there's exactly one body arg that starts with '@', stream it from file
        if self.body.len() == 1 && self.body[0].starts_with('@') {
            let file_path = &self.body[0][1..];
            let file = tokio::fs::File::open(file_path).await?;
            let stream = tokio_util::io::ReaderStream::new(file);
            Ok(Either::Left(stream.map(|r| Ok(r.unwrap()))))
        } else {
            let chunks: Vec<Bytes> = self
                .body
                .iter()
                .map(|s| Bytes::from(s.to_owned()))
                .collect();
            let stream = futures_util::stream::iter(chunks.into_iter().map(Ok));
            Ok(Either::Right(stream))
        }
    }
}

pub fn load_body_content(content: &str) -> Result<Bytes> {
    // Remove @ prefix and treat as file path
    if let Some(file_path) = content.strip_prefix('@') {
        let file_content = std::fs::read(file_path)?;
        return Ok(Bytes::from(file_content));
    }

    Ok(Bytes::from(content.to_string()))
}

fn parse_secs(arg: &str) -> Result<Duration, std::num::ParseIntError> {
    let seconds = arg.parse()?;
    Ok(Duration::from_secs(seconds))
}

fn parse_http_method(arg: &str) -> Result<Method, InvalidMethod> {
    arg.parse()
}

fn parse_key_val(s: &str) -> Result<(String, String), String> {
    s.find(':')
        .ok_or_else(|| "invalid argument (expected KEY:VALUE)".to_string())
        .map(|index| {
            let (key, value) = s.split_at(index);
            (key.to_string(), value[1..].to_string())
        })
}
