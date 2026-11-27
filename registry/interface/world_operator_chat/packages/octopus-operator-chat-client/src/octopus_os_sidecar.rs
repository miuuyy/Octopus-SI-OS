use super::*;

pub(crate) fn plan_octopus_os_sidecar(
    interface_root: &Path,
    interface_id: &str,
    identity: &PlannedIdentity,
    component: &str,
    include_choices: bool,
    requested_authoring_profile: Option<&OctopusOsAuthoringProfile>,
    replace_authoring_profile: bool,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    if !identity.owns_client() {
        return Ok(());
    }
    let preserved = if include_choices {
        preserve_octopus_os_client_configuration(interface_root, interface_id, identity)?
    } else {
        None
    };
    let preserved_profile = preserved.as_ref().and_then(|value| value.declaration.authoring_profile.clone());
    let authoring_profile = match (requested_authoring_profile, preserved_profile) {
        (Some(requested), Some(preserved))
            if requested != &preserved && !replace_authoring_profile =>
        {
            return Err(format!(
                "Octopus OS client sidecar '{interface_id}' conflicts with the requested authoring profile"
            ));
        }
        (Some(requested), _) => {
            validate_octopus_os_authoring_profile(requested, Some(identity))?;
            Some(requested.clone())
        }
        (None, preserved) => preserved,
    };
    let mut presentation = preserved.as_ref().and_then(|value| value.declaration.presentation.clone());
    if include_choices {
        if let Some(title) = identity.native_communication.as_ref().and_then(|value| value.client_title.as_ref()) {
            if presentation.as_ref().is_some_and(|value| value.name != *title) {
                return Err(format!("Octopus OS client sidecar '{interface_id}' has a different authored title"));
            }
            presentation = Some(OctopusOsPresentation { name: title.clone(), projects: Vec::new() });
        }
    }
    let sidecar = OctopusOsInterfaceSidecar {
        schema_version: 1,
        workspace: "octopus_os".to_string(),
        interface: interface_id.to_string(),
        owner_interface: identity.chat_interface_id.clone(),
        component: component.to_string(),
        surface_role: identity.surface_role.clone(),
        provider_choice_interface: include_choices
            .then(|| identity.provider_choice_interface.clone()),
        worker_choice_interface: include_choices.then(|| identity.worker_choice_interface.clone()),
        authoring_profile,
        task_authoring_profiles: preserved.as_ref().map(|value| value.declaration.task_authoring_profiles.clone()).unwrap_or_default(),
        presentation,
        voice_context: include_choices.then(|| identity.voice.as_ref().map(|voice| voice.context.clone())).flatten(),
    };
    let mut encoded = serde_json::to_value(sidecar)
        .map_err(|error| format!("Octopus OS sidecar did not encode: {error}"))?;
    if let Some(preserved) = preserved {
        let previous = serde_json::to_value(preserved.declaration)
            .map_err(|error| format!("validated Octopus OS sidecar did not encode: {error}"))?;
        preserve_unchanged_client_json(&preserved.authored, &previous, &mut encoded);
    }
    push_json_write(
        interface_root.join(interface_id).join("octopus_os.json"),
        pretty_json(&encoded)?,
        format!("octopus-os:{interface_id}"),
        writes,
    )
}

pub(crate) fn preserve_unchanged_client_json(
    authored: &serde_json::Value,
    previous: &serde_json::Value,
    next: &mut serde_json::Value,
) {
    if previous == next {
        *next = authored.clone();
        return;
    }
    let (Some(authored), Some(previous), Some(next)) =
        (authored.as_object(), previous.as_object(), next.as_object_mut())
    else {
        return;
    };
    for (key, value) in authored {
        match (previous.get(key), next.get_mut(key)) {
            (Some(previous), Some(next)) => preserve_unchanged_client_json(value, previous, next),
            (None, None) => { next.insert(key.clone(), value.clone()); }
            _ => {}
        }
    }
}

