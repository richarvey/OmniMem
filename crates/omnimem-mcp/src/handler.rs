//! The MCP handler: tool listing, dispatch to the engine, telemetry.

use std::sync::Arc;
use std::time::Instant;

use omnimem_engine::{
    AsOf, DomainFilter, Engine, EngineError, HumanApproval, ProposalCheck, ResolutionReview,
    ResolveRequest, Retire, SkillReview,
};
use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock,
    Implementation, JsonObject, ListToolsResult, PaginatedRequestParams, ProtocolVersion,
    ServerCapabilities, ServerConfig, Tool,
};
use rmcp::schemars::JsonSchema;
use rmcp::service::{Peer, RequestContext};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tracing::{error, info, warn};

use crate::approval::{self, APPROVAL_TIMEOUT, Answer};
use crate::args::{
    Archive, Bless, Briefing, BulkProject, CheckContradictions, CompileProject, CompileSkill,
    Deprioritise, DeprioritiseProject, DomainArg, DumpToFile, EndValidity, FindDuplicates,
    FindSkills, Forget, GetSkill, Key, KeyOrQuery, ListProjects, LogAbandoned, MemoryAudit, NoArgs,
    OptionalProject, ProjectName, PromoteKnowledge, Query, Recall, RecallDetail, RecallIndex,
    RecentKnowledge, RecordExperience, Reindex, Remember, RememberDocument, RestoreFromFile, Retag,
    RetireArg, SetLicence, SetProjectContext, SetProvenance, SuppressTopic, Topic,
    UpdateProjectState,
};
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
                object.insert("properties".into(), Value::Object(JsonObject::new()));
            }
            Arc::new(object)
        }
        _ => Arc::new(JsonObject::new()),
    }
}

