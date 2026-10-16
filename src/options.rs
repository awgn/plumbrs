use std::{convert::Infallible, path::PathBuf, time::Duration};

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
        help = "Print complete statistics as a CSV row (no header)",
        long = "stats-csv",
        default_value_t = false
    )]
    pub stats_csv: bool,
    #[arg(
        help = "Print complete statistics as CSV, including the header row",
        long = "stats-csv-header",
        default_value_t = false
    )]
    pub stats_csv_header: bool,
    #[arg(
        help = "Number of connections or HTTP2 streams",
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
    #[arg(help = "Path to cookie file (Netscape/curl format)", long = "cookies")]
    pub cookies: Option<PathBuf>,
    #[arg(
        help = "Body of the request; can be specified multiple times. @path read body from file",
        short = 'b',
        long = "body"
    )]
    pub body: Vec<String>,
    #[arg(
        help = "Requests per connection to issue (--rpc 1 corresponds to connections per second)",
        long = "rpc",
        default_value_t = u32::MAX
    )]
    pub rpc: u32,
    #[arg(help = "Use http2 only", long = "http2")]
    pub http2: bool,
    #[arg(
        help = "Skip TLS certificate verification",
        short = 'k',
        long = "insecure"
    )]
    pub insecure: bool,
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
    #[cfg(feature = "mcp")]
    #[arg(
        help = "Enable MCP (Model Context Protocol) mode with Streamable HTTP transport.",
        long = "mcp",
        default_value_t = false
    )]
    pub mcp: bool,

    #[cfg(feature = "mcp")]
    #[arg(
        help = "Force legacy Server-Sent Events (SSE) transport for MCP (implies --mcp).",
        long = "mcp-sse",
        default_value_t = false
    )]
    pub mcp_sse: bool,

    #[cfg(feature = "mcp")]
    #[arg(
        help = "Length of the random string for tools/call arguments. If not specified, a random length in [5,20] is used.",
        long = "mcp-rand-string-len"
    )]
    pub mcp_rand_string_len: Option<usize>,

    #[cfg(feature = "mcp")]
    #[arg(
        help = "Filter MCP tools to invoke by name; can be repeated or comma-separated (implies --mcp). If empty, all tools are invoked.",
        long = "mcp-tool",
        visible_alias = "mcp-tools",
        value_delimiter = ','
    )]
    pub mcp_tool: Vec<String>,

    #[cfg(all(
        target_os = "linux",
        any(feature = "tokio_uring", feature = "monoio", feature = "compio")
    ))]
    #[arg(
        long,
        help = "Size of the io_uring Submission Queue (SQ)",
        default_value_t = 4096
    )]
    pub uring_entries: u32,

    #[cfg(all(
        target_os = "linux",
        any(feature = "tokio_uring", feature = "monoio", feature = "compio")
    ))]
    #[arg(
        long,
        help = "Enable kernel-side submission polling with idle timeout in milliseconds."
    )]
    pub uring_sqpoll: Option<u32>,

    #[arg(
        help = "Set the HTTP Host header (repeatable, comma-separated, supports [min-max] ranges)",
        long = "host",
        value_delimiter = ','
    )]
    pub host: Vec<String>,
    #[arg(help = "Set the port to connect to (e.g. 8080)", long = "port")]
    pub port: Option<u16>,
    #[arg(
        help = "Set the TLS SNI server name (repeatable, comma-separated, supports [min-max] ranges)",
        long = "sni",
        value_delimiter = ','
    )]
    pub sni: Vec<String>,
    #[arg(
        help = "Send HTTP/1 request-target in absolute-form (e.g. http://host/path) instead of origin-form (/path)",
        long = "absolute-uri",
        default_value_t = false
    )]
    pub absolute_uri: bool,

    #[arg(
        help = "Source IP address to bind outgoing connections to",
        long = "local-addr"
    )]
    pub local_addr: Option<std::net::IpAddr>,
    #[arg(
        help = "Source port range for outgoing connections, e.g. 40000-41000 (assigned round-robin)",
        long = "local-port-range",
        value_parser = crate::rss::parse_port_range
    )]
    pub local_port_range: Option<(u16, u16)>,
    #[arg(
        help = "RSS hash key (hex, from `ethtool --show-rxfh` on the server); prefix with @ to read from file",
        long = "rss-key"
    )]
    pub rss_key: Option<String>,
    #[arg(
        help = "RSS indirection: queue count (e.g. 8), queue list (e.g. 0,1,2,3) or @file with `ethtool -x` output",
        long = "rss-indir"
    )]
    pub rss_indir: Option<String>,
    #[arg(skip)]
    pub conn_base: usize,
    #[arg(skip)]
    pub total_connections: usize,
    #[arg(skip)]
    pub rss: Option<crate::rss::RssState>,
    #[arg(help = "HTTP/HTTPS URIs used in the request (e.g. http://192.168.0.1:80)")]
    pub uri: Vec<String>,
}

