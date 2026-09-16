//! The MCP server's OAuth state: registered clients, authorisation codes, and
//! access and refresh tokens. In 6.x these were `oauth:*` Valkey keys with
//! TTLs.
//!
//! Codes and tokens are stored under the SHA-256 of their value, so a copy of
//! the database holds nothing a client could present. A read treats an
//! expired row as absent and deletes it, as a Valkey TTL would have.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::time::now;
use crate::{Result, Store, StoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Access,
    Refresh,
}

impl TokenKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Access => "access",
            Self::Refresh => "refresh",
        }
    }
}

/// The SHA-256 of a secret, as lowercase hex.
pub fn secret_hash(secret: &str) -> String {
    use std::fmt::Write as _;
    Sha256::digest(secret.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        })
}

fn parse(what: &str, raw: &str) -> Result<Value> {
    serde_json::from_str(raw).map_err(|source| StoreError::CorruptRecord {
        key: what.to_owned(),
        source,
    })
}

/// The OAuth tables, inside the transaction [`Store::with_oauth`] opened.
pub struct OAuthStore<'a> {
    conn: &'a Connection,
}

impl OAuthStore<'_> {
    /// Register or replace a client. Clients never expire.
    pub fn save_client(&self, client_id: &str, info: &Value) -> Result<()> {
        self.conn.execute(
            "INSERT INTO oauth_clients (client_id, info, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (client_id) DO UPDATE SET info = excluded.info",
            params![client_id, info.to_string(), now()],
        )?;
        Ok(())
    }

    pub fn client_count(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM oauth_clients", [], |r| r.get(0))?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    /// Delete clients registered more than `idle_seconds` ago that hold no
    /// live token: registrations that never signed in, which anyone who can
    /// reach the server can create. Returns how many went.
    pub fn purge_idle_clients(&self, idle_seconds: f64) -> Result<usize> {
        let at = now();
        Ok(self.conn.execute(
            "DELETE FROM oauth_clients
             WHERE created_at <= ?1
               AND client_id NOT IN (
                   SELECT json_extract(record, '$.client_id') FROM oauth_tokens
                   WHERE expires_at > ?2 AND json_extract(record, '$.client_id') IS NOT NULL
               )",
            params![at - idle_seconds, at],
        )?)
    }

    pub fn client(&self, client_id: &str) -> Result<Option<Value>> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT info FROM oauth_clients WHERE client_id = ?1",
                params![client_id],
                |r| r.get(0),
            )
            .optional()?;
        raw.map(|raw| parse(&format!("oauth client {client_id}"), &raw))
            .transpose()
    }

    pub fn save_code(&self, code: &str, grant: &Value, expires_at: f64) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO oauth_codes (code_hash, grant_info, expires_at) VALUES (?1, ?2, ?3)",
            params![secret_hash(code), grant.to_string(), expires_at],
        )?;
        Ok(())
    }

    pub fn code(&self, code: &str) -> Result<Option<Value>> {
        let hash = secret_hash(code);
        let row: Option<(String, f64)> = self
            .conn
            .query_row(
                "SELECT grant_info, expires_at FROM oauth_codes WHERE code_hash = ?1",
                params![hash],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((raw, expires_at)) = row else {
            return Ok(None);
        };
        if expires_at <= now() {
            self.conn.execute(
                "DELETE FROM oauth_codes WHERE code_hash = ?1",
                params![hash],
            )?;
            return Ok(None);
        }
        parse("oauth code", &raw).map(Some)
    }

    /// True when the code was there to delete.
    pub fn delete_code(&self, code: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "DELETE FROM oauth_codes WHERE code_hash = ?1",
            params![secret_hash(code)],
        )? > 0)
    }

    pub fn save_token(
        &self,
        kind: TokenKind,
        token: &str,
        record: &Value,
        expires_at: f64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO oauth_tokens (token_hash, kind, record, expires_at) VALUES (?1, ?2, ?3, ?4)",
            params![secret_hash(token), kind.as_str(), record.to_string(), expires_at],
        )?;
        Ok(())
    }

    pub fn token(&self, kind: TokenKind, token: &str) -> Result<Option<Value>> {
        let hash = secret_hash(token);
        let row: Option<(String, f64)> = self
            .conn
            .query_row(
                "SELECT record, expires_at FROM oauth_tokens WHERE kind = ?1 AND token_hash = ?2",
                params![kind.as_str(), hash],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((raw, expires_at)) = row else {
            return Ok(None);
        };
        if expires_at <= now() {
            self.conn.execute(
                "DELETE FROM oauth_tokens WHERE kind = ?1 AND token_hash = ?2",
                params![kind.as_str(), hash],
            )?;
            return Ok(None);
        }
        parse("oauth token", &raw).map(Some)
    }

    /// True for a token that exists and hasn't expired, without touching the
    /// table: the check every `/mcp` request makes.
    pub fn token_is_live(&self, kind: TokenKind, token: &str) -> Result<bool> {
        let expires_at: Option<f64> = self
            .conn
            .query_row(
                "SELECT expires_at FROM oauth_tokens WHERE kind = ?1 AND token_hash = ?2",
                params![kind.as_str(), secret_hash(token)],
                |r| r.get(0),
            )
            .optional()?;
        Ok(expires_at.is_some_and(|t| t > now()))
    }

    /// True when the token was there to delete.
    pub fn delete_token(&self, kind: TokenKind, token: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "DELETE FROM oauth_tokens WHERE kind = ?1 AND token_hash = ?2",
            params![kind.as_str(), secret_hash(token)],
        )? > 0)
    }

    /// The raw connection, for tests that need to backdate rows.
    #[doc(hidden)]
    pub fn conn_for_tests(&self) -> &Connection {
        self.conn
    }

    /// Delete every expired code and token. Reads already ignore them; this
    /// keeps the tables from growing with every login and refresh.
    pub fn purge_expired(&self) -> Result<usize> {
        let at = now();
        Ok(self.conn.execute(
            "DELETE FROM oauth_codes WHERE expires_at <= ?1",
            params![at],
        )? + self.conn.execute(
            "DELETE FROM oauth_tokens WHERE expires_at <= ?1",
            params![at],
        )?)
    }
}

