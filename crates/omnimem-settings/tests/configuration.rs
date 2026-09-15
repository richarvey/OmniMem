//! The configuration page through `Panel::handle`, with an in-memory
//! keychain.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::http::{Request, Response};
use omnimem_settings::{Panel, SecretStore};

#[derive(Default)]
struct MemoryKeychain(Mutex<HashMap<String, String>>);

impl SecretStore for MemoryKeychain {
    fn get(&self, name: &str) -> Result<Option<String>, String> {
        Ok(self.0.lock().unwrap().get(name).cloned())
    }
    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        self.0
            .lock()
            .unwrap()
            .insert(name.to_owned(), value.to_owned());
        Ok(())
    }
    fn delete(&self, name: &str) -> Result<(), String> {
        self.0.lock().unwrap().remove(name);
        Ok(())
    }
}

struct Refusing;

impl SecretStore for Refusing {
    fn get(&self, _: &str) -> Result<Option<String>, String> {
        Err("no Secret Service on this session".to_owned())
    }
    fn set(&self, _: &str, _: &str) -> Result<(), String> {
        Err("no Secret Service on this session".to_owned())
    }
    fn delete(&self, _: &str) -> Result<(), String> {
        Err("no Secret Service on this session".to_owned())
    }
}

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch() -> Scratch {
    let dir = std::env::temp_dir().join(format!("omnimem-config-{}", ulid::Ulid::generate()));
    std::fs::create_dir_all(&dir).unwrap();
    Scratch(dir)
}

async fn get(panel: &Panel, uri: &str) -> Response<Vec<u8>> {
    panel
        .handle(
            Request::get(format!("omnimem://localhost{uri}"))
                .body(Vec::new())
                .unwrap(),
        )
        .await
}

async fn post(panel: &Panel, form: &str) -> Response<Vec<u8>> {
    panel
        .handle(
            Request::post("omnimem://localhost/configuration")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(form.as_bytes().to_vec())
                .unwrap(),
        )
        .await
}

fn text(response: &Response<Vec<u8>>) -> String {
    String::from_utf8_lossy(response.body()).into_owned()
}

fn landed(response: &Response<Vec<u8>>) -> String {
    assert_eq!(response.status(), 200, "{}", text(response));
    response.headers()["x-omnimem-redirect"]
        .to_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn without_the_desktop_app_there_is_nothing_to_configure() {
    let panel = Panel::new();
    let page = text(&get(&panel, "/configuration").await);
    assert!(page.contains("only available in the desktop app"), "{page}");
    assert!(page.contains("class=\"nav-link active\">Configuration"));
}

#[tokio::test]
async fn settings_go_to_the_file_and_secrets_to_the_keychain() {
    let dir = scratch();
    let file = dir.0.join("omnimem.env");
    std::fs::write(&file, "# mine\nCUSTOM_THING=kept\nMCP_PORT=8000\n").unwrap();
    let keychain = Arc::new(MemoryKeychain::default());
    let panel = Panel::new();
    panel.set_settings_path(file.clone());
    panel.set_secret_store(keychain.clone());

    let page = text(&get(&panel, "/configuration").await);
    assert!(
        page.contains("MCP server") && page.contains("Embeddings"),
        "{page}"
    );
    assert!(
        page.contains(r#"value="8000""#),
        "the file's value is shown"
    );
    assert!(
        page.contains(r#"placeholder="127.0.0.1""#),
        "defaults are placeholders"
    );
    assert!(page.contains("Not set"));
    assert!(!page.contains("keychain isn&#39;t available"));

    let saved = landed(&post(&panel, "MCP_PORT=9000&INGEST_MODE=raw&RSS_REQUIRE_LICENCE=true&ANTHROPIC_API_KEY=sk-ant-test&MCP_HOST=").await);
    assert!(
        saved.starts_with("/configuration?message=Saved."),
        "{saved}"
    );
    let written = std::fs::read_to_string(&file).unwrap();
    assert!(written.contains("MCP_PORT=9000\n"), "{written}");
    assert!(
        written.contains("INGEST_MODE=raw\n") && written.contains("RSS_REQUIRE_LICENCE=true\n")
    );
    assert!(
        written.contains("CUSTOM_THING=kept"),
        "hand-written lines survive: {written}"
    );
    assert!(
        !written.contains("sk-ant-test") && !written.contains("ANTHROPIC"),
        "secrets never reach the file"
    );
    assert_eq!(
        keychain.get("ANTHROPIC_API_KEY").unwrap().as_deref(),
        Some("sk-ant-test")
    );

    let page = text(&get(&panel, "/configuration").await);
    assert!(page.contains("Stored in the keychain"), "{page}");
    assert!(
        !page.contains("sk-ant-test"),
        "a stored secret is never shown"
    );
    assert!(page.contains(r#"<option value="raw" selected>"#));

    let refused = landed(&post(&panel, "MCP_PORT=lots&INGEST_MODE=sometimes").await);
    assert!(
        refused.starts_with("/configuration?error=Nothing%20was%20saved."),
        "{refused}"
    );
    assert!(refused.contains("MCP_PORT"), "{refused}");
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("MCP_PORT=9000"),
        "a refused save changes nothing"
    );

    landed(&post(&panel, "MCP_PORT=&clear%3AANTHROPIC_API_KEY=on").await);
    assert!(
        !std::fs::read_to_string(&file).unwrap().contains("MCP_PORT"),
        "an empty field goes back to the default"
    );
    assert_eq!(keychain.get("ANTHROPIC_API_KEY").unwrap(), None);
}

#[tokio::test]
async fn a_missing_keychain_is_explained_and_the_file_still_saves() {
    let dir = scratch();
    let file = dir.0.join("omnimem.env");
    let panel = Panel::new();
    panel.set_settings_path(file.clone());
    panel.set_secret_store(Arc::new(Refusing));

    let page = text(&get(&panel, "/configuration").await);
    assert!(page.contains("no Secret Service on this session"), "{page}");
    assert!(page.contains("disabled"));

    let answer = landed(&post(&panel, "MCP_PORT=9100&HF_TOKEN=hf_x").await);
    assert!(answer.contains("keychain%20refused"), "{answer}");
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("MCP_PORT=9100")
    );
}
