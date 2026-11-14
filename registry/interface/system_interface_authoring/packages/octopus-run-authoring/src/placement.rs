use super::*;

fn placed_ee(reader: &mut (impl CheckedRunEvidenceReader + ?Sized), write: &foreign_edit::ProvenWrite) -> Result<Option<String>, String> {
    if let Some(hash) = ContentHash::from_hex(&write.before_hash) {
        if let Ok(before) = reader.snapshot(&hash) {
            if ContentHash::of_bytes(&before) != hash { return Ok(None); }
            let Ok(after) = foreign_edit::reconstructed_after(&write.write, &before) else { return Ok(None); };
            if ContentHash::of_bytes(&after).to_hex() != write.after_hash || foreign_edit::validate(&before, &after).is_err() {
                return Ok(None);
            }
            return RunRecord::from_bytes(&after).map(|record| Some(record.ee.as_str().to_string())).map_err(|error| error.to_string());
        }
    }
    if let Some(serde_json::Value::String(content)) = write.write.get("content") {
        if ContentHash::of_bytes(content.as_bytes()).to_hex() != write.after_hash { return Ok(None); }
        return Ok(RunRecord::from_bytes(content.as_bytes()).ok().map(|record| record.ee.as_str().to_string()));
    }
    let Some(serde_json::Value::Array(operations)) = write.write.get("patch") else { return Ok(None); };
    Ok(operations.iter().rev()
        .find(|operation| operation.get("op").and_then(serde_json::Value::as_str) == Some("replace")
            && operation.get("path").and_then(serde_json::Value::as_str) == Some("/ee"))
        .and_then(|operation| operation.get("value").and_then(serde_json::Value::as_str).map(str::to_string)))
}

pub(super) fn prove_move(
    evidence: &CheckedRunEvidence<'_>,
    path: &Path,
    record: &RunRecord,
    proven_ee: &str,
    after_seq: u64,
    before_seq: u64,
) -> Result<(), String> {
    let mut reader = FileCheckedRunReader::open(evidence, Some(after_seq))?;
    let mut candidates = Vec::new();
    let mut sequence = reader.next_sequence();
    while sequence < before_seq {
        let Some(movement) = reader.movement(sequence)? else { break; };
        sequence = sequence.checked_add(1).ok_or("placement evidence sequence overflow")?;
        if movement.seq.0 > after_seq && movement.kind == MovementKind::InterfaceOutbound && movement.artifact.is_some() {
            candidates.push(movement);
        }
    }
    let writes = foreign_edit::proven_writes(&mut reader, path, &candidates)?;
    let mut placed = None;
    for write in writes.iter().rev().filter(|write| write.by != record.id) {
        if let Some(ee) = placed_ee(&mut reader, write)? {
            placed = Some(ee);
            break;
        }
    }
    if placed.as_deref() == Some(record.ee.as_str()) {
        return Ok(());
    }
    Err(format!(
        "run '{}' names EE '{}', but no write by another run whose grant covers its record proves that move; its proven place is '{}', and restoring it heals the run",
        record.id.as_str(),
        record.ee.as_str(),
        proven_ee
    ))
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PlacementReceipt {
    run: String,
    ee: String,
    through_seq: u64,
}

pub(super) fn read_receipt(root: &Path, run: &RunId) -> Result<Option<(u64, String)>, String> {
    let path = root.join(format!("{}.json", run.as_str()));
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("placement receipt '{}' cannot be read: {error}", path.display())),
    };
    let receipt: PlacementReceipt = serde_json::from_slice(&bytes)
        .map_err(|error| format!("placement receipt '{}' is malformed: {error}", path.display()))?;
    if receipt.run != run.as_str() {
        return Err(format!("placement receipt '{}' names run '{}'", path.display(), receipt.run));
    }
    Ok(Some((receipt.through_seq, receipt.ee)))
}

pub(super) fn write_receipt(root: &Path, run: &RunId, ee: &octopus_descriptors::EeId, through_seq: u64) -> Result<(), String> {
    use std::io::Write;
    std::fs::create_dir_all(root).map_err(|error| format!("placement receipts cannot be created: {error}"))?;
    let bytes = serde_json::to_vec(&PlacementReceipt { run: run.as_str().into(), ee: ee.as_str().into(), through_seq })
        .map_err(|error| error.to_string())?;
    let path = root.join(format!("{}.json", run.as_str()));
    let staged = root.join(format!(".{}.{}.tmp", run.as_str(), std::process::id()));
    let mut file = std::fs::File::create(&staged).map_err(|error| error.to_string())?;
    file.write_all(&bytes).and_then(|_| file.sync_all()).map_err(|error| error.to_string())?;
    std::fs::rename(&staged, &path).map_err(|error| error.to_string())?;
    std::fs::File::open(root).and_then(|directory| directory.sync_all()).map_err(|error| error.to_string())
}
