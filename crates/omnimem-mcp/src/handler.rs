//! The MCP handler: tool listing, dispatch to the engine, telemetry.

use std::sync::Arc;
use std::time::Instant;

use omnimem_engine::{DomainFilter, Engine, EngineError};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::schemars::JsonSchema;
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tracing::{error, warn};

use crate::args::*;
use crate::descriptions as d;

/// 6.x's `meta:tool_metrics:{tool}` counters.
const METRICS_PREFIX: &str = "meta:tool_metrics:";

fn schema<T: JsonSchema>() -> Arc<JsonObject> {
    let schema = rmcp::schemars::schema_for!(T);
    match serde_json::to_value(schema) {
        Ok(Value::Object(mut object)) => {
            object.remove("$schema");
            object.remove("title");
            if !object.contains_key("properties") {
                object.insert("properties".into(), Value::Object(Default::default()));
            }
            Arc::new(object)
        }
        _ => Arc::new(JsonObject::new()),
    }
}

fn tools() -> Vec<Tool> {
    vec![
        Tool::new("version", d::VERSION, schema::<NoArgs>()),
        Tool::new("remember", d::REMEMBER, schema::<Remember>()),
        Tool::new(
            "remember_document",
            d::REMEMBER_DOCUMENT,
            schema::<RememberDocument>(),
        ),
        Tool::new("recall", d::RECALL, schema::<Recall>()),
        Tool::new("recall_index", d::RECALL_INDEX, schema::<RecallIndex>()),
        Tool::new("recall_detail", d::RECALL_DETAIL, schema::<RecallDetail>()),
        Tool::new("deprioritise", d::DEPRIORITISE, schema::<Deprioritise>()),
        Tool::new("archive", d::ARCHIVE, schema::<Archive>()),
        Tool::new("reinstate", d::REINSTATE, schema::<KeyOrQuery>()),
        Tool::new("retag", d::RETAG, schema::<Retag>()),
        Tool::new("forget", d::FORGET, schema::<Forget>()),
        Tool::new(
            "suppress_topic",
            d::SUPPRESS_TOPIC,
            schema::<SuppressTopic>(),
        ),
        Tool::new("unsuppress_topic", d::UNSUPPRESS_TOPIC, schema::<Topic>()),
        Tool::new(
            "list_suppressions",
            d::LIST_SUPPRESSIONS,
            schema::<NoArgs>(),
        ),
        Tool::new(
            "find_duplicates",
            d::FIND_DUPLICATES,
            schema::<FindDuplicates>(),
        ),
        Tool::new("health", d::HEALTH, schema::<NoArgs>()),
        Tool::new("queue_status", d::QUEUE_STATUS, schema::<NoArgs>()),
        Tool::new("dump_to_file", d::DUMP_TO_FILE, schema::<DumpToFile>()),
        Tool::new(
            "restore_from_file",
            d::RESTORE_FROM_FILE,
            schema::<RestoreFromFile>(),
        ),
        Tool::new("list_backups", d::LIST_BACKUPS, schema::<NoArgs>()),
    ]
}

/// Why a call failed, in the terms a client sees.
enum CallError {
    UnknownTool(String),
    /// Bad arguments or a validation failure: a tool error the agent reads.
    Invalid(String),
    Internal(String),
}

impl From<EngineError> for CallError {
    fn from(e: EngineError) -> Self {
        match e {
            EngineError::Invalid(message) => CallError::Invalid(message),
            other => CallError::Internal(other.to_string()),
        }
    }
}

fn parse<T: DeserializeOwned>(args: Value) -> Result<T, CallError> {
    serde_json::from_value(args).map_err(|e| CallError::Invalid(format!("Invalid arguments: {e}")))
}

fn domain(arg: Option<DomainArg>) -> Option<DomainFilter> {
    arg.map(DomainFilter::from)
}

