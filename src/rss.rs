//! Manual source-port selection and RSS-aware port assignment.
//!
//! By default the kernel picks an ephemeral source port for every connection.
//! On the server side, Receive Side Scaling (RSS) hashes the 4-tuple
//! `(src IP, dst IP, src port, dst port)` with the Toeplitz function to select
//! the RX queue (and hence the CPU) that handles the connection. Sequential
//! ephemeral ports do not hash uniformly, so a benchmark can overload a few
//! server queues while others stay idle.
//!
//! This module lets plumbrs bind an explicit source port per connection and,
//! when the server RSS configuration is known, pick ports that spread evenly
//! across the server RX queues.
//!
//! # Typical workflow (server side, Linux)
//!
//! ```text
//! # on the server, dump the RSS configuration:
//! ethtool --show-rxfh eth0 > rxfh.txt
//!
//! # on the load generator, feed it to plumbrs:
//! plumbrs -c 64 --rss-key @rxfh.txt --rss-indir @rxfh.txt http://server/
//! ```
//!
//! `ethtool -x eth0` shows only the indirection table; the Toeplitz key needed
//! to predict the hash is printed by `ethtool --show-rxfh`. Either dump can be
//! passed with `@file`; the relevant section is extracted automatically.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, SocketAddr};

use anyhow::{Result, anyhow, bail};

use crate::Options;
use crate::client::ClientType;

/// RSS runtime state, computed once at startup by [`init_source_ports`].
///
/// `ports` is the balanced source-port list: it interleaves server queues
/// round-robin, so task `i` takes `ports[i]` and reconnects stride forward by
/// the total connection count (see [`source_candidates`]).
#[derive(Clone, Debug)]
pub struct RssState {
    pub src: IpAddr,
    pub num_queues: usize,
    pub ports: Vec<u16>,
}

/// Default source-port search space (unprivileged ports).
pub const DEFAULT_PORT_MIN: u16 = 1024;
pub const DEFAULT_PORT_MAX: u16 = 65535;

/// Size of the synthetic indirection table built for `--rss-indir N`.
const SYNTHETIC_INDIR_LEN: usize = 128;

/// How many bind candidates are tried per connection before giving up.
const MAX_BIND_CANDIDATES: usize = 8;

/// Parse `--local-port-range`, accepting `START-END` or `START:END`.
pub fn parse_port_range(s: &str) -> std::result::Result<(u16, u16), String> {
    let (start, end) = s
        .split_once(['-', ':'])
        .ok_or_else(|| "expected START-END (e.g. 40000-41000)".to_string())?;
    let start: u16 = start
        .trim()
        .parse()
        .map_err(|_| format!("invalid range start: '{start}'"))?;
    let end: u16 = end
        .trim()
        .parse()
        .map_err(|_| format!("invalid range end: '{end}'"))?;
    if start == 0 || end == 0 {
        return Err("ports must be > 0".to_string());
    }
    if start > end {
        return Err(format!("range start ({start}) is greater than end ({end})"));
    }
    Ok((start, end))
}

/// Parse an RSS key in hex form: `6d:5a:...`, `6d5a...` or space separated.
/// Accepts any key length >= 4 bytes (RSS normally uses 40).
pub fn parse_hex_key(s: &str) -> std::result::Result<Vec<u8>, String> {
    for c in s.chars() {
        if !c.is_ascii_hexdigit() && !matches!(c, ':' | '-' | ' ' | '\t' | '\r' | '\n') {
            return Err(format!("invalid character '{c}' in RSS key"));
        }
    }
    let hex: String = s
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect();
    if hex.len() < 8 {
        return Err("RSS key is too short (need at least 4 bytes of hex)".to_string());
    }
    if !hex.len().is_multiple_of(2) {
        return Err("RSS key has an odd number of hex digits".to_string());
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hex[i..i + 2], 16)
                .map_err(|_| format!("invalid hex byte: '{}'", &hex[i..i + 2]))
        })
        .collect()
}

