//! `omnimem trust`: a person confirms or rejects what OmniMem concluded
//! (#23). Like the settings panel, and unlike every MCP tool, this can
//! change a memory's trust, so an agent can't vouch for its own conclusions.

use std::path::Path;

use anyhow::{Result, anyhow};
use clap::Subcommand;
use omnimem_engine::Engine;
use omnimem_engine::trust::TrustAction;
use serde_json::json;

#[derive(Subcommand)]
pub(crate) enum TrustCommand {
    /// List candidates: facts and preferences OmniMem concluded that nobody
    /// has confirmed yet. Newest first.
    List {
        /// Only candidates extracted from this memory.
        #[arg(long)]
        source: Option<String>,
        /// Most to show.
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// One JSON object per line.
        #[arg(long)]
        json: bool,
    },
    /// Believe candidates after all.
    Confirm {
        #[arg(required = true)]
        keys: Vec<String>,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Mark a memory wrong. It stops being recalled; `restore` undoes it.
    Reject {
        key: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Undo a rejection.
    Restore {
        key: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Confirm every candidate extracted from one memory.
    ConfirmSource {
        source_key: String,
        #[arg(long)]
        reason: Option<String>,
    },
}

/// One line of text from a record, for a list.
fn snippet(text: &str) -> String {
    text.chars().take(80).collect::<String>().replace('\n', " ")
}

pub(crate) fn run(db: &Path, command: TrustCommand) -> Result<()> {
    let engine = crate::hook::open_engine(db)?;
    run_with(&engine, command)
}

fn run_with(engine: &Engine, command: TrustCommand) -> Result<()> {
    let act = |key: &str, action: TrustAction, reason: Option<&str>| -> Result<()> {
        // `Invalid` carries a message meant for a person, so show it as is.
        let done = engine
            .set_trust(key, action, reason)
            .map_err(|e| anyhow!("{e}"))?;
        println!(
            "{key}: {} -> {}",
            done["was"].as_str().unwrap_or("?"),
            done["trust"].as_str().unwrap_or("?")
        );
        Ok(())
    };
    match command {
        TrustCommand::List {
            source,
            limit,
            json,
        } => {
            let rows = engine
                .unconfirmed_from(source.as_deref(), limit)
                .map_err(|e| anyhow!("{e}"))?;
            if rows.is_empty() && !json {
                eprintln!("no unconfirmed memories");
            }
            for (key, fields) in rows {
                let get = |name: &str| fields.get(name).map_or("", String::as_str);
                if json {
                    println!(
                        "{}",
                        json!({
                            "key": key,
                            "content": get("content"),
                            "enriched_from": get("enriched_from"),
                            "project": get("project"),
                            "state": get("state"),
                            "created_at": get("created_at"),
                        })
                    );
                } else {
                    let from = get("enriched_from");
                    println!(
                        "{key}  {}{}",
                        snippet(get("content")),
                        if from.is_empty() {
                            String::new()
                        } else {
                            format!("  (from {from})")
                        }
                    );
                }
            }
            Ok(())
        }
        TrustCommand::Confirm { keys, reason } => {
            let what = if keys.len() == 1 {
                "confirm this memory".to_owned()
            } else {
                format!("confirm these {} memories", keys.len())
            };
            crate::in_person::confirm(&what, &keys[0])?;
            // Go on past a refusal so one stale key doesn't strand the rest,
            // then fail if any were refused.
            let mut refused = 0;
            for key in &keys {
                if let Err(e) = act(key, TrustAction::Confirm, reason.as_deref()) {
                    eprintln!("{e}");
                    refused += 1;
                }
            }
            if refused > 0 {
                return Err(anyhow!("{refused} of {} not confirmed", keys.len()));
            }
            Ok(())
        }
        TrustCommand::Reject { key, reason } => {
            crate::in_person::confirm("reject this memory", &key)?;
            act(&key, TrustAction::Reject, reason.as_deref())
        }
        TrustCommand::Restore { key, reason } => {
            crate::in_person::confirm("restore this memory", &key)?;
            act(&key, TrustAction::Restore, reason.as_deref())
        }
        TrustCommand::ConfirmSource { source_key, reason } => {
            crate::in_person::confirm(
                "confirm everything extracted from this memory",
                &source_key,
            )?;
            let n = engine
                .confirm_from_source(&source_key, reason.as_deref())
                .map_err(|e| anyhow!("{e}"))?;
            println!(
                "confirmed {n} candidate{} from {source_key}",
                if n == 1 { "" } else { "s" }
            );
            Ok(())
        }
    }
}
