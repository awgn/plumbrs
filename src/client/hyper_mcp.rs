use crate::Options;
use crate::stats::{RealtimeStats, Statistics};

use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;

use http::{HeaderMap, Request, StatusCode, header};

use rand::RngExt;
use rmcp::model::{
    CallToolRequest, CallToolRequestParams, ClientCapabilities, ErrorCode, ErrorData,
    Implementation, InitializeRequest, InitializeRequestParams, InitializeResult,
    InitializedNotification, JsonObject, JsonRpcError, JsonRpcRequest, JsonRpcResponse,
    ListToolsRequest, ListToolsResult, NumberOrString, Tool,
};
use rmcp::serde_json::{self, Map, Value};

use crate::client::utils::*;
use crate::fatal;
use const_format::concatcp;
use http_body_util::{BodyExt, Full};

/// MIME type for JSON content
const MIME_APPLICATION_JSON: &str = "application/json";
/// MIME type for Server-Sent Events stream
const MIME_TEXT_EVENT_STREAM: &str = "text/event-stream";
/// Combined MIME types for Accept header (JSON and SSE)
const MIME_APPLICATION_JSON_AND_EVENT_STREAM: &str =
    concatcp!(MIME_APPLICATION_JSON, ", ", MIME_TEXT_EVENT_STREAM);

/// Header carrying the Streamable HTTP session identifier
const HEADER_MCP_SESSION_ID: header::HeaderName = header::HeaderName::from_static("mcp-session-id");

/// Result of MCP initialization: URI for requests and pre-compiled bodies for each tool
#[derive(Debug, Default)]
pub struct McpSetup {
    /// URI where to send MCP requests (obtained from SSE handshake or original URI for Streamable HTTP)
    pub uri: hyper::Uri,
    /// Pre-compiled JSON bodies to invoke each tool with tools/call
    pub tool_bodies: Vec<Bytes>,
    /// Task that keeps the SSE connection open (only for SSE transport)
    pub sse_task: Option<tokio::task::JoinHandle<()>>,
    /// Session ID for Streamable HTTP transport (from Mcp-Session-Id header)
    pub session_id: Option<String>,
}

pub async fn http_hyper_mcp(
    tid: usize,
    cid: usize,
    opts: Arc<Options>,
    rt_stats: &RealtimeStats,
) -> Statistics {
    if opts.http2 {
        http_hyper_mcp_client::<Http2>(tid, cid, opts.as_ref(), rt_stats).await
    } else {
        http_hyper_mcp_client::<Http1>(tid, cid, opts.as_ref(), rt_stats).await
    }
}

