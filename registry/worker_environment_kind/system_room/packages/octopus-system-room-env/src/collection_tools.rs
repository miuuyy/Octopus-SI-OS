use std::path::Path;

use octopus_context_collections::{BoundCollection, CollectionSelection, ListRequest, ScopeContext, parse_entry};
use octopus_interfaces::{ArtifactBindingPayload, InterfaceInvocationEnvelope};
use octopus_ledger::{ArtifactRef, ContentHash};
use octopus_tool_handlers::{ToolCall, ToolHandlerError, ToolHandlerResult};
use serde::Deserialize;
use serde_json::{json, Value};

#[cfg(test)]
#[path = "../tests/support/collection_tools.rs"]
mod tests;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiscoverArguments {
    source: Option<ArtifactRef>,
    query: Option<String>,
    limit: Option<usize>,
    cursor: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArguments {
    source: ArtifactRef,
    entry_id: String,
    revision: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrepareArguments {
    source: ArtifactRef,
    entry: Value,
    expected_revision: Option<String>,
}

pub(crate) fn readable_source<'a>(input: &'a InterfaceInvocationEnvelope, id: &str) -> Result<&'a ArtifactBindingPayload, String> {
    let metadata = input.metadata().map_err(|error| error.to_string())?;
    if !metadata.grant.may_read_artifact(id) {
        return Err("collection source is not readable under the invocation grant".into());
    }
    let mut matches = input.artifact_binding_payloads.iter().filter(|payload| payload.artifact.id.as_str() == id);
    let source = matches.next().ok_or("collection source is not bound to this invocation")?;
    if matches.next().is_some() || metadata.artifact_binding_refs.iter().filter(|reference| reference.id.as_str() == id).count() != 1
        || !metadata.artifact_binding_refs.contains(&source.artifact) {
        return Err("collection source has no unique exact admitted reference".into());
    }
    Ok(source)
}

fn exact_source<'a>(input: &'a InterfaceInvocationEnvelope, requested: &ArtifactRef) -> Result<&'a ArtifactBindingPayload, String> {
    let source = readable_source(input, requested.id.as_str())?;
    if source.artifact.content != requested.content {
        return Err(format!(
            "collection source '{}' is bound to this invocation with different content than requested; take its current source object from context.discover without arguments",
            requested.id));
    }
    Ok(source)
}

pub(crate) fn open(root: &Path, source: &ArtifactBindingPayload) -> Result<BoundCollection, String> {
    let bytes = source.read_bytes().map_err(|error| error.to_string())?;
    let value = serde_json::from_slice(&bytes).map_err(|error| format!("collection source JSON: {error}"))?;
    BoundCollection::open(root, &source.artifact, &value).map_err(|error| error.to_string())
}

fn describe(root: &Path, source: &ArtifactBindingPayload) -> Result<Value, String> {
    let collection = open(root, source)?;
    Ok(json!({"source":collection.source(),"description":collection.descriptor().description,
        "limits":collection.descriptor().limits,"directory":collection.descriptor().directory}))
}

fn scope(ee: &str, worker: &str) -> ScopeContext {
    ScopeContext { ee_id:ee.into(), worker_interface:worker.into() }
}

fn discover(root: &Path, source: &ArtifactBindingPayload, ee: &str, worker: &str,
    query: Option<String>, limit: Option<usize>, cursor: Option<Value>) -> Result<Value, String> {
    let collection = open(root, source)?;
    let limit = limit.unwrap_or(32.min(collection.descriptor().limits.max_entries as usize));
    if limit == 0 || limit > 200 { return Err("collection page limit must be between 1 and 200".into()); }
    let request = ListRequest { query, limit:limit as u32,
        cursor:cursor.map(serde_json::from_value).transpose().map_err(|error| error.to_string())? };
    let page = collection.list(&scope(ee, worker), &request).map_err(|error| error.to_string())?;
    serde_json::to_value(page).map_err(|error| error.to_string())
}

fn read(root: &Path, source: &ArtifactBindingPayload, ee: &str, worker: &str, entry_id: &str, revision: &str) -> Result<Value, String> {
    let pin = CollectionSelection { source:source.artifact.clone(), entry_id:entry_id.into(),
        revision:ContentHash::from_hex(revision).ok_or("publication revision must be an exact content hash")? };
    let result = open(root, source)?.read(&pin, &scope(ee, worker)).map_err(|error| error.to_string())?;
    serde_json::to_value(result).map_err(|error| error.to_string())
}