/// Parse an inline `--rss-indir` value: either a queue count (`8`) or an
/// explicit queue list (`0,1,2,3,0,1,2,3`).
pub fn parse_indir_inline(s: &str) -> std::result::Result<Vec<u32>, String> {
    let items: Vec<&str> = s
        .split([',', ' ', '\t', '\n'])
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    if items.is_empty() {
        return Err("empty RSS indirection table".to_string());
    }
    let queues: Vec<u32> = items
        .iter()
        .map(|t| {
            t.parse()
                .map_err(|_| format!("invalid queue id: '{t}'"))
        })
        .collect::<std::result::Result<_, _>>()?;
    if items.len() == 1 {
        // A single number is the queue count: synthesize a uniform table.
        let n = queues[0];
        if n == 0 {
            return Err("RSS queue count must be > 0".to_string());
        }
        return Ok((0..SYNTHETIC_INDIR_LEN)
            .map(|i| i as u32 % n)
            .collect());
    }
    if !items.len().is_power_of_two() {
        // If an explicit list is not a power of two (e.g. `0,1,2,3,4,5`),
        // synthesize a standard RETA of length SYNTHETIC_INDIR_LEN cycling through it,
        // so that (hash % 128) matches hardware indexing.
        return Ok((0..SYNTHETIC_INDIR_LEN)
            .map(|i| queues[i % queues.len()])
            .collect());
    }
    Ok(queues)
}

/// If `spec` refers to a file (`@path` or an existing path), read it.
fn read_spec_file(spec: &str) -> Option<Result<String>> {
    let path = spec.strip_prefix('@').or_else(|| {
        if std::path::Path::new(spec).is_file() {
            Some(spec)
        } else {
            None
        }
    })?;
    Some(std::fs::read_to_string(path).map_err(|e| anyhow!("cannot read '{path}': {e}")))
}

/// Extract the RSS hash key from an `ethtool --show-rxfh` dump.
///
/// Looks for the `RSS hash key:` section and collects hex bytes until a blank
/// line or the `RSS hash function:` section.
pub fn extract_ethtool_key(text: &str) -> Option<Vec<u8>> {
    let mut in_key = false;
    let mut hex = String::new();
    for line in text.lines() {
        let lower = line.to_lowercase();
        if !in_key {
            if lower.contains("hash key") {
                in_key = true;
            }
            continue;
        }
        if lower.contains("hash function") {
            break;
        }
        if line.trim().is_empty() {
            if !hex.trim().is_empty() {
                break;
            }
            continue;
        }
        hex.push_str(line);
        hex.push(' ');
    }
    if !in_key || hex.trim().is_empty() {
        return None;
    }
    parse_hex_key(&hex).ok()
}

/// Extract the indirection table from an `ethtool -x` / `--show-rxfh` dump.
///
/// Table rows look like `    0:      0     1     2 ...`.
pub fn extract_ethtool_indir(text: &str) -> Option<Vec<u32>> {
    let mut table = Vec::new();
    for line in text.lines() {
        let Some((idx, rest)) = line.split_once(':') else {
            continue;
        };
        // Row labels are small integers followed by queue ids.
        if idx.trim().parse::<usize>().is_err() {
            continue;
        }
        let mut row = Vec::new();
        for tok in rest.split_whitespace() {
            let Ok(q) = tok.parse::<u32>() else {
                row.clear();
                break;
            };
            row.push(q);
        }
        if row.is_empty() {
            // A non-numeric row (e.g. a wrapped key line) ends the table.
            if !table.is_empty() {
                break;
            }
            continue;
        }
        table.extend(row);
    }
    if table.is_empty() { None } else { Some(table) }
}

/// Resolve `--rss-key` to raw bytes (file dump, raw hex file, or inline hex).
fn resolve_key(spec: &str) -> Result<Vec<u8>> {
    if let Some(content) = read_spec_file(spec) {
        let content = content?;
        if let Some(key) = extract_ethtool_key(&content) {
            return Ok(key);
        }
        return parse_hex_key(&content)
            .map_err(|e| anyhow!("cannot parse RSS key from file: {e}"));
    }
    parse_hex_key(spec).map_err(|e| anyhow!("invalid --rss-key: {e}"))
}

/// Resolve `--rss-indir` to a queue list (ethtool dump file, count, or list).
fn resolve_indir(spec: &str) -> Result<Vec<u32>> {
    if let Some(content) = read_spec_file(spec) {
        let content = content?;
        if let Some(table) = extract_ethtool_indir(&content) {
            return Ok(table);
        }
        return parse_indir_inline(&content)
            .map_err(|e| anyhow!("cannot parse RSS indirection table from file: {e}"));
    }
    parse_indir_inline(spec).map_err(|e| anyhow!("invalid --rss-indir: {e}"))
}