async fn http_hyper_mcp_client<B: HttpConnectionBuilder>(
    tid: usize,
    cid: usize,
    opts: &Options,
    rt_stats: &RealtimeStats,
) -> Statistics {
    let mut statistics = Statistics::new(opts.latency);
    let mut total: u32 = 0;
    let mut conn_req_count: u32;
    // `uri_str` is fixed for the lifetime of this task, so a flag replaces the set.
    let mut banner_shown = false;
    let uri_str = opts.uri[cid % opts.uri.len()].as_str();
    let mut uri = uri_str
        .parse::<hyper::Uri>()
        .unwrap_or_else(|e| fatal!(1, "invalid uri: {e}"));

    let (mut host, mut port) =
        get_conn_address(opts, &uri).unwrap_or_else(|| fatal!(1, "no host specified in uri"));
    let mut endpoint = build_conn_endpoint(&host, port);

    let mut headers =
        build_headers(&uri, opts).unwrap_or_else(|e| fatal!(2, "could not build headers: {e}"));

    // For SSE transport, initialize before connection loop
    let mut mcp = McpSetup::default();
    if opts.mcp_sse {
        mcp = mcp_sse_initialize::<B>(uri_str, opts, &headers).await;
        uri = mcp.uri;

        if opts.host.is_none() {
            if let Some(h) = uri.host() {
                host = h.to_owned();
            }
            if let Some(p) = uri.port_u16() {
                port = p;
            }
            endpoint = build_conn_endpoint(&host, port);
        }
    }

    let req_uri = request_uri(&uri, opts.absolute_uri || opts.http2);

    // MCP requires Content-Type: application/json for JSON-RPC requests
    // Accept header must include both application/json and text/event-stream for Streamable HTTP
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static(MIME_APPLICATION_JSON),
    );
    let transport = if opts.mcp_sse {
        headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static(MIME_APPLICATION_JSON),
        );
        "sse"
    } else {
        headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static(MIME_APPLICATION_JSON_AND_EVENT_STREAM),
        );
        "streamableHttp"
    };

    // Pre-build the connection-closing header set once: the hot loop then only
    // clones the matching set instead of cloning + inserting per request.
    let mut headers_close = headers.clone();
    headers_close.insert(
        header::CONNECTION,
        header::HeaderValue::from_static("close"),
    );

    let clock = quanta::Clock::new();
    let start = Instant::now();
    // Wrapping cursor over `mcp.tool_bodies`: avoids a division per request.
    let mut body_idx: usize = 0;
    'connection: loop {
        if should_stop(total, start, opts) {
            break 'connection;
        }

        if cid < opts.uri.len() && !banner_shown {
            banner_shown = true;
            eprintln!(
                "hyper-mcp [{tid:>2}] -> connecting to {}:{}, method = POST uri = {} {} (transport {transport})...",
                host,
                port,
                uri,
                B::SCHEME
            );
        }

        let (mut sender, mut conn_task) = match B::build_connection(
            endpoint,
            tls_server_name(opts, &uri),
            &mut statistics,
            rt_stats,
            opts,
        )
        .await
        {
            Some(s) => s,
            None => {
                total += 1;
                continue 'connection;
            }
        };

        statistics.inc_conn();
        conn_req_count = 0;

        // For Streamable HTTP transport, initialize after connection is established
        if !opts.mcp_sse && mcp.tool_bodies.is_empty() {
            match mcp_streamable_http_initialize(&uri, &headers, &mut sender, opts).await {
                Ok(setup) => {
                    mcp = setup;
                    // Add session ID to headers for subsequent requests
                    if let Some(ref session_id) = mcp.session_id {
                        headers.insert(
                            HEADER_MCP_SESSION_ID.clone(),
                            http::header::HeaderValue::from_str(session_id)
                                .unwrap_or_else(|e| fatal!(3, "invalid session id: {e}")),
                        );
                        // Keep the pre-built closing set in sync.
                        headers_close.clone_from(&headers);
                        headers_close.insert(
                            header::CONNECTION,
                            header::HeaderValue::from_static("close"),
                        );
                    }
                }
                Err(e) => {
                    fatal!(3, "MCP Streamable HTTP initialization failed: {e}");
                }
            }
        }

        loop {
            // Round-robin over the pre-compiled tool bodies without modulo.
            let body = if mcp.tool_bodies.is_empty() {
                Full::new(Bytes::new())
            } else {
                if body_idx >= mcp.tool_bodies.len() {
                    body_idx = 0;
                }
                let body = Full::new(mcp.tool_bodies[body_idx].clone());
                body_idx += 1;
                body
            };

            conn_req_count += 1;
            let is_last = conn_req_count >= opts.rpc;

            let mut req = Request::new(body);
            // MCP JSON-RPC requests must use POST method
            *req.method_mut() = http::Method::POST;
            *req.uri_mut() = req_uri.clone();
            *req.headers_mut() = if is_last {
                headers_close.clone()
            } else {
                headers.clone()
            };

            let start_lat = opts.latency.then_some(clock.raw());

            match sender.send_request(req).await {
                Ok(res) => match discard_body(res).await {
                    Ok(StatusCode::OK) => statistics.inc_ok(rt_stats),
                    Ok(StatusCode::ACCEPTED) => statistics.inc_ok(rt_stats),
                    Ok(code) => statistics.set_http_status(code, rt_stats),
                    Err(ref err) => {
                        statistics.set_error(err.as_ref(), rt_stats);
                        total += 1;
                        continue 'connection;
                    }
                },
                Err(ref err) => {
                    statistics.set_error(err, rt_stats);
                    total += 1;
                    continue 'connection;
                }
            }

            if let Some(start_lat) = start_lat
                && let Some(hist) = &mut statistics.latency
            {
                hist.record(clock.delta_as_nanos(start_lat, clock.raw()) / 1000)
                    .ok();
            };

            total += 1;

            if should_stop(total, start, opts) {
                break 'connection;
            }

            if is_last {
                conn_task.abort();
                continue 'connection;
            } else {
                tokio::select! {
                    res = sender.ready() => {
                        if let Err(ref err) = res {
                            statistics.set_error(err, rt_stats);
                            continue 'connection;
                        }
                    }
                    _ = &mut conn_task => {
                        continue 'connection;
                    }
                }
            }
        }
    }

    statistics
}