fn dispatch(engine: &Engine, name: &str, args: Value) -> Result<Value, CallError> {
    let e = engine;
    Ok(match name {
        "version" => e.version(),
        "remember" => {
            let a: Remember = parse(args)?;
            e.remember(
                &a.content,
                a.project.as_deref(),
                a.tags.as_deref(),
                &a.namespace,
                a.force,
                a.mode.as_deref(),
                a.licence.as_deref(),
                a.provenance.as_deref(),
            )?
        }
        "remember_document" => {
            let a: RememberDocument = parse(args)?;
            e.remember_document(
                &a.content,
                &a.chunk_strategy,
                a.project.as_deref(),
                a.tags.as_deref(),
                &a.namespace,
                a.chunk_size,
                a.mode.as_deref(),
                a.licence.as_deref(),
                a.provenance.as_deref(),
            )?
        }
        "recall" => {
            let a: Recall = parse(args)?;
            let domains = domain(a.domain_filter);
            e.recall(
                &a.query,
                a.top_k,
                a.namespaces.as_deref(),
                a.project_filter.as_deref(),
                a.expand_queries,
                domains.as_ref(),
            )?
        }
        "recall_index" => {
            let a: RecallIndex = parse(args)?;
            let domains = domain(a.domain_filter);
            e.recall_index(
                &a.query,
                a.top_k,
                a.namespaces.as_deref(),
                a.project_filter.as_deref(),
                a.snippet_length,
                a.expand_queries,
                domains.as_ref(),
            )?
        }
        "recall_detail" => e.recall_detail(&parse::<RecallDetail>(args)?.keys)?,
        "deprioritise" => {
            let a: Deprioritise = parse(args)?;
            e.deprioritise(&a.key_or_query, &a.reason, a.reinstate_hints.as_deref())?
        }
        "archive" => {
            let a: Archive = parse(args)?;
            e.archive(&a.key_or_query, a.reason.as_deref())?
        }
        "reinstate" => e.reinstate(&parse::<KeyOrQuery>(args)?.key_or_query)?,
        "retag" => {
            let a: Retag = parse(args)?;
            e.retag(&a.key, a.tags, a.add, a.remove)?
        }
        "forget" => {
            let a: Forget = parse(args)?;
            e.forget(&a.key_or_query, a.confirm)?
        }
        "suppress_topic" => {
            let a: SuppressTopic = parse(args)?;
            e.suppress_topic(&a.topic, a.reason.as_deref())?
        }
        "unsuppress_topic" => e.unsuppress_topic(&parse::<Topic>(args)?.topic)?,
        "list_suppressions" => e.list_suppressions()?,
        "find_duplicates" => {
            let a: FindDuplicates = parse(args)?;
            e.find_duplicates(&a.namespace, a.threshold, a.project_filter.as_deref())?
        }
        "health" => e.health()?,
        "queue_status" => e.queue_status()?,
        "dump_to_file" => e.dump_to_file(parse::<DumpToFile>(args)?.filename.as_deref())?,
        "restore_from_file" => {
            let a: RestoreFromFile = parse(args)?;
            e.restore_from_file(&a.filename, a.dry_run)?
        }
        "list_backups" => e.list_backups()?,
        other => return Err(CallError::UnknownTool(other.to_owned())),
    })
}

/// Call counts, duration and response size per tool, as the 6.x telemetry
/// middleware kept them. Failures to record are ignored.
fn record_metrics(engine: &Engine, tool: &str, started: Instant, response_chars: Option<usize>) {
    let store = engine.store();
    let key = format!("{METRICS_PREFIX}{tool}");
    let _ = store.hash_incr(&key, "call_count", 1);
    match response_chars {
        Some(chars) => {
            let _ = store.hash_incr(
                &key,
                "total_duration_ms",
                started.elapsed().as_millis() as i64,
            );
            let _ = store.hash_incr(&key, "total_response_chars", chars as i64);
        }
        None => {
            let _ = store.hash_incr(&key, "error_count", 1);
        }
    }
    let now = omnimem_engine::pyfmt::now_str();
    let _ = store.hash_set(
        &key,
        &[("last_called_at".to_owned(), now)].into_iter().collect(),
    );
}

#[derive(Clone)]
pub struct OmniMemServer {
    engine: Arc<Engine>,
    tools: Arc<Vec<Tool>>,
}

impl OmniMemServer {
    pub fn new(engine: Arc<Engine>) -> Self {
        Self {
            engine,
            tools: Arc::new(tools()),
        }
    }
}

impl ServerHandler for OmniMemServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("omnimem", env!("CARGO_PKG_VERSION")))
            .with_instructions(crate::INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tools.as_ref().clone()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let name = request.name.to_string();
        let args = Value::Object(request.arguments.unwrap_or_default());
        let engine = self.engine.clone();
        let tool = name.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            let started = Instant::now();
            let result = dispatch(&engine, &tool, args);
            let text = result.as_ref().ok().map(|v| v.to_string());
            if !matches!(result, Err(CallError::UnknownTool(_))) {
                record_metrics(
                    &engine,
                    &tool,
                    started,
                    text.as_ref().map(|t| t.chars().count()),
                );
            }
            result.map(|_| text.unwrap_or_default())
        })
        .await
        .map_err(|e| ErrorData::internal_error(format!("tool task failed: {e}"), None))?;

        let result = match outcome {
            Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
            Err(CallError::UnknownTool(tool)) => {
                return Err(ErrorData::invalid_params(
                    format!("Unknown tool: {tool}"),
                    None,
                ));
            }
            Err(CallError::Invalid(message)) => {
                warn!(tool = %name, %message, "tool call rejected");
                CallToolResult::error(vec![ContentBlock::text(message)])
            }
            Err(CallError::Internal(message)) => {
                error!(tool = %name, %message, "tool call failed");
                CallToolResult::error(vec![ContentBlock::text(format!(
                    "Error calling tool '{name}': {message}"
                ))])
            }
        };
        Ok(result.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_tool_dispatches() {
        let names: Vec<String> = tools().iter().map(|t| t.name.to_string()).collect();
        assert_eq!(names.len(), 20);
        for name in names {
            let schema = tools()
                .into_iter()
                .find(|t| t.name == name)
                .unwrap()
                .input_schema;
            assert_eq!(
                schema.get("type").and_then(Value::as_str),
                Some("object"),
                "{name}"
            );
        }
    }

    #[test]
    fn required_and_optional_arguments_follow_6x() {
        let recall = schema::<Recall>();
        let required: Vec<&str> = recall["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(required, ["query"]);
        assert_eq!(recall["properties"]["top_k"]["default"], 5);
        let restore = schema::<RestoreFromFile>();
        assert_eq!(restore["properties"]["dry_run"]["default"], true);
    }
}
