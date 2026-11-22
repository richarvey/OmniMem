//! Every URL a template writes must carry the `/admin` prefix. A bare
//! `/feeds/...` reaches the MCP router instead of the panel, which answers an
//! empty 404: a blank white page after clicking Save.

use std::fs;
use std::path::Path;

const ATTRIBUTES: [&str; 6] = ["action", "href", "src", "hx-get", "hx-post", "formaction"];

fn walk(dir: &Path, found: &mut Vec<String>) {
    for entry in fs::read_dir(dir).expect("templates folder") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            walk(&path, found);
            continue;
        }
        if path.extension().is_none_or(|e| e != "html") {
            continue;
        }
        let body = fs::read_to_string(&path).expect("template");
        for (number, line) in body.lines().enumerate() {
            for attribute in ATTRIBUTES {
                let needle = format!(" {attribute}=\"");
                let mut rest = line;
                while let Some(at) = rest.find(&needle) {
                    let value = &rest[at + needle.len()..];
                    let bare_path = value.starts_with('/') && !value.starts_with("//");
                    if bare_path || value.starts_with("{%") {
                        found.push(format!(
                            "{}:{}: {}",
                            path.display(),
                            number + 1,
                            line.trim()
                        ));
                    }
                    rest = value;
                }
            }
        }
    }
}

#[test]
fn every_template_url_starts_with_the_admin_prefix() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
    let mut found = Vec::new();
    walk(&dir, &mut found);
    assert!(
        found.is_empty(),
        "URLs without {{{{ admin }}}} in front:\n{}",
        found.join("\n")
    );
}