pub(crate) fn tools() -> Vec<Tool> {
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
        Tool::new(
            "record_experience",
            d::RECORD_EXPERIENCE,
            schema::<RecordExperience>(),
        ),
        Tool::new("log_abandoned", d::LOG_ABANDONED, schema::<LogAbandoned>()),
        Tool::new("get_experience", d::GET_EXPERIENCE, schema::<Key>()),
        Tool::new(
            "experience_summary",
            d::EXPERIENCE_SUMMARY,
            schema::<OptionalProject>(),
        ),
        Tool::new("warn_if_abandoned", d::WARN_IF_ABANDONED, schema::<Query>()),
        Tool::new(
            "set_project_context",
            d::SET_PROJECT_CONTEXT,
            schema::<SetProjectContext>(),
        ),
        Tool::new(
            "get_project_context",
            d::GET_PROJECT_CONTEXT,
            schema::<ProjectName>(),
        ),
        Tool::new("list_projects", d::LIST_PROJECTS, schema::<ListProjects>()),
        Tool::new(
            "compile_project_domains",
            d::COMPILE_PROJECT_DOMAINS,
            schema::<CompileProject>(),
        ),
        Tool::new(
            "update_project_state",
            d::UPDATE_PROJECT_STATE,
            schema::<UpdateProjectState>(),
        ),
        Tool::new("delete_project", d::DELETE_PROJECT, schema::<BulkProject>()),
        Tool::new(
            "deprioritise_project",
            d::DEPRIORITISE_PROJECT,
            schema::<DeprioritiseProject>(),
        ),
        Tool::new(
            "reinstate_project",
            d::REINSTATE_PROJECT,
            schema::<BulkProject>(),
        ),
        Tool::new(
            "compile_project_context",
            d::COMPILE_PROJECT_CONTEXT,
            schema::<CompileProject>(),
        ),
        Tool::new("memory_audit", d::MEMORY_AUDIT, schema::<MemoryAudit>()),
        Tool::new(
            "why_did_you_mention",
            d::WHY_DID_YOU_MENTION,
            schema::<Query>(),
        ),
        Tool::new("explain_memory", d::EXPLAIN_MEMORY, schema::<Key>()),
        Tool::new("end_validity", d::END_VALIDITY, schema::<EndValidity>()),
        Tool::new("reindex", d::REINDEX, schema::<Reindex>()),
        Tool::new("set_licence", d::SET_LICENCE, schema::<SetLicence>()),
        Tool::new(
            "set_provenance",
            d::SET_PROVENANCE,
            schema::<SetProvenance>(),
        ),
        Tool::new(
            "check_contradictions",
            d::CHECK_CONTRADICTIONS,
            schema::<CheckContradictions>(),
        ),
        Tool::new("briefing", d::BRIEFING, schema::<Briefing>()),
        Tool::new(
            "recent_knowledge",
            d::RECENT_KNOWLEDGE,
            schema::<RecentKnowledge>(),
        ),
        Tool::new("compile_skill", d::COMPILE_SKILL, schema::<CompileSkill>()),
        Tool::new("find_skills", d::FIND_SKILLS, schema::<FindSkills>()),
        Tool::new("get_skill", d::GET_SKILL, schema::<GetSkill>()),
        Tool::new("bless", d::BLESS, schema::<Bless>()),
        Tool::new(
            "promote_knowledge",
            d::PROMOTE_KNOWLEDGE,
            schema::<PromoteKnowledge>(),
        ),
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
            EngineError::Invalid(message) => Self::Invalid(message),
            other => Self::Internal(other.to_string()),
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
            e.remember_valid(
                &a.content,
                a.project.as_deref(),
                a.tags.as_deref(),
                &a.namespace,
                a.force,
                a.mode.as_deref(),
                a.licence.as_deref(),
                a.provenance.as_deref(),
                &omnimem_engine::Validity {
                    valid_from: a.valid_from,
                    valid_to: a.valid_to,
                },
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
            let as_of = AsOf::parse(a.valid_at.as_deref(), a.known_at.as_deref())?;
            e.recall_at(
                &a.query,
                a.top_k,
                a.namespaces.as_deref(),
                a.project_filter.as_deref(),
                a.expand_queries,
                domains.as_ref(),
                as_of,
            )?
        }
        "recall_index" => {
            let a: RecallIndex = parse(args)?;
            let domains = domain(a.domain_filter);
            let as_of = AsOf::parse(a.valid_at.as_deref(), a.known_at.as_deref())?;
            e.recall_index_at(
                &a.query,
                a.top_k,
                a.namespaces.as_deref(),
                a.project_filter.as_deref(),
                a.snippet_length,
                a.expand_queries,
                domains.as_ref(),
                as_of,
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
        "record_experience" => {
            let a: RecordExperience = parse(args)?;
            e.record_experience(
                &a.key,
                a.effort_score,
                &a.outcome,
                a.iterations,
                a.abandoned_approaches,
                a.breakthrough.as_deref(),
                a.gotchas.as_deref(),
                a.lesson.as_deref(),
            )?
        }
        "log_abandoned" => {
            let a: LogAbandoned = parse(args)?;
            e.log_abandoned(&a.key, &a.name, &a.kind, &a.reason)?
        }
        "get_experience" => e.get_experience(&parse::<Key>(args)?.key)?,
        "experience_summary" => {
            e.experience_summary(parse::<OptionalProject>(args)?.project.as_deref())?
        }
        "warn_if_abandoned" => e.warn_if_abandoned(&parse::<Query>(args)?.query)?,
        "set_project_context" => {
            let a: SetProjectContext = parse(args)?;
            let domains = domain(a.domains);
            e.set_project_context(
                &a.project_name,
                &a.description,
                &a.stack,
                &a.goals,
                &a.current_state,
                a.notes.as_deref(),
                domains.as_ref(),
            )?
        }
        "get_project_context" => {
            e.get_project_context(&parse::<ProjectName>(args)?.project_name)?
        }
        "list_projects" => e.list_projects(parse::<ListProjects>(args)?.domain.as_deref())?,
        "compile_project_domains" => {
            let a: CompileProject = parse(args)?;
            e.compile_project_domains(&a.project_name, a.auto_save)?
        }
        "update_project_state" => {
            let a: UpdateProjectState = parse(args)?;
            e.update_project_state(&a.project_name, &a.current_state, a.notes.as_deref())?
        }
        "delete_project" => {
            let a: BulkProject = parse(args)?;
            e.delete_project(&a.project_name, a.confirm, a.include_context)?
        }
        "deprioritise_project" => {
            let a: DeprioritiseProject = parse(args)?;
            e.deprioritise_project(
                &a.project_name,
                a.confirm,
                a.reason.as_deref(),
                a.include_context,
            )?
        }
        "reinstate_project" => {
            let a: BulkProject = parse(args)?;
            e.reinstate_project(&a.project_name, a.confirm, a.include_context)?
        }
        "compile_project_context" => {
            let a: CompileProject = parse(args)?;
            e.compile_project_context(&a.project_name, a.auto_save)?
        }
        "memory_audit" => {
            let a: MemoryAudit = parse(args)?;
            e.memory_audit(
                a.project.as_deref(),
                a.namespace.as_deref(),
                a.include_archived,
                a.limit,
                a.offset,
            )?
        }
        "why_did_you_mention" => e.why_did_you_mention(&parse::<Query>(args)?.query)?,
        "end_validity" => {
            let a: EndValidity = parse(args)?;
            e.end_validity(&a.key, a.valid_to.as_deref(), a.reason.as_deref())?
        }
        "explain_memory" => e.explain_memory(&parse::<Key>(args)?.key)?,
        "reindex" => e.reindex(parse::<Reindex>(args)?.namespace.as_deref())?,
        "set_licence" => {
            let a: SetLicence = parse(args)?;
            e.set_licence(
                &a.licence,
                a.keys.as_deref(),
                a.feed_name.as_deref(),
                a.note.as_deref(),
            )?
        }
        "set_provenance" => {
            let a: SetProvenance = parse(args)?;
            e.set_provenance(&a.provenance, &a.keys)?
        }
        "check_contradictions" => {
            let a: CheckContradictions = parse(args)?;
            match a.mode.as_deref().map(str::trim) {
                None | Some("" | "check") => {}
                // Only reached when no person is asked: `HUMAN_APPROVAL=agent`.
                Some("resolve") => {
                    return Ok(e.resolve_contradiction(&resolve_request(&a)?, "mcp")?);
                }
                Some(other) => {
                    return Err(CallError::Invalid(format!(
                        "Unknown mode '{other}'. Use 'check' (the default) or 'resolve'."
                    )));
                }
            }
            e.check_contradictions(
                a.query.as_deref(),
                &a.namespace,
                a.project_filter.as_deref(),
                a.use_api,
            )?
        }
        "briefing" => {
            let a: Briefing = parse(args)?;
            e.briefing(a.project.as_deref(), a.include_knowledge)?
        }
        "recent_knowledge" => {
            let a: RecentKnowledge = parse(args)?;
            e.recent_knowledge(
                a.days,
                a.feed_name.as_deref(),
                a.topics.as_deref(),
                a.limit,
                a.licence.as_deref(),
            )?
        }
        "compile_skill" => {
            let a: CompileSkill = parse(args)?;
            e.compile_skill(
                &a.domain,
                &a.mode,
                a.min_reinforcement,
                a.include_graveyard,
                a.export_path.as_deref(),
                a.description.as_deref(),
            )?
        }
        "find_skills" => e.find_skills(&parse::<FindSkills>(args)?.query_or_domain)?,
        "get_skill" => e.get_skill(&parse::<GetSkill>(args)?.skill_id)?,
        "bless" => e.bless(&parse::<Bless>(args)?.memory_key)?,
        "promote_knowledge" => {
            let a: PromoteKnowledge = parse(args)?;
            let rules = a
                .rules
                .map(|r| Value::Array(r.into_iter().map(Value::Object).collect()));
            e.promote_knowledge(&a.key, a.domain.as_deref(), a.demote, rules.as_ref())?
        }
        other => return Err(CallError::UnknownTool(other.to_owned())),
    })
}

