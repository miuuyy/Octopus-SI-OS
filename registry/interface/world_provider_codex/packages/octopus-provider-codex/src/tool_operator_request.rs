use super::*;
use serde::{Deserialize, Serialize};
use octopus_app_server_interface::{OperatorPermissionDecision as AppDecision, OperatorPermissionPersistence};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(transparent)]
pub(crate) struct ToolOperatorRequest(octopus_app_server_interface::OwnerOperatorRequest);

impl std::ops::Deref for ToolOperatorRequest {
    type Target = octopus_app_server_interface::OwnerOperatorRequest;
    fn deref(&self) -> &Self::Target { &self.0 }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ToolOperatorPhase {
    AwaitingOperator,
    AwaitingResume {
        operator_artifact: ArtifactRef,
        decision: AppDecision,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selected_persistence: Option<OperatorPermissionPersistence>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ToolOperatorContinuity {
    pub request: ToolOperatorRequest,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub payload_artifact_bindings: Vec<ArtifactRef>,
    pub request_artifact: ArtifactRef,
    pub operator_interface: InterfaceId,
    pub phase: ToolOperatorPhase,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResumedResult {
    request_id: String,
    original_result: ToolResultArtifact,
    #[serde(rename = "evidence")]
    _evidence: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(rename = "viewer")]
    _viewer: Option<serde_json::Map<String, serde_json::Value>>,
}

fn reject(message: impl Into<String>) -> InterfaceDriverError {
    InterfaceDriverError::FailedClosed(format!("tool operator continuation: {}", message.into()))
}

impl ToolOperatorRequest {
    pub(super) fn from_result(
        result: &ToolResultArtifact,
    ) -> Result<Option<Self>, InterfaceDriverError> {
        let Some(value) = result
            .body
            .as_ref()
            .and_then(|body| body.get("operator_request"))
        else {
            return Ok(None);
        };
        let request: Self = serde_json::from_value(value.clone())
            .map_err(|error| reject(format!("malformed owner request: {error}")))?;
        request.validate()?;
        if !result.ok
            || result
                .body
                .as_ref()
                .and_then(|body| body.get("effect_performed"))
                != Some(&serde_json::Value::Bool(false))
        {
            return Err(reject(
                "request result must explicitly prove no application effect",
            ));
        }
        if result.tool != request.original.tool
            || result.correlation_id.as_deref() != Some(request.call_id.as_str())
            || result.label != format!("{}.result", request.original.tool)
        {
            return Err(reject("request differs from its original result tuple"));
        }
        Ok(Some(request))
    }

    pub(super) fn validate(&self) -> Result<(), InterfaceDriverError> {
        self.0.validate().map_err(reject)
    }

    pub(super) fn validate_pending(
        &self,
        state: &CodexContinuityState,
    ) -> Result<(), InterfaceDriverError> {
        let pending = state
            .pending
            .as_ref()
            .or(state.interrupted_pending.as_ref());
        if state.run != self.original.run
            || !matches!(pending, Some(CodexPendingContinuity::DynamicTool { call_id, tool, .. })
                if call_id == &self.call_id && tool == &self.original.tool)
        {
            return Err(reject(
                "request no longer names the exact native DynamicTool",
            ));
        }
        Ok(())
    }

    pub(super) fn validate_envelope(
        &self,
        input: &InterfaceInvocationEnvelope,
        source: &InterfaceId,
    ) -> Result<(), InterfaceDriverError> {
        let metadata = input.metadata()?;
        self.0.validate_bound_files(metadata).map_err(reject)?;
        let runtime = input.runtime()?;
        let bindings = metadata
            .checkpoint_binding_policy
            .as_ref()
            .ok_or_else(|| reject("missing admitted checkpoint binding snapshot"))?;
        if runtime.run.as_str() != self.original.run
            || runtime.worker != self.original.worker
            || bindings.inbound.interface != *source
            || !metadata
                .outbound_interface_bindings
                .contains(&runtime.interface)
            || metadata.verification_failure.is_some()
        {
            return Err(reject("source/scope differs from the admitted movement"));
        }
        for tool in [&self.original.tool, &self.resume.tool] {
            if !metadata.grant.tools.contains(tool)
                || metadata.grant.hard_blocked_tools.contains(tool)
            {
                return Err(reject("application tool is outside the formed grant"));
            }
        }
        if !metadata
            .grant
            .interfaces
            .contains(&self.original.door_interface.as_str().to_string())
        {
            return Err(reject("application door is outside the formed grant"));
        }
        Ok(())
    }

    pub(super) fn validate_original(
        &self,
        input: &InterfaceInvocationEnvelope,
        record: &serde_json::Value,
        context: &[SessionContextArtifact],
        hidden_tools: &[String],
    ) -> Result<(), InterfaceDriverError> {
        self.validate_envelope(input, &self.original.door_interface)?;
        let client = octopus_run_authoring::bound_client_interface(record, &input.runtime()?.interface).map_err(reject)?;
        self.0.validate_operator_interface(&client).map_err(reject)?;
        if !hidden_tools.contains(&self.resume.tool) {
            return Err(reject(
                "owner continuation is not declared internal by the exact provider",
            ));
        }
        for tool in [&self.original.tool, &self.resume.tool] {
            let door = octopus_run_authoring::bound_tool_door(
                record,
                &input.runtime()?.interface,
                tool,
            )
            .map_err(reject)?;
            if door != self.original.door_interface {
                return Err(reject("tool route names another owner door"));
            }
        }
        let source = context
            .iter()
            .find(|entry| entry.source_artifact == self.original.source_artifact)
            .ok_or_else(|| reject("original tool call is absent from bound source evidence"))?;
        let call: octopus_tool_handlers::ToolCallArtifact =
            serde_json::from_value(source.payload.clone())
                .map_err(|error| reject(format!("original source is not a tool call: {error}")))?;
        if source.label != TOOL_CALL_LABEL
            || call.tool != self.original.tool
            || call.correlation_id.as_deref() != Some(self.call_id.as_str())
            || call.arguments != self.original.arguments
        {
            return Err(reject(
                "original source artifact differs from the native tool call",
            ));
        }
        Ok(())
    }

    pub(super) fn inputs(&self, config: &OperatorContinuityConfig) -> Vec<ProviderVerifiedInput> {
        config
            .approval_inputs(&self.request_id)
            .into_iter()
            .map(|mut input| {
                input.required_verified_artifact.params =
                    self.0.verification_params();
                input.also_deliver_to_tool = Some(self.resume.tool.clone());
                input
            })
            .collect()
    }

    pub(super) fn projection(&self, config: &OperatorContinuityConfig) -> CodexTurnOutput {
        CodexTurnOutput::client(
            OPERATOR_APPROVAL_REQUEST_LABEL,
            self.0.projection(),
        )
        .with_verified_inputs(self.inputs(config))
    }
}

impl ToolOperatorContinuity {
    fn validate_payload_bindings(&self, input: &InterfaceInvocationEnvelope) -> Result<(), InterfaceDriverError> {
        octopus_run_authoring::validate_payload_artifact_selection(&self.payload_artifact_bindings).map_err(reject)?;
        let metadata = input.metadata()?;
        for reference in &self.payload_artifact_bindings {
            if !metadata.artifact_binding_refs.iter().filter(|bound| bound.id == reference.id).eq([reference])
                || metadata.grant.artifact_access.get(reference.id.as_str()).is_none_or(|access| !access.read)
                || input.artifact_binding_payloads.iter().filter(|bound| bound.artifact.id == reference.id).map(|bound| &bound.artifact).ne([reference]) {
                return Err(reject("operator continuation lost or changed an exact selected payload read binding"));
            }
        }
        Ok(())
    }

    pub(super) fn admit(
        &mut self,
        input: &InterfaceInvocationEnvelope,
        config: &OperatorContinuityConfig,
    ) -> Result<(), InterfaceDriverError> {
        self.validate_payload_bindings(input)?;
        if self.phase != ToolOperatorPhase::AwaitingOperator {
            return Err(reject("operator response was already consumed"));
        }
        let payload: serde_json::Value = serde_json::from_slice(&input.read_artifact_payload()?)
            .map_err(|error| reject(error.to_string()))?;
        let response = self.request.parse_operator_response(
            payload
                .get("operator_approval_response")
                .cloned()
                .ok_or_else(|| reject("missing exact operator response wrapper"))?,
        )
        .map_err(|error| reject(format!("malformed application decision: {error}")))?;
        self.request.validate_operator_interface(&self.operator_interface).map_err(reject)?;
        let metadata = input.metadata()?;
        let bindings = metadata
            .checkpoint_binding_policy
            .as_ref()
            .ok_or_else(|| reject("missing admitted operator binding"))?;
        self.request
            .validate_envelope(input, &self.operator_interface)?;
        let expected = self
            .request
            .inputs(config)
            .remove(match response.decision {
                AppDecision::Accept => 0,
                AppDecision::Decline | AppDecision::Cancel => 1,
            })
            .required_verified_artifact;
        if input.read_artifact_manifest()?.label != OPERATOR_APPROVAL_RESPONSE_LABEL
            || metadata.required_verified_artifacts != vec![expected.clone()]
            || !metadata
                .grant
                .mark_outcomes
                .contains(&expected.mark.as_str().to_string())
            || metadata.outbound_interface_bindings
                != vec![
                    input.runtime()?.interface.clone(),
                    self.request.original.door_interface.clone(),
                ]
            || bindings
                .outbound
                .iter()
                .map(|binding| &binding.interface)
                .ne(metadata.outbound_interface_bindings.iter())
            || metadata
                .checkpoint_admission
                .as_ref()
                .is_none_or(|admission| {
                    !admission.complete
                        || admission.artifact_label != OPERATOR_APPROVAL_RESPONSE_LABEL
                        || admission.required_artifact_labels
                            != vec![OPERATOR_APPROVAL_RESPONSE_LABEL.to_string()]
                })
        {
            return Err(reject(
                "operator decision lacks its exact admitted verifier/mark edge",
            ));
        }
        self.phase = ToolOperatorPhase::AwaitingResume {
            operator_artifact: input.artifact.clone(),
            decision: response.decision,
            selected_persistence: response.selected_persistence,
        };
        Ok(())
    }

    pub(super) fn resume_result(
        &self,
        result: &ToolResultArtifact,
        input: &InterfaceInvocationEnvelope,
    ) -> Result<ToolResultArtifact, InterfaceDriverError> {
        self.validate_payload_bindings(input)?;
        let ToolOperatorPhase::AwaitingResume { decision, selected_persistence, .. } = &self.phase else {
            return Err(reject(
                "GUI result cannot settle before the provider admits the operator decision",
            ));
        };
        octopus_app_server_interface::OperatorPermissionResponse {
            request_id: self.request.request_id.clone(), call_id: self.request.call_id.clone(),
            decision: *decision, selected_persistence: *selected_persistence,
        }.validate(&self.request.request_id, &self.request.call_id, self.request.permission_offer.as_ref()).map_err(reject)?;
        self.request
            .validate_envelope(input, &self.request.original.door_interface)?;
        if result.tool != self.request.resume.tool
            || result.label != format!("{}.result", self.request.resume.tool)
            || result.correlation_id.as_deref() != Some(self.request.resume.correlation_id.as_str())
            || !result.ok
        {
            return Err(reject(
                "resume result differs from the distinct declared tool/correlation",
            ));
        }
        let body: ResumedResult = serde_json::from_value(
            result
                .body
                .clone()
                .ok_or_else(|| reject("resume result has no body"))?,
        )
        .map_err(|error| reject(format!("malformed resume result: {error}")))?;
        if body.request_id != self.request.request_id
            || body.original_result.tool != self.request.original.tool
            || body.original_result.correlation_id.as_deref() != Some(self.request.call_id.as_str())
            || body.original_result.label != format!("{}.result", self.request.original.tool)
            || body
                .original_result
                .body
                .as_ref()
                .and_then(|body| body.get("operator_request"))
                .is_some()
            || (*decision != AppDecision::Accept && body.original_result.ok)
        {
            return Err(reject(
                "resume result does not close the stored original operation/decision",
            ));
        }
        body.original_result
            .validate()
            .map_err(InterfaceDriverError::Protocol)?;
        Ok(body.original_result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_descriptors::{ArtifactId, CheckpointId, EeId, RunId};
    use octopus_interfaces::{
        CheckpointAdmissionMetadata, CheckpointBindingPolicySnapshot, GrantProjection,
    };
    use octopus_ledger::ContentHash;

    #[test]
    fn application_continuation_preserves_exact_call_and_requires_admitted_phase() {
        let original = ArtifactRef {
            id: ArtifactId::new("original"),
            content: ContentHash::of_bytes(b"original"),
        };
        let digest = |request: &octopus_app_server_interface::ApplicationOperatorRequest| {
            let mut commitment = serde_json::json!({
                "schema_version": request.schema_version,
                "original": request.original,
                "scope": request.scope,
                "application": request.application,
                "approval_scope": request.approval_scope,
            });
            if let Some(offer) = &request.permission_offer {
                commitment["permission_offer"] = serde_json::json!(offer);
            }
            if let Some(policy) = &request.native_policy { commitment["native_policy"] = serde_json::json!(policy); }
            sha256_hex(&octopus_descriptors::canonical_bytes(&commitment))
        };
        let mut request: octopus_app_server_interface::ApplicationOperatorRequest = serde_json::from_value(serde_json::json!({
            "schema_version":1,"request_id":"pending","call_id":"call-1",
            "original":{"run":"run-1","ee":"ee-1","worker":"system_worker_test","door_interface":"system_gui_test","tool":"gui.app_state__approval_task","correlation_id":"call-1","arguments":{"app":{"kind":"bundle_id","value":"com.example.app"}},"source_movement_seq":3,"source_artifact":original},
            "scope":{"worker_environment_id":"room-1","formation_pin":"pin-1","host_generation":"host-1"},
            "application":{"bundle_id":"com.example.app","bundle_path":"/Applications/Example.app","display_name":"Example","instance":{"app":{"host_generation":"host-1","app_instance_id":"app-1"},"process_id":123}},
            "approval_scope":"formed_app_target","resume":{"tool":"gui.app_resume__approval_task","correlation_id":"pending"}
        })).unwrap();
        let request_digest = digest(&request);
        request.request_id = format!("sha256:{request_digest}");
        request.resume.correlation_id = format!("app-resume:{request_digest}");
        request.validate().unwrap();
        assert_eq!(
            request
                .application
                .instance
                .as_ref()
                .unwrap()
                .launch_time_unix_ms,
            None
        );
        let mut changed = request.clone();
        changed.scope.host_generation = "other".to_string();
        assert!(changed.validate().is_err());
        changed = request.clone();
        changed
            .application
            .instance
            .as_mut()
            .unwrap()
            .launch_time_unix_ms = Some(0);
        assert!(changed.validate().is_err());
        let mut nonrunning = request.clone();
        nonrunning.application.instance = None;
        let nonrunning_digest = digest(&nonrunning);
        nonrunning.request_id = format!("sha256:{nonrunning_digest}");
        nonrunning.resume.correlation_id = format!("app-resume:{nonrunning_digest}");
        nonrunning.validate().unwrap();
        assert_ne!(nonrunning.request_id, request.request_id);
        assert!(serde_json::to_value(&nonrunning).unwrap()["application"]["instance"].is_null());
        assert!(serde_json::to_value(&request).unwrap()["application"]["instance"]["launch_time_unix_ms"].is_null());
        let mut offered = request.clone();
        offered.permission_offer = Some(serde_json::from_value(serde_json::json!({"client_interface":"world_client_test",
            "offered_choices":[{"decision":"accept","persistence":"session"},{"decision":"accept","persistence":"always"},{"decision":"decline","persistence":"once"}]})).unwrap());
        offered.native_policy = Some(serde_json::from_value(serde_json::json!({"decision":"allowed","allow_persistent_approval":true,
            "risk":"high","warning_subtitle":"Original native warning","target":{"bundle_id":offered.application.bundle_id,
            "bundle_path":offered.application.bundle_path,"display_name":offered.application.display_name}})).unwrap());
        let offered_digest = digest(&offered);
        offered.request_id = format!("sha256:{offered_digest}");offered.resume.correlation_id = format!("app-resume:{offered_digest}");
        offered.validate().unwrap();
        let projected = octopus_app_server_interface::OwnerOperatorRequest::from(offered.clone()).projection();
        assert_eq!(projected["detail"]["native_policy"]["warning_subtitle"],"Original native warning");
        offered.native_policy.as_mut().unwrap().allow_persistent_approval=false;
        assert!(offered.validate().is_err());
        let request = ToolOperatorRequest(request.into());
        let config = test_operator_continuity_config();
        let mut continuity = ToolOperatorContinuity {
            payload_artifact_bindings: Vec::new(),
            request: request.clone(),
            request_artifact: original.clone(),
            operator_interface: InterfaceId::new("world_client_test"),
            phase: ToolOperatorPhase::AwaitingOperator,
        };
        let dir = tempfile::tempdir().unwrap();
        let response = serde_json::json!({"operator_approval_response":{"request_id":request.request_id,"call_id":"call-1","decision":"accept"}});
        let file = InterfaceArtifactFile::write(
            dir.path(),
            OPERATOR_APPROVAL_RESPONSE_LABEL,
            &serde_json::to_vec(&response).unwrap(),
        )
        .unwrap();
        let bindings: CheckpointBindingPolicySnapshot = serde_json::from_value(serde_json::json!({
            "inbound":{"interface":"world_client_test","config":{"artifact_policy":{"mode":"allow","names":[OPERATOR_APPROVAL_RESPONSE_LABEL],"namespaces":[]}}},
            "outbound":[{"interface":"world_provider_test","config":{"artifact_policy":{"mode":"all"}}},{"interface":"system_gui_test","config":{"artifact_policy":{"mode":"all"}}}]
        })).unwrap();
        let metadata = InvocationMetadata::new(GrantProjection {
            tools: vec![
                "gui.app_state__approval_task".into(),
                "gui.app_resume__approval_task".into(),
            ],
            interfaces: vec!["system_gui_test".into()],
            mark_outcomes: vec!["operator.approval".into()],
            ..Default::default()
        })
        .with_runtime(RuntimeInvocationMetadata {
            run: RunId::new("run-1"),
            ee: EeId::new("ee-1"),
            worker: InterfaceId::new("system_worker_test"),
            checkpoint: CheckpointId::new("already_removed"),
            interface: InterfaceId::new("world_provider_test"),
            epoch: 0,
            run_record_hash: Some(ContentHash::of_bytes(b"pinned run").to_hex()),
        })
        .with_outbound_interface_bindings(vec![
            InterfaceId::new("world_provider_test"),
            InterfaceId::new("system_gui_test"),
        ])
        .with_checkpoint_binding_policy(bindings)
        .with_required_verified_artifacts(vec![
            request.inputs(&config)[0]
                .required_verified_artifact
                .clone(),
        ]);
        let mut input = InterfaceInvocationEnvelope {
            artifact: original,
            artifact_file: file,
            artifact_binding_payloads: vec![],
            metadata: Some(metadata),
        };
        input.metadata.as_mut().unwrap().grant.interface_affordance_specs =
            serde_json::from_value(serde_json::json!([
                {"name":request.original.tool,"description":"Application state","args_schema":{"type":"object"}},
                {"name":request.resume.tool,"description":"Operator continuation","args_schema":{"type":"object"}}
            ])).unwrap();
        let original_grant = input.granted_tool_names().unwrap();
        let visible = model_visible_affordances(
            input.interface_affordance_specs().unwrap(),
            &[request.resume.tool.clone()],
        )
        .unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0]["name"], request.original.tool);
        assert_eq!(input.granted_tool_names().unwrap(), original_grant);
        let result: ToolResultArtifact = serde_json::from_value(serde_json::json!({"label":"gui.app_resume__approval_task.result","tool":"gui.app_resume__approval_task","correlation_id":request.resume.correlation_id,"ok":true,"body":{"request_id":request.request_id,"evidence":{"status":"ready"},"viewer":{"protocol":"desktop_session","host_generation":"host-1"},"original_result":{"label":"gui.app_state__approval_task.result","tool":"gui.app_state__approval_task","correlation_id":"call-1","ok":true,"body":{"state":"ready"}}}})).unwrap();
        assert!(continuity.resume_result(&result, &input).is_err());
        assert!(continuity.admit(&input, &config).is_err());
        input.metadata.as_mut().unwrap().checkpoint_admission = Some(CheckpointAdmissionMetadata {
            artifact_label: OPERATOR_APPROVAL_RESPONSE_LABEL.into(),
            required_artifact_labels: vec![OPERATOR_APPROVAL_RESPONSE_LABEL.into()],
            complete: true,
        });
        continuity.admit(&input, &config).unwrap();
        assert!(continuity.admit(&input, &config).is_err());
        let restored: ToolOperatorContinuity =
            serde_json::from_slice(&serde_json::to_vec(&continuity).unwrap()).unwrap();
        input
            .metadata
            .as_mut()
            .unwrap()
            .checkpoint_binding_policy
            .as_mut()
            .unwrap()
            .inbound
            .interface = InterfaceId::new("system_gui_test");
        let nested = restored.resume_result(&result, &input).unwrap();
        assert_eq!(nested.correlation_id.as_deref(), Some("call-1"));
        let mut unknown = result.clone();
        unknown.body.as_mut().unwrap()["approved"] = serde_json::Value::Bool(true);
        assert!(restored.resume_result(&unknown, &input).is_err());
        let mut foreign = result;
        foreign.correlation_id = Some("other".into());
        assert!(restored.resume_result(&foreign, &input).is_err());
        assert_upload_variant_preserves_the_native_call(dir.path(), &input, &config);
    }
    fn assert_upload_variant_preserves_the_native_call(root: &Path, template: &InterfaceInvocationEnvelope, config: &OperatorContinuityConfig) {
        use sha2::{Digest,Sha256};
        use octopus_interfaces::ArtifactBindingPayload;
        let file_bytes = b"\0\xffexact chosen bytes";
        let selected = InterfaceArtifactFile::write(root.join("selected"),"fixture.upload.file",file_bytes).unwrap();
        let selected_ref = ArtifactRef { id:ArtifactId::new("selected-file"),content:ContentHash::of_bytes(&std::fs::read(selected.manifest_path()).unwrap()) };
        let arguments=serde_json::json!({"reservation_id":"a".repeat(32),"file_chooser_id":"b".repeat(32),"files":[{"artifact_ref":selected_ref,"filename":"chosen.bin"}]});
        let original=serde_json::json!({"run":"run-1","ee":"ee-1","worker":"system_worker_test","door_interface":"system_browser_test","tool":"browser.filechooser_set_files__approval_task","correlation_id":"upload-1","arguments":arguments,"source_movement_seq":3,"source_artifact":template.artifact});
        let scope=serde_json::json!({"worker_environment_id":"browser-room","formation_pin":"browser-pin","host_generation":"c".repeat(32)});
        let upload=serde_json::json!({"browser_context_id":"retained-context","target_id":"retained-tab","reservation_id":"a".repeat(32),"file_chooser_id":"b".repeat(32),"document_token":"d".repeat(32),"backend_node_id":21,"top_level_origin":"https://example.test","files":[{"artifact_ref":selected_ref,"filename":"chosen.bin","payload_sha256":format!("{:x}",Sha256::digest(file_bytes)),"size_bytes":file_bytes.len()}]});
        let offer=serde_json::json!({"client_interface":"world_client_test","offered_choices":[
            {"decision":"accept","persistence":"session"},{"decision":"accept","persistence":"always"},{"decision":"decline","persistence":"once"}]});
        let commitment=serde_json::json!({"schema_version":1,"original":original,"scope":scope,"approval_scope":"browser_file_upload","upload":upload,"permission_offer":offer});
        let digest=format!("{:x}",Sha256::digest(octopus_descriptors::canonical_bytes(&commitment)));
        let wire=serde_json::json!({"schema_version":1,"request_id":format!("sha256:{digest}"),"call_id":"upload-1","original":original,"scope":scope,"approval_scope":"browser_file_upload","upload":upload,"permission_offer":offer,"resume":{"tool":"browser.filechooser_resume__approval_task","correlation_id":format!("browser-upload-resume:{digest}")}});
        let request:ToolOperatorRequest=serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(&request).unwrap(),wire);
        let mut mixed=wire.clone();mixed["application"]=serde_json::json!({});
        assert!(serde_json::from_value::<ToolOperatorRequest>(mixed).is_err());
        let mut changed=wire.clone();changed["upload"]["files"][0]["artifact_ref"]["content"][0]=serde_json::json!(255);
        assert!(serde_json::from_value::<ToolOperatorRequest>(changed).is_err());
        let projection=request.0.projection();
        assert_eq!(projection["kind"],"browser_file_upload");assert!(projection["detail"].get("application").is_none());
        let mut input=template.clone();
        {
            let metadata=input.metadata.as_mut().unwrap();
            metadata.grant.tools=vec![request.original.tool.clone(),request.resume.tool.clone()];
            metadata.grant.interfaces=vec!["system_browser_test".into(),"world_client_test".into()];
            metadata.artifact_binding_refs=vec![selected_ref.clone()];
            metadata.grant.artifact_access=serde_json::from_value(serde_json::json!({"selected-file":{"read":true,"change":false,"create":false,"delete":false}})).unwrap();
            metadata.required_verified_artifacts=vec![request.inputs(config)[0].required_verified_artifact.clone()];
            metadata.outbound_interface_bindings=vec![InterfaceId::new("world_provider_test"),InterfaceId::new("system_browser_test")];
            let binding=metadata.checkpoint_binding_policy.as_mut().unwrap();
            binding.inbound.interface=InterfaceId::new("world_client_test");binding.outbound[1].interface=InterfaceId::new("system_browser_test");
        }
        input.artifact_binding_payloads=vec![ArtifactBindingPayload::new(selected_ref.clone(),file_bytes.to_vec())];
        let response=serde_json::json!({"operator_approval_response":{"request_id":request.request_id,"call_id":request.call_id,"decision":"accept","selected_persistence":"always"}});
        for selection in [serde_json::Value::Null, serde_json::json!("unknown")] {
            let mut changed=response["operator_approval_response"].clone();changed["selected_persistence"]=selection;
            assert!(request.parse_operator_response(changed).is_err());
        }
        assert!(request.parse_operator_response(serde_json::json!({"request_id":request.request_id,"call_id":request.call_id,"decision":"accept"})).is_err());
        let cancelled=request.parse_operator_response(serde_json::json!({"request_id":request.request_id,"call_id":request.call_id,"decision":"cancel"})).unwrap();
        assert_eq!(cancelled.decision,AppDecision::Cancel);assert_eq!(cancelled.selected_persistence,None);
        let mut retired=wire.clone();retired["consent_offer"]=serde_json::json!({});
        assert!(serde_json::from_value::<ToolOperatorRequest>(retired).is_err());
        assert!(request.validate_operator_interface(&InterfaceId::new("other-client")).is_err());
        input.artifact_file=InterfaceArtifactFile::write(root.join("upload-response"),OPERATOR_APPROVAL_RESPONSE_LABEL,&serde_json::to_vec(&response).unwrap()).unwrap();
        input.artifact=ArtifactRef { id:ArtifactId::new("upload-response"),content:ContentHash::of_bytes(&std::fs::read(input.artifact_file.manifest_path()).unwrap()) };
        let pending=CodexPendingContinuity::DynamicTool{
            native_operation: None,turn_id:"native-turn".into(),call_id:request.call_id.clone(),tool:request.original.tool.clone()};
        let mut native=CodexContinuityState{schema_version:1,provider_interface:InterfaceId::new("world_provider_test"),run:"run-1".into(),thread_id:"native-thread".into(),thread_shape_fingerprint:"a".repeat(64),context_generation:1,static_context_fingerprint:"b".repeat(64),turn_input:None,pending:Some(pending.clone()),parallel_pending:Default::default(),interrupted_pending:None,native_terminal:None,context_refresh:None,native_history_retirement:None,tool_operator_request:Some(ToolOperatorContinuity{request:request.clone(),payload_artifact_bindings:vec![selected_ref.clone()],request_artifact:template.artifact.clone(),operator_interface:InterfaceId::new("world_client_test"),phase:ToolOperatorPhase::AwaitingOperator})};
        request.validate_pending(&native).unwrap();
        let resume:ToolResultArtifact=serde_json::from_value(serde_json::json!({"label":format!("{}.result",request.resume.tool),"tool":request.resume.tool,"correlation_id":request.resume.correlation_id,"ok":true,"body":{"request_id":request.request_id,"original_result":{"label":format!("{}.result",request.original.tool),"tool":request.original.tool,"correlation_id":request.call_id,"ok":true,"body":{"effect_performed":true,"upload":upload}}}})).unwrap();
        assert!(native.tool_operator_request.as_ref().unwrap().resume_result(&resume,&input).is_err());
        let mut unbound=input.clone();unbound.metadata.as_mut().unwrap().artifact_binding_refs.clear();
        assert!(native.tool_operator_request.as_mut().unwrap().admit(&unbound,config).is_err());
        assert_eq!(native.pending,Some(pending.clone()));
        let mut changed_requirement=input.clone();changed_requirement.metadata.as_mut().unwrap().required_verified_artifacts[0].params.as_object_mut().unwrap().remove("offered_choices");
        assert!(native.tool_operator_request.as_mut().unwrap().admit(&changed_requirement,config).is_err());
        native.tool_operator_request.as_mut().unwrap().admit(&input,config).unwrap();
        let restored:CodexContinuityState=serde_json::from_slice(&serde_json::to_vec(&native).unwrap()).unwrap();
        assert_eq!(restored.pending,Some(pending));
        assert!(matches!(&restored.tool_operator_request.as_ref().unwrap().phase,
            ToolOperatorPhase::AwaitingResume { operator_artifact, selected_persistence:Some(OperatorPermissionPersistence::Always), .. } if operator_artifact==&input.artifact));
        assert_eq!(input.read_artifact_payload().unwrap(),serde_json::to_vec(&response).unwrap());
        request.validate_pending(&restored).unwrap();
        input.metadata.as_mut().unwrap().checkpoint_binding_policy.as_mut().unwrap().inbound.interface=InterfaceId::new("system_browser_test");
        let complete=restored.tool_operator_request.as_ref().unwrap().resume_result(&resume,&input).unwrap();
        assert_eq!(complete.tool,request.original.tool);assert_eq!(complete.correlation_id.as_deref(),Some("upload-1"));
        input.metadata.as_mut().unwrap().artifact_binding_refs[0].content=ContentHash::of_bytes(b"another file");
        assert!(restored.tool_operator_request.as_ref().unwrap().resume_result(&resume,&input).is_err());
    }
}
