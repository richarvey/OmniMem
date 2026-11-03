//! The human gate on `compile_skill(mode="write")`, over a real MCP session.
//!
//! The client here can answer a server's `elicitation/create` the way a
//! client with a person behind it does: the question arrives on the stream of
//! the `tools/call` that caused it, and the answer is a separate POST. A
//! client that declares no elicitation capability is the same session
//! without the declaration.

use std::sync::Arc;

use omnimem_core::{EmbeddingError, TextEmbedder, VECTOR_DIM};
use omnimem_engine::{Engine, EngineConfig, HumanApproval};
use omnimem_mcp::{ServerConfig, serve};
use omnimem_store::{Fields, Store};
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

const SKILL: &str = "mem:skill:gen:python-local";
const PROPOSAL: &str = "meta:skill:proposal:python-local";
const LESSON: &str = "always use boring queues for python background work";

/// Two episodic lessons that recur, enough for a rule in the python domain.
fn seed(engine: &Engine) {
    let now = omnimem_engine::pyfmt::now_str();
    for (key, project) in [("mem:episodic:01A", "alpha"), ("mem:episodic:01B", "beta")] {
        let content = format!("work on {project}");
        let fields: Fields = [
            ("content", content.as_str()),
            ("state", "active"),
            ("created_at", now.as_str()),
            ("updated_at", now.as_str()),
            ("tags", r#"["python"]"#),
            ("project", project),
            ("outcome", "succeeded"),
            ("effort_score", "3"),
            ("lesson", LESSON),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
        let vector = engine.embed_texts(&[&content]).unwrap().remove(0);
        engine.store().upsert(key, &fields, Some(&vector)).unwrap();
    }
}

async fn start(approval: HumanApproval) -> (Arc<Engine>, String, CancellationToken) {
    let engine = Arc::new(Engine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(Words),
        EngineConfig {
            human_approval: approval,
            skill_scan_interval_hours: 0.0,
            ..EngineConfig::default()
        },
    ));
    seed(&engine);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = ServerConfig {
        port,
        ..ServerConfig::default()
    };
    let shutdown = CancellationToken::new();
    tokio::spawn(serve(
        engine.clone(),
        config,
        Some(listener),
        shutdown.clone(),
    ));
    (engine, format!("http://127.0.0.1:{port}"), shutdown)
}

/// How the person answers a question, if one arrives.
#[derive(Clone)]
enum Reply {
    Accept,
    AcceptUnticked,
    Decline,
    Cancel,
}

impl Reply {
    fn result(&self) -> Value {
        match self {
            Self::Accept => json!({"action": "accept", "content": {"approve": true}}),
            Self::AcceptUnticked => json!({"action": "accept", "content": {"approve": false}}),
            Self::Decline => json!({"action": "decline"}),
            Self::Cancel => json!({"action": "cancel"}),
        }
    }
}

struct Client {
    http: reqwest::Client,
    base: String,
    id: Option<String>,
    next: u64,
}

/// The JSON-RPC messages in the SSE events read so far; the buffer keeps
/// whatever event is still incomplete.
fn drain_events(buffer: &mut String) -> Vec<Value> {
    let mut found = Vec::new();
    while let Some(end) = buffer.find("\n\n") {
        let event: String = buffer.drain(..end + 2).collect();
        let data: String = event
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim)
            .collect();
        if let Ok(message) = serde_json::from_str::<Value>(&data) {
            found.push(message);
        }
    }
    found
}

