//! terse proxy -- LLM gateway that compresses tool_result content before
//! forwarding to the upstream API (Bedrock or Anthropic direct).
//!
//! ## Bedrock mode (default when AWS_REGION is set):
//!   ANTHROPIC_BEDROCK_BASE_URL=http://localhost:7778
//!   CLAUDE_CODE_SKIP_BEDROCK_AUTH=1
//!
//! ## Anthropic direct mode:
//!   ANTHROPIC_BASE_URL=http://localhost:7778
//!   (pass x-api-key header through)
//!
//! terse detects the mode from the request path:
//!   /model/{id}/invoke* -> Bedrock (SigV4 via AWS SDK)
//!   /v1/messages        -> Anthropic direct (passthrough with API key)

use crate::compress::compress_targets;
use crate::extract;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, server::conn::http1, service::service_fn, Request, Response};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::net::TcpListener;

struct ProxyState {
    bedrock_client: Option<aws_sdk_bedrockruntime::Client>,
    http_client: reqwest::Client,
    #[allow(dead_code)]
    region: String,
}

pub async fn run_proxy(port: u16) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let region = std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".into());

    // try to load AWS credentials for Bedrock mode
    let bedrock_client = match aws_config::from_env()
        .region(aws_config::Region::new(region.clone()))
        .load()
        .await
        .credentials_provider()
    {
        Some(_) => {
            let config = aws_config::from_env()
                .region(aws_config::Region::new(region.clone()))
                .load()
                .await;
            let client = aws_sdk_bedrockruntime::Client::new(&config);
            eprintln!("  bedrock: AWS credentials loaded (region: {region})");
            Some(client)
        }
        None => {
            eprintln!("  bedrock: no AWS credentials (bedrock mode disabled)");
            None
        }
    };

    let state = Arc::new(ProxyState {
        bedrock_client,
        http_client: reqwest::Client::new(),
        region,
    });

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(addr).await?;
    eprintln!("terse proxy listening on http://{addr}");
    eprintln!();
    eprintln!("  for bedrock:");
    eprintln!("    ANTHROPIC_BEDROCK_BASE_URL=http://localhost:{port}");
    eprintln!("    CLAUDE_CODE_SKIP_BEDROCK_AUTH=1");
    eprintln!();
    eprintln!("  for anthropic direct:");
    eprintln!("    ANTHROPIC_BASE_URL=http://localhost:{port}");

    loop {
        let (stream, _peer) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let state = state.clone();

        tokio::spawn(async move {
            let svc = service_fn(move |req| {
                let state = state.clone();
                handle_request(req, state)
            });
            if let Err(e) = http1::Builder::new().serve_connection(io, svc).await {
                eprintln!("  connection error: {e}");
            }
        });
    }
}

async fn handle_request(
    req: Request<Incoming>,
    state: Arc<ProxyState>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let method = req.method().clone();
    let uri_path = req.uri().path().to_string();
    let headers = req.headers().clone();

    // read request body
    let body_bytes = req.collect().await?.to_bytes();
    let body_len = body_bytes.len();

    // compress tool_result content in the request body
    let (compressed_body, stats) = if method == hyper::Method::POST && body_len > 0 {
        compress_request_body(&body_bytes)
    } else {
        (body_bytes.to_vec(), None)
    };

    if let Some((orig, comp, ms)) = stats {
        let saved = orig.saturating_sub(comp);
        let pct = if orig > 0 {
            saved as f64 / orig as f64 * 100.0
        } else {
            0.0
        };
        eprintln!(
            "  {} {} -- {} -> {} ({:.1}% saved, {:.0}ms)",
            method,
            uri_path,
            fmt_bytes(orig),
            fmt_bytes(comp),
            pct,
            ms,
        );
    } else {
        eprintln!(
            "  {} {} -- {} (passthrough)",
            method,
            uri_path,
            fmt_bytes(body_len)
        );
    }

    // route based on URL path
    if uri_path.starts_with("/model/") {
        handle_bedrock(state, &uri_path, &compressed_body, &headers).await
    } else {
        handle_anthropic(state, &uri_path, &compressed_body, &headers, &method).await
    }
}

