use super::*;

pub struct CodexAppServerInterfaceDriver {
    pub(crate) life_context: std::sync::OnceLock<octopus_interface_transport::InterfaceLiftContext>,
    pub(crate) client: Mutex<AppServerClient>,
    pub(crate) transport_state: Mutex<BTreeMap<TransportStateScope, TransportState>>,
    pub(crate) login_states: Mutex<BTreeMap<String, ProviderLoginState>>,
    pub(crate) pending_server_requests: PendingServerRequests,
    pub(crate) next_request_id: Arc<std::sync::atomic::AtomicI64>,
    pub(crate) interface_private_config: serde_json::Map<String, serde_json::Value>,
    pub(crate) secret_store: Option<Arc<dyn ScopedSecretStore>>,
    pub(crate) auth_manager: Option<Arc<AuthManager>>,
    pub(crate) auth_store_home: Option<PathBuf>,
    pub(crate) connected_auth_revision: Option<u64>,
}

pub(crate) fn auth_manager_revision(manager: &AuthManager) -> u64 {
    let receiver = manager.auth_change_receiver();
    *receiver.borrow()
}

impl CodexAppServerInterfaceDriver {
    pub fn new(client: AppServerClient) -> Self {
        Self::new_with_config(client, None)
    }

    pub fn new_with_config(
        client: AppServerClient,
        interface_driver_config: Option<&InterfaceDriverConfig>,
    ) -> Self {
        Self::new_with_config_and_boundaries(client, interface_driver_config, None, None, None)
    }

    #[cfg(test)]
    pub(crate) fn new_with_config_and_secret_store(
        client: AppServerClient,
        interface_driver_config: Option<&InterfaceDriverConfig>,
        secret_store: Arc<dyn ScopedSecretStore>,
    ) -> Self {
        Self::new_with_config_and_boundaries(
            client,
            interface_driver_config,
            Some(secret_store),
            None,
            None,
        )
    }

    pub(crate) fn new_with_config_and_auth_boundaries(
        client: AppServerClient,
        interface_driver_config: Option<&InterfaceDriverConfig>,
        secret_store: Option<Arc<dyn ScopedSecretStore>>,
        auth_manager: Arc<AuthManager>,
        auth_store_home: PathBuf,
    ) -> Self {
        Self::new_with_config_and_boundaries(
            client,
            interface_driver_config,
            secret_store,
            Some(auth_manager),
            Some(auth_store_home),
        )
    }

    pub(crate) fn new_with_config_and_boundaries(
        client: AppServerClient,
        interface_driver_config: Option<&InterfaceDriverConfig>,
        secret_store: Option<Arc<dyn ScopedSecretStore>>,
        auth_manager: Option<Arc<AuthManager>>,
        auth_store_home: Option<PathBuf>,
    ) -> Self {
        let interface_private_config = interface_driver_config
            .map(|config| config.private_config.clone())
            .unwrap_or_default();
        let connected_auth_revision = auth_manager
            .as_ref()
            .map(|manager| auth_manager_revision(manager));
        Self {
            life_context: std::sync::OnceLock::new(),
            client: Mutex::new(client),
            transport_state: Mutex::new(BTreeMap::new()),
            login_states: Mutex::new(BTreeMap::new()),
            pending_server_requests: Arc::new(Mutex::new(BTreeMap::new())),
            next_request_id: Arc::new(std::sync::atomic::AtomicI64::new(1)),
            interface_private_config,
            secret_store,
            auth_manager,
            auth_store_home,
            connected_auth_revision,
        }
    }

    pub async fn shutdown(self) -> std::io::Result<()> {
        self.client.into_inner().shutdown().await
    }

    pub(crate) async fn is_strictly_quiescent(&self) -> bool {
        if !self.pending_server_requests.lock().await.is_empty() {
            return false;
        }
        !self
            .login_states
            .lock()
            .await
            .values()
            .any(|state| matches!(state, ProviderLoginState::Pending { .. }))
    }