impl Client {
    /// Connect, declaring elicitation (the bare form clients used before
    /// form and url modes were told apart) or not.
    async fn connect(base: &str, elicitation: bool) -> Self {
        let mut client = Self {
            http: reqwest::Client::new(),
            base: base.to_owned(),
            id: None,
            next: 0,
        };
        let capabilities = if elicitation {
            json!({"elicitation": {}})
        } else {
            json!({})
        };
        let response = client
            .send(json!({
                "jsonrpc": "2.0", "id": 0, "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": capabilities,
                    "clientInfo": {"name": "test", "version": "1"},
                },
            }))
            .await;
        assert!(response.status().is_success());
        client.id = Some(
            response.headers()["mcp-session-id"]
                .to_str()
                .unwrap()
                .to_owned(),
        );
        let _ = response.text().await;
        let ack = client
            .send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        assert!(ack.status().is_success());
        client
    }

    async fn send(&self, body: Value) -> reqwest::Response {
        let mut request = self
            .http
            .post(format!("{}/mcp", self.base))
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(body.to_string());
        if let Some(id) = &self.id {
            request = request.header("mcp-session-id", id);
        }
        request.send().await.unwrap()
    }

    /// Call a tool. Any question the server asks along the way is answered
    /// with `reply` and recorded; the result is the tool's JSON text.
    async fn call(&mut self, tool: &str, arguments: Value, reply: &Reply) -> (Value, Vec<Value>) {
        self.next += 1;
        let id = self.next;
        let mut response = self
            .send(json!({
                "jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": {"name": tool, "arguments": arguments},
            }))
            .await;
        assert!(response.status().is_success());
        let mut questions = Vec::new();
        let mut buffer = String::new();
        loop {
            let Some(chunk) = response.chunk().await.unwrap() else {
                panic!("the stream ended without a result for {tool}");
            };
            buffer.push_str(&String::from_utf8_lossy(&chunk).replace("\r\n", "\n"));
            for message in drain_events(&mut buffer) {
                if message["method"] == "elicitation/create" {
                    let answered = self
                        .send(json!({
                            "jsonrpc": "2.0", "id": message["id"], "result": reply.result(),
                        }))
                        .await;
                    assert!(answered.status().is_success(), "{}", answered.status());
                    questions.push(message["params"].clone());
                } else if message["id"] == id && message.get("result").is_some() {
                    let text = message["result"]["content"][0]["text"].as_str().unwrap();
                    let parsed =
                        serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.into()));
                    return (parsed, questions);
                }
            }
        }
    }
}

async fn propose(client: &mut Client) -> Value {
    let (proposal, questions) = client
        .call(
            "compile_skill",
            json!({"domain": "python", "mode": "propose"}),
            &Reply::Cancel,
        )
        .await;
    assert_eq!(proposal["status"], "proposal", "{proposal}");
    assert!(questions.is_empty(), "proposing never asks anyone");
    proposal
}

fn write_args() -> Value {
    json!({"domain": "python", "mode": "write"})
}

fn stored_body(engine: &Engine) -> Option<String> {
    engine
        .store()
        .get(SKILL)
        .unwrap()
        .and_then(|f| f.get("body").cloned())
}

fn proposal_waits(engine: &Engine) -> bool {
    engine.store().hash_get_all(PROPOSAL).unwrap().is_some()
}

#[tokio::test]
async fn an_accepted_question_commits_the_stash_byte_for_byte() {
    let (engine, base, stop) = start(HumanApproval::Human).await;
    let mut client = Client::connect(&base, true).await;
    let proposal = propose(&mut client).await;

    let (result, questions) = client
        .call("compile_skill", write_args(), &Reply::Accept)
        .await;
    assert_eq!(result["status"], "written", "{result}");
    assert_eq!(
        stored_body(&engine).unwrap(),
        proposal["draft"].as_str().unwrap()
    );
    assert!(!proposal_waits(&engine), "a commit consumes the stash");

    // One question, in form mode, that names the skill and shows the draft.
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0]["mode"], "form");
    let message = questions[0]["message"].as_str().unwrap();
    assert!(message.contains("\"python\""), "{message}");
    assert!(message.contains("create"), "{message}");
    assert!(message.contains(LESSON), "{message}");
    assert_eq!(
        questions[0]["requestedSchema"]["properties"]["approve"]["type"],
        "boolean"
    );
    stop.cancel();
}

