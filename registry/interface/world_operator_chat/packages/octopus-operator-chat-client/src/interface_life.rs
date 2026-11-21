use super::*;

pub(crate) fn plan_interface_life(
    interface_root: &Path,
    source_interface_id: &str,
    target_interface_id: &str,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    plan_interface_life_between_roots(interface_root, interface_root, source_interface_id, target_interface_id, writes)
}

pub(crate) fn plan_interface_life_between_roots(
    source_interface_root: &Path,
    interface_root: &Path,
    source_interface_id: &str,
    target_interface_id: &str,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    let source = source_interface_root.join(source_interface_id).join("life.json");
    let bytes = std::fs::read(&source).map_err(|error| {
        format!(
            "interface '{}' has no executable life at '{}': {error}",
            source_interface_id,
            source.display()
        )
    })?;
    let mut life: SpawnLifeConfig = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "interface life '{}' is malformed: {error}",
            source.display()
        )
    })?;
    import_owner_executables(&mut life, source_interface_id)?;
    push_json_write(
        interface_root.join(target_interface_id).join("life.json"),
        pretty_json(
            &serde_json::to_value(life)
                .map_err(|error| format!("imported interface life did not encode: {error}"))?,
        )?,
        format!("life:{target_interface_id}"),
        writes,
    )
}

pub(crate) fn plan_interface_life_with_root_slot_access(
    interface_root: &Path,
    source_interface_id: &str,
    target_interface_id: &str,
    root_slots: &[(&str, bool)],
    private_environment_root_slot: Option<&str>,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    let source = interface_root.join(source_interface_id).join("life.json");
    let bytes = std::fs::read(&source).map_err(|error| {
        format!(
            "interface '{}' has no executable life at '{}': {error}",
            source_interface_id,
            source.display()
        )
    })?;
    let mut life: SpawnLifeConfig = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "interface life '{}' is malformed: {error}",
            source.display()
        )
    })?;
    if life.processes.is_empty() {
        return Err(format!(
            "interface life '{}' contains no processes",
            source.display()
        ));
    }
    import_owner_executables(&mut life, source_interface_id)?;
    if let Some(slot) = private_environment_root_slot {
        life.private_environment_root_slot = Some(slot.to_string());
    }
    for process in &mut life.processes {
        for (root_slot, writable) in root_slots {
            process
                .authority
                .read_root_slots
                .insert((*root_slot).to_string());
            if *writable {
                process
                    .authority
                    .write_root_slots
                    .insert((*root_slot).to_string());
            }
        }
    }
    let content = pretty_json(
        &serde_json::to_value(life)
            .map_err(|error| format!("private-state-seeded life did not encode: {error}"))?,
    )?;
    push_json_write(
        interface_root.join(target_interface_id).join("life.json"),
        content,
        format!("life:{target_interface_id}"),
        writes,
    )
}

pub(crate) fn import_owner_executables(
    life: &mut SpawnLifeConfig,
    source_interface_id: &str,
) -> Result<(), String> {
    let source_interface_id =
        safe_segment(source_interface_id, "source interface executable owner")?;
    for process in &mut life.processes {
        let Some(relative) = process.bin.strip_prefix("owner://") else {
            continue;
        };
        let relative_path = Path::new(relative);
        if relative.is_empty()
            || relative_path.is_absolute()
            || relative_path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(format!(
                "interface '{source_interface_id}' declares invalid owner executable '{}'",
                process.bin
            ));
        }
        process.bin = format!("interface://{source_interface_id}/{relative}");
    }
    Ok(())
}
