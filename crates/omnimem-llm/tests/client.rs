//! The client against a scripted local HTTP server.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;

use omnimem_core::LanguageModel;
use omnimem_llm::{AnthropicClient, AnthropicConfig};
use serde_json::Value;

struct Seen {
    headers: Vec<String>,
    body: Value,
}

/// Serve each scripted `(status, extra headers, body)` to one request, in order.
fn server(responses: Vec<(u16, &'static str, &'static str)>) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    thread::spawn(move || {
        for (status, extra, body) in responses {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut headers = Vec::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let line = line.trim_end().to_owned();
                if line.is_empty() {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap();
                }
                headers.push(line.to_ascii_lowercase());
            }
            let mut raw = vec![0; length];
            reader.read_exact(&mut raw).unwrap();
            log.lock().unwrap().push(Seen {
                headers,
                body: serde_json::from_slice(&raw).unwrap(),
            });
            let mut stream = stream;
            write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n{extra}\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    (url, seen)
}

fn client(url: &str) -> AnthropicClient {
    let mut config = AnthropicConfig::new("sk-test");
    config.base_url = url.to_owned();
    AnthropicClient::new(config).unwrap()
}

const REPLY: &str = r#"{"content": [{"type": "text", "text": "[\"a\", \"b\"]"}]}"#;

#[test]
fn sends_a_messages_request_and_reads_the_first_text_block() {
    let (url, seen) = server(vec![(200, "", REPLY)]);
    let text = client(&url)
        .complete("claude-haiku-4-5-20251001", "hello", 512)
        .unwrap();
    assert_eq!(text, r#"["a", "b"]"#);
    let seen = seen.lock().unwrap();
    assert!(seen[0].headers.contains(&"x-api-key: sk-test".to_owned()));
    assert!(
        seen[0]
            .headers
            .contains(&"anthropic-version: 2023-06-01".to_owned())
    );
    assert_eq!(seen[0].body["model"], "claude-haiku-4-5-20251001");
    assert_eq!(seen[0].body["max_tokens"], 512);
    assert_eq!(seen[0].body["messages"][0]["content"], "hello");
}

#[test]
fn retries_overload_and_rate_limits_then_succeeds() {
    let (url, seen) = server(vec![
        (
            529,
            "retry-after: 0\r\n",
            r#"{"error": {"message": "Overloaded"}}"#,
        ),
        (
            429,
            "retry-after-ms: 1\r\n",
            r#"{"error": {"message": "slow down"}}"#,
        ),
        (200, "", REPLY),
    ]);
    assert!(client(&url).complete("m", "p", 10).is_ok());
    assert_eq!(seen.lock().unwrap().len(), 3);
}

#[test]
fn gives_up_after_two_retries_and_never_retries_a_bad_request() {
    let overloaded = (
        529,
        "retry-after: 0\r\n",
        r#"{"error": {"message": "Overloaded"}}"#,
    );
    let (url, seen) = server(vec![overloaded, overloaded, overloaded]);
    let error = client(&url).complete("m", "p", 10).unwrap_err().to_string();
    assert_eq!(error, "Anthropic API returned 529: Overloaded");
    assert_eq!(seen.lock().unwrap().len(), 3);

    let (url, seen) = server(vec![(400, "", r#"{"error": {"message": "bad model"}}"#)]);
    let error = client(&url).complete("m", "p", 10).unwrap_err().to_string();
    assert_eq!(error, "Anthropic API returned 400: bad model");
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn the_placeholder_key_counts_as_no_key() {
    // SAFETY: tests in this file don't read the environment concurrently.
    unsafe { std::env::set_var("ANTHROPIC_API_KEY", "your_key_here") };
    assert!(AnthropicConfig::from_env().is_none());
    unsafe { std::env::set_var("ANTHROPIC_API_KEY", " sk-real ") };
    assert_eq!(AnthropicConfig::from_env().unwrap().api_key, "sk-real");
    unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };
}
