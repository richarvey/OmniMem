//! Non-memory records: what were `meta:*`, `log:recall:*`, `topics:*` and
//! cache keys in Valkey. Hashes, sets and strings, with optional expiry.

use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use crate::store::{Fields, validate_key};
use crate::time::now;
use crate::{Result, Store, StoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Hash,
    Set,
    String,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Hash => "hash",
            Kind::Set => "set",
            Kind::String => "string",
        }
    }

    fn parse(s: &str) -> Kind {
        match s {
            "set" => Kind::Set,
            "string" => Kind::String,
            _ => Kind::Hash,
        }
    }
}

struct Entry {
    kind: Kind,
    value: Value,
    expires_at: Option<f64>,
}

/// A live entry. An expired one is deleted on sight and reads as absent.
fn load(conn: &Connection, key: &str) -> Result<Option<Entry>> {
    let row: Option<(String, String, Option<f64>)> = conn
        .query_row(
            "SELECT kind, value, expires_at FROM kv WHERE key = ?1",
            params![key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((kind, raw, expires_at)) = row else {
        return Ok(None);
    };
    if expires_at.is_some_and(|t| t <= now()) {
        conn.execute("DELETE FROM kv WHERE key = ?1", params![key])?;
        return Ok(None);
    }
    let value = serde_json::from_str(&raw).map_err(|source| StoreError::CorruptRecord {
        key: key.to_owned(),
        source,
    })?;
    Ok(Some(Entry {
        kind: Kind::parse(&kind),
        value,
        expires_at,
    }))
}

fn save(
    conn: &Connection,
    key: &str,
    kind: Kind,
    value: &Value,
    expires_at: Option<f64>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO kv (key, kind, value, expires_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (key) DO UPDATE SET kind = excluded.kind, value = excluded.value, expires_at = excluded.expires_at",
        params![key, kind.as_str(), value.to_string(), expires_at],
    )?;
    Ok(())
}

fn expect(key: &str, entry: &Entry, expected: Kind) -> Result<()> {
    if entry.kind == expected {
        Ok(())
    } else {
        Err(StoreError::WrongType {
            key: key.to_owned(),
            expected: expected.as_str(),
            actual: entry.kind.as_str(),
        })
    }
}

fn hash_from(value: &Value) -> Fields {
    value
        .as_object()
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

/// HSET on a non-memory key: merge fields, keep any expiry.
pub(crate) fn hash_merge(conn: &Connection, key: &str, fields: &Fields) -> Result<()> {
    hash_merge_expiring(conn, key, fields, None)
}

/// As [`hash_merge`], setting the expiry when `expires_at` is given.
pub(crate) fn hash_merge_expiring(
    conn: &Connection,
    key: &str,
    fields: &Fields,
    expires_at: Option<f64>,
) -> Result<()> {
    let (mut current, kept_expiry) = match load(conn, key)? {
        Some(entry) => {
            expect(key, &entry, Kind::Hash)?;
            (hash_from(&entry.value), entry.expires_at)
        }
        None => (Fields::new(), None),
    };
    current.extend(fields.iter().map(|(k, v)| (k.clone(), v.clone())));
    save(
        conn,
        key,
        Kind::Hash,
        &serde_json::to_value(&current).unwrap_or_default(),
        expires_at.or(kept_expiry),
    )
}

pub(crate) fn set_union(conn: &Connection, key: &str, members: &[String]) -> Result<usize> {
    let (mut current, expiry) = match load(conn, key)? {
        Some(entry) => {
            expect(key, &entry, Kind::Set)?;
            let members: Vec<String> = entry
                .value
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            (members, entry.expires_at)
        }
        None => (Vec::new(), None),
    };
    let before = current.len();
    current.extend(members.iter().cloned());
    current.sort();
    current.dedup();
    let added = current.len() - before;
    save(conn, key, Kind::Set, &Value::from(current), expiry)?;
    Ok(added)
}

impl Store {
    /// All fields of a non-memory hash.
    pub fn hash_get_all(&self, key: &str) -> Result<Option<Fields>> {
        let conn = self.conn();
        match load(&conn, key)? {
            Some(entry) => {
                expect(key, &entry, Kind::Hash)?;
                Ok(Some(hash_from(&entry.value)).filter(|f| !f.is_empty()))
            }
            None => Ok(None),
        }
    }

    pub fn hash_set(&self, key: &str, fields: &Fields) -> Result<()> {
        validate_key(key)?;
        hash_merge(&self.conn(), key, fields)
    }

    /// HINCRBY: add `by` to an integer field, creating it at zero.
    pub fn hash_incr(&self, key: &str, field: &str, by: i64) -> Result<i64> {
        validate_key(key)?;
        let conn = self.conn();
        let mut fields = match load(&conn, key)? {
            Some(entry) => {
                expect(key, &entry, Kind::Hash)?;
                hash_from(&entry.value)
            }
            None => Fields::new(),
        };
        let current = match fields.get(field) {
            None => 0,
            Some(raw) => raw
                .trim()
                .parse::<i64>()
                .map_err(|_| StoreError::NotAnInteger {
                    key: key.to_owned(),
                    field: field.to_owned(),
                })?,
        };
        // A counter fed from outside (a backup, a bundle) may already sit at
        // the edge; clamp rather than panic in debug or wrap in release.
        let next = current.saturating_add(by);
        fields.insert(field.to_owned(), next.to_string());
        hash_merge(&conn, key, &fields)?;
        Ok(next)
    }

    pub fn hash_delete_fields(&self, key: &str, fields: &[&str]) -> Result<usize> {
        let conn = self.conn();
        let Some(entry) = load(&conn, key)? else {
            return Ok(0);
        };
        expect(key, &entry, Kind::Hash)?;
        let mut current = hash_from(&entry.value);
        let removed = fields
            .iter()
            .filter(|f| current.remove(**f).is_some())
            .count();
        if current.is_empty() {
            conn.execute("DELETE FROM kv WHERE key = ?1", params![key])?;
        } else {
            save(
                &conn,
                key,
                Kind::Hash,
                &serde_json::to_value(&current).unwrap_or_default(),
                entry.expires_at,
            )?;
        }
        Ok(removed)
    }

    /// SADD: the number of members that were new.
    pub fn set_add(&self, key: &str, members: &[String]) -> Result<usize> {
        validate_key(key)?;
        set_union(&self.conn(), key, members)
    }

    /// SREM: the number of members removed.
    pub fn set_remove(&self, key: &str, members: &[String]) -> Result<usize> {
        let conn = self.conn();
        let Some(entry) = load(&conn, key)? else {
            return Ok(0);
        };
        expect(key, &entry, Kind::Set)?;
        let current: Vec<String> = entry
            .value
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let kept: Vec<String> = current
            .iter()
            .filter(|m| !members.contains(m))
            .cloned()
            .collect();
        let removed = current.len() - kept.len();
        if kept.is_empty() {
            conn.execute("DELETE FROM kv WHERE key = ?1", params![key])?;
        } else {
            save(&conn, key, Kind::Set, &Value::from(kept), entry.expires_at)?;
        }
        Ok(removed)
    }

    /// SMEMBERS, sorted.
    pub fn set_members(&self, key: &str) -> Result<Vec<String>> {
        let conn = self.conn();
        match load(&conn, key)? {
            Some(entry) => {
                expect(key, &entry, Kind::Set)?;
                Ok(entry
                    .value
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default())
            }
            None => Ok(Vec::new()),
        }
    }

    pub fn string_get(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn();
        match load(&conn, key)? {
            Some(entry) => {
                expect(key, &entry, Kind::String)?;
                Ok(entry.value.as_str().map(str::to_owned))
            }
            None => Ok(None),
        }
    }

    /// SET, with EX when `ttl` is given. Without one, any expiry is cleared.
    pub fn string_set(&self, key: &str, value: &str, ttl: Option<Duration>) -> Result<()> {
        validate_key(key)?;
        let expires_at = ttl.map(|t| now() + t.as_secs_f64());
        save(
            &self.conn(),
            key,
            Kind::String,
            &Value::from(value),
            expires_at,
        )
    }

    /// EXPIRE: false when the key doesn't exist.
    pub fn expire(&self, key: &str, ttl: Duration) -> Result<bool> {
        let conn = self.conn();
        if key.starts_with("mem:") {
            return Ok(false);
        }
        if load(&conn, key)?.is_none() {
            return Ok(false);
        }
        conn.execute(
            "UPDATE kv SET expires_at = ?1 WHERE key = ?2",
            params![now() + ttl.as_secs_f64(), key],
        )?;
        Ok(true)
    }

    /// DEL on a non-memory key: true when something was removed.
    pub fn kv_delete(&self, key: &str) -> Result<bool> {
        Ok(self
            .conn()
            .execute("DELETE FROM kv WHERE key = ?1", params![key])?
            > 0)
    }

    /// Delete every expired entry. Reads already ignore them; this reclaims space.
    pub fn purge_expired(&self) -> Result<usize> {
        Ok(self.conn().execute(
            "DELETE FROM kv WHERE expires_at IS NOT NULL AND expires_at <= ?1",
            params![now()],
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(pairs: &[(&str, &str)]) -> Fields {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn hashes_merge_and_count() {
        let store = Store::open_in_memory().unwrap();
        store
            .hash_set("meta:tool_metrics:recall", &fields(&[("call_count", "2")]))
            .unwrap();
        assert_eq!(
            store
                .hash_incr("meta:tool_metrics:recall", "call_count", 3)
                .unwrap(),
            5
        );
        assert_eq!(
            store
                .hash_incr("meta:tool_metrics:recall", "error_count", 1)
                .unwrap(),
            1
        );
        store
            .hash_set(
                "meta:tool_metrics:recall",
                &fields(&[("last_called_at", "9")]),
            )
            .unwrap();
        let all = store
            .hash_get_all("meta:tool_metrics:recall")
            .unwrap()
            .unwrap();
        assert_eq!(
            all,
            fields(&[
                ("call_count", "5"),
                ("error_count", "1"),
                ("last_called_at", "9")
            ])
        );
    }

    #[test]
    fn incrementing_text_is_an_error() {
        let store = Store::open_in_memory().unwrap();
        store.hash_set("meta:x", &fields(&[("n", "many")])).unwrap();
        assert!(matches!(
            store.hash_incr("meta:x", "n", 1),
            Err(StoreError::NotAnInteger { .. })
        ));
    }

    #[test]
    fn sets_are_unique_and_sorted() {
        let store = Store::open_in_memory().unwrap();
        let members = |m: &[&str]| m.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            store
                .set_add("topics:suppressed", &members(&["b", "a", "b"]))
                .unwrap(),
            2
        );
        assert_eq!(
            store
                .set_add("topics:suppressed", &members(&["a", "c"]))
                .unwrap(),
            1
        );
        assert_eq!(
            store.set_members("topics:suppressed").unwrap(),
            ["a", "b", "c"]
        );
        assert_eq!(
            store
                .set_remove("topics:suppressed", &members(&["a", "zz"]))
                .unwrap(),
            1
        );
        assert_eq!(store.set_members("topics:suppressed").unwrap(), ["b", "c"]);
    }

    #[test]
    fn strings_expire() {
        let store = Store::open_in_memory().unwrap();
        store
            .string_set("meta:dashboard_stats", "{}", Some(Duration::from_secs(60)))
            .unwrap();
        assert_eq!(
            store.string_get("meta:dashboard_stats").unwrap().as_deref(),
            Some("{}")
        );
        store
            .string_set("meta:gone", "x", Some(Duration::from_millis(1)))
            .unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(store.string_get("meta:gone").unwrap(), None);
        assert!(
            !store
                .scan_prefix("meta:")
                .unwrap()
                .contains(&"meta:gone".to_owned())
        );
    }

    #[test]
    fn wrong_types_are_refused() {
        let store = Store::open_in_memory().unwrap();
        store.string_set("meta:s", "x", None).unwrap();
        assert!(matches!(
            store.hash_get_all("meta:s"),
            Err(StoreError::WrongType { .. })
        ));
        assert!(matches!(
            store.set_members("meta:s"),
            Err(StoreError::WrongType { .. })
        ));
    }

    #[test]
    fn unknown_prefixes_are_refused() {
        let store = Store::open_in_memory().unwrap();
        assert!(matches!(
            store.string_set("secret:x", "y", None),
            Err(StoreError::InvalidKey(_))
        ));
    }
}
