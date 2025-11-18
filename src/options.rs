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
    #[arg(help = "Sets the initial max concurrent stream.", long)]
    pub http2_max_concurrent_streams: Option<u32>,
    #[arg(help = "Sets the initial maximum of concurrently reset streams.", long)]
    pub http2_max_concurrent_reset_streams: Option<usize>,
    #[cfg(feature = "orion_client")]
    #[arg(help = "Set if http2 can share connection", long)]
    pub http2_can_share: bool,
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