#[tokio::test]
async fn a_change_shows_the_diff_and_its_counts() {
    let (engine, base, stop) = start(HumanApproval::Human).await;
    let mut client = Client::connect(&base, true).await;
    propose(&mut client).await;
    client
        .call("compile_skill", write_args(), &Reply::Accept)
        .await;

    // A third project repeats the lesson, so a recompile reinforces the rule.
    let now = omnimem_engine::pyfmt::now_str();
    let fields: Fields = [
        ("content", "work on gamma"),
        ("state", "active"),
        ("created_at", now.as_str()),
        ("updated_at", now.as_str()),
        ("tags", r#"["python"]"#),
        ("project", "gamma"),
        ("outcome", "succeeded"),
        ("effort_score", "3"),
        ("lesson", LESSON),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
    .collect();
    let vector = engine.embed_texts(&["work on gamma"]).unwrap().remove(0);
    engine
        .store()
        .upsert("mem:episodic:01C", &fields, Some(&vector))
        .unwrap();
    let proposal = propose(&mut client).await;

    let (result, questions) = client
        .call("compile_skill", write_args(), &Reply::Decline)
        .await;
    assert_eq!(result["status"], "declined");
    let message = questions[0]["message"].as_str().unwrap();
    assert!(message.contains("change"), "{message}");
    assert!(message.contains("The diff:"), "{message}");
    assert!(message.contains("(reinforced x3)"), "{message}");
    assert!(message.contains("line added") || message.contains("lines added"));
    assert!(
        proposal["diff"]
            .as_str()
            .unwrap()
            .contains("(reinforced x3)")
    );
    stop.cancel();
}

#[tokio::test]
async fn a_declined_question_writes_nothing_and_keeps_the_stash() {
    let (engine, base, stop) = start(HumanApproval::Human).await;
    let mut client = Client::connect(&base, true).await;
    propose(&mut client).await;

    for (reply, status) in [
        (Reply::Decline, "declined"),
        (Reply::AcceptUnticked, "declined"),
        (Reply::Cancel, "cancelled"),
    ] {
        let (result, questions) = client.call("compile_skill", write_args(), &reply).await;
        assert_eq!(result["status"], status, "{result}");
        assert_eq!(questions.len(), 1);
        assert!(stored_body(&engine).is_none(), "nothing is written");
        // Kept, so the person can still accept it from the settings window.
        assert!(proposal_waits(&engine));
    }
    stop.cancel();
}

#[tokio::test]
async fn a_client_that_cannot_be_asked_gets_awaiting_approval() {
    let (engine, base, stop) = start(HumanApproval::Human).await;
    let mut client = Client::connect(&base, false).await;
    propose(&mut client).await;

    let (result, questions) = client
        .call("compile_skill", write_args(), &Reply::Accept)
        .await;
    assert_eq!(result["status"], "awaiting_approval", "{result}");
    assert!(questions.is_empty());
    let note = result["note"].as_str().unwrap();
    assert!(note.contains("Pending proposals"), "{note}");
    assert!(note.contains("headless"), "{note}");
    assert!(stored_body(&engine).is_none());
    assert!(proposal_waits(&engine));
    stop.cancel();
}

#[tokio::test]
async fn human_approval_agent_writes_on_the_agents_say_so() {
    let (engine, base, stop) = start(HumanApproval::Agent).await;
    // Both a client that could be asked and one that can't: neither is.
    for elicitation in [true, false] {
        let mut client = Client::connect(&base, elicitation).await;
        let proposal = propose(&mut client).await;
        let (result, questions) = client
            .call("compile_skill", write_args(), &Reply::Decline)
            .await;
        assert_eq!(result["status"], "written", "{result}");
        assert!(questions.is_empty());
        assert_eq!(
            stored_body(&engine).unwrap(),
            proposal["draft"].as_str().unwrap()
        );
        engine.store().delete(SKILL).unwrap();
    }
    stop.cancel();
}

#[tokio::test]
async fn a_stale_or_missing_proposal_is_reported_without_asking() {
    let (engine, base, stop) = start(HumanApproval::Human).await;
    let mut client = Client::connect(&base, true).await;

    let (result, questions) = client
        .call("compile_skill", write_args(), &Reply::Accept)
        .await;
    assert_eq!(result["status"], "no_proposal");
    assert!(questions.is_empty());

    propose(&mut client).await;
    client
        .call("compile_skill", write_args(), &Reply::Accept)
        .await;
    assert!(stored_body(&engine).is_some());

    // A recompile that differs, then the stored skill changes underneath it.
    let now = omnimem_engine::pyfmt::now_str();
    let fields: Fields = [
        ("content", "work on gamma"),
        ("state", "active"),
        ("created_at", now.as_str()),
        ("updated_at", now.as_str()),
        ("tags", r#"["python"]"#),
        ("project", "gamma"),
        ("outcome", "succeeded"),
        ("effort_score", "3"),
        ("lesson", LESSON),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
    .collect();
    let vector = engine.embed_texts(&["work on gamma"]).unwrap().remove(0);
    engine
        .store()
        .upsert("mem:episodic:01C", &fields, Some(&vector))
        .unwrap();
    propose(&mut client).await;
    engine
        .store()
        .set_field(SKILL, "body", "edited underneath")
        .unwrap();

    let (result, questions) = client
        .call("compile_skill", write_args(), &Reply::Accept)
        .await;
    assert_eq!(result["status"], "stale_proposal", "{result}");
    assert!(
        questions.is_empty(),
        "no point asking about a draft that can't be written"
    );
    assert_eq!(stored_body(&engine).unwrap(), "edited underneath");
    stop.cancel();
}