/// Sends a JSON-RPC request over an established sender and returns the response.
async fn post_json<S>(
    sender: &mut S,
    req_uri: &hyper::Uri,
    headers: &HeaderMap,
    body: Vec<u8>,
    context: &'static str,
) -> Result<http::Response<hyper::body::Incoming>, Box<dyn std::error::Error + Send + Sync>>
where
    S: RequestSender<Full<Bytes>>,
{
    let mut req = Request::new(Full::new(Bytes::from(body)));
    *req.method_mut() = http::Method::POST;
    *req.uri_mut() = req_uri.clone();
    *req.headers_mut() = headers.clone();

    sender
        .ready()
        .await
        .map_err(|e| format!("{context}: sender not ready: {e}"))?;
    sender
        .send_request(req)
        .await
        .map_err(|e| format!("{context} failed: {e}").into())
}

/// Collects a JSON-RPC response body, transparently unwrapping SSE framing.
///
/// Returns the raw JSON bytes: `serde_json::from_slice` can parse them without
/// an intermediate UTF-8 validation copy.
async fn collect_json_body(
    response: http::Response<hyper::body::Incoming>,
    context: &'static str,
) -> Result<Bytes, Box<dyn std::error::Error + Send + Sync>> {
    let is_sse = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains(MIME_TEXT_EVENT_STREAM));

    let body = response
        .into_body()
        .collect()
        .await
        .map_err(|e| format!("failed to read {context} response body: {e}"))?
        .to_bytes();

    if is_sse {
        extract_sse_data(&String::from_utf8_lossy(&body)).map(Bytes::from)
    } else {
        Ok(body)
    }
}

/// Pre-compiles a `tools/call` JSON-RPC body per tool, sharing one RNG.
fn compile_tool_bodies(tools: &[Tool], opts: &Options) -> Vec<Bytes> {
    let mut rng = rand::rng();
    tools
        .iter()
        .enumerate()
        .map(|(idx, tool)| {
            let call_params = match create_tool_request(&tool.input_schema, opts, &mut rng) {
                Some(args) => CallToolRequestParams::new(tool.name.clone()).with_arguments(args),
                None => CallToolRequestParams::new(tool.name.clone()),
            };

            let call_request = CallToolRequest::new(call_params);

            let call_jsonrpc = JsonRpcRequest {
                jsonrpc: Default::default(),
                id: NumberOrString::Number((idx + 100) as i64),
                request: call_request,
            };

            serde_json::to_vec(&call_jsonrpc)
                .unwrap_or_else(|e| {
                    fatal!(
                        3,
                        "failed to serialize tools/call request for {}: {e}",
                        tool.name
                    )
                })
                .into()
        })
        .collect()
}

/// Generates a tool request with fuzzy values based on the input schema.
/// Returns None if the schema is invalid or missing required fields.
fn create_tool_request<R: RngExt>(
    arguments: &JsonObject,
    opts: &Options,
    rng: &mut R,
) -> Option<Map<String, Value>> {
    let required = arguments.get("required")?.as_array()?;
    let properties = arguments.get("properties")?.as_object()?;

    let mut generated_request_args = Map::with_capacity(required.len());

    for field_name in required.iter().filter_map(|v| v.as_str()) {
        let field_schema = properties.get(field_name)?;
        let value = generate_value_from_schema(field_schema, rng, opts, 0);
        generated_request_args.insert(field_name.to_owned(), value);
    }

    Some(generated_request_args)
}