/// Compute the 32-bit big-endian key window starting at bit `offset`.
#[inline]
fn toeplitz_word(key: &[u8], offset: usize) -> u32 {
    let key_len = key.len();
    let key_bits = key_len * 8;
    let offset = offset % key_bits;
    let byte_idx = offset / 8;
    let bit_shift = offset % 8;
    let mut val: u64 = 0;
    for i in 0..5 {
        val = (val << 8) | (key[(byte_idx + i) % key_len] as u64);
    }
    if bit_shift != 0 {
        ((val >> (8 - bit_shift)) & 0xffff_ffff) as u32
    } else {
        (val >> 8) as u32
    }
}

/// Toeplitz hash as used by RSS (Microsoft RSS specification).
///
/// For every set bit `n` of `input` (MSB first), the 32-bit key window
/// starting at bit `n` is XORed into the hash. The key wraps if the input is
/// longer than the key (only relevant for IPv6-sized inputs with short keys).
pub fn toeplitz_hash(key: &[u8], input: &[u8]) -> u32 {
    debug_assert!(!key.is_empty());
    let mut hash: u32 = 0;
    for (i, &byte) in input.iter().enumerate() {
        if byte == 0 {
            continue;
        }
        let base = i * 8;
        for b in 0..8 {
            if byte & (0x80 >> b) != 0 {
                hash ^= toeplitz_word(key, base + b);
            }
        }
    }
    hash
}

/// Write 4-tuple input bytes to stack buffer; returns the written length (0 on mismatched families).
fn write_rss_input(src: &IpAddr, dst: &IpAddr, sport: u16, dport: u16, buf: &mut [u8; 36]) -> usize {
    match (src, dst) {
        (IpAddr::V4(s), IpAddr::V4(d)) => {
            buf[0..4].copy_from_slice(&s.octets());
            buf[4..8].copy_from_slice(&d.octets());
            buf[8..10].copy_from_slice(&sport.to_be_bytes());
            buf[10..12].copy_from_slice(&dport.to_be_bytes());
            12
        }
        (IpAddr::V6(s), IpAddr::V6(d)) => {
            buf[0..16].copy_from_slice(&s.octets());
            buf[16..32].copy_from_slice(&d.octets());
            buf[32..34].copy_from_slice(&sport.to_be_bytes());
            buf[34..36].copy_from_slice(&dport.to_be_bytes());
            36
        }
        _ => 0,
    }
}

/// Server RX queue selected by RSS for the given 4-tuple.
pub fn rss_queue(
    key: &[u8],
    indir: &[u32],
    src: &IpAddr,
    dst: &IpAddr,
    sport: u16,
    dport: u16,
) -> Option<u32> {
    if key.is_empty() || indir.is_empty() {
        return None;
    }
    let mut buf = [0u8; 36];
    let len = write_rss_input(src, dst, sport, dport, &mut buf);
    if len == 0 {
        return None;
    }
    let hash = toeplitz_hash(key, &buf[..len]);
    Some(indir[(hash % indir.len() as u32) as usize])
}

