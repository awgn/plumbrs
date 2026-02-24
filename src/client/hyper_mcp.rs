use crate::Options;
use crate::stats::{RealtimeStats, Statistics};

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;

use http::{HeaderMap, Request, StatusCode, header};

use rand::Rng;
use rmcp::model::{
    CallToolRequest, CallToolRequestParam, ClientCapabilities, Implementation, InitializeRequest,
    InitializeRequestParam, InitializeResult, InitializedNotification, JsonObject, JsonRpcRequest,
    JsonRpcResponse, ListRootsResult, ListToolsRequest, ListToolsResult, NumberOrString,
    ProtocolVersion,
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
        http_hyper_mcp_client::<Http2>(tid, cid, opts, rt_stats).await
    } else {
        http_hyper_mcp_client::<Http1>(tid, cid, opts, rt_stats).await
    }
}

async fn http_hyper_mcp_client<B: HttpConnectionBuilder>(
    tid: usize,
    cid: usize,
    opts: Arc<Options>,
    rt_stats: &RealtimeStats,
) -> Statistics {
    let mut statistics = Statistics::new(opts.latency);
    let mut total: u32 = 0;
    let mut banner = HashSet::new();
    let uri_str = opts.uri[cid % opts.uri.len()].as_str();
    let mut uri = uri_str
        .parse::<hyper::Uri>()
        .unwrap_or_else(|e| fatal!(1, "invalid uri: {e}"));

    let (mut host, mut port) =
        get_conn_address(&opts, &uri).unwrap_or_else(|| fatal!(1, "no host specified in uri"));
    let mut endpoint = build_conn_endpoint(&host, port);

    let mut headers = build_headers(&uri, opts.as_ref())
        .unwrap_or_else(|e| fatal!(2, "could not build headers: {e}"));

    // For SSE transport, initialize before connection loop
    let mut mcp = McpSetup::default();
    if opts.mcp_sse {
        mcp = mcp_sse_initialize::<B>(&uri_str, opts.as_ref(), &headers).await;
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

    let clock = quanta::Clock::new();
    let start = Instant::now();
    'connection: loop {
        if should_stop(total, start, &opts) {
            break 'connection;
        }

        if cid < opts.uri.len() && !banner.contains(uri_str) {
            banner.insert(uri_str.to_owned());
            println!(
                "hyper-mcp [{tid:>2}] -> connecting to {}:{}, method = POST uri = {} {} (transport {transport})...",
                host,
                port,
                uri,
                B::SCHEME
            );
        }

        let (mut sender, mut conn_task) =
            match B::build_connection(endpoint, &mut statistics, rt_stats, &opts).await {
                Some(s) => s,
                None => {
                    total += 1;
                    continue 'connection;
                }
            };

        statistics.inc_conn();

        // For Streamable HTTP transport, initialize after connection is established
        if !opts.mcp_sse && mcp.tool_bodies.is_empty() {
            match mcp_streamable_http_initialize(&uri, &headers, &mut sender).await {
                Ok(setup) => {
                    mcp = setup;
                    // Add session ID to headers for subsequent requests
                    if let Some(ref session_id) = mcp.session_id {
                        headers.insert(
                            http::header::HeaderName::from_static("mcp-session-id"),
                            http::header::HeaderValue::from_str(session_id)
                                .unwrap_or_else(|e| fatal!(3, "invalid session id: {e}")),
                        );
                    }
                }
                Err(e) => {
                    fatal!(3, "MCP Streamable HTTP initialization failed: {e}");
                }
            }
        }

        let bodies: Vec<Full<Bytes>> = mcp.tool_bodies.clone().into_iter().map(Full::new).collect();

        loop {
            let body = bodies
                .get(total as usize % bodies.len())
                .cloned()
                .unwrap_or_else(|| Full::new(Bytes::from("")));

            let mut req = Request::new(body);
            // MCP JSON-RPC requests must use POST method
            *req.method_mut() = http::Method::POST;
            *req.uri_mut() = uri.clone();
            *req.headers_mut() = headers.clone();

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
                hist.record(clock.delta_as_nanos(start_lat, clock.raw()) / 1000).ok();
            };

            total += 1;

            if should_stop(total, start, &opts) {
                break 'connection;
            }

            if opts.cps {
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

/// Generates a tool request with fuzzy values based on the input schema.
/// Returns None if the schema is invalid or missing required fields.
fn create_tool_request(arguments: &Arc<JsonObject>) -> Option<Map<String, Value>> {
    let required = arguments.get("required").and_then(|v| v.as_array())?;
    let properties = arguments.get("properties").and_then(|v| v.as_object())?;

    let mut generated_request_args = Map::new();
    let mut rng = rand::thread_rng();

    for required_arg in required {
        let field_name = required_arg.as_str()?;
        let field_schema = properties.get(field_name)?;

        let value = generate_value_from_schema(field_schema, &mut rng, 0);
        generated_request_args.insert(field_name.to_string(), value);
    }

    Some(generated_request_args)
}

const MAX_RECURSION_DEPTH: usize = 10;

fn generate_value_from_schema<R: Rng>(schema: &Value, rng: &mut R, depth: usize) -> Value {
    // Prevent infinite recursion with self-referencing schemas
    if depth >= MAX_RECURSION_DEPTH {
        return Value::Null;
    }

    if let Some(default) = schema.get("default") {
        return default.clone();
    }

    let type_str = schema
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("string");

    match type_str {
        "string" => {
            let len = rng.gen_range(5..20);
            let s: String = (0..len)
                .map(|_| rng.sample(rand::distributions::Alphanumeric) as char)
                .collect();
            Value::String(s)
        }
        "number" => Value::Number(rng.gen_range(-1000..1000i64).into()),
        "integer" => Value::Number(rng.gen_range(0..1000).into()),
        "boolean" => Value::Bool(rng.gen_bool(0.5)),
        "array" => {
            let len = rng.gen_range(1..4);
            let items_schema = schema.get("items");
            let items: Vec<Value> = (0..len)
                .map(|_| {
                    items_schema
                        .map(|s| generate_value_from_schema(s, rng, depth + 1))
                        .unwrap_or_else(|| generate_primitive_value(rng))
                })
                .collect();
            Value::Array(items)
        }
        "object" => {
            let mut obj = Map::new();
            if let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) {
                if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
                    // Generate values for required properties
                    for required_field in required {
                        if let Some(field_name) = required_field.as_str() {
                            if let Some(field_schema) = properties.get(field_name) {
                                let value = generate_value_from_schema(field_schema, rng, depth + 1);
                                obj.insert(field_name.to_string(), value);
                            }
                        }
                    }
                }
            }
            Value::Object(obj)
        }
        "null" => Value::Null,
        _ => {
            // Unknown type, default to string
            Value::String("".to_string())
        }
    }
}

/// Generates a primitive value when no schema is available.
/// Used as a fallback for array items without a defined schema.
fn generate_primitive_value<R: Rng>(rng: &mut R) -> Value {
    match rng.gen_range(0..4) {
        0 => Value::String("sample".to_string()),
        1 => Value::Number(rng.gen_range(-100..100i64).into()),
        2 => Value::Number(rng.gen_range(0..100).into()),
        3 => Value::Bool(rng.gen_bool(0.5)),
        _ => Value::Null,
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
) -> Result<McpSetup, Box<dyn std::error::Error + Send + Sync>>
where
    S: RequestSender<Full<Bytes>>,
{
    // Step 1: Send MCP initialize request
    let init_params = InitializeRequestParam {
        protocol_version: ProtocolVersion::LATEST,
        capabilities: ClientCapabilities::builder().enable_roots().build(),
        client_info: Implementation {
            name: "plumbrs".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            ..Default::default()
        },
    };

    let init_request = InitializeRequest::new(init_params);

    let init_jsonrpc = JsonRpcRequest {
        jsonrpc: Default::default(),
        id: NumberOrString::Number(1),
        request: init_request,
    };

    let init_body = serde_json::to_vec(&init_jsonrpc)?;

    let mut req = Request::new(Full::new(Bytes::from(init_body)));
    *req.method_mut() = http::Method::POST;
    *req.uri_mut() = uri.clone();
    *req.headers_mut() = base_headers.clone();

    let response = sender
        .send_request(req)
        .await
        .map_err(|e| format!("initialize request failed: {e}"))?;

    // Extract session ID from response headers
    let session_id = response
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    if response.status() != StatusCode::OK {
        return Err(format!(
            "initialize request failed with status: {}",
            response.status()
        )
        .into());
    }

    // Check content-type to determine how to parse response
    let content_type = response
        .headers()
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or(MIME_APPLICATION_JSON)
        .to_string();

    // Read response body
    let body_bytes = response
        .into_body()
        .collect()
        .await
        .map_err(|e| format!("failed to read initialize response body: {e}"))?
        .to_bytes();

    // Parse JSON - either directly or from SSE data field
    let json_body = if content_type.contains(MIME_TEXT_EVENT_STREAM) {
        // Parse SSE format: extract JSON from "data:" line
        let body_str = String::from_utf8_lossy(&body_bytes);
        extract_sse_data(&body_str)?
    } else {
        String::from_utf8_lossy(&body_bytes).to_string()
    };

    let _init_result: JsonRpcResponse<InitializeResult> = serde_json::from_str(&json_body)
        .map_err(|e| format!("failed to parse initialize response: {e}"))?;

    eprintln!(
        "MCP Streamable HTTP: initialized successfully, session_id={:?}",
        session_id
    );

    // Build headers with session ID for subsequent requests
    let mut headers_with_session = base_headers.clone();
    if let Some(ref sid) = session_id {
        headers_with_session.insert(
            http::header::HeaderName::from_static("mcp-session-id"),
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

    let mut req = Request::new(Full::new(Bytes::from(initialized_body)));
    *req.method_mut() = http::Method::POST;
    *req.uri_mut() = uri.clone();
    *req.headers_mut() = headers_with_session.clone();

    // Wait for sender to be ready
    sender
        .ready()
        .await
        .map_err(|e| format!("sender not ready: {e}"))?;

    let response = sender
        .send_request(req)
        .await
        .map_err(|e| format!("initialized notification failed: {e}"))?;

    // Notification may return 200 OK, 202 Accepted, or 204 No Content
    if response.status() != StatusCode::OK
        && response.status() != StatusCode::ACCEPTED
        && response.status() != StatusCode::NO_CONTENT
    {
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

    let mut req = Request::new(Full::new(Bytes::from(tools_list_body)));
    *req.method_mut() = http::Method::POST;
    *req.uri_mut() = uri.clone();
    *req.headers_mut() = headers_with_session.clone();

    // Wait for sender to be ready
    sender
        .ready()
        .await
        .map_err(|e| format!("sender not ready: {e}"))?;

    let response = sender
        .send_request(req)
        .await
        .map_err(|e| format!("tools/list request failed: {e}"))?;

    if response.status() != StatusCode::OK {
        return Err(format!(
            "tools/list request failed with status: {}",
            response.status()
        )
        .into());
    }

    // Check content-type to determine how to parse response
    let content_type = response
        .headers()
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or(MIME_APPLICATION_JSON)
        .to_string();

    // Read and parse tools/list response
    let body_bytes = response
        .into_body()
        .collect()
        .await
        .map_err(|e| format!("failed to read tools/list response body: {e}"))?
        .to_bytes();

    // Parse JSON - either directly or from SSE data field
    let json_body = if content_type.contains(MIME_TEXT_EVENT_STREAM) {
        let body_str = String::from_utf8_lossy(&body_bytes);
        extract_sse_data(&body_str)?
    } else {
        String::from_utf8_lossy(&body_bytes).to_string()
    };

    let tools_result: JsonRpcResponse<ListToolsResult> = serde_json::from_str(&json_body)
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
    let tool_bodies: Vec<Bytes> = tools
        .iter()
        .enumerate()
        .map(|(idx, tool)| {
            let args = create_tool_request(&tool.input_schema);
            let call_params = CallToolRequestParam {
                name: tool.name.clone(),
                arguments: args,
            };

            let call_request = CallToolRequest::new(call_params);

            let call_jsonrpc = JsonRpcRequest {
                jsonrpc: Default::default(),
                id: NumberOrString::Number((idx + 100) as i64),
                request: call_request,
            };

            let json = serde_json::to_vec(&call_jsonrpc).unwrap_or_else(|e| {
                fatal!(
                    3,
                    "failed to serialize tools/call request for {}: {e}",
                    tool.name
                )
            });
            Bytes::from(json)
        })
        .collect();

    Ok(McpSetup {
        uri: uri.clone(),
        tool_bodies,
        sse_task: None,
        session_id,
    })
}

/// Extract JSON data from SSE format response (accumulating multiple data lines)
fn extract_sse_data(body: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let mut buffer = String::new();
    let mut found = false;

    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            found = true;
            let content = rest.strip_prefix(' ').unwrap_or(rest);
            buffer.push_str(content);
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
    let mut event_data = String::new();

    loop {
        if let Some(idx) = buffer.find('\n') {
            let line_full: String = buffer.drain(..=idx).collect();
            let line = line_full.trim_end();

            if line.is_empty() {
                if !event_data.is_empty() {
                    return Ok(event_data);
                }
                // heartbeat or empty event, continue reading
                continue;
            }

            if let Some(rest) = line.strip_prefix("data:") {
                let content = rest.strip_prefix(' ').unwrap_or(rest);
                event_data.push_str(content);
                event_data.push('\n');
            }
            // ignore other fields like event:, id:, retry:
            continue;
        }

        match body.frame().await {
            Some(Ok(frame)) => {
                if let Some(chunk) = frame.data_ref() {
                    buffer.push_str(&String::from_utf8_lossy(chunk));
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

    let host = base_uri
        .host()
        .unwrap_or_else(|| fatal!(3, "no host in uri"))
        .to_owned();
    let port = base_uri.port_u16().unwrap_or(80);
    let endpoint: &'static str = build_conn_endpoint(&host, port);

    // Create dummy stats for connection building
    let mut stats = Statistics::new(false);
    let rt_stats = RealtimeStats::default();

    // Step 1: SSE handshake to get the message endpoint
    // Build connection for SSE GET request
    let (mut sse_sender, sse_conn_task) =
        B::build_connection::<Full<Bytes>>(endpoint, &mut stats, &rt_stats, opts)
            .await
            .unwrap_or_else(|| fatal!(3, "SSE connection failed"));

    // Build SSE GET request
    let mut sse_req_builder = Request::builder()
        .method(http::Method::GET)
        .uri(base_uri.clone())
        .header(http::header::HOST, format!("{}:{}", host, port))
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

    let (mut post_sender, _) =
        Http1::build_connection::<Full<Bytes>>(endpoint, &mut stats, &rt_stats, opts)
            .await
            .unwrap_or_else(|| fatal!(3, "POST connection failed"));

    // Helper function to send POST request
    async fn send_post<S>(
        uri: &hyper::Uri,
        headers: &HeaderMap,
        body: Vec<u8>,
        sender: &mut S,
    ) -> http::Response<hyper::body::Incoming>
    where
        S: RequestSender<Full<Bytes>>,
    {
        let mut req_builder = Request::builder()
            .method(http::Method::POST)
            .uri(uri.clone())
            .header(http::header::CONTENT_TYPE, MIME_APPLICATION_JSON);

        for (key, value) in headers.iter() {
            req_builder = req_builder.header(key, value);
        }

        let req = req_builder
            .body(Full::new(Bytes::from(body)))
            .unwrap_or_else(|e| fatal!(3, "failed to build POST request: {e}"));

        sender
            .send_request(req)
            .await
            .unwrap_or_else(|e| fatal!(3, "POST request failed: {e}"))
    }

    // Step 2: Send MCP initialize request
    let init_params = InitializeRequestParam {
        protocol_version: ProtocolVersion::LATEST,
        capabilities: ClientCapabilities::builder().enable_roots().build(),
        client_info: Implementation {
            name: "plumbrs".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            ..Default::default()
        },
    };

    let init_request = InitializeRequest::new(init_params);

    let init_jsonrpc = JsonRpcRequest {
        jsonrpc: Default::default(),
        id: NumberOrString::Number(1),
        request: init_request,
    };

    let init_body = serde_json::to_vec(&init_jsonrpc)
        .unwrap_or_else(|e| fatal!(3, "failed to serialize initialize request: {e}"));

    // Send the initialize request via POST
    let _ = send_post(&new_uri, headers, init_body, &mut post_sender).await;

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

    let _ = send_post(&new_uri, headers, initialized_body, &mut post_sender).await;

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
    let _ = send_post(&new_uri, headers, tools_list_body, &mut post_sender).await;

    // Read the response from the SSE stream, handling any server requests (like roots/list)
    let tools_response_body;

    loop {
        let data = read_sse_event(&mut sse_body, &mut buffer)
            .await
            .unwrap_or_else(|e| fatal!(3, "failed to read SSE event (tools/list): {e}"));

        // Check if this is a server request (has "method" and "id")
        if let Ok(server_req) = serde_json::from_str::<serde_json::Value>(&data) {
            if let Some(method) = server_req.get("method").and_then(|m| m.as_str()) {
                if let Some(req_id) = server_req.get("id") {
                    // Handle roots/list request from server
                    if method == "roots/list" {
                        let roots_result = ListRootsResult { roots: vec![] };
                        let roots_response = JsonRpcResponse {
                            jsonrpc: Default::default(),
                            id: req_id
                                .as_i64()
                                .map(NumberOrString::Number)
                                .or_else(|| {
                                    req_id.as_str().map(|s| NumberOrString::String(s.into()))
                                })
                                .unwrap_or(NumberOrString::Number(0)),
                            result: roots_result,
                        };

                        let roots_body = serde_json::to_vec(&roots_response).unwrap_or_else(|e| {
                            fatal!(3, "failed to serialize roots/list response: {e}")
                        });

                        let _ = send_post(&new_uri, headers, roots_body, &mut post_sender).await;

                        continue; // Keep reading for tools/list response
                    }
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
    let tool_bodies: Vec<Bytes> = tools
        .iter()
        .enumerate()
        .map(|(idx, tool)| {
            let call_params = CallToolRequestParam {
                name: tool.name.clone(),
                arguments: None,
            };

            let call_request = CallToolRequest::new(call_params);

            let call_jsonrpc = JsonRpcRequest {
                jsonrpc: Default::default(),
                id: NumberOrString::Number((idx + 100) as i64),
                request: call_request,
            };

            let json = serde_json::to_vec(&call_jsonrpc).unwrap_or_else(|e| {
                fatal!(
                    3,
                    "failed to serialize tools/call request for {}: {e}",
                    tool.name
                )
            });
            Bytes::from(json)
        })
        .collect();

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