const MAX_RECURSION_DEPTH: usize = 10;

fn generate_value_from_schema<R: RngExt>(
    schema: &Value,
    rng: &mut R,
    opts: &Options,
    depth: usize,
) -> Value {
    // Prevent infinite recursion with self-referencing schemas
    if depth >= MAX_RECURSION_DEPTH {
        return Value::Null;
    }

    if let Some(default) = schema.get("default") {
        return default.clone();
    }
    if let Some(c) = schema.get("const") {
        return c.clone();
    }
    if let Some(variants) = schema.get("enum").and_then(|v| v.as_array())
        && !variants.is_empty()
    {
        return variants[rng.random_range(0..variants.len())].clone();
    }

    // `type` may be a single name or an array (e.g. ["string", "null"]);
    // pick the first non-null member without allocating.
    let type_str: &str = match schema.get("type") {
        Some(Value::String(s)) => s.as_str(),
        Some(Value::Array(types)) => types
            .iter()
            .filter_map(Value::as_str)
            .find(|t| *t != "null")
            .unwrap_or("string"),
        _ => "string",
    };

    match type_str {
        "string" => {
            let len = opts
                .mcp_rand_string_len
                .unwrap_or_else(|| rng.random_range(5..20));
            let mut s = String::with_capacity(len);
            for _ in 0..len {
                s.push(rng.sample(rand::distr::Alphanumeric) as char);
            }
            Value::String(s)
        }
        "integer" => {
            let lo = schema.get("minimum").and_then(Value::as_i64).unwrap_or(0);
            let hi = schema
                .get("maximum")
                .and_then(Value::as_i64)
                .unwrap_or(1000);
            let (lo, hi) = if hi > lo { (lo, hi) } else { (0, 1000) };
            Value::from(rng.random_range(lo..hi))
        }
        "number" => {
            let lo = schema
                .get("minimum")
                .and_then(Value::as_f64)
                .unwrap_or(-1000.0);
            let hi = schema
                .get("maximum")
                .and_then(Value::as_f64)
                .unwrap_or(1000.0);
            let (lo, hi) = if hi > lo { (lo, hi) } else { (-1000.0, 1000.0) };
            Value::from(rng.random_range(lo..hi))
        }
        "boolean" => Value::Bool(rng.random_bool(0.5)),
        "array" => {
            let len = rng.random_range(1..4);
            let items_schema = schema.get("items");
            let mut items = Vec::with_capacity(len);
            for _ in 0..len {
                items.push(match items_schema {
                    Some(s) => generate_value_from_schema(s, rng, opts, depth + 1),
                    None => generate_primitive_value(rng),
                });
            }
            Value::Array(items)
        }
        "object" => {
            let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) else {
                return Value::Object(Map::new());
            };
            let Some(required) = schema.get("required").and_then(|r| r.as_array()) else {
                return Value::Object(Map::new());
            };
            let mut obj = Map::with_capacity(required.len());
            for field_name in required.iter().filter_map(|v| v.as_str()) {
                if let Some(field_schema) = properties.get(field_name) {
                    obj.insert(
                        field_name.to_owned(),
                        generate_value_from_schema(field_schema, rng, opts, depth + 1),
                    );
                }
            }
            Value::Object(obj)
        }
        "null" => Value::Null,
        _ => {
            // Unknown type, default to string
            Value::String(String::new())
        }
    }
}

/// Generates a primitive value when no schema is available.
/// Used as a fallback for array items without a defined schema.
fn generate_primitive_value<R: RngExt>(rng: &mut R) -> Value {
    match rng.random_range(0..4) {
        0 => Value::from("sample"),
        1 => Value::from(rng.random_range(-100..100i64)),
        2 => Value::from(rng.random_range(0..100i64)),
        _ => Value::Bool(rng.random_bool(0.5)),
    }
}

