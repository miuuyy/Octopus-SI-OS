use super::*;

pub struct ProvenWrite {
    pub(super) writer: CheckedRunWriter,
    pub(super) call: Movement,
    pub(super) result: Movement,
    pub(super) write: serde_json::Value,
    pub(super) before_hash: String,
    pub(super) after_hash: String,
    pub(super) by: RunId,
}

struct AdmittedCall {
    movement: Movement,
    label: String,
    call: octopus_tool_handlers::ToolCallArtifact,
    by: RunId,
}

fn admitted_under_grant(movement: &Movement, tool: &str, door: &InterfaceId) -> Option<RunId> {
    let metadata = movement.invocation_metadata.as_ref().and_then(|metadata| metadata.invocation())?;
    let runtime = metadata.runtime.as_ref().filter(|runtime| &runtime.interface == door)?;
    (metadata.verification_failure.is_none()
        && metadata.grant.tools.iter().any(|granted| granted == tool)
        && !metadata.grant.hard_blocked_tools.iter().any(|blocked| blocked == tool)
        && metadata.grant.interfaces.iter().any(|granted| granted == door.as_str()))
    .then(|| runtime.run.clone())
}

fn returned_to(movement: &Movement, owner: &InterfaceId) -> bool {
    movement.invocation_metadata.as_ref().and_then(|metadata| metadata.invocation()).is_some_and(|metadata| {
        metadata.verification_failure.is_none()
            && metadata.runtime.as_ref().is_some_and(|runtime| &runtime.interface == owner)
    })
}

/// Run-state changes are file edits: a run whose formed grant covers a run's record changes
/// it through an ordinary file-write tool, and the ledger proves the edit. What the edit
/// changed is not judged here; every interface holding state in the record reads the result
/// under its own contract.
pub(super) fn proven_writes(
    reader: &mut (impl CheckedRunEvidenceReader + ?Sized),
    path: &Path,
    candidates: &[Movement],
) -> Result<Vec<ProvenWrite>, String> {
    let mut calls: BTreeMap<(InterfaceId, String), AdmittedCall> = BTreeMap::new();
    let mut writes = Vec::new();
    for movement in candidates {
        let (Endpoint::Interface(from), Endpoint::Interface(to)) = (&movement.from, &movement.to);
        let Some(reference) = movement.artifact.as_ref() else { continue; };
        let Ok((manifest, payload)) = reader.artifact(reference) else { continue; };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&payload) else { continue; };
        if value.get("arguments").is_some() {
            let Ok(call) = serde_json::from_value::<octopus_tool_handlers::ToolCallArtifact>(value) else { continue; };
            let Some(correlation) = call.correlation_id.clone() else { continue; };
            if call.arguments.get("ordered").is_some() { continue; }
            let Some(by) = admitted_under_grant(movement, &call.tool, to) else { continue; };
            let admitted = AdmittedCall { movement: movement.clone(), label: manifest.label, call, by };
            if calls.insert((to.clone(), correlation), admitted).is_some() {
                return Err("a proven run-record write repeats its exact call correlation".into());
            }
            continue;
        }
        let Ok(result) = serde_json::from_value::<ToolResultArtifact>(value) else { continue; };
        let Some(correlation) = result.correlation_id.clone() else { continue; };
        let Some(admitted) = calls.remove(&(from.clone(), correlation)) else { continue; };
        if result.tool != admitted.call.tool || !result.ok || result.error.is_some()
            || admitted.movement.seq >= movement.seq || !returned_to(movement, to) {
            continue;
        }
        let Some(Ok(body)) = result.body.clone().map(serde_json::from_value::<FileWriteResultBody>) else { continue; };
        if body.result != "files_written" { continue; }
        let indexed = body.writes.iter().enumerate().filter(|(_, evidence)| evidence.path == path).collect::<Vec<_>>();
        let [(index, evidence)] = indexed.as_slice() else { continue; };
        let write = match admitted.call.arguments.get("writes") {
            Some(serde_json::Value::Array(entries)) if entries.len() == body.writes.len() => entries[*index].clone(),
            Some(_) => continue,
            None if body.writes.len() == 1 => admitted.call.arguments.clone(),
            None => continue,
        };
        let Some(before_hash) = evidence.before_hash.clone() else { continue; };
        let Endpoint::Interface(source) = &admitted.movement.from;
        writes.push(ProvenWrite {
            writer: CheckedRunWriter {
                owner: to.clone(),
                source: source.clone(),
                door: from.clone(),
                tool: admitted.call.tool.clone(),
                call_label: admitted.label,
                result_label: result.label.clone(),
            },
            call: admitted.movement,
            result: movement.clone(),
            write,
            before_hash,
            after_hash: evidence.after_hash.clone(),
            by: admitted.by,
        });
    }
    Ok(writes)
}

pub(super) fn reconstructed_after(write: &serde_json::Value, before: &[u8]) -> Result<Vec<u8>, String> {
    match (write.get("content"), write.get("patch")) {
        (Some(serde_json::Value::String(content)), None) => Ok(content.as_bytes().to_vec()),
        (None, Some(patch)) => {
            let patch: json_patch::Patch = serde_json::from_value(patch.clone()).map_err(|error| error.to_string())?;
            let format: Option<JsonFormat> = write.get("json_format").cloned()
                .map(serde_json::from_value).transpose().map_err(|error| error.to_string())?;
            let mut value: serde_json::Value = serde_json::from_slice(before).map_err(|error| error.to_string())?;
            json_patch::patch(&mut value, &patch).map_err(|error| error.to_string())?;
            format.unwrap_or_default().encode(&value)
        }
        _ => Err("a proven run-record write carries neither exact content nor one patch".into()),
    }
}