/// Build a port list that spreads evenly across the server RX queues.
///
/// Exploits the linearity of the Toeplitz hash over GF(2): the hash contribution
/// from (src, dst, dport) is constant across all candidate source ports, so only
/// the 16 bits of `sport` vary. Using 256-entry lookup tables for the two bytes
/// of `sport`, each candidate is classified with zero heap allocations and two XORs.
pub fn build_balanced_ports(
    key: &[u8],
    indir: &[u32],
    src: IpAddr,
    dst: IpAddr,
    dport: u16,
    candidates: impl Iterator<Item = u16>,
) -> (Vec<u16>, Vec<u32>) {
    let mut buckets: BTreeMap<u32, Vec<u16>> = BTreeMap::new();
    if key.is_empty() || indir.is_empty() {
        return (Vec::new(), Vec::new());
    }

    // Fixed baseline input with sport = 0, and the bit offset where sport starts.
    let mut fixed_buf = [0u8; 36];
    let (fixed_len, sport_bit_offset) = match (&src, &dst) {
        (IpAddr::V4(_), IpAddr::V4(_)) => (write_rss_input(&src, &dst, 0, dport, &mut fixed_buf), 64),
        (IpAddr::V6(_), IpAddr::V6(_)) => (write_rss_input(&src, &dst, 0, dport, &mut fixed_buf), 256),
        _ => return (Vec::new(), Vec::new()),
    };
    if fixed_len == 0 {
        return (Vec::new(), Vec::new());
    }
    let fixed_hash = toeplitz_hash(key, &fixed_buf[..fixed_len]);

    // Precompute 256-entry tables for the high and low bytes of source port.
    let mut hi_table = [0u32; 256];
    let mut lo_table = [0u32; 256];
    for byte in 0..=255u8 {
        let mut hi_h = 0u32;
        let mut lo_h = 0u32;
        for b in 0..8 {
            if byte & (0x80 >> b) != 0 {
                hi_h ^= toeplitz_word(key, sport_bit_offset + b);
                lo_h ^= toeplitz_word(key, sport_bit_offset + 8 + b);
            }
        }
        hi_table[byte as usize] = hi_h;
        lo_table[byte as usize] = lo_h;
    }

    let indir_len = indir.len() as u32;
    for port in candidates {
        let hi = (port >> 8) as usize;
        let lo = (port & 0xff) as usize;
        let hash = fixed_hash ^ hi_table[hi] ^ lo_table[lo];
        let q = indir[(hash % indir_len) as usize];
        buckets.entry(q).or_default().push(port);
    }
    // Round-robin across queues (sorted for determinism).
    let mut ports = Vec::new();
    let mut queues = Vec::new();
    let order: Vec<u32> = buckets.keys().copied().collect();
    let mut cursor: BTreeMap<u32, usize> = BTreeMap::new();
    loop {
        let mut progress = false;
        for q in &order {
            let bucket = &buckets[q];
            let next = cursor.entry(*q).or_insert(0);
            if *next < bucket.len() {
                ports.push(bucket[*next]);
                queues.push(*q);
                *next += 1;
                progress = true;
            }
        }
        if !progress {
            break;
        }
    }
    (ports, queues)
}

/// Number of distinct queues in an indirection table.
pub fn distinct_queues(indir: &[u32]) -> usize {
    let mut set = BTreeSet::<u32>::new();
    set.extend(indir.iter());
    set.len()
}

fn supports_source_ports(client: ClientType) -> bool {
    match client {
        ClientType::Auto
        | ClientType::Hyper
        | ClientType::HyperChunked
        | ClientType::HyperH2 => true,
        #[cfg(feature = "mcp")]
        ClientType::HyperMcp => true,
        #[cfg(feature = "compio")]
        ClientType::Compio => true,
        _ => false,
    }
}

/// Clients that open sockets outside tokio and cannot bind a source address.
fn is_raw_socket_client(client: ClientType) -> bool {
    match client {
        #[cfg(all(target_os = "linux", feature = "tokio_uring"))]
        ClientType::TokioUring => true,
        #[cfg(all(target_os = "linux", feature = "monoio"))]
        ClientType::Monoio => true,
        _ => false,
    }
}

fn resolve_host(host: &str, port: u16, prefer: Option<IpAddr>) -> Result<IpAddr> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(ip);
    }
    let addrs: Vec<SocketAddr> = std::net::ToSocketAddrs::to_socket_addrs(&(host, port))
        .map_err(|e| anyhow!("cannot resolve '{host}': {e}"))?
        .collect();
    if addrs.is_empty() {
        bail!("cannot resolve '{host}'");
    }
    if let Some(IpAddr::V4(_)) = prefer
        && let Some(a) = addrs.iter().find(|a| a.is_ipv4())
    {
        return Ok(a.ip());
    }
    if let Some(IpAddr::V6(_)) = prefer
        && let Some(a) = addrs.iter().find(|a| a.is_ipv6())
    {
        return Ok(a.ip());
    }
    Ok(addrs[0].ip())
}

/// Discover the local IP the kernel would use towards `dst` (no traffic sent).
fn detect_src_ip(dst: SocketAddr) -> Result<IpAddr> {
    let bind = if dst.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let sock =
        std::net::UdpSocket::bind(bind).map_err(|e| anyhow!("cannot detect source IP: {e}"))?;
    sock.connect(dst)
        .map_err(|e| anyhow!("cannot detect source IP: {e}"))?;
    sock.local_addr()
        .map(|a| a.ip())
        .map_err(|e| anyhow!("cannot detect source IP: {e}"))
}