/// Initialize MCP connection via Streamable HTTP transport using hyper sender.
///
/// 1. Sends MCP `initialize` request and extracts `Mcp-Session-Id` from response header
/// 2. Sends `notifications/initialized` notification
/// 3. Sends `tools/list` request to get available tools
/// 4. Pre-compiles JSON-RPC bodies for each tool's `tools/call` invocation
async fn mcp_streamable_http_initialize<S>(
    uri: &hyper::Uri,
    base_headers: &http::HeaderMap,
    sender: &mut S,
    opts: &Options,
) -> Result<McpSetup, Box<dyn std::error::Error + Send + Sync>>
where
    S: RequestSender<Full<Bytes>>,
{
    // Step 1: Send MCP initialize request
    // Note: no client capabilities advertised (roots was removed by SEP-2577).
    let init_params = InitializeRequestParams::new(
        ClientCapabilities::builder().build(),
        Implementation::new("plumbrs".to_string(), env!("CARGO_PKG_VERSION").to_string()),
    );

    let init_request = InitializeRequest::new(init_params);

    let init_jsonrpc = JsonRpcRequest {
        jsonrpc: Default::default(),
        id: NumberOrString::Number(1),
        request: init_request,
    };

    let init_body = serde_json::to_vec(&init_jsonrpc)?;
    let req_uri = request_uri(uri, opts.absolute_uri || opts.http2);

    let response = post_json(sender, &req_uri, base_headers, init_body, "initialize").await?;

    // Extract session ID from response headers before consuming the body
    let session_id = response
        .headers()
        .get(HEADER_MCP_SESSION_ID.clone())
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_owned());

    if response.status() != StatusCode::OK {
        return Err(format!(
            "initialize request failed with status: {}",
            response.status()
        )
        .into());
    }

    let body = collect_json_body(response, "initialize").await?;
    let _init_result: JsonRpcResponse<InitializeResult> = serde_json::from_slice(&body)
        .map_err(|e| format!("failed to parse initialize response: {e}"))?;

    eprintln!(
        "MCP Streamable HTTP: initialized successfully, session_id={:?}",
        session_id
    );

    // Build headers with session ID for subsequent requests
    let mut headers_with_session = base_headers.clone();
    if let Some(ref sid) = session_id {
        headers_with_session.insert(
            HEADER_MCP_SESSION_ID.clone(),
            http::header::HeaderValue::from_str(sid)?,
        );
    }

    // Step 2: Send initialized notification
    let initialized_notif = InitializedNotification::default();

    let notif_jsonrpc = rmcp::model::JsonRpcNotification {
        jsonrpc: Default::default(),
        notification: initialized_notif,
    };

    let initialized_body = serde_json::to_vec(&notif_jsonrpc)?;

    let response = post_json(
        sender,
        &req_uri,
        &headers_with_session,
        initialized_body,
        "initialized notification",
    )
    .await?;

    // Notification may return 200 OK, 202 Accepted, or 204 No Content
    if !matches!(
        response.status(),
        StatusCode::OK | StatusCode::ACCEPTED | StatusCode::NO_CONTENT
    ) {
        return Err(format!(
            "initialized notification failed with status: {}",
            response.status()
        )
        .into());
    }

    // Consume the response body
    let _ = response.into_body().collect().await;

    // Step 3: Send tools/list request
    let tools_list_request = ListToolsRequest::default();

    let tools_list_jsonrpc = JsonRpcRequest {
        jsonrpc: Default::default(),
        id: NumberOrString::Number(2),
        request: tools_list_request,
    };

    let tools_list_body = serde_json::to_vec(&tools_list_jsonrpc)?;

    let response = post_json(
        sender,
        &req_uri,
        &headers_with_session,
        tools_list_body,
        "tools/list",
    )
    .await?;

    if response.status() != StatusCode::OK {
        return Err(format!(
            "tools/list request failed with status: {}",
            response.status()
        )
        .into());
    }

    let body = collect_json_body(response, "tools/list").await?;
    let tools_result: JsonRpcResponse<ListToolsResult> = serde_json::from_slice(&body)
        .map_err(|e| format!("failed to parse tools/list response: {e}"))?;

    let tools = &tools_result.result.tools;

    if tools.is_empty() {
        return Err("no tools available from MCP server".into());
    }

    eprintln!(
        "MCP Streamable HTTP: found {} tools: {:?}",
        tools.len(),
        tools.iter().map(|t| t.name.as_ref()).collect::<Vec<_>>()
    );

    // Step 4: Pre-compile JSON bodies for each tool's tools/call invocation
    let tool_bodies = compile_tool_bodies(tools, opts);

    Ok(McpSetup {
        uri: uri.clone(),
        tool_bodies,
        sse_task: None,
        session_id,
    })
}

