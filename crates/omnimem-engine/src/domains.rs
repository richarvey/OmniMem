//! Work-type domains and domain-to-project routing
//! (`memory/project_domains.py`, and the domain helpers of `memory/skills.py`).

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};
use std::time::Instant;

use regex::Regex;
use serde_json::Value;

use crate::{Engine, Result};

pub const MAX_PROJECT_DOMAINS: usize = 20;

pub const DOMAIN_ALIASES: [(&str, &str); 8] = [
    ("py", "python"),
    ("python3", "python"),
    ("js", "javascript"),
    ("ts", "typescript"),
    ("golang", "go"),
    ("rs", "rust"),
    ("k8s", "kubernetes"),
    ("postgres", "postgresql"),
];

static SAFE_DOMAIN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9._\-]{0,63}$").expect("valid"));
static WHITESPACE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("valid"));
static SOURCE_SPLIT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[,/|+&\n;]+|\s+and\s+").expect("valid"));

pub fn normalise_domain(domain: &str) -> String {
    WHITESPACE_RE
        .replace_all(&domain.trim().to_lowercase(), "-")
        .into_owned()
}

/// (canonical, was_aliased)
pub fn resolve_domain(domain: &str) -> (String, bool) {
    let normalised = normalise_domain(domain);
    match DOMAIN_ALIASES
        .iter()
        .find(|(alias, _)| *alias == normalised)
    {
        Some((_, canonical)) => ((*canonical).to_owned(), true),
        None => (normalised, false),
    }
}

pub fn is_valid_domain(domain: &str) -> bool {
    !domain.is_empty() && SAFE_DOMAIN_RE.is_match(domain)
}

/// A stored or submitted domain list in any shape it arrives in.
pub enum DomainInput<'a> {
    Text(&'a str),
    List(&'a [String]),
}

pub fn parse_domains(raw: DomainInput<'_>) -> Vec<String> {
    let values: Vec<String> = match raw {
        DomainInput::List(items) => items.to_vec(),
        DomainInput::Text(text) => {
            let text = text.trim();
            if text.is_empty() {
                return Vec::new();
            }
            let decoded = text
                .starts_with('[')
                .then(|| serde_json::from_str::<Vec<Value>>(text).ok())
                .flatten();
            match decoded {
                Some(items) => items
                    .into_iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect(),
                None => SOURCE_SPLIT_RE.split(text).map(str::to_owned).collect(),
            }
        }
    };
    let mut out: Vec<String> = Vec::new();
    for value in values {
        let item = value.trim();
        if !item.is_empty() && !out.iter().any(|o| o == item) {
            out.push(item.to_owned());
        }
    }
    out
}

pub struct Normalised {
    pub domains: Vec<String>,
    pub aliased: Vec<(String, String)>,
    pub rejected: Vec<String>,
}

pub fn normalise_domains(raw: DomainInput<'_>) -> Normalised {
    let mut result = Normalised {
        domains: Vec::new(),
        aliased: Vec::new(),
        rejected: Vec::new(),
    };
    for item in parse_domains(raw) {
        let (canonical, was_aliased) = resolve_domain(&item);
        if !is_valid_domain(&canonical) {
            result.rejected.push(item);
            continue;
        }
        if was_aliased {
            result.aliased.push((item, canonical.clone()));
        }
        if result.domains.contains(&canonical) {
            continue;
        }
        result.domains.push(canonical);
        if result.domains.len() >= MAX_PROJECT_DOMAINS {
            break;
        }
    }
    result
}

pub struct DomainResolution {
    pub requested: Vec<String>,
    /// In requested order.
    pub matched: Vec<(String, Vec<String>)>,
    pub unmatched: Vec<String>,
}

impl DomainResolution {
    /// Every project named by any matched domain, deduplicated and sorted.
    pub fn projects(&self) -> Vec<String> {
        let mut all: Vec<String> = self
            .matched
            .iter()
            .flat_map(|(_, p)| p.iter().cloned())
            .collect();
        all.sort();
        all.dedup();
        all
    }

    pub fn fully_unmatched(&self) -> bool {
        !self.requested.is_empty() && self.matched.is_empty()
    }
}

fn project_name_from(key: &str, row: &omnimem_store::Fields) -> String {
    row.get("project_name")
        .filter(|v| !v.is_empty())
        .or_else(|| row.get("project").filter(|v| !v.is_empty()))
        .cloned()
        .unwrap_or_else(|| key.rsplit(':').next().unwrap_or("").to_owned())
}

impl Engine {
    /// {domain: [project names]} across every live project context.
    pub fn domain_map(&self) -> Result<Arc<BTreeMap<String, Vec<String>>>> {
        let ttl = self.config.domain_cache_ttl;
        {
            let cache = self
                .domain_map
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some((at, map)) = cache.as_ref()
                && !ttl.is_zero()
                && at.elapsed() < ttl
            {
                return Ok(map.clone());
            }
        }
        let mut mapping: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let keys = self.store.scan_prefix("mem:project:")?;
        let rows = self
            .store
            .get_fields_multi(&keys, &["project_name", "project", "domains", "state"])?;
        for (key, row) in keys.iter().zip(rows) {
            let Some(row) = row else { continue };
            if matches!(
                row.get("state").map(String::as_str),
                Some("archived" | "deleted")
            ) {
                continue;
            }
            let domains = normalise_domains(DomainInput::Text(
                row.get("domains").map_or("", String::as_str),
            ))
            .domains;
            if domains.is_empty() {
                continue;
            }
            let name = project_name_from(key, &row);
            for domain in domains {
                let bucket = mapping.entry(domain).or_default();
                if !bucket.contains(&name) {
                    bucket.push(name.clone());
                }
            }
        }
        for bucket in mapping.values_mut() {
            bucket.sort_by_key(|name| name.to_lowercase());
        }
        let mapping = Arc::new(mapping);
        *self
            .domain_map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((Instant::now(), mapping.clone()));
        Ok(mapping)
    }

    pub fn resolve_projects_for_domains(
        &self,
        domains: DomainInput<'_>,
    ) -> Result<DomainResolution> {
        let requested = normalise_domains(domains).domains;
        let mapping = self.domain_map()?;
        let mut matched = Vec::new();
        let mut unmatched = Vec::new();
        for domain in &requested {
            match mapping.get(domain).filter(|names| !names.is_empty()) {
                Some(names) => matched.push((domain.clone(), names.clone())),
                None => unmatched.push(domain.clone()),
            }
        }
        Ok(DomainResolution {
            requested,
            matched,
            unmatched,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_and_validation() {
        assert_eq!(resolve_domain("  Py "), ("python".to_owned(), true));
        assert_eq!(
            resolve_domain("Technical Blogging"),
            ("technical-blogging".to_owned(), false)
        );
        assert!(!is_valid_domain("-bad"));
        assert!(is_valid_domain("wcag-accessibility"));
    }

    #[test]
    fn parsing_shapes() {
        assert_eq!(
            parse_domains(DomainInput::Text("Python and Docker, css/html")),
            ["Python", "Docker", "css", "html"]
        );
        assert_eq!(
            parse_domains(DomainInput::Text(r#"["python", "go"]"#)),
            ["python", "go"]
        );
        let n = normalise_domains(DomainInput::Text("py, python, k8s, !!"));
        assert_eq!(n.domains, ["python", "kubernetes"]);
        assert_eq!(n.rejected, ["!!"]);
    }
}
