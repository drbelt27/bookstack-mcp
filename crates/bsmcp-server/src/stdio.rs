//! `bsmcp-server --stdio`: MCP over stdin/stdout for a single local user.
//!
//! Reuses `mcp::handle_request` unchanged. No HTTP listener, no database, no
//! encryption key. Credentials come from the environment (never argv):
//! `BSMCP_BOOKSTACK_URL`, `BSMCP_BOOKSTACK_TOKEN_ID`, `BSMCP_BOOKSTACK_TOKEN_SECRET`.
//! These are deliberately distinct from the embedder's `BSMCP_EMBED_TOKEN_*`
//! and there is no fallback between the two.
//!
//! stdout carries protocol messages only; logs go to stderr (see
//! `bsmcp_common::logging::init_stderr`). Discovery (`initialize`, `tools/list`)
//! works without credentials; `tools/call` is refused until all three are set.

use std::env;
use std::sync::Arc;
use std::time::Duration;

use bsmcp_common::backend::{Backend, BackendConfig, BackendKind};
use bsmcp_common::time::TimezoneConfig;
use serde_json::{json, Value};
use tokio::io::{stdin, stdout, AsyncBufReadExt, AsyncWriteExt, BufReader};

struct Ctx {
    client: Arc<dyn Backend>,
    configured: Option<&'static str>, // Some(reason) when tools/call must be refused
    staging: crate::staging::StagingStore,
    tz: TimezoneConfig,
}

fn env_trim(name: &str) -> String {
    env::var(name).unwrap_or_default().trim().to_string()
}

fn build_ctx(url: &str, token_id: &str, token_secret: &str) -> Ctx {
    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .build()
        .expect("Failed to build HTTP client");
    let public = env_trim("BSMCP_BOOKSTACK_PUBLIC_URL");
    let cfg = BackendConfig::new(BackendKind::BookStack, url, Some(&public));
    let configured = if url.is_empty() {
        Some("BSMCP_BOOKSTACK_URL is not set")
    } else if token_id.is_empty() || token_secret.is_empty() {
        Some("BSMCP_BOOKSTACK_TOKEN_ID and BSMCP_BOOKSTACK_TOKEN_SECRET must be set")
    } else {
        None
    };
    Ctx {
        client: cfg.client(token_id, token_secret, http),
        configured,
        staging: crate::staging::new_staging_store(),
        tz: TimezoneConfig::from_env(),
    }
}

fn error_response(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// One input line -> at most one output line (None for notifications).
async fn handle_line(line: &str, ctx: &Ctx) -> Option<String> {
    let request: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return Some(error_response(&Value::Null, -32700, "Parse error").to_string()),
    };
    if !request.is_object() {
        return Some(error_response(&Value::Null, -32600, "Invalid Request").to_string());
    }
    // Client responses (no method) and notifications (no id) never get a reply.
    request.get("method")?;
    let id = request.get("id")?.clone();

    if request["method"] == "tools/call" {
        if let Some(reason) = ctx.configured {
            let result = json!({
                "content": [{ "type": "text", "text": format!("Not configured: {reason}") }],
                "isError": true,
            });
            return Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string());
        }
    }

    crate::mcp::handle_request(&request, &ctx.client, None, None, &ctx.staging, &ctx.tz)
        .await
        .map(|v| v.to_string())
}

/// Runs until stdin closes (the MCP client's shutdown signal). Returns the exit code.
pub async fn run() -> i32 {
    let ctx = build_ctx(
        env_trim("BSMCP_BOOKSTACK_URL").trim_end_matches('/'),
        &env_trim("BSMCP_BOOKSTACK_TOKEN_ID"),
        &env_trim("BSMCP_BOOKSTACK_TOKEN_SECRET"),
    );
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        configured = ctx.configured.is_none(),
        "stdio_started"
    );

    let mut lines = BufReader::new(stdin()).lines();
    let mut out = stdout();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if line.trim().is_empty() {
                    continue;
                }
                if let Some(resp) = handle_line(&line, &ctx).await {
                    if out.write_all(resp.as_bytes()).await.is_err()
                        || out.write_all(b"\n").await.is_err()
                        || out.flush().await.is_err()
                    {
                        return 1; // stdout closed: client is gone
                    }
                }
            }
            Ok(None) => return 0,
            Err(e) => {
                tracing::error!(error = %e, "stdin_read_failed");
                return 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Port 9 (discard) is closed locally: backend calls fail fast, no network.
    fn ctx(id: &str, secret: &str) -> Ctx {
        build_ctx("http://127.0.0.1:9", id, secret)
    }

    async fn call(ctx: &Ctx, req: Value) -> Option<Value> {
        handle_line(&req.to_string(), ctx)
            .await
            .map(|s| serde_json::from_str(&s).unwrap())
    }

    #[tokio::test]
    async fn tools_list_works_without_credentials_and_hides_server_only_tools() {
        let c = ctx("", "");
        let r = call(&c, json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
            .await
            .unwrap();
        let names: Vec<&str> = r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(names.contains(&"search_content"));
        assert!(!names.contains(&"directory"));
        assert!(!names.contains(&"prepare_upload"));
        assert!(!names.contains(&"semantic_search"));
    }

    #[tokio::test]
    async fn tools_call_refused_without_credentials() {
        let c = ctx("", "");
        let r = call(
            &c,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_books","arguments":{}}}),
        )
        .await
        .unwrap();
        assert_eq!(r["result"]["isError"], true);
        assert_eq!(r["id"], 2);
    }

    #[tokio::test]
    async fn server_only_tool_refused_even_with_credentials() {
        let c = ctx("id", "secret");
        let r = call(
            &c,
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"directory","arguments":{}}}),
        )
        .await
        .unwrap();
        assert!(r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("STDIO"));
    }

    #[tokio::test]
    async fn initialize_ping_notifications_and_garbage() {
        let c = ctx("", "");
        let init = call(
            &c,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        )
        .await
        .unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "BookStack MCP");
        assert!(call(
            &c,
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .await
        .is_none());
        assert!(
            call(&c, json!({"jsonrpc":"2.0","method":"unknown/notification"}))
                .await
                .is_none()
        );
        assert!(call(&c, json!({"jsonrpc":"2.0","id":9,"result":{}}))
            .await
            .is_none());
        let ping = call(&c, json!({"jsonrpc":"2.0","id":4,"method":"ping"}))
            .await
            .unwrap();
        assert_eq!(ping["result"], json!({}));
        let bad = handle_line("{not json", &c).await.unwrap();
        assert!(bad.contains("-32700"));
    }
}