/// How much of a proposed diff goes into the question. A person reads it in
/// a dialog, so the rest is summarised and left to the settings panel.
const DIFF_PREVIEW_CHARS: usize = 3_000;

/// What a person is asked when an agent wants to write a skill: the domain,
/// counts, and the start of the diff (or the draft, for a new skill).
fn skill_approval_message(review: &SkillReview) -> String {
    let mut message = format!(
        "An agent wants to {} the \"{}\" skill.\n{} line{} added, {} removed.",
        if review.new_skill { "create" } else { "change" },
        review.domain,
        review.added,
        if review.added == 1 { "" } else { "s" },
        review.removed,
    );
    if let Some(rules) = review.rules.as_object().filter(|r| !r.is_empty()) {
        let counts: Vec<String> = rules.iter().map(|(k, n)| format!("{n} {k}")).collect();
        message.push_str(&format!(" Rules: {}.", counts.join(", ")));
    }
    if !review.description.is_empty() {
        message.push_str(&format!("\nDescription: {}", review.description));
    }
    message.push_str(if review.new_skill {
        "\n\nThe draft:\n"
    } else {
        "\n\nThe diff:\n"
    });
    let total = review.shown.chars().count();
    message.push_str(
        &review
            .shown
            .chars()
            .take(DIFF_PREVIEW_CHARS)
            .collect::<String>(),
    );
    if total > DIFF_PREVIEW_CHARS {
        message.push_str(&format!(
            "\n... and {} more characters (see the Skills page in OmniMem's settings panel).",
            total - DIFF_PREVIEW_CHARS
        ));
    }
    message.push_str("\n\nAccept to write it. Decline to leave things as they are.");
    message
}