/// Extract JSON data from SSE format response (accumulating multiple data lines)
fn extract_sse_data(body: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let mut buffer = String::with_capacity(body.len());
    let mut found = false;

    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            found = true;
            buffer.push_str(rest.strip_prefix(' ').unwrap_or(rest));
            buffer.push('\n');
        }
    }

    if found {
        Ok(buffer)
    } else {
        Err("no data field found in SSE response".into())
    }
}

/// Helper to read a complete SSE event from an Incoming body stream.
/// Accumulates "data:" lines until an empty line is encountered.
async fn read_sse_event(
    body: &mut hyper::body::Incoming,
    buffer: &mut String,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let mut event_data = String::with_capacity(256);

    loop {
        if let Some(idx) = buffer.find('\n') {
            // Borrow the line, copy its payload into `event_data`, then drain.
            // This avoids the per-line String allocation of drain-then-collect.
            // (`\n` is single-byte: slicing a UTF-8 String at its index is safe.)
            let empty_line = {
                let line = buffer[..idx].trim_end();
                if line.is_empty() {
                    true
                } else {
                    if let Some(rest) = line.strip_prefix("data:") {
                        event_data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
                        event_data.push('\n');
                    }
                    // ignore other fields like event:, id:, retry:
                    false
                }
            };
            buffer.drain(..=idx);
            if empty_line && !event_data.is_empty() {
                return Ok(event_data);
            }
            // heartbeat or empty event: keep reading
            continue;
        }

        match body.frame().await {
            Some(Ok(frame)) => {
                if let Some(chunk) = frame.data_ref() {
                    // Fast path: SSE payloads are UTF-8; only fall back to
                    // lossy conversion for invalid sequences.
                    match std::str::from_utf8(chunk) {
                        Ok(s) => buffer.push_str(s),
                        Err(_) => buffer.push_str(&String::from_utf8_lossy(chunk)),
                    }
                }
            }
            Some(Err(e)) => return Err(format!("SSE read error: {e}").into()),
            None => return Err("SSE stream ended".into()),
        }
    }
}