pub(crate) fn preserve_octopus_os_client_configuration(
    interface_root: &Path,
    interface_id: &str,
    identity: &PlannedIdentity,
) -> Result<Option<PreservedOctopusOsClientConfiguration>, String> {
    let path = interface_root.join(interface_id).join("octopus_os.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => {
            return Err(format!(
                "Octopus OS client sidecar '{}' cannot be read: {error}",
                path.display()
            ));
        }
    };
    let sidecar: OctopusOsInterfaceSidecar = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "Octopus OS client sidecar '{}' is malformed: {error}",
            path.display()
        )
    })?;
    if sidecar.schema_version != 1
        || sidecar.workspace != "octopus_os"
        || sidecar.interface != interface_id
        || sidecar.owner_interface != interface_id
        || sidecar.component != "client"
        || sidecar.surface_role != identity.surface_role
        || sidecar.provider_choice_interface.as_deref()
            != Some(identity.provider_choice_interface.as_str())
        || sidecar.worker_choice_interface.as_deref()
            != Some(identity.worker_choice_interface.as_str())
    {
        return Err(format!(
            "Octopus OS authoring profile '{}' conflicts with its live client identity",
            path.display()
        ));
    }
    if sidecar.authoring_profile.is_none()
        && (!sidecar.task_authoring_profiles.is_empty() || sidecar.presentation.is_some())
    {
        return Err(format!(
            "Octopus OS client configuration '{}' has task configuration without its own authoring profile",
            path.display()
        ));
    }
    if let Some(profile) = sidecar.authoring_profile.as_ref() {
        validate_octopus_os_authoring_profile(profile, Some(identity))?;
    }
    if !sidecar.task_authoring_profiles.is_empty() && identity.surface_role != "home" {
        return Err(format!(
            "Octopus OS task authoring profiles '{}' do not belong to Home",
            path.display()
        ));
    }
    let mut profile_keys = BTreeSet::new();
    for profile in &sidecar.task_authoring_profiles {
        validate_octopus_os_authoring_profile(profile, None)?;
        if !profile_keys.insert((
            profile.ee_id.as_str(),
            profile.worker_choice_interface.as_str(),
        )) {
            return Err(format!(
                "Octopus OS client configuration '{}' repeats a task authoring profile",
                path.display()
            ));
        }
    }
    if let Some(presentation) = sidecar.presentation.as_ref() {
        if identity.surface_role == "task"
            && (!presentation.projects.is_empty() || presentation.name.is_empty()
                || presentation.name.chars().any(char::is_control))
        {
            return Err(format!(
                "Octopus OS task presentation '{}' must contain a name and no project presentations",
                path.display()
            ));
        }
        if !presentation.name.is_empty()
            && (presentation.name.trim() != presentation.name
                || presentation.name.chars().count() > 150)
        {
            return Err(format!(
                "Octopus OS presentation '{}' contains an invalid name",
                path.display()
            ));
        }
        let mut ee_ids = BTreeSet::new();
        for project in &presentation.projects {
            safe_segment(&project.ee_id, "presentation.projects.ee_id")?;
            if project.name.trim().is_empty()
                || project.name.chars().count() > 180
                || project.icon.chars().count() > 64
                || project.color.chars().count() > 64
                || !ee_ids.insert(project.ee_id.as_str())
            {
                return Err(format!(
                    "Octopus OS presentation '{}' contains an invalid project",
                    path.display()
                ));
            }
        }
    }
    let authored = serde_json::from_slice(&bytes)
        .map_err(|error| format!("validated Octopus OS sidecar JSON did not decode: {error}"))?;
    Ok(Some(PreservedOctopusOsClientConfiguration { declaration: sidecar, authored }))
}

pub(crate) fn validate_octopus_os_authoring_profile(
    profile: &OctopusOsAuthoringProfile,
    identity: Option<&PlannedIdentity>,
) -> Result<(), String> {
    safe_segment(&profile.ee_id, "authoring_profile.ee_id")?;
    publication_selection::retain_publication_evidence(&[], &profile.publication_evidence)?;
    safe_segment(
        &profile.worker_choice_interface,
        "authoring_profile.worker_choice_interface",
    )?;
    if let Some(identity) = identity {
        if profile.worker_choice_interface != identity.worker_choice_interface {
            return Err(
                "Octopus OS authoring profile conflicts with its live client identity".to_string(),
            );
        }
    }
    for (interface, slots) in &profile.door_host_slots {
        safe_segment(interface, "authoring_profile door interface")?;
        if slots.is_empty() {
            return Err(format!(
                "Octopus OS authoring profile door '{interface}' has no host slots"
            ));
        }
        for (slot, root) in slots {
            safe_segment(slot, "authoring_profile root slot")?;
            require_absolute("authoring_profile root path", root)?;
        }
    }
    validate_external_route_shapes(
        &profile.external_inputs,
        &profile.external_relays,
        &profile.model_outputs,
    )?;
    let mut artifacts = BTreeSet::new();
    for binding in &profile.context_artifact_bindings {
        context_bindings::validate_artifact_id(&binding.id)?;
        let id = binding.id.clone();
        if !artifacts.insert(id.clone()) {
            return Err(format!(
                "Octopus OS authoring profile repeats context artifact binding '{id}'"
            ));
        }
    }
    octopus_run_authoring::validate_payload_artifact_selection(&profile.payload_artifact_bindings)?;
    for reference in &profile.payload_artifact_bindings {
        if artifacts.contains(reference.id.as_str()) {
            return Err("authoring profile payload selection collides with context bindings".into());
        }
    }
    let mut kinds = BTreeSet::new();
    for kind in &profile.selected_worker_environment_kinds {
        let kind = safe_segment(kind, "authoring_profile worker environment kind")?;
        if !kinds.insert(kind.clone()) {
            return Err(format!(
                "Octopus OS authoring profile repeats worker environment kind '{kind}'"
            ));
        }
    }
    let mut tools = BTreeSet::new();
    for selection in &profile.selected_tools {
        let tool = safe_policy_axis_value(&selection.tool, "authoring_profile selected tool")?;
        safe_segment(
            &selection.interface,
            "authoring_profile selected tool interface",
        )?;
        safe_segment(
            &selection.worker_environment_kind,
            "authoring_profile selected tool worker environment kind",
        )?;
        if !tools.insert(tool.clone()) {
            return Err(format!(
                "Octopus OS authoring profile repeats selected tool '{tool}'"
            ));
        }
    }
    Ok(())
}