/// Run engine work on a blocking thread and flatten the join error.
async fn blocking<T: Send + 'static>(
    engine: &Arc<Engine>,
    work: impl FnOnce(&Engine) -> Result<T, EngineError> + Send + 'static,
) -> Result<T, CallError> {
    let engine = engine.clone();
    tokio::task::spawn_blocking(move || work(&engine))
        .await
        .map_err(|e| CallError::Internal(format!("tool task failed: {e}")))?
        .map_err(CallError::from)
}

/// The resolution a `check_contradictions(mode="resolve")` call proposes.
fn resolve_request(a: &CheckContradictions) -> Result<ResolveRequest, CallError> {
    fn need<'a>(name: &str, value: &'a Option<String>) -> Result<&'a str, CallError> {
        value
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| CallError::Invalid(format!("mode='resolve' needs {name}.")))
    }
    let retire = match &a.retire_loser {
        None => Retire::No,
        Some(RetireArg::Flag(true)) => Retire::Deprioritise,
        Some(RetireArg::Flag(false)) => Retire::No,
        Some(RetireArg::Mode(mode)) => Retire::parse(mode)?,
    };
    Ok(ResolveRequest::new(
        need("key_a", &a.key_a)?,
        need("key_b", &a.key_b)?,
        need("outcome", &a.outcome)?,
        a.winner.as_deref(),
        a.reason.as_deref(),
        retire,
    )?)
}

/// What the person is shown: both memories, the outcome, the winner and the
/// reason, as far as a client's dialog can take.
fn contradiction_approval_message(review: &ResolutionReview) -> String {
    const SHOWN_CHARS: usize = 400;
    let r = &review.request;
    let shown = |text: &str| {
        let mut s: String = text.chars().take(SHOWN_CHARS).collect();
        if text.chars().count() > SHOWN_CHARS {
            s.push_str("...");
        }
        s
    };
    let mut message = format!(
        "An agent wants to settle a contradiction between two memories.\n\nA ({}):\n{}\n\nB ({}):\n{}\n\nOutcome: {}",
        r.key_a,
        shown(&review.content_a),
        r.key_b,
        shown(&review.content_b),
        r.outcome.as_str(),
    );
    if let Some(winner) = &r.winner {
        let side = if *winner == r.key_a { "A" } else { "B" };
        message.push_str(&format!("\nCurrent: {side} ({winner})"));
    }
    message.push_str(&format!(
        "\nReason: {}",
        r.reason.as_deref().unwrap_or("(none given)")
    ));
    match r.retire {
        Retire::No => {}
        Retire::Deprioritise => message.push_str("\nThe out-of-date memory will be deprioritised."),
        Retire::Archive => message.push_str("\nThe out-of-date memory will be archived."),
    }
    message.push_str("\n\nAccept to record it. Decline to leave things as they are.");
    message
}