fn prepare(root: &Path, source: &ArtifactBindingPayload, ee: &str, worker: &str, entry: Value, revision: Option<String>) -> Result<Value, String> {
    let entry = parse_entry(&entry).map_err(|error| error.to_string())?;
    let revision = revision.map(|value| ContentHash::from_hex(&value).ok_or("publication expected_revision must be an exact content hash")).transpose()?;
    let result = open(root, source)?.prepare_publication(&entry, revision, &scope(ee, worker)).map_err(|error| error.to_string())?;
    Ok(json!({"status":"prepared","publication":result.pin,"file_write":result.file_write,
        "commit":"Execute file_write with the existing granted file.write tool. Only its successful receipt proves publication."}))
}

pub(crate) fn execute(input: &InterfaceInvocationEnvelope, call: &ToolCall, root: Option<&Path>, maximum_bytes: u64) -> Result<ToolHandlerResult, ToolHandlerError> {
    execute_inner(input, call, root, maximum_bytes).map(ToolHandlerResult::new).map_err(|reason| ToolHandlerError::Execution {
        tool_id: call.tool_id.clone(), reason,
    })
}

fn execute_inner(input: &InterfaceInvocationEnvelope, call: &ToolCall, root: Option<&Path>, maximum_bytes: u64) -> Result<Value, String> {
    let root = root.ok_or("collection operation has no granted registry_artifact_root")?;
    let runtime = input.runtime().map_err(|error| error.to_string())?;
    let operation = call.handler_config.get("operation").and_then(Value::as_str).ok_or("collection operation is absent")?;
    match operation {
        "collection_discover" => {
            let args: DiscoverArguments = serde_json::from_value(call.arguments.clone()).map_err(|error| error.to_string())?;
            if let Some(requested) = args.source {
                let source = exact_source(input, &requested)?;
                discover(root, source, runtime.ee.as_str(), runtime.worker.as_str(), args.query, args.limit, args.cursor)
            } else {
                if args.query.is_some() || args.limit.is_some() || args.cursor.is_some() {
                    return Err("query, limit and cursor require an exact collection source".into());
                }
                #[derive(Deserialize)]
                struct Header { label: Option<String> }
                let mut retained = b"{\"sources\":[],\"has_more\":false}".len() as u64;
                if retained > maximum_bytes { return Err("collection discovery exceeds its retained output ceiling".into()); }
                let mut sources = Vec::new();
                for payload in &input.artifact_binding_payloads {
                    if !input.grant().map_err(|error| error.to_string())?.may_read_artifact(payload.artifact.id.as_str()) { continue; }
                    let is_collection = {
                        let bytes = payload.read_bounded(maximum_bytes - retained).map_err(|error| error.to_string())?;
                        serde_json::from_slice::<Header>(&bytes).is_ok_and(|header| header.label.as_deref() == Some("context.collection"))
                    };
                    if !is_collection { continue; }
                    let source = readable_source(input, payload.artifact.id.as_str())?;
                    let description = describe(root, source)?;
                    let size = serde_json::to_vec(&description).map_err(|error| error.to_string())?.len() as u64;
                    retained = retained.checked_add(size).and_then(|size| size.checked_add(u64::from(!sources.is_empty())))
                        .filter(|size| *size <= maximum_bytes).ok_or("collection discovery exceeds its retained output ceiling")?;
                    sources.push(description);
                }
                Ok(json!({"sources":sources,"has_more":false}))
            }
        }
        "collection_read" => {
            let args: ReadArguments = serde_json::from_value(call.arguments.clone()).map_err(|error| error.to_string())?;
            let source = exact_source(input, &args.source)?;
            read(root, source, runtime.ee.as_str(), runtime.worker.as_str(), &args.entry_id, &args.revision)
        }
        "collection_prepare" => {
            if !call.arguments.as_object().is_some_and(|value| value.contains_key("expected_revision")) {
                return Err("publication preparation requires expected_revision; null means a new entry".into());
            }
            let args: PrepareArguments = serde_json::from_value(call.arguments.clone()).map_err(|error| error.to_string())?;
            let source = exact_source(input, &args.source)?;
            prepare(root, source, runtime.ee.as_str(), runtime.worker.as_str(), args.entry, args.expected_revision)
        }
        _ => Err("unsupported collection operation".into()),
    }
}