/// A proven write keeps the run the file names. Everything else is the record's
/// content, re-read fail-closed by formation and by each interface holding state in it.
pub fn validate(before: &[u8], after: &[u8]) -> Result<(), String> {
    let prior = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
    let next = RunRecord::from_bytes(after).map_err(|error| format!("a proven write left a record that does not read: {error}"))?;
    if prior.id != next.id {
        return Err("a proven write changed which run the record is".into());
    }
    Ok(())
}

pub(super) fn transition_from(
    writes: &[ProvenWrite],
    current: &[u8],
    previous_call: u64,
    declared: &[CheckedRunWriter],
) -> Result<Option<CheckedRunTransition>, String> {
    let hash = ContentHash::of_bytes(current).to_hex();
    let record = RunRecord::from_bytes(current).map_err(|error| error.to_string())?;
    let matches = writes.iter().filter(|write| write.before_hash == hash && write.call.seq.0 > previous_call
        && (write.by != record.id || !declared.iter().any(|writer| writer.door == write.writer.door))).collect::<Vec<_>>();
    let write = match matches.as_slice() {
        [] => return Ok(None),
        [write] => *write,
        _ => return Err("proven run-record writes branch from one run snapshot".into()),
    };
    let after = reconstructed_after(&write.write, current)?;
    if ContentHash::of_bytes(&after).to_hex() != write.after_hash {
        return Err("a proven run-record write does not reconstruct its admitted file effect".into());
    }
    validate(current, &after)?;
    if write.by == record.id && RunRecord::from_bytes(&after).map_err(|error| error.to_string())?.ee != record.ee {
        return Err("a run never changes its own place; another run whose grant covers its record moves it".into());
    }
    Ok(Some(CheckedRunTransition {
        writer: write.writer.clone(),
        before: current.to_vec(),
        after,
        call: write.call.clone(),
        result: write.result.clone(),
        foreign: true,
    }))
}


#[cfg(test)]
mod tests {
    use super::*;

    fn proven(by: &str, door: &str, before: &[u8], after: &[u8]) -> ProvenWrite {
        let row = |seq: u64| Movement {
            seq: octopus_ledger::Seq(seq), lamport: octopus_ledger::Lamport(seq + 1), recorded_at_unix_ms: seq + 1,
            trace: octopus_ledger::TraceId("trace:edit".into()), kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(InterfaceId::new("world_provider_a")), to: Endpoint::Interface(InterfaceId::new(door)),
            artifact: None, invocation_metadata: None,
        };
        ProvenWrite {
            writer: CheckedRunWriter { owner: InterfaceId::new("world_provider_a"), source: InterfaceId::new("world_provider_a"),
                door: InterfaceId::new(door), tool: "file.write".into(), call_label: "file.write".into(), result_label: "file.write.result".into() },
            call: row(5), result: row(6), write: serde_json::json!({"content": String::from_utf8(after.to_vec()).unwrap()}),
            before_hash: ContentHash::of_bytes(before).to_hex(), after_hash: ContentHash::of_bytes(after).to_hex(), by: RunId::new(by),
        }
    }

    #[test]
    fn a_run_edits_its_own_record_by_an_ordinary_write_but_never_its_place_or_through_a_declared_writer() {
        let before = br#"{"id":"run_a","ee":"ee_existence","worker":"system_worker_a","trace":"trace:run_a","status":"idle","interface_state":{},"checkpoints":[],"active_checkpoints":[]}"#;
        let edited = br#"{"id":"run_a","ee":"ee_existence","worker":"system_worker_b","trace":"trace:run_a","status":"idle","interface_state":{"world_provider_b":{}},"checkpoints":[],"active_checkpoints":[]}"#;
        let moved = br#"{"id":"run_a","ee":"ee_memory","worker":"system_worker_a","trace":"trace:run_a","status":"idle","interface_state":{},"checkpoints":[],"active_checkpoints":[]}"#;
        let declared = [CheckedRunWriter { owner: InterfaceId::new("world_provider_a"), source: InterfaceId::new("system_interface_authoring_a"),
            door: InterfaceId::new("system_room_authoring_a"), tool: "file.write".into(), call_label: "file.write".into(), result_label: "file.write.result".into() }];
        let own = transition_from(&[proven("run_a", "system_room_a", before, edited)], before, 0, &declared).unwrap().unwrap();
        assert!(own.foreign && own.after == edited);
        assert!(transition_from(&[proven("run_a", "system_room_authoring_a", before, edited)], before, 0, &declared).unwrap().is_none());
        assert!(transition_from(&[proven("run_a", "system_room_a", before, moved)], before, 0, &declared).unwrap_err().contains("its own place"));
        assert!(transition_from(&[proven("run_b", "system_room_b", before, moved)], before, 0, &declared).unwrap().is_some());
    }

    #[test]
    fn a_proven_write_may_change_anything_but_which_run_the_record_is() {
        let before = br#"{"id":"run_a","ee":"ee_existence","worker":"system_worker_a","trace":"trace:run_a","status":"idle","interface_state":{},"checkpoints":[],"active_checkpoints":[]}"#;
        let moved = br#"{"id":"run_a","ee":"ee_memory","worker":"system_worker_b","trace":"trace:run_a","status":"idle","interface_state":{"world_provider_b":{}},"checkpoints":[],"active_checkpoints":[]}"#;
        let other = br#"{"id":"run_b","ee":"ee_existence","worker":"system_worker_a","trace":"trace:run_b","status":"idle","interface_state":{},"checkpoints":[],"active_checkpoints":[]}"#;
        validate(before, moved).unwrap();
        assert!(validate(before, other).is_err());
        assert!(validate(before, b"{}").is_err());
    }
}