impl Options {
    pub fn body_size(&self) -> u64 {
        self.body
            .iter()
            .map(|b| {
                if let Some(path) = b.strip_prefix('@') {
                    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
                } else {
                    b.len() as u64
                }
            })
            .sum()
    }

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

/// Expand numeric `[min-max]` range patterns in a single string.
///
/// Multiple ranges in the same string are expanded via Cartesian product from
/// left to right (e.g. `app-[1-2].tier-[1-2].com` -> 4 strings).
/// Leading zeros on `min` preserve zero-padded width (e.g. `[01-03]` -> `01`, `02`, `03`).
/// Bracketed expressions that are not numeric ranges (such as IPv6 literals `[::1]`)
/// are left untouched. An inverted numeric range (`min > max`) returns an error.
pub fn expand_range_pattern(pattern: &str) -> Result<Vec<String>> {
    let mut results = vec![pattern.to_string()];
    let mut search_from = 0;

    loop {
        let sample = &results[0];
        if search_from >= sample.len() {
            break;
        }
        let Some(rel_open) = sample[search_from..].find('[') else {
            break;
        };
        let open_idx = search_from + rel_open;
        let Some(rel_close) = sample[open_idx + 1..].find(']') else {
            break;
        };
        let close_idx = open_idx + 1 + rel_close;
        let inner = &sample[open_idx + 1..close_idx];

        if let Some((min_str, max_str)) = inner.split_once('-')
            && !min_str.is_empty()
            && !max_str.is_empty()
            && min_str.bytes().all(|b| b.is_ascii_digit())
            && max_str.bytes().all(|b| b.is_ascii_digit())
        {
            let min_val: u64 = min_str
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid range start in '[{inner}]'"))?;
            let max_val: u64 = max_str
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid range end in '[{inner}]'"))?;
            if min_val > max_val {
                anyhow::bail!(
                    "invalid range '[{inner}]' in '{pattern}': start ({min_val}) > end ({max_val})"
                );
            }
            let width = if min_str.len() > 1 && min_str.starts_with('0') {
                min_str.len()
            } else {
                0
            };
            let count = (max_val - min_val + 1) as usize;
            // All strings in `results` share the exact same suffix starting at `open_idx`,
            // so `s.len() - sample.len()` gives the exact offset adjustment for each `s`.
            let sample_len = sample.len();
            let mut next_results = Vec::with_capacity(results.len().saturating_mul(count));
            for s in &results {
                let s_open = open_idx + s.len() - sample_len;
                let s_close = close_idx + s.len() - sample_len;
                let prefix = &s[..s_open];
                let suffix = &s[s_close + 1..];
                for val in min_val..=max_val {
                    if width > 0 {
                        next_results.push(format!("{prefix}{val:0width$}{suffix}"));
                    } else {
                        next_results.push(format!("{prefix}{val}{suffix}"));
                    }
                }
            }
            results = next_results;
            // Advance search_from to the start of the just-expanded segment in `results[0]`.
            search_from = open_idx;
        } else {
            // Not a numeric [min-max] range (e.g. IPv6 `[::1]`): skip past this bracket.
            search_from = close_idx + 1;
        }
    }

    Ok(results)
}

/// Expand all numeric `[min-max]` patterns across a list of specifications.
pub fn expand_range_patterns(specs: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for spec in specs {
        out.extend(expand_range_pattern(spec)?);
    }
    Ok(out)
}