/// `check_contradictions(mode="resolve")` under `HUMAN_APPROVAL=human`: the
/// agent proposes, a person's answer is what writes. Without a person to
/// ask, the proposal waits for the settings panel (or `omnimem
/// contradictions`).
async fn resolve_with_approval(
    engine: &Arc<Engine>,
    peer: &Peer<RoleServer>,
    client: &str,
    args: Value,
) -> Result<Value, CallError> {
    let a: CheckContradictions = parse(args)?;
    let request = resolve_request(&a)?;
    let proposed = request.clone();
    let review = blocking(engine, move |e| e.review_resolution(&proposed)).await?;
    let answer = approval::ask_human(
        peer,
        &contradiction_approval_message(&review),
        APPROVAL_TIMEOUT,
    )
    .await;
    let client = client.to_owned();
    match answer {
        Answer::Accepted => {
            blocking(engine, move |e| {
                // The write is the client's, and the log says a person let it through.
                let _audit = omnimem_store::enter(omnimem_store::Actor::Mcp, &client);
                let _note = omnimem_store::op(
                    omnimem_store::Op::new("approved")
                        .reason(Some("approved by the user via elicitation")),
                );
                e.resolve_contradiction(&review.request, "mcp-elicitation")
            })
            .await
        }
        Answer::Unsupported => {
            let mut stored = blocking(engine, move |e| {
                let _audit = omnimem_store::enter(omnimem_store::Actor::Mcp, &client);
                e.propose_resolution(&request, "mcp")
            })
            .await?;
            stored["note"] = json!(
                "A person has to accept this, and this client can't ask them: it didn't declare MCP elicitation. \
                 Nothing has been written. They can accept it in OmniMem's settings panel (/admin), on the Contradictions \
                 page under Pending, or with `omnimem contradictions` on the server. Don't call resolve again."
            );
            Ok(stored)
        }
        declined_or_not => Ok(json!({
            "status": declined_or_not.status(),
            "key_a": review.request.key_a,
            "key_b": review.request.key_b,
            "note": if declined_or_not == Answer::Declined {
                "The person declined, so nothing was written. Don't ask again unless they do."
            } else {
                "The person didn't answer, so nothing was written."
            },
        })),
    }
}

/// `compile_skill(mode="write")` under `HUMAN_APPROVAL=human`: the agent's
/// call is a request, and a person's answer is the commit. Without a person
/// to ask, the proposal waits for the settings panel.
async fn write_with_approval(
    engine: &Arc<Engine>,
    peer: &Peer<RoleServer>,
    client: &str,
    args: Value,
) -> Result<Value, CallError> {
    let a: CompileSkill = parse(args)?;
    let domain = a.domain.clone();
    let review = match blocking(engine, move |e| e.review_skill_proposal(&domain)).await? {
        ProposalCheck::Ready(review) => review,
        // No proposal, a stale one, or a skill the compiler doesn't own: the
        // result is the one a plain write gives, and no one needs asking.
        ProposalCheck::Blocked(result) => return Ok(result),
    };
    let ttl = engine.config().skill_proposal_ttl;
    let answer = if approval::can_ask(peer) {
        // The stash expires on its own clock; asking past it only wastes the answer.
        let timeout = APPROVAL_TIMEOUT.min(ttl);
        approval::ask_human(peer, &skill_approval_message(&review), timeout).await
    } else {
        Answer::Unsupported
    };
    let note = match answer {
        Answer::Accepted => {
            let (domain, export, sha) = (a.domain, a.export_path, review.body_sha);
            let client = client.to_owned();
            return blocking(engine, move |e| {
                // The write is the client's, and the log says a person let it through.
                let _audit = omnimem_store::enter(omnimem_store::Actor::Mcp, &client);
                let _note = omnimem_store::op(
                    omnimem_store::Op::new("approved")
                        .reason(Some("approved by the user via elicitation")),
                );
                e.commit_approved_skill(&domain, export.as_deref(), &sha)
            })
            .await;
        }
        Answer::Unsupported => format!(
            "A person has to accept this proposal, and this client can't ask them: it didn't declare MCP elicitation. \
             Accept it in OmniMem's settings panel (/admin), on the Skills page under Pending proposals. Don't call \
             write again; the proposal waits {}s for them.",
            ttl.as_secs()
        ),
        Answer::Declined => "The person declined, so nothing was written. The proposal is still stashed until it expires, \
             and they can accept it later from OmniMem's settings panel. Don't ask again unless they do."
            .to_owned(),
        _ => "The person didn't answer, so nothing was written. The proposal is still stashed until it expires, \
             and they can accept it from OmniMem's settings panel."
            .to_owned(),
    };
    Ok(json!({
        "status": answer.status(),
        "skill_id": review.skill_id,
        "domain": review.domain,
        "note": note,
    }))
}