    pub(crate) fn connected_auth_revision(&self) -> Option<u64> {
        self.connected_auth_revision
    }

    pub(crate) fn attach_life_context(&self, ctx: octopus_interface_transport::InterfaceLiftContext) -> Result<(), String> {
        self.life_context.set(ctx).map_err(|_| "provider life context was already attached".into())
    }

    pub(crate) async fn wait_for_peer_artifact(
        &self, record: &octopus_run_record::RunRecord, call_id: &str, timeout_ms: u64,
        interrupted: &std::sync::atomic::AtomicBool,
    ) -> Result<crate::peer_reader::PeerWait, InterfaceDriverError> {
        let ctx = self.life_context.get().ok_or_else(|| InterfaceDriverError::FailedClosed("peer wait has no declared interface life reader".into()))?;
        crate::peer_reader::wait(ctx, &record.id, call_id, timeout_ms, interrupted).await.map_err(InterfaceDriverError::FailedClosed)
    }

    pub(crate) fn mint_request_id(&self) -> RequestId {
        RequestId::Integer(
            self.next_request_id
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        )
    }

    pub(crate) async fn acquire_transport_thread(
        &self,
        scope: &TransportStateScope,
        acquisition: ThreadAcquisition<'_>,
    ) -> Result<(String, bool, bool), InterfaceDriverError> {
        let ThreadAcquisition {
            recovered,
            retired_history,
            require_exact_resume,
            context_generation,
            input_changes_context,
            model,
            bound_instructions,
            native_affordances,
            static_context_fingerprint,
            session_context_available,
        } = acquisition;
        let covered_context_generation = context_generation
            .checked_sub(u64::from(input_changes_context))
            .ok_or_else(|| InterfaceDriverError::FailedClosed(
                "Codex invocation context generation has no covered predecessor".to_string(),
            ))?;
        if require_exact_resume && recovered.is_none() {
            return Err(InterfaceDriverError::FailedClosed(
                "Codex operation requires exact existing thread continuity".to_string(),
            ));
        }
        let fingerprint = thread_shape_fingerprint(
            model.as_deref(),
            bound_instructions,
            native_affordances,
            static_context_fingerprint,
        )?;
        if let Some(proof) = retired_history {
            let source = recovered.ok_or_else(|| InterfaceDriverError::FailedClosed("native history retirement lost its source continuity".into()))?;
            proof.validate(source)?;
            if require_exact_resume || self.private_thread_cold_start(Some(source))? != PrivateThreadColdStart::ResumeCompatible
                || self.has_pending_request(scope).await
            {
                return Err(InterfaceDriverError::FailedClosed("native history retirement requires declared resume_compatible and no current native wait".into()));
            }
            validate_recovered_reseed_coordinates(source, context_generation, session_context_available)?;
            let transport = self.transport_state.lock().await;
            if let Some(cached) = transport.get(scope)
                && (cached.thread_id.as_deref() != Some(source.thread_id.as_str()) || cached.context_generation > context_generation)
            {
                return Err(InterfaceDriverError::FailedClosed("native history retirement conflicts with the current transport".into()));
            }
        }
        let recovered = if retired_history.is_some() { None } else if let Some(previous) = recovered
            && !require_exact_resume
            && self.private_thread_cold_start(Some(previous))? == PrivateThreadColdStart::ResumeCompatible
            && (previous.thread_shape_fingerprint != fingerprint
                || previous.static_context_fingerprint != static_context_fingerprint)
        {
            validate_recovered_thread_coordinates(previous, &previous.thread_shape_fingerprint,
                covered_context_generation, &previous.static_context_fingerprint)?;
            validate_recovered_reseed_coordinates(previous, context_generation, session_context_available)?;
            let state = self.transport_state.lock().await;
            if let Some(cached) = state.get(scope)
                && cached.thread_id.is_some()
                && (cached.thread_id.as_deref() != Some(previous.thread_id.as_str())
                    || cached.thread_shape_fingerprint.as_deref() != Some(previous.thread_shape_fingerprint.as_str())
                    || cached.static_context_fingerprint.as_deref() != Some(previous.static_context_fingerprint.as_str())
                    || cached.context_generation > previous.context_generation)
            {
                return Err(InterfaceDriverError::FailedClosed("admitted instruction/tool change conflicts with the live provider history".into()));
            }
            None
        } else { recovered };
        if let Some(recovered) = recovered {
            {
                let mut transport_state = self.transport_state.lock().await;
                let state = transport_state.entry(scope.clone()).or_default();
                match (&state.thread_id, &state.thread_shape_fingerprint) {
                    (Some(thread_id), Some(shape)) => {
                        validate_recovered_thread_coordinates(
                            recovered,
                            &fingerprint,
                            covered_context_generation,
                            static_context_fingerprint,
                        )?;
                        if recovered.context_generation < state.context_generation {
                            return Err(InterfaceDriverError::FailedClosed(
                                "Codex continuity regressed behind the live transport context generation"
                                    .to_string(),
                            ));
                        }
                        if thread_id != &recovered.thread_id
                            || shape != &fingerprint
                            || state.static_context_fingerprint.as_deref()
                                != Some(static_context_fingerprint)
                        {
                            return Err(InterfaceDriverError::FailedClosed(
                                "Codex continuity conflicts with the live transport thread for this run"
                                    .to_string(),
                            ));
                        }
                        state.context_generation = context_generation;
                        let first_turn_of_run = !state.hooks_catalog_emitted;
                        state.hooks_catalog_emitted = true;
                        return Ok((thread_id.clone(), false, first_turn_of_run));
                    }
                    (None, None) => {}
                    _ => {
                        return Err(InterfaceDriverError::FailedClosed(
                            "Codex continuity conflicts with the live transport thread for this run"
                                .to_string(),
                        ));
                    }
                }
            }
            match if require_exact_resume {
                PrivateThreadColdStart::ResumeExact
            } else {
                self.private_thread_cold_start(Some(recovered))?
            } {
                PrivateThreadColdStart::ResumeExact | PrivateThreadColdStart::ResumeCompatible => {
                    validate_recovered_thread_coordinates(
                        recovered,
                        &fingerprint,
                        covered_context_generation,
                        static_context_fingerprint,
                    )?;
                    let resume = ClientRequest::ThreadResume {
                        request_id: self.mint_request_id(),
                        params: ThreadResumeParams {
                            thread_id: recovered.thread_id.clone(),
                            model: model.clone(),
                            service_tier: self.bound_service_tier()?,
                            config: Some(self.native_tool_thread_config()?),
                            developer_instructions: Some(bound_instructions.to_string()),
                            sandbox: Some(self.native_thread_sandbox()?),
                            exclude_turns: true,
                            ..ThreadResumeParams::default()
                        },
                    };
                    let resumed: ThreadResumeResponse = {
                        let client = self.client.lock().await;
                        client
                            .request_typed::<ThreadResumeResponse>(resume)
                            .await
                            .map_err(typed_error)?
                    };
                    validate_native_service_tier_response(
                        self.bound_service_tier()?.as_ref().and_then(Option::as_deref),
                        resumed.service_tier.as_deref(),
                    )?;
                    if resumed.thread.id != recovered.thread_id {
                        return Err(InterfaceDriverError::FailedClosed(format!(
                            "thread/resume returned thread '{}', expected '{}'",
                            resumed.thread.id, recovered.thread_id
                        )));
                    }
                    let resumed_rollout_path = resumed.thread.path.clone();
                    let read = ClientRequest::ThreadRead {
                        request_id: self.mint_request_id(),
                        params: ThreadReadParams {
                            thread_id: recovered.thread_id.clone(),
                            include_turns: false,
                        },
                    };
                    let read: ThreadReadResponse = {
                        let client = self.client.lock().await;
                        client
                            .request_typed::<ThreadReadResponse>(read)
                            .await
                            .map_err(typed_error)?
                    };
                    if read.thread.id != recovered.thread_id {
                        return Err(InterfaceDriverError::FailedClosed(format!(
                            "thread/read returned thread '{}', expected '{}'",
                            read.thread.id, recovered.thread_id
                        )));
                    }
                    let rollout_path = match (resumed_rollout_path, read.thread.path) {
                        (Some(resumed), Some(read)) if resumed != read => {
                            return Err(InterfaceDriverError::FailedClosed(format!(
                                "thread/resume rollout path '{}' differs from thread/read path '{}'",
                                resumed.display(),
                                read.display()
                            )));
                        }
                        (Some(path), _) | (_, Some(path)) => Some(path),
                        (None, None) => None,
                    };
                    let mut transport_state = self.transport_state.lock().await;
                    let state = transport_state.entry(scope.clone()).or_default();
                    state.context_generation = context_generation;
                    state.thread_id = Some(recovered.thread_id.clone());
                    state.thread_shape_fingerprint = Some(fingerprint);
                    state.static_context_fingerprint = Some(static_context_fingerprint.to_string());
                    state.rollout_path = rollout_path;
                    let first_turn_of_run = !state.hooks_catalog_emitted;
                    state.hooks_catalog_emitted = true;
                    return Ok((recovered.thread_id.clone(), false, first_turn_of_run));
                }
                PrivateThreadColdStart::ReseedFromSessionContext => {
                    validate_recovered_reseed_coordinates(
                        recovered,
                        context_generation,
                        session_context_available,
                    )?;
                }
            }
        }
        let mut transport_state = self.transport_state.lock().await;
        let state = transport_state.entry(scope.clone()).or_default();
        if retired_history.is_none()
            && let ThreadAction::Reuse { thread_id } = plan_thread_action(state, &fingerprint) {
            state.context_generation = context_generation;
            state.static_context_fingerprint = Some(static_context_fingerprint.to_string());
            let first_turn_of_run = !state.hooks_catalog_emitted;
            state.hooks_catalog_emitted = true;
            return Ok((thread_id, false, first_turn_of_run));
        }
        drop(transport_state);
        let mut params = isolated_thread_start_params(
            model,
            bound_instructions.to_string(),
            native_affordances.to_vec(),
        );
        params.sandbox = Some(self.native_thread_sandbox()?);
        params.config = Some(self.native_tool_thread_config()?);
        params.service_tier = self.bound_service_tier()?;
        let request = ClientRequest::ThreadStart {
            request_id: self.mint_request_id(),
            params,
        };
        let response: ThreadStartResponse = {
            let client = self.client.lock().await;
            client
                .request_typed::<ThreadStartResponse>(request)
                .await
                .map_err(typed_error)?
        };
        validate_native_service_tier_response(
            self.bound_service_tier()?.as_ref().and_then(Option::as_deref),
            response.service_tier.as_deref(),
        )?;
        let rollout_path = response.thread.path.clone();
        let thread_id = response.thread.id;
        let mut transport_state = self.transport_state.lock().await;
        let state = transport_state.entry(scope.clone()).or_default();
        state.context_generation = context_generation;
        state.thread_id = Some(thread_id.clone());
        state.thread_shape_fingerprint = Some(fingerprint);
        state.static_context_fingerprint = Some(static_context_fingerprint.to_string());
        state.rollout_path = rollout_path;
        let first_turn_of_run = !state.hooks_catalog_emitted;
        state.hooks_catalog_emitted = true;
        Ok((thread_id, true, first_turn_of_run))
    }

