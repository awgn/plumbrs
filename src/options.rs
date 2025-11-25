use std::time::Duration;

use clap::Parser;
use http::{Method, method::InvalidMethod};

use crate::client::ClientType;

#[derive(Parser, Debug, Clone)]
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
    #[arg(help = "Body of the request (can be repeated multiple times as chunks)", short = 'B', long = "body")]
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
    #[arg(help = "Sets the initial window size for HTTP/2 stream-level flow control.", long)]
    pub http2_initial_stream_window_size: Option<u32>,
    #[arg(help = "Sets the initial window size for HTTP/2 connection-level flow control.", long)]
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
    #[arg(help = "Sets the exact size of the read buffer to always use for HTTP/1.", long)]
    pub http1_read_buf_exact_size: Option<usize>,
    #[arg(help = "Set whether HTTP/1 connections should try to use vectored writes.", long)]
    pub http1_writev: Option<bool>,
    #[arg(help = "Set whether HTTP/1 connections will write header names as title case.", long)]
    pub http1_title_case_headers: bool,
    #[arg(help = "Set whether to support preserving original header cases for HTTP/1.", long)]
    pub http1_preserve_header_case: bool,
    #[arg(help = "Set the maximum number of headers for HTTP/1.", long)]
    pub http1_max_headers: Option<usize>,
    #[arg(help = "Set whether HTTP/1 connections will accept spaces after header name in responses.", long)]
    pub http1_allow_spaces_after_header_name_in_responses: bool,
    #[arg(help = "Set whether HTTP/1 connections will accept obsolete line folding for header values.", long)]
    pub http1_allow_obsolete_multiline_headers_in_responses: bool,
    #[arg(help = "Set whether HTTP/1 connections will silently ignore malformed header lines.", long)]
    pub http1_ignore_invalid_headers_in_responses: bool,
    #[arg(help = "Set whether HTTP/0.9 responses should be tolerated.", long)]
    pub http09_responses: bool,
    #[arg(
        help = "Set the host to benchmark (e.g. http://192.168.0.1:8080)",
        long = "host"
    )]
    pub host: Option<String>,
    #[arg(
        help = "HTTP uri used in the request (e.g. http://192.168.0.1:80)",
        default_value = ""
    )]
    pub uri: Vec<String>,
}

fn parse_secs(arg: &str) -> Result<Duration, std::num::ParseIntError> {
    let seconds = arg.parse()?;
    Ok(Duration::from_secs(seconds))
}

fn parse_http_method(arg: &str) -> Result<Option<Method>, InvalidMethod> {
    let method = arg.parse()?;
    Ok(Some(method))
}

fn parse_key_val(s: &str) -> Result<(String, String), String> {
    s.find(':')
        .ok_or_else(|| "invalid argument (expected KEY:VALUE)".to_string())
        .map(|index| {
            let (key, value) = s.split_at(index);
            (key.to_string(), value[1..].to_string())
        })
}