/// Count and time a finished call, and log it. Returns the reply text.
/// Failures to record are ignored.
fn observe(
    engine: &Engine,
    tool: &str,
    started: Instant,
    result: Result<Value, CallError>,
) -> Result<String, CallError> {
    let text = result.as_ref().ok().map(std::string::ToString::to_string);
    if !matches!(result, Err(CallError::UnknownTool(_))) {
        record_metrics(
            engine,
            tool,
            started,
            text.as_ref().map(|t| t.chars().count()),
        );
    }
    // Successful calls were invisible: the server logged writes and
    // failures, so "recall returned nothing" and "recall was never
    // called" looked identical from the outside. Deliberately no
    // arguments and no result content, because a query or a memory
    // can hold anything the user typed.
    if let Some(text) = text.as_ref() {
        info!(
            tool = %tool,
            duration_ms = started.elapsed().as_millis() as u64,
            chars = text.chars().count(),
            "tool call served"
        );
    }
    result.map(|_| text.unwrap_or_default())
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
        &omnimem_store::Fields::from([("last_called_at".to_owned(), now)]),
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
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("omnimem", env!("CARGO_PKG_VERSION")))
            .with_instructions(crate::INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let result = ListToolsResult::with_all_items(self.tools.as_ref().clone());
        // Protocol 2026-07-28 requires ttlMs and cacheScope on paginated results
        // (SEP-2549). rmcp models them as Option with skip_serializing_if, so
        // with_all_items alone omits them, and a strict client rejects the entire
        // tools/list response: Claude Code shows the server as connected with zero
        // tools. Gated on the negotiated version, with 0 / Public, exactly as
        // rmcp's own #[tool_handler] macro emits them.
        let supports_cache_hints = context
            .protocol_version()
            .is_some_and(|version| version >= ProtocolVersion::V_2026_07_28);
        Ok(if supports_cache_hints {
            result.with_ttl_ms(0).with_cache_scope(CacheScope::Public)
        } else {
            result
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let client = crate::audit_client::describe(&context);
        let name = request.name.to_string();
        let args = Value::Object(request.arguments.unwrap_or_default());
        let engine = self.engine.clone();
        let tool = name.clone();
        // A skill write is the one call the agent can't make on its own say-so
        // (unless `HUMAN_APPROVAL=agent`): it goes through a person first.
        // Settling a contradiction is the second: an agent's `resolve` is a
        // proposal until a person says yes.
        let mode = args.get("mode").and_then(Value::as_str);
        let skill_write = name == "compile_skill" && mode == Some("write");
        let resolves = name == "check_contradictions" && mode.map(str::trim) == Some("resolve");
        // Each gate has its own setting (#38), defaulting to HUMAN_APPROVAL.
        let needs_person = (skill_write
            && engine.config().skill_approval() == HumanApproval::Human)
            || (resolves && engine.config().contradiction_approval() == HumanApproval::Human);
        let outcome = if needs_person {
            let started = Instant::now();
            let result = if resolves {
                resolve_with_approval(&engine, &context.peer, &client, args).await
            } else {
                write_with_approval(&engine, &context.peer, &client, args).await
            };
            tokio::task::spawn_blocking(move || observe(&engine, &tool, started, result))
                .await
                .map_err(|e| ErrorData::internal_error(format!("tool task failed: {e}"), None))?
        } else {
            tokio::task::spawn_blocking(move || {
                // Changes this call makes are the client's, in the audit log.
                let _audit = omnimem_store::enter(omnimem_store::Actor::Mcp, &client);
                let started = Instant::now();
                let result = dispatch(&engine, &tool, args);
                observe(&engine, &tool, started, result)
            })
            .await
            .map_err(|e| ErrorData::internal_error(format!("tool task failed: {e}"), None))?
        };

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
        assert_eq!(names.len(), 49);
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