    pub(crate) fn private_thread_cold_start(
        &self,
        recovered: Option<&CodexContinuityState>,
    ) -> Result<PrivateThreadColdStart, InterfaceDriverError> {
        let configured = match self
            .interface_private_config
            .get("private_thread_cold_start")
        {
            None => PrivateThreadColdStart::ResumeExact,
            Some(serde_json::Value::String(value)) if value == "resume_exact" => {
                PrivateThreadColdStart::ResumeExact
            }
            Some(serde_json::Value::String(value)) if value == "resume_compatible" => {
                PrivateThreadColdStart::ResumeCompatible
            }
            Some(serde_json::Value::String(value)) if value == "reseed_from_session_context" => {
                PrivateThreadColdStart::ReseedFromSessionContext
            }
            Some(_) => {
                return Err(InterfaceDriverError::FailedClosed(
                    "interface_driver_config.private_config.private_thread_cold_start must be 'resume_exact', 'resume_compatible' or 'reseed_from_session_context'"
                        .to_string(),
                ));
            }
        };
        Ok(
            if recovered.is_some_and(codex_continuity_waits_for_result) {
                PrivateThreadColdStart::ResumeExact
            } else {
                configured
            },
        )
    }

    pub(crate) fn native_tool_namespace(&self) -> Result<NativeToolNamespace, InterfaceDriverError> {
        let value = self
            .interface_private_config
            .get("native_tool_namespace")
            .cloned()
            .ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "codex_app_server descriptor declares no native_tool_namespace".to_string(),
                )
            })?;
        serde_json::from_value::<NativeToolNamespace>(value)
            .map_err(|error| {
                InterfaceDriverError::Protocol(format!(
                    "interface_driver_config.private_config.native_tool_namespace is malformed: {error}"
                ))
            })?
            .validate()
    }

    pub(crate) fn native_thread_sandbox(&self) -> Result<SandboxMode, InterfaceDriverError> {
        match self.interface_private_config.get("native_thread_sandbox") {
            None => Ok(SandboxMode::ReadOnly),
            Some(value) => serde_json::from_value(value.clone()).map_err(|error| {
                InterfaceDriverError::Protocol(format!(
                    "interface_driver_config.private_config.native_thread_sandbox is malformed: {error}"
                ))
            }),
        }
    }

    pub(crate) fn operation_approval_policy(&self) -> Result<&'static str, InterfaceDriverError> {
        match self.interface_private_config.get("approval_policy").and_then(serde_json::Value::as_str) {
            Some("on-request") => Ok("on-request"),
            Some("never") => Ok("never"),
            _ => Err(InterfaceDriverError::FailedClosed(
                "interface_driver_config.private_config.approval_policy must be 'on-request' or 'never'".into(),
            )),
        }
    }

    pub(crate) fn native_web_search_mode(&self) -> Result<WebSearchMode, InterfaceDriverError> {
        match self.interface_private_config.get("web_search") {
            None => Ok(WebSearchMode::Disabled),
            Some(value) => serde_json::from_value(value.clone()).map_err(|error| {
                InterfaceDriverError::Protocol(format!(
                    "interface_driver_config.private_config.web_search is malformed: {error}"
                ))
            }),
        }
    }

    pub(crate) fn native_mcp_resource_discovery(&self) -> Result<bool, InterfaceDriverError> {
        match self
            .interface_private_config
            .get("native_mcp_resource_discovery")
        {
            None => Ok(false),
            Some(value) => serde_json::from_value(value.clone()).map_err(|error| {
                InterfaceDriverError::Protocol(format!(
                    "interface_driver_config.private_config.native_mcp_resource_discovery is malformed: {error}"
                ))
            }),
        }
    }

    pub(crate) fn enforce_native_web_search_policy(
        &self,
        input: &InterfaceInvocationEnvelope,
    ) -> Result<(), InterfaceDriverError> {
        let mode = self.native_web_search_mode()?;
        if mode == WebSearchMode::Disabled {
            return Ok(());
        }
        let values = input
            .interface_policy()?
            .and_then(|policy| policy.get("web_search_modes"))
            .ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "Codex native web search requires formed interface policy web_search_modes"
                        .to_string(),
                )
            })?;
        let allowed: Vec<WebSearchMode> = serde_json::from_value(values.clone()).map_err(|error| {
            InterfaceDriverError::FailedClosed(format!(
                "Codex formed web_search_modes is malformed: {error}"
            ))
        })?;
        if !allowed.contains(&mode) {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "Codex web_search '{mode}' is outside its freshly formed interface policy"
            )));
        }
        Ok(())
    }

    pub(crate) fn native_tool_thread_config(
        &self,
    ) -> Result<HashMap<String, serde_json::Value>, InterfaceDriverError> {
        Ok(HashMap::from([
            ("features.view_image".to_string(), serde_json::json!(false)),
            (
                "features.code_mode.direct_only_tool_namespaces".to_string(),
                serde_json::json!([]),
            ),
            (
                "web_search".to_string(),
                serde_json::json!(self.native_web_search_mode()?),
            ),
        ]))
    }

    pub(crate) fn resolve_private_config_string(
        &self,
        key: &str,
    ) -> Result<Option<String>, InterfaceDriverError> {
        non_empty_string_field(
            self.interface_private_config.get(key),
            &format!("interface_driver_config.private_config.{key}"),
        )
    }

    pub(crate) fn bound_service_tier(&self) -> Result<Option<Option<String>>, InterfaceDriverError> {
        let tier = self.resolve_private_config_string("service_tier")?;
        if tier.as_ref().is_some_and(|tier| tier.trim() != tier || tier.len() > 80 || tier.chars().any(char::is_control)) {
            return Err(InterfaceDriverError::Protocol("provider service_tier must be a trimmed native tier id".into()));
        }
        Ok(tier.map(Some))
    }

    pub(crate) async fn validate_native_service_tier(&self, model: &str) -> Result<(), InterfaceDriverError> {
        let Some(Some(tier)) = self.bound_service_tier()? else { return Ok(()); };
        if tier == "default" { return Ok(()); }
        let mut cursor = None;
        let mut seen = BTreeSet::new();
        loop {
            let response = self.client.lock().await.request_typed::<ModelListResponse>(ClientRequest::ModelList {
                request_id: self.mint_request_id(),
                params: ModelListParams { cursor, limit: None, include_hidden: Some(true) },
            }).await.map_err(typed_error)?;
            if let Some(selected) = response.data.iter().find(|entry| entry.model == model) {
                return validate_model_service_tier(selected, &tier);
            }
            match response.next_cursor {
                Some(next) if !next.is_empty() && seen.insert(next.clone()) && seen.len() < 100 => cursor = Some(next),
                None => return Err(InterfaceDriverError::FailedClosed(format!("native model catalogue does not advertise selected model '{model}' for service tier '{tier}'"))),
                _ => return Err(InterfaceDriverError::Protocol("native model catalogue returned an invalid service-tier cursor".into())),
            }
        }
    }

    pub(crate) fn context_output_labels(&self) -> Result<BTreeSet<String>, InterfaceDriverError> {
        self.private_config_string_set("context_output_labels")
    }

    pub(crate) fn session_snapshot_labels(&self) -> Result<BTreeSet<String>, InterfaceDriverError> {
        self.private_config_string_set("session_snapshot_labels")
    }

    pub(crate) fn model_result_projection(&self) -> Result<ModelResultProjection, InterfaceDriverError> {
        ModelResultProjection::from_private_config(&self.interface_private_config)
    }

    pub(crate) fn operator_continuity(&self) -> Result<OperatorContinuityConfig, InterfaceDriverError> {
        operator_continuity_from_private_config(&self.interface_private_config)
    }

    pub(crate) fn client_projection_output_labels(
        &self,
        context_output_labels: &BTreeSet<String>,
        session_snapshot_labels: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, InterfaceDriverError> {
        validated_client_projection_output_labels(
            &self.interface_private_config,
            context_output_labels,
            session_snapshot_labels,
        )
    }

    pub(crate) fn private_config_string_set(
        &self,
        key: &str,
    ) -> Result<BTreeSet<String>, InterfaceDriverError> {
        private_config_string_set(&self.interface_private_config, key)
    }
}