/// Initialize MCP connection via SSE handshake, fetch available tools,
/// and prepare pre-compiled JSON bodies for tools/call requests.
///
/// 1. Connects to the SSE endpoint and reads the message URI from the `data:` field
/// 2. Sends MCP `initialize` request and waits for response
/// 3. Sends `notifications/initialized` notification
/// 4. Sends `tools/list` request to get available tools
/// 5. Pre-compiles JSON-RPC bodies for each tool's `tools/call` invocation
pub async fn mcp_sse_initialize<B>(uri: &str, opts: &Options, headers: &HeaderMap) -> McpSetup
where
    B: HttpConnectionBuilder,
{
    use crate::stats::{RealtimeStats, Statistics};

    let base_uri: hyper::Uri = uri
        .parse()
        .unwrap_or_else(|e| fatal!(3, "invalid base uri: {e}"));

    let (host, port) =
        get_conn_address(opts, &base_uri).unwrap_or_else(|| fatal!(3, "no host in uri"));
    let endpoint: &'static str = build_conn_endpoint(&host, port);
    let tls_name = tls_server_name(opts, &base_uri);

    // Create dummy stats for connection building
    let mut stats = Statistics::new(false);
    let rt_stats = RealtimeStats::default();

    // Step 1: SSE handshake to get the message endpoint
    // Build connection for SSE GET request
    let (mut sse_sender, sse_conn_task) =
        B::build_connection::<Full<Bytes>>(endpoint, tls_name, &mut stats, &rt_stats, opts)
            .await
            .unwrap_or_else(|| fatal!(3, "SSE connection failed"));

    // Build SSE GET request
    let sse_req_uri = request_uri(&base_uri, opts.absolute_uri || opts.http2);
    let mut sse_req_builder = Request::builder()
        .method(http::Method::GET)
        .uri(sse_req_uri)
        .header(http::header::ACCEPT, MIME_TEXT_EVENT_STREAM)
        .header(http::header::CACHE_CONTROL, "no-cache");

    for (key, value) in headers.iter() {
        sse_req_builder = sse_req_builder.header(key, value);
    }

    let sse_req = sse_req_builder
        .body(Full::new(Bytes::new()))
        .unwrap_or_else(|e| fatal!(3, "failed to build SSE request: {e}"));

    let sse_response = sse_sender
        .send_request(sse_req)
        .await
        .unwrap_or_else(|e| fatal!(3, "SSE handshake request failed: {e}"));

    let mut sse_body = sse_response.into_body();
    let mut buffer = String::new();

    let endpoint_data = read_sse_event(&mut sse_body, &mut buffer)
        .await
        .unwrap_or_else(|e| fatal!(3, "failed to read SSE handshake event: {e}"));

    let new_path = endpoint_data.trim().to_string();

    if new_path.is_empty() {
        fatal!(3, "could not find endpoint in SSE handshake");
    }

    let new_uri = if new_path.starts_with("http://") || new_path.starts_with("https://") {
        new_path
            .parse::<hyper::Uri>()
            .unwrap_or_else(|e| fatal!(3, "invalid uri from SSE: {e}"))
    } else {
        let mut parts = base_uri.clone().into_parts();
        parts.path_and_query = Some(
            new_path
                .parse()
                .unwrap_or_else(|e| fatal!(3, "invalid path from SSE: {e}")),
        );
        hyper::Uri::from_parts(parts).unwrap_or_else(|e| fatal!(3, "invalid new uri: {e}"))
    };
    let post_uri = request_uri(&new_uri, opts.absolute_uri || opts.http2);

    let (mut post_sender, _) =
        Http1::build_connection::<Full<Bytes>>(endpoint, tls_name, &mut stats, &rt_stats, opts)
            .await
            .unwrap_or_else(|| fatal!(3, "POST connection failed"));

    // POST a JSON-RPC message and drain its (usually empty) response body so
    // the connection is reusable for the next message.
    async fn send_post<S>(uri: &hyper::Uri, headers: &HeaderMap, body: Vec<u8>, sender: &mut S)
    where
        S: RequestSender<Full<Bytes>>,
    {
        let response = post_json(sender, uri, headers, body, "POST")
            .await
            .unwrap_or_else(|e| fatal!(3, "POST request failed: {e}"));
        let _ = response.into_body().collect().await;
    }

    // Step 2: Send MCP initialize request (no client capabilities: roots was removed by SEP-2577)
    let init_params = InitializeRequestParams::new(
        ClientCapabilities::builder().build(),
        Implementation::new("plumbrs".to_string(), env!("CARGO_PKG_VERSION").to_string()),
    );

    let init_request = InitializeRequest::new(init_params);

    let init_jsonrpc = JsonRpcRequest {
        jsonrpc: Default::default(),
        id: NumberOrString::Number(1),
        request: init_request,
    };

    let init_body = serde_json::to_vec(&init_jsonrpc)
        .unwrap_or_else(|e| fatal!(3, "failed to serialize initialize request: {e}"));

    // Send the initialize request via POST
    send_post(&post_uri, headers, init_body, &mut post_sender).await;

    // Read the initialize response from the SSE stream
    let init_response_body = read_sse_event(&mut sse_body, &mut buffer)
        .await
        .unwrap_or_else(|e| fatal!(3, "failed to read initialize response: {e}"));

    let init_result: JsonRpcResponse<InitializeResult> = serde_json::from_str(&init_response_body)
        .unwrap_or_else(|e| fatal!(3, "failed to parse initialize response: {e}"));

    // JsonRpcResponse doesn't have error field - check if result is valid
    let _ = init_result.result;

    eprintln!("MCP: initialized successfully");

    // Step 3: Send initialized notification
    let initialized_notif = InitializedNotification::default();

    let notif_jsonrpc = rmcp::model::JsonRpcNotification {
        jsonrpc: Default::default(),
        notification: initialized_notif,
    };

    let initialized_body = serde_json::to_vec(&notif_jsonrpc)
        .unwrap_or_else(|e| fatal!(3, "failed to serialize initialized notification: {e}"));

    send_post(&post_uri, headers, initialized_body, &mut post_sender).await;

    // Step 4: Send tools/list request
    let tools_list_request = ListToolsRequest::default();

    let tools_list_jsonrpc = JsonRpcRequest {
        jsonrpc: Default::default(),
        id: NumberOrString::Number(2),
        request: tools_list_request,
    };

    let tools_list_body = serde_json::to_vec(&tools_list_jsonrpc)
        .unwrap_or_else(|e| fatal!(3, "failed to serialize tools/list request: {e}"));

    // Send the request via POST
    send_post(&post_uri, headers, tools_list_body, &mut post_sender).await;

    // Read the response from the SSE stream, handling any server requests (like roots/list)
    let tools_response_body;

    loop {
        let data = read_sse_event(&mut sse_body, &mut buffer)
            .await
            .unwrap_or_else(|e| fatal!(3, "failed to read SSE event (tools/list): {e}"));

        // Fast path: JSON-RPC responses carry no "method" member.
        if !data.contains("\"method\"") {
            tools_response_body = data;
            break;
        }

        // Check if this is a server request (has "method" and "id")
        if let Ok(server_req) = serde_json::from_str::<serde_json::Value>(&data) {
            if let Some(method) = server_req.get("method").and_then(|m| m.as_str())
                && let Some(req_id) = server_req.get("id")
            {
                // Roots were removed by SEP-2577, so we don't advertise the
                // capability; answer legacy servers with MethodNotFound.
                if method == "roots/list" {
                    let roots_error = JsonRpcError::new(
                        Some(
                            req_id
                                .as_i64()
                                .map(NumberOrString::Number)
                                .or_else(|| {
                                    req_id.as_str().map(|s| NumberOrString::String(s.into()))
                                })
                                .unwrap_or(NumberOrString::Number(0)),
                        ),
                        ErrorData::new(
                            ErrorCode::METHOD_NOT_FOUND,
                            "roots/list is not supported (removed by SEP-2577)",
                            None,
                        ),
                    );

                    let roots_body = serde_json::to_vec(&roots_error).unwrap_or_else(|e| {
                        fatal!(3, "failed to serialize roots/list response: {e}")
                    });

                    send_post(&post_uri, headers, roots_body, &mut post_sender).await;

                    continue; // Keep reading for tools/list response
                }
            }

            // If it's not roots/list, assume it's our response
            // (Strictly we should check id=2, but let's be robust)
            if server_req.get("method").is_none() {
                tools_response_body = data;
                break;
            }
        }
    }

    let tools_result: JsonRpcResponse<ListToolsResult> = serde_json::from_str(&tools_response_body)
        .unwrap_or_else(|e| fatal!(3, "failed to parse tools/list response: {e}"));

    let tools = &tools_result.result.tools;

    if tools.is_empty() {
        fatal!(3, "no tools available from MCP server");
    }

    eprintln!(
        "MCP: found {} tools: {:?}",
        tools.len(),
        tools.iter().map(|t| t.name.as_ref()).collect::<Vec<_>>()
    );

    // Step 5: Pre-compile JSON bodies for each tool's tools/call invocation
    let tool_bodies = compile_tool_bodies(tools, opts);

    // Spawn task to keep SSE connection alive
    let sse_task = tokio::spawn(async move {
        // Keep the connection task alive
        let _conn = sse_conn_task;
        while let Some(frame) = sse_body.frame().await {
            if let Err(e) = frame {
                eprintln!("SSE: error reading frame: {e}");
            }
        }
        eprintln!("SSE: connection closed.");
    });

    McpSetup {
        uri: new_uri,
        tool_bodies,
        sse_task: Some(sse_task),
        session_id: None,
    }
}