/// forward to Bedrock via AWS SDK (handles SigV4 automatically)
async fn handle_bedrock(
    state: Arc<ProxyState>,
    uri_path: &str,
    body: &[u8],
    _headers: &hyper::HeaderMap,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let client = match &state.bedrock_client {
        Some(c) => c,
        None => {
            return Ok(error_response(
                503,
                "bedrock mode unavailable -- no AWS credentials",
            ));
        }
    };

    // extract model ID from path: /model/{model_id}/invoke or /model/{model_id}/invoke-with-response-stream
    let parts: Vec<&str> = uri_path.split('/').collect();
    let model_id = if parts.len() >= 3 && parts[1] == "model" {
        parts[2]
    } else {
        return Ok(error_response(400, "invalid bedrock path"));
    };

    let is_streaming = uri_path.contains("invoke-with-response-stream");

    if is_streaming {
        // streaming: use invoke_model_with_response_stream
        match client
            .invoke_model_with_response_stream()
            .model_id(model_id)
            .content_type("application/json")
            .accept("application/vnd.amazon.eventstream")
            .body(aws_sdk_bedrockruntime::primitives::Blob::new(body))
            .send()
            .await
        {
            Ok(output) => {
                // collect the stream into a single response
                // (for now -- TODO: true streaming passthrough)
                let mut all_bytes = Vec::new();
                let mut stream = output.body;
                loop {
                    match stream.recv().await {
                        Ok(Some(event)) => {
                            use aws_sdk_bedrockruntime::types::ResponseStream;
                            match event {
                                ResponseStream::Chunk(chunk) => {
                                    if let Some(b) = chunk.bytes() {
                                        all_bytes.extend_from_slice(b.as_ref());
                                    }
                                }
                                _ => {}
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            eprintln!("  bedrock stream error: {e}");
                            return Ok(error_response(502, &format!("stream error: {e}")));
                        }
                    }
                }
                Ok(Response::builder()
                    .status(200)
                    .header("content-type", "application/json")
                    .body(Full::new(Bytes::from(all_bytes)))
                    .unwrap())
            }
            Err(e) => {
                eprintln!("  bedrock error: {e}");
                Ok(error_response(502, &format!("bedrock error: {e}")))
            }
        }
    } else {
        // non-streaming: use invoke_model
        match client
            .invoke_model()
            .model_id(model_id)
            .content_type("application/json")
            .body(aws_sdk_bedrockruntime::primitives::Blob::new(body))
            .send()
            .await
        {
            Ok(output) => {
                let resp_body = output.body().as_ref().to_vec();
                Ok(Response::builder()
                    .status(200)
                    .header("content-type", "application/json")
                    .body(Full::new(Bytes::from(resp_body)))
                    .unwrap())
            }
            Err(e) => {
                eprintln!("  bedrock error: {e}");
                Ok(error_response(502, &format!("bedrock error: {e}")))
            }
        }
    }
}

/// forward to Anthropic API directly (passthrough with API key)
async fn handle_anthropic(
    state: Arc<ProxyState>,
    uri_path: &str,
    body: &[u8],
    headers: &hyper::HeaderMap,
    method: &hyper::Method,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let upstream = "https://api.anthropic.com";
    let url = format!("{upstream}{uri_path}");

    let mut req = state.http_client.request(method.clone(), &url);

    // forward relevant headers (API key, content-type, etc.)
    for (name, value) in headers.iter() {
        let n = name.as_str();
        // skip hop-by-hop headers
        if n == "host" || n == "transfer-encoding" || n == "connection" {
            continue;
        }
        if let Ok(v) = value.to_str() {
            req = req.header(n, v);
        }
    }

    req = req.body(body.to_vec());

    match req.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let resp_headers = resp.headers().clone();
            let resp_body = resp.bytes().await.unwrap_or_default();

            let mut builder = Response::builder().status(status);
            for (k, v) in &resp_headers {
                builder = builder.header(k, v);
            }
            Ok(builder.body(Full::new(resp_body)).unwrap())
        }
        Err(e) => {
            eprintln!("  anthropic error: {e}");
            Ok(error_response(502, &format!("anthropic error: {e}")))
        }
    }
}

fn compress_request_body(body: &[u8]) -> (Vec<u8>, Option<(usize, usize, f64)>) {
    let start = Instant::now();

    let mut value: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (body.to_vec(), None),
    };

    let mut targets = extract::extract_targets(&value);
    if targets.is_empty() {
        return (body.to_vec(), None);
    }

    let _results = compress_targets(&mut targets);

    // apply compressed content back to the JSON body
    let messages = match value.get_mut("messages") {
        Some(Value::Array(arr)) => arr,
        _ => return (body.to_vec(), None),
    };

    let mut any_compressed = false;
    for target in &targets {
        if let Some(ref compressed) = target.compressed {
            if let Some(msg) = messages.get_mut(target.msg_idx) {
                if let Some(Value::Array(content)) = msg.get_mut("content") {
                    if let Some(block) = content.get_mut(target.block_idx) {
                        // tool_result content can be string or array of content blocks
                        if let Some(Value::Array(inner)) = block.get_mut("content") {
                            for item in inner.iter_mut() {
                                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                                    if let Some(text) = item.get_mut("text") {
                                        *text = Value::String(compressed.clone());
                                        any_compressed = true;
                                    }
                                }
                            }
                        } else if let Some(content_str) = block.get_mut("content") {
                            if content_str.is_string() {
                                *content_str = Value::String(compressed.clone());
                                any_compressed = true;
                            }
                        }
                    }
                }
            }
        }
    }

    if !any_compressed {
        return (body.to_vec(), None);
    }

    let compressed_json = serde_json::to_vec(&value).unwrap_or_else(|_| body.to_vec());
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
    let comp_len = compressed_json.len();
    let orig_len = body.len();

    (compressed_json, Some((orig_len, comp_len, elapsed)))
}

fn error_response(status: u16, msg: &str) -> Response<Full<Bytes>> {
    let body = serde_json::json!({ "error": msg });
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body.to_string())))
        .unwrap()
}

fn fmt_bytes(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}MB", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}KB", n as f64 / 1_000.0)
    } else {
        format!("{n}B")
    }
}