/// Validate source-port/RSS options and precompute the balanced port list.
///
/// Called once at startup from `check_options`; per-connection assignment is
/// done later with [`source_candidates`].
pub fn init_source_ports(opts: &mut Options) -> Result<()> {
    let manual = opts.local_port_range.is_some()
        || opts.rss_key.is_some()
        || opts.rss_indir.is_some();

    if !manual && opts.local_addr.is_none() {
        return Ok(());
    }

    opts.total_connections = opts.connections;

    // Source-port selection only pays off for persistent connections
    // (default --rpc): with `--rpc N` every connection is short-lived, so
    // per-connection bind work is pure overhead and queue stickiness is
    // impossible anyway. Fail fast instead of silently ignoring flags.
    if opts.rpc != u32::MAX && (manual || opts.local_addr.is_some()) {
        bail!(
            "--local-addr/--local-port-range/--rss-* need persistent connections (omit --rpc); \
             with --rpc N all connections use kernel-assigned ports"
        );
    }

    // Raw-socket clients cannot bind a source port.
    if manual && !supports_source_ports(opts.client_type) {
        bail!("--local-port-range/--rss-* need a direct-connect client (hyper, hyper-chunked, hyper-h2, compio); not supported with '{}'", opts.client_type);
    }
    if opts.local_addr.is_some() && is_raw_socket_client(opts.client_type) {
        bail!(
            "--local-addr is not supported with '{}' client",
            opts.client_type
        );
    }

    if opts.connections == 0 {
        return Ok(());
    }

    if let Some((start, end)) = opts.local_port_range {
        let len = (end as u32 - start as u32 + 1) as usize;
        if len < opts.connections {
            bail!(
                "--local-port-range holds {len} ports but {n} connections were requested; \
                 widen the range",
                n = opts.connections
            );
        }
    }

    let want_rss = opts.rss_key.is_some() || opts.rss_indir.is_some();
    if !want_rss {
        return Ok(());
    }

    let key_spec = opts
        .rss_key
        .as_deref()
        .ok_or_else(|| {
            anyhow!(
                "--rss-indir needs --rss-key (dump it on the server with \
                 `ethtool --show-rxfh <iface>`, then pass --rss-key @file)"
            )
        })?;
    let indir_spec = opts.rss_indir.as_deref().ok_or_else(|| {
        anyhow!("--rss-key needs --rss-indir (queue count, queue list, or @file with `ethtool -x` output)")
    })?;

    let key = resolve_key(key_spec)?;
    if key.len() != 40 {
        eprintln!(
            "warning: RSS key is {} bytes (standard is 40); hash prediction assumes \
             the NIC uses this exact key",
            key.len()
        );
    }
    let indir = resolve_indir(indir_spec)?;
    if indir.is_empty() {
        bail!("empty RSS indirection table");
    }

    // Destination is taken from the first URI (per-task URIs share it in the
    // common case); warn when URIs point at different hosts.
    let first_uri = opts.uri.first().ok_or_else(|| anyhow!("missing URI"))?;
    let parsed: http::Uri = first_uri
        .parse()
        .map_err(|e| anyhow!("invalid uri: {e}"))?;
    let host = parsed
        .host()
        .ok_or_else(|| anyhow!("no host in uri"))?
        .to_owned();
    let mut dport = parsed.port_u16().unwrap_or_else(|| {
        if parsed.scheme_str() == Some("https") {
            443
        } else {
            80
        }
    });
    if let Some(p) = opts.port {
        dport = p;
    }
    let dst_ip = resolve_host(&host, dport, opts.local_addr)?;
    let dst = SocketAddr::new(dst_ip, dport);

    if opts.uri.len() > 1 {
        let mut hosts = BTreeSet::new();
        hosts.insert(format!("{host}:{dport}"));
        for u in &opts.uri[1..] {
            if let Ok(parsed) = u.parse::<http::Uri>() {
                let h = parsed.host().unwrap_or(&host);
                let p = parsed.port_u16().unwrap_or_else(|| {
                    if parsed.scheme_str() == Some("https") {
                        443
                    } else {
                        80
                    }
                });
                let p = opts.port.unwrap_or(p);
                hosts.insert(format!("{h}:{p}"));
            } else {
                hosts.insert(u.clone());
            }
        }
        if hosts.len() > 1 {
            eprintln!(
                "warning: RSS balancing is computed for {host}:{dport}; \
                 other URIs may hash to different server queues"
            );
        }
    }

    let src_ip = match opts.local_addr {
        Some(ip) => ip,
        None => detect_src_ip(dst).map_err(|e| {
            anyhow!("{e} (specify the source IP explicitly with --local-addr)")
        })?,
    };
    if std::mem::discriminant(&src_ip) != std::mem::discriminant(&dst_ip) {
        bail!(
            "source IP {src_ip} and destination {dst_ip} are of different families; \
             use --local-addr with a matching address"
        );
    }

    let (min, max) = opts
        .local_port_range
        .unwrap_or((DEFAULT_PORT_MIN, DEFAULT_PORT_MAX));
    let candidates = (min as u32..=max as u32).map(|p| p as u16);
    let candidate_len = (max as usize) - (min as usize) + 1;
    if candidate_len < opts.connections {
        bail!(
            "port range holds {candidate_len} ports but {n} connections were requested",
            n = opts.connections
        );
    }

    let (ports, _) = build_balanced_ports(&key, &indir, src_ip, dst_ip, dport, candidates);
    if ports.len() < opts.connections {
        bail!("only {} usable source ports found; widen --local-port-range", ports.len());
    }

    let nq = distinct_queues(&indir);
    eprintln!(
        "rss: {} source ports spread over {} server queue(s) (src={} dst={}:{})",
        ports.len(),
        nq,
        src_ip,
        dst_ip,
        dport
    );

    opts.rss = Some(RssState {
        src: src_ip,
        num_queues: nq,
        ports,
    });
    Ok(())
}