impl Store {
    /// Run `f` against the OAuth tables in one transaction that holds the
    /// store's connection throughout, so a check and the write that depends on
    /// it (a code consumed once, a refresh token rotated once) can't
    /// interleave with another request. Nothing is committed if `f` fails.
    pub fn with_oauth<T, E: From<StoreError>>(
        &self,
        f: impl FnOnce(&OAuthStore<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::from)?;
        let value = f(&OAuthStore { conn: &tx })?;
        tx.commit().map_err(StoreError::from)?;
        Ok(value)
    }

    /// A read of the OAuth tables that takes no write lock, for the check on
    /// every `/mcp` request. `f` must not write.
    pub fn read_oauth<T, E: From<StoreError>>(
        &self,
        f: impl FnOnce(&OAuthStore<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        let conn = self.conn();
        f(&OAuthStore { conn: &conn })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn clients_round_trip() {
        let store = Store::open_in_memory().unwrap();
        store
            .with_oauth(|o| o.save_client("c1", &json!({"client_id": "c1"})))
            .unwrap();
        assert_eq!(
            store.with_oauth(|o| o.client("c1")).unwrap(),
            Some(json!({"client_id": "c1"}))
        );
        assert_eq!(store.with_oauth(|o| o.client("nope")).unwrap(), None);
    }

    #[test]
    fn codes_are_kept_by_hash_and_expire() {
        let store = Store::open_in_memory().unwrap();
        store
            .with_oauth(|o| {
                o.save_code("live", &json!({"n": 1}), now() + 60.0)?;
                o.save_code("stale", &json!({"n": 2}), now() - 1.0)
            })
            .unwrap();
        {
            let conn = store.conn();
            let mut statement = conn.prepare("SELECT code_hash FROM oauth_codes").unwrap();
            let stored: Vec<String> = statement
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            assert!(stored.contains(&secret_hash("live")));
            assert!(!stored.iter().any(|h| h == "live"));
        }
        store
            .with_oauth(|o| {
                assert_eq!(o.code("live")?, Some(json!({"n": 1})));
                assert_eq!(o.code("stale")?, None);
                assert!(o.delete_code("live")?);
                assert!(!o.delete_code("live")?);
                Ok::<_, StoreError>(())
            })
            .unwrap();
    }

    #[test]
    fn tokens_are_separated_by_kind_and_purged() {
        let store = Store::open_in_memory().unwrap();
        store
            .with_oauth(|o| {
                o.save_token(TokenKind::Access, "t", &json!({"k": "a"}), now() + 60.0)?;
                o.save_token(TokenKind::Refresh, "t", &json!({"k": "r"}), now() + 60.0)?;
                o.save_token(TokenKind::Access, "old", &json!({}), now() - 1.0)?;
                assert!(o.delete_token(TokenKind::Access, "t")?);
                assert_eq!(o.token(TokenKind::Access, "t")?, None);
                assert_eq!(o.token(TokenKind::Refresh, "t")?, Some(json!({"k": "r"})));
                assert_eq!(o.purge_expired()?, 1);
                Ok::<_, StoreError>(())
            })
            .unwrap();
    }

    #[test]
    fn idle_clients_are_purged_and_live_ones_kept() {
        let store = Store::open_in_memory().unwrap();
        store
            .with_oauth(|o| {
                o.save_client("idle", &json!({"client_id": "idle"}))?;
                o.save_client("active", &json!({"client_id": "active"}))?;
                o.save_token(
                    TokenKind::Access,
                    "t",
                    &json!({"client_id": "active"}),
                    now() + 60.0,
                )?;
                assert_eq!(o.client_count()?, 2);
                // Nothing is old enough yet.
                assert_eq!(o.purge_idle_clients(3600.0)?, 0);
                assert_eq!(o.purge_idle_clients(0.0)?, 1);
                assert!(o.client("idle")?.is_none());
                assert!(o.client("active")?.is_some());
                assert!(o.token_is_live(TokenKind::Access, "t")?);
                assert!(!o.token_is_live(TokenKind::Refresh, "t")?);
                Ok::<_, StoreError>(())
            })
            .unwrap();
        assert!(
            store
                .read_oauth(|o| o.token_is_live(TokenKind::Access, "t"))
                .unwrap()
        );
    }

    #[test]
    fn a_failed_closure_writes_nothing() {
        let store = Store::open_in_memory().unwrap();
        let result: Result<(), StoreError> = store.with_oauth(|o| {
            o.save_client("c1", &json!({}))?;
            Err(StoreError::Backup("stop".into()))
        });
        assert!(result.is_err());
        assert_eq!(store.with_oauth(|o| o.client("c1")).unwrap(), None);
    }
}
