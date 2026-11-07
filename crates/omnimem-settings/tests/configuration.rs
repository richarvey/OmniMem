//! The configuration page through `Panel::handle`, with an in-memory
//! keychain.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::http::{Request, Response};
use omnimem_settings::{Panel, SecretStore, SettingsFileSecrets};

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
    assert_eq!(response.status(), 303, "{}", text(response));
    response.headers()["x-omnimem-redirect"]
        .to_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn without_a_settings_file_there_is_nothing_to_configure() {
    let panel = Panel::new();
    let page = text(&get(&panel, "/configuration").await);
    assert!(page.contains("no settings file to edit"), "{page}");
    assert!(page.contains("aria-current=\"page\"><span class=\"nav-label\">Configuration"));
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
    assert!(page.contains("secrets\n  to the system keychain"), "{page}");
    assert!(!page.contains("Secrets can&#39;t be saved"));

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
    assert!(page.contains("Saved. Type to replace it"), "{page}");
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
    assert!(answer.contains("not%20every%20secret"), "{answer}");
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("MCP_PORT=9100")
    );
}

#[tokio::test]
async fn the_audit_settings_are_on_the_page_and_the_choice_is_checked() {
    let dir = scratch();
    let file = dir.0.join("omnimem.env");
    let panel = Panel::new();
    panel.set_settings_path(file.clone());
    panel.set_secret_store(Arc::new(MemoryKeychain::default()));

    let page = text(&get(&panel, "/configuration").await);
    assert!(
        page.contains("AUDIT_LOG") && page.contains("AUDIT_RETENTION_DAYS"),
        "{page}"
    );
    // `full` is the one level that holds memory text, and the page says so.
    assert!(page.contains("the one level that stores what your memories say"));
    for level in ["off", "lifecycle", "writes", "full"] {
        assert!(
            page.contains(&format!(r#"<option value="{level}""#)),
            "{level}"
        );
    }

    let saved = landed(&post(&panel, "AUDIT_LOG=writes&AUDIT_RETENTION_DAYS=30").await);
    assert!(
        saved.starts_with("/configuration?message=Saved."),
        "{saved}"
    );
    let written = std::fs::read_to_string(&file).unwrap();
    assert!(
        written.contains("AUDIT_LOG=writes\n") && written.contains("AUDIT_RETENTION_DAYS=30\n")
    );

    let refused = landed(&post(&panel, "AUDIT_LOG=everything").await);
    assert!(
        refused.contains("AUDIT_LOG") || refused.contains("Audit"),
        "{refused}"
    );
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("AUDIT_LOG=writes")
    );
}

#[tokio::test]
async fn on_a_server_secrets_go_into_the_settings_file_and_stay_hidden() {
    let dir = scratch();
    let file = dir.0.join("omnimem.env");
    std::fs::write(&file, "MCP_PORT=9100\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
    }
    let panel = Panel::new();
    panel.set_settings_path(file.clone());
    panel.set_secret_store(Arc::new(SettingsFileSecrets::new(file.clone())));

    let page = text(&get(&panel, "/configuration").await);
    assert!(page.contains("secrets\n  to the settings file"), "{page}");

    let saved = landed(&post(&panel, "MCP_PORT=9100&OAUTH_ADMIN_PASSWORD=s3cret").await);
    assert!(
        saved.starts_with("/configuration?message=Saved."),
        "{saved}"
    );
    let written = std::fs::read_to_string(&file).unwrap();
    assert!(written.contains("OAUTH_ADMIN_PASSWORD=s3cret"), "{written}");
    assert!(written.contains("MCP_PORT=9100"), "{written}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640, "the rewrite keeps the file's permissions");
    }

    let page = text(&get(&panel, "/configuration").await);
    assert!(!page.contains("s3cret"), "a saved secret is never shown");
    assert!(page.contains("Saved. Type to replace it"));

    // Saving ordinary settings again leaves the secret in place.
    landed(&post(&panel, "MCP_PORT=9200").await);
    let written = std::fs::read_to_string(&file).unwrap();
    assert!(written.contains("OAUTH_ADMIN_PASSWORD=s3cret") && written.contains("MCP_PORT=9200"));

    landed(&post(&panel, "MCP_PORT=9200&clear:OAUTH_ADMIN_PASSWORD=on").await);
    assert!(
        !std::fs::read_to_string(&file)
            .unwrap()
            .contains("OAUTH_ADMIN_PASSWORD")
    );
}
