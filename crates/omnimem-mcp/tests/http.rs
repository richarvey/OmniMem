//! A real MCP session over streamable HTTP: initialize, list tools,
//! remember, recall. Uses a deterministic fake embedder and in-memory SQLite.

use std::sync::Arc;

use omnimem_core::{EmbeddingError, TextEmbedder, VECTOR_DIM};
use omnimem_engine::{Engine, EngineConfig};
use omnimem_mcp::{ServerConfig, serve};
use omnimem_store::Store;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

struct Words;

impl TextEmbedder for Words {
    fn dimension(&self) -> usize {
        VECTOR_DIM
    }
    fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0.0f32; VECTOR_DIM];
                for w in t.split_whitespace() {
                    let h = w
                        .bytes()
                        .fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b)));
                    v[(h % VECTOR_DIM as u64) as usize] += 1.0;
                }
                let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
                v.iter().map(|x| x / n).collect()
            })
            .collect())
    }
}

async fn start(auth_token: Option<&str>) -> (String, CancellationToken) {
    let engine = Arc::new(Engine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(Words),
        EngineConfig::default(),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = ServerConfig {
        port,
        auth_token: auth_token.map(str::to_owned),
        ..ServerConfig::default()
    };
    let shutdown = CancellationToken::new();
    tokio::spawn(serve(engine, config, Some(listener), shutdown.clone()));
    (format!("http://127.0.0.1:{port}"), shutdown)
}

/// The JSON-RPC message in a response, whether sent as JSON or as SSE.
fn message(body: &str) -> Value {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        return v;
    }
    let data: String = body
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("");
    serde_json::from_str(&data).unwrap_or_else(|e| panic!("unparseable response {body:?}: {e}"))
}

struct Session {
    client: reqwest::Client,
    base: String,
    token: Option<String>,
    session: Option<String>,
    next_id: u64,
}

impl Session {
    async fn post(&mut self, body: Value) -> (reqwest::StatusCode, String) {
        let mut request = self
            .client
            .post(format!("{}/mcp", self.base))
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(body.to_string());
        if let Some(token) = &self.token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        if let Some(session) = &self.session {
            request = request.header("mcp-session-id", session);
        }
        let response = request.send().await.unwrap();
        if let Some(id) = response.headers().get("mcp-session-id") {
            self.session = Some(id.to_str().unwrap().to_owned());
        }
        let status = response.status();
        (status, response.text().await.unwrap())
    }

    async fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let (status, body) = self
            .post(json!({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params}))
            .await;
        assert!(status.is_success(), "{method}: {status} {body}");
        message(&body)
    }

    async fn initialise(&mut self) -> Value {
        let init = self
            .rpc(
                "initialize",
                json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}),
            )
            .await;
        self.post(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        init
    }

    async fn call(&mut self, tool: &str, arguments: Value) -> (bool, Value) {
        let reply = self
            .rpc("tools/call", json!({"name": tool, "arguments": arguments}))
            .await;
        let result = &reply["result"];
        let text = result["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        let is_error = result["isError"].as_bool().unwrap_or(false);
        (
            is_error,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }
}

fn session(base: &str, token: Option<&str>) -> Session {
    Session {
        client: reqwest::Client::new(),
        base: base.to_owned(),
        token: token.map(str::to_owned),
        session: None,
        next_id: 0,
    }
}

#[tokio::test]
async fn a_session_remembers_and_recalls() {
    let (base, shutdown) = start(None).await;
    let mut s = session(&base, None);

    let init = s.initialise().await;
    assert_eq!(init["result"]["serverInfo"]["name"], "omnimem");
    assert!(
        init["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("OmniMem")
    );

    let tools = s.rpc("tools/list", json!({})).await;
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(
        names.contains(&"remember") && names.contains(&"recall"),
        "{names:?}"
    );

    let (is_error, stored) = s
        .call(
            "remember",
            json!({"content": "sqlite replaces valkey in omnimem seven", "mode": "raw"}),
        )
        .await;
    assert!(!is_error, "{stored}");
    let key = stored["key"].as_str().unwrap().to_owned();

    let (_, recalled) = s
        .call(
            "recall",
            json!({"query": "sqlite replaces valkey in omnimem seven"}),
        )
        .await;
    assert_eq!(recalled[0]["key"], key.as_str());

    let (is_error, message) = s.call("remember", json!({"content": "   "})).await;
    assert!(is_error);
    assert_eq!(message, "Content cannot be empty");

    shutdown.cancel();
}

#[tokio::test]
async fn a_bearer_token_is_required_when_configured() {
    let (base, shutdown) = start(Some("s3cret")).await;

    let mut anonymous = session(&base, None);
    let (status, _) = anonymous
        .post(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}))
        .await;
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);

    let mut wrong = session(&base, Some("guess"));
    let (status, _) = wrong
        .post(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}))
        .await;
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);

    let mut authorised = session(&base, Some("s3cret"));
    assert_eq!(
        authorised.initialise().await["result"]["serverInfo"]["name"],
        "omnimem"
    );

    let health = reqwest::get(format!("{base}/healthz")).await.unwrap();
    assert!(
        health.status().is_success(),
        "the health check needs no token"
    );

    // Every method on /mcp sits behind the token, not only POST: the GET
    // event stream and DELETE (session close) included.
    let client = reqwest::Client::new();
    for method in [reqwest::Method::GET, reqwest::Method::DELETE] {
        let response = client
            .request(method.clone(), format!("{base}/mcp"))
            .header("accept", "text/event-stream")
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "{method} /mcp without a token"
        );
    }
    shutdown.cancel();
}
