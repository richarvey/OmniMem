//! Memory namespaces and key shapes.
//!
//! Keys keep the 6.x form, `mem:{namespace}:{id}`, so a backup import and
//! every link the web UI has ever rendered stay valid. New memories get a
//! ULID id; imported ones keep whatever id they arrived with (older keys and
//! RSS articles use other id shapes), which is why parsing accepts any
//! non-empty id rather than insisting on a ULID.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use ulid::Ulid;

/// Every key starts with this.
pub const KEY_PREFIX: &str = "mem:";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Namespace {
    Episodic,
    Project,
    Knowledge,
    Preference,
    /// Compiled skills. Searchable, but written only through the
    /// propose-and-accept compile gate, never by `remember`.
    Skill,
}

impl Namespace {
    pub const ALL: [Self; 5] = [
        Self::Episodic,
        Self::Project,
        Self::Knowledge,
        Self::Preference,
        Self::Skill,
    ];

    /// The namespaces `remember` may write to.
    pub const WRITABLE: [Self; 4] = [
        Self::Episodic,
        Self::Project,
        Self::Knowledge,
        Self::Preference,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Episodic => "episodic",
            Self::Project => "project",
            Self::Knowledge => "knowledge",
            Self::Preference => "preference",
            Self::Skill => "skill",
        }
    }

    pub fn is_writable(self) -> bool {
        self != Self::Skill
    }
}

impl fmt::Display for Namespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Namespace {
    type Err = KeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|ns| ns.as_str() == s)
            .ok_or_else(|| KeyError::UnknownNamespace(s.to_owned()))
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum KeyError {
    #[error("key must start with '{KEY_PREFIX}': {0}")]
    MissingPrefix(String),
    #[error("unknown namespace '{0}'")]
    UnknownNamespace(String),
    #[error("key has no id after the namespace: {0}")]
    MissingId(String),
}

/// A parsed `mem:{namespace}:{id}` key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MemoryKey {
    namespace: Namespace,
    id: String,
}

impl MemoryKey {
    /// A fresh key with a new ULID.
    pub fn generate(namespace: Namespace) -> Self {
        Self {
            namespace,
            id: Ulid::generate().to_string(),
        }
    }

    pub fn namespace(&self) -> Namespace {
        self.namespace
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

impl fmt::Display for MemoryKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{KEY_PREFIX}{}:{}", self.namespace, self.id)
    }
}

impl FromStr for MemoryKey {
    type Err = KeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let rest = s
            .strip_prefix(KEY_PREFIX)
            .ok_or_else(|| KeyError::MissingPrefix(s.to_owned()))?;
        let (ns, id) = rest
            .split_once(':')
            .ok_or_else(|| KeyError::MissingId(s.to_owned()))?;
        let namespace = ns.parse()?;
        if id.is_empty() {
            return Err(KeyError::MissingId(s.to_owned()));
        }
        Ok(Self {
            namespace,
            id: id.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_6x_keys() {
        for raw in [
            "mem:episodic:01M2J7Z8BMBBA08VSKEMJ76R19",
            "mem:knowledge:8f216ec363408e7c",
            "mem:skill:gen:preferences-local",
            "mem:project:omnimem",
        ] {
            let key: MemoryKey = raw.parse().unwrap();
            assert_eq!(key.to_string(), raw);
        }
    }

    #[test]
    fn skill_ids_keep_their_colons() {
        let key: MemoryKey = "mem:skill:gen:python-local".parse().unwrap();
        assert_eq!(key.namespace(), Namespace::Skill);
        assert_eq!(key.id(), "gen:python-local");
    }

    #[test]
    fn rejects_malformed_keys() {
        assert!(matches!(
            "episodic:01A".parse::<MemoryKey>(),
            Err(KeyError::MissingPrefix(_))
        ));
        assert!(matches!(
            "mem:secret:01A".parse::<MemoryKey>(),
            Err(KeyError::UnknownNamespace(_))
        ));
        assert!(matches!(
            "mem:episodic".parse::<MemoryKey>(),
            Err(KeyError::MissingId(_))
        ));
        assert!(matches!(
            "mem:episodic:".parse::<MemoryKey>(),
            Err(KeyError::MissingId(_))
        ));
    }

    #[test]
    fn generated_keys_are_ulids_in_the_namespace() {
        let key = MemoryKey::generate(Namespace::Preference);
        assert!(key.to_string().starts_with("mem:preference:"));
        assert!(Ulid::from_string(key.id()).is_ok());
    }

    #[test]
    fn only_skill_is_unwritable() {
        assert!(Namespace::WRITABLE.iter().all(|ns| ns.is_writable()));
        assert!(!Namespace::Skill.is_writable());
    }
}