/// Bind addresses to try for connection `(cid, generation)`, primary first.
///
/// `cid` is the per-instance connection index (combined with `conn_base` into
/// a global index), `generation` counts reconnects of that task. An empty vector
/// means "let the kernel choose" (no heap allocation in that case).
///
/// This runs once per *connection*, never per request: the per-request hot
/// loop is untouched.
pub fn source_candidates(opts: &Options, cid: usize, generation: u64) -> Vec<SocketAddr> {
    // No selection for short-lived connections (enforced in init_source_ports).
    if opts.rpc != u32::MAX {
        return Vec::new();
    }
    let total = opts.total_connections as u64;
    if total == 0 {
        return Vec::new();
    }
    let global = opts.conn_base as u64 + cid as u64;

    if let Some(rss) = opts.rss.as_ref() {
        let len = rss.ports.len() as u64;
        let stride = (rss.num_queues as u64).max(1);
        let src = rss.src;
        // Step forward in multiples of the queue stride to leap past all active
        // tasks on the same queue, keeping this task on its assigned RX queue across reconnects.
        let tasks_per_queue = (total + stride - 1) / stride;
        let queue_step = generation.wrapping_mul(tasks_per_queue * stride);
        let mut out = Vec::with_capacity(MAX_BIND_CANDIDATES);
        let mut seen = BTreeSet::new();
        for k in 0..MAX_BIND_CANDIDATES as u64 {
            let idx = ((global + queue_step + k * stride) % len) as usize;
            let port = rss.ports[idx];
            if seen.insert(port) {
                out.push(SocketAddr::new(src, port));
            }
        }
        return out;
    }

    let step = generation.wrapping_mul(total);
    if let Some((start, end)) = opts.local_port_range {
        let len = (end as u64) - (start as u64) + 1;
        let base = (global + step) % len;
        let ip = opts.local_addr.unwrap_or(IpAddr::from([0, 0, 0, 0]));
        let count = (MAX_BIND_CANDIDATES as u64).min(len);
        return (0..count)
            .map(|k| SocketAddr::new(ip, start + ((base + k) % len) as u16))
            .collect();
    }

    match opts.local_addr {
        Some(ip) => vec![SocketAddr::new(ip, 0)],
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RXFH: &str = "\
RX flow hash indirection table for eth0 with 4 RX ring(s):\n\
    0:      0     1     2     3     0     1     2     3\n\
    8:      0     1     2     3     0     1     2     3\n\
RSS hash key:\n\
6d:5a:56:da:25:5b:0e:c2:41:67:25:3d:43:a3:8f:b0:d0:ca:2b:cb:ae:7b:30:b4:77:cb:2d:a3:30:28:6c:1d:36:5d:5b:02:e3:a6:6c:82\n\
RSS hash function:\n\
    toeplitz: on\n";

    #[test]
    fn toeplitz_cross_check() {
        // Vectors from an independent Python implementation of the RSS spec.
        let key = parse_hex_key("39:0c:8c:7d:72:47:34:2c:d8:10:0f:2f:6f:77:0d:65:d6:70:e5:8e:03:51:d8:ae:8e:4f:6e:ac:34:2f:c2:31:b7:b0:87:16:eb:3f:c1:28").unwrap();
        let cases: &[(&[u8], u32)] = &[
            (&[192, 168, 0, 1, 10, 0, 0, 1, 0x9c, 0x40, 0, 80], 0x43d07682),
            (&[10, 1, 2, 3, 172, 16, 0, 9, 0x12, 0x34, 0x1f, 0x90], 0x43e6b796),
        ];
        for (input, expected) in cases {
            assert_eq!(toeplitz_hash(&key, input), *expected);
        }
        // IPv6-sized input exercises key wrapping past bit 320.
        let v6: Vec<u8> = (0..32).collect::<Vec<u8>>()
            .into_iter()
            .chain([0xab, 0xcd, 0x00, 0xbb])
            .collect();
        assert_eq!(toeplitz_hash(&key, &v6), 0x1467255f);
    }

    #[test]
    fn toeplitz_single_bit_matches_key_window() {
        let key: Vec<u8> = (0..40).map(|i| (i * 7 + 1) as u8).collect();
        // Only the very first input bit set -> hash = first 32 key bits.
        let hash = toeplitz_hash(&key, &[0x80, 0, 0, 0]);
        let expected =
            ((key[0] as u32) << 24) | ((key[1] as u32) << 16) | ((key[2] as u32) << 8) | key[3] as u32;
        assert_eq!(hash, expected);
        // Only the second input bit set -> key window shifted by one.
        let hash = toeplitz_hash(&key, &[0x40, 0, 0, 0]);
        let expected2 = ((key[0] as u32 & 0x7f) << 25)
            | ((key[1] as u32) << 17)
            | ((key[2] as u32) << 9)
            | ((key[3] as u32) << 1)
            | ((key[4] as u32) >> 7);
        assert_eq!(hash, expected2);
        // Zero key -> zero hash.
        assert_eq!(toeplitz_hash(&[0u8; 40], &[0xff; 12]), 0);
    }

    #[test]
    fn toeplitz_is_deterministic_and_sensitive() {
        let key = parse_hex_key(
            "6d:5a:56:da:25:5b:0e:c2:41:67:25:3d:43:a3:8f:b0:d0:ca:2b:cb:ae:7b:30:b4",
        )
        .unwrap();
        let a = toeplitz_hash(&key, &[192, 168, 0, 1, 10, 0, 0, 1, 0x9c, 0x40, 0, 80]);
        let b = toeplitz_hash(&key, &[192, 168, 0, 1, 10, 0, 0, 1, 0x9c, 0x40, 0, 80]);
        let c = toeplitz_hash(&key, &[192, 168, 0, 1, 10, 0, 0, 1, 0x9c, 0x41, 0, 80]);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn ethtool_parsers() {
        let key = extract_ethtool_key(SAMPLE_RXFH).expect("key");
        assert_eq!(key.len(), 40);
        assert_eq!(key[0], 0x6d);
        assert_eq!(key[39], 0x82);
        let indir = extract_ethtool_indir(SAMPLE_RXFH).expect("indir");
        assert_eq!(indir.len(), 16);
        assert_eq!(&indir[0..4], &[0, 1, 2, 3]);
    }

    #[test]
    fn indir_inline_forms() {
        assert_eq!(parse_indir_inline("4").unwrap().len(), SYNTHETIC_INDIR_LEN);
        assert_eq!(parse_indir_inline("4").unwrap()[5], 1);
        assert_eq!(parse_indir_inline("0,1,2,3").unwrap(), vec![0, 1, 2, 3]);
    }

    #[test]
    fn port_range_parser() {
        assert_eq!(parse_port_range("40000-41000").unwrap(), (40000, 41000));
        assert_eq!(parse_port_range("40000:41000").unwrap(), (40000, 41000));
        assert!(parse_port_range("41000-40000").is_err());
        assert!(parse_port_range("0-100").is_err());
        assert!(parse_port_range("abc").is_err());
    }

    #[test]
    fn balanced_ports_spread_evenly() {
        let key = extract_ethtool_key(SAMPLE_RXFH).unwrap();
        let indir = extract_ethtool_indir(SAMPLE_RXFH).unwrap();
        let src: IpAddr = "192.168.0.10".parse().unwrap();
        let dst: IpAddr = "192.168.0.1".parse().unwrap();
        let (ports, queues) =
            build_balanced_ports(&key, &indir, src, dst, 80, 40000..41000);
        assert_eq!(ports.len(), 1000);
        // First 4 ports cover all 4 queues exactly once.
        let mut first: Vec<u32> = queues[..4].to_vec();
        first.sort_unstable();
        assert_eq!(first, vec![0, 1, 2, 3]);
        // Overall distribution is roughly uniform (sequential ports hash
        // pseudo-randomly; round-robin then spreads them optimally).
        let mut counts = [0usize; 4];
        for q in &queues {
            counts[*q as usize] += 1;
        }
        assert!(counts.iter().all(|&c| (200..=300).contains(&c)), "{counts:?}");
        // Every port really hashes to its assigned queue.
        for (p, q) in ports.iter().zip(queues.iter()).take(64) {
            assert_eq!(rss_queue(&key, &indir, &src, &dst, *p, 80), Some(*q));
        }
    }

    #[test]
    fn parse_hex_key_rejects_invalid_chars() {
        assert!(parse_hex_key("6d:5a:zz:12").is_err());
        assert!(parse_hex_key("6d5a1234").is_ok());
    }

    #[test]
    fn parse_indir_inline_synthesizes_non_power_of_two() {
        // 6 queues is not a power of two: expands to SYNTHETIC_INDIR_LEN (128)
        let indir = parse_indir_inline("0,1,2,3,4,5").unwrap();
        assert_eq!(indir.len(), SYNTHETIC_INDIR_LEN);
        assert_eq!(indir[0], 0);
        assert_eq!(indir[5], 5);
        assert_eq!(indir[6], 0);
    }

    #[test]
    fn microsoft_rss_verification_vectors() {
        // Official Microsoft RSS specification verification key and vectors
        let key = parse_hex_key(
            "6d:5a:56:da:25:5b:0e:c2:41:67:25:3d:43:a3:8f:b0:\
             d0:ca:2b:cb:ae:7b:30:b4:77:cb:2d:a3:80:30:f2:0c:\
             6a:42:b7:3b:be:ac:01:fa",
        )
        .unwrap();

        let src: IpAddr = "66.9.149.187".parse().unwrap();
        let dst: IpAddr = "161.142.100.80".parse().unwrap();
        let mut buf = [0u8; 36];
        let len = write_rss_input(&src, &dst, 2794, 1766, &mut buf);
        assert_eq!(toeplitz_hash(&key, &buf[..len]), 0x51ccc178);

        let src_v6: IpAddr = "3ffe:2501:200:1fff::7".parse().unwrap();
        let dst_v6: IpAddr = "3ffe:2501:200:3::1".parse().unwrap();
        let len_v6 = write_rss_input(&src_v6, &dst_v6, 2794, 1766, &mut buf);
        assert_eq!(toeplitz_hash(&key, &buf[..len_v6]), 0x40207d3d);
    }

    #[test]
    fn source_candidates_reconnect_preserves_queue() {
        let key = extract_ethtool_key(SAMPLE_RXFH).unwrap();
        let indir = extract_ethtool_indir(SAMPLE_RXFH).unwrap();
        let src: IpAddr = "192.168.0.10".parse().unwrap();
        let dst: IpAddr = "192.168.0.1".parse().unwrap();
        let (ports, _) = build_balanced_ports(&key, &indir, src, dst, 80, 40000..41000);

        use clap::Parser;
        let mut opts = Options::parse_from(["plumbrs", "-c", "6", "http://192.168.0.1/"]);
        opts.total_connections = 6; // Not a multiple of 4 queues!
        opts.rss = Some(RssState {
            src,
            num_queues: 4,
            ports,
        });

        // Task 0 initial queue
        let c0_gen0 = source_candidates(&opts, 0, 0);
        let q0 = rss_queue(&key, &indir, &src, &dst, c0_gen0[0].port(), 80).unwrap();

        // Task 0 reconnect 1, 2, 3 must remain on the exact same queue q0!
        for generation_idx in 1..=3 {
            let c0_reconnect = source_candidates(&opts, 0, generation_idx);
            let q_recon = rss_queue(&key, &indir, &src, &dst, c0_reconnect[0].port(), 80).unwrap();
            assert_eq!(q_recon, q0, "reconnect at gen {generation_idx} shifted queue from {q0} to {q_recon}");
            // And must not collide with initial port
            assert_ne!(c0_reconnect[0].port(), c0_gen0[0].port());
        }
    }
}
