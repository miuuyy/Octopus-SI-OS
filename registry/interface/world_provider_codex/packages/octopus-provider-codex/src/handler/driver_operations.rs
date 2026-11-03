use super::*;

impl CodexAppServerInterfaceDriver {
    pub fn interface_driver_ref(&self) -> &str {
        CODEX_APP_SERVER_INTERFACE_DRIVER_REF
    }

    pub(crate) fn provider_operations(&self) -> Result<Option<ProviderOperations>, InterfaceDriverError> {
        provider_operations_from_private_config(&self.interface_private_config)
    }

    pub(crate) fn configured_provider_operation(
        &self,
        label: &str,
    ) -> Result<Option<ProviderOperation>, InterfaceDriverError> {
        configured_provider_operation(&self.interface_private_config, label)
    }

    pub(crate) fn configured_thread_compact_request(&self, label: &str) -> Result<bool, InterfaceDriverError> {
        Ok(self
            .provider_operations()?
            .and_then(|operations| operations.thread_compact_start)
            .is_some_and(|binding| binding.request_label == label))
    }

    pub(crate) async fn handle_provider_operation(
        &self,
        operation: ProviderOperation,
        exact_interface: &InterfaceId,
        payload: &[u8],
        sink: &TurnSink<'_>,
    ) -> Result<(), InterfaceDriverError> {
        self.emit_ready_account_notifications(sink).await?;
        match operation {
            ProviderOperation::AccountLoginStart => {
                self.handle_account_login_start(exact_interface, payload, sink)
                    .await
            }
            ProviderOperation::AccountLoginCancel => {
                self.handle_account_login_cancel(payload, sink).await
            }
            ProviderOperation::AccountLoginStatusRead => {
                self.handle_account_login_status(payload, sink).await
            }
            ProviderOperation::AccountLogout => self.handle_account_logout(payload, sink).await,
            ProviderOperation::AccountRead => self.handle_account_read(payload, sink).await,
            ProviderOperation::AccountRateLimitsRead => {
                self.handle_account_rate_limits(payload, sink).await
            }
            ProviderOperation::AccountRateLimitResetConsume => {
                self.handle_account_rate_limit_reset_consume(payload, sink)
                    .await
            }
            ProviderOperation::AccountUsageRead => self.handle_account_usage(payload, sink).await,
            ProviderOperation::AccountWorkspaceMessagesRead => {
                self.handle_account_workspace_messages(payload, sink).await
            }
            ProviderOperation::CapabilitiesRead => {
                self.handle_provider_capabilities(payload, sink).await
            }
            ProviderOperation::AccountSessionsRead => {
                self.handle_account_sessions_read(payload, sink).await
            }
        }
    }

    pub(crate) async fn handle_account_login_start(
        &self,
        exact_interface: &InterfaceId,
        payload: &[u8],
        sink: &TurnSink<'_>,
    ) -> Result<(), InterfaceDriverError> {
        let request: AccountLoginStartRequest = match serde_json::from_slice(payload) {
            Ok(request) => request,
            Err(_) => {
                return sink.send(provider_operation_error(
                    PROVIDER_ACCOUNT_LOGIN_START_LABEL,
                    "operation_payload_malformed",
                    "protocol",
                    false,
                    "account login start payload does not match the declared provider vocabulary",
                ));
            }
        };
        let (method, secret_handle, params) = match request {
            AccountLoginStartRequest::ApiKey { secret_handle } => {
                let Some(secret_store) = self.secret_store.as_ref() else {
                    return sink.send(provider_operation_error(
                        PROVIDER_ACCOUNT_LOGIN_START_LABEL,
                        "secret_store_unavailable",
                        "unsupported",
                        false,
                        "this provider life has no scoped secret-store boundary",
                    ));
                };
                let secret = match secret_store.resolve(exact_interface, &secret_handle) {
                    Ok(secret) => secret,
                    Err(error) => {
                        return sink.send(scoped_secret_operation_error(error));
                    }
                };
                (
                    "api_key",
                    Some(secret_handle),
                    LoginAccountParams::ApiKey {
                        api_key: secret.expose().to_string(),
                    },
                )
            }
            AccountLoginStartRequest::ChatgptBrowser {
                codex_streamlined_login,
            } => (
                "chatgpt_browser",
                None,
                LoginAccountParams::Chatgpt {
                    codex_streamlined_login,
                },
            ),
            AccountLoginStartRequest::ChatgptDeviceCode => (
                "chatgpt_device_code",
                None,
                LoginAccountParams::ChatgptDeviceCode,
            ),
        };
        let request = ClientRequest::LoginAccount {
            request_id: self.mint_request_id(),
            params,
        };
        let credential_handle_fingerprint = secret_handle
            .as_deref()
            .map(|handle| blake3::hash(handle.as_bytes()).to_hex().to_string());
        sink.admit_app_server_dispatch(&request, credential_handle_fingerprint)
            .await?;
        let response = {
            let client = self.client.lock().await;
            client.request_typed::<LoginAccountResponse>(request).await
        };
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                return sink.send(upstream_operation_error(
                    PROVIDER_ACCOUNT_LOGIN_START_LABEL,
                    &error,
                    "app-server account login start failed",
                ));
            }
        };
        let value = match response {
            LoginAccountResponse::ApiKey {} => serde_json::json!({
                "operation": PROVIDER_ACCOUNT_LOGIN_START_LABEL,
                "method": method,
                "status": "completed",
                "credential": {
                    "secret_handle": secret_handle,
                    "redacted": true,
                },
            }),
            LoginAccountResponse::Chatgpt { login_id, auth_url } => {
                self.login_states.lock().await.insert(
                    login_id.clone(),
                    ProviderLoginState::Pending {
                        method: "chatgpt_browser",
                    },
                );
                serde_json::json!({
                    "operation": PROVIDER_ACCOUNT_LOGIN_START_LABEL,
                    "method": method,
                    "status": "pending",
                    "login_id": login_id,
                    "auth_url": auth_url,
                })
            }
            LoginAccountResponse::ChatgptDeviceCode {
                login_id,
                verification_url,
                user_code,
            } => {
                self.login_states.lock().await.insert(
                    login_id.clone(),
                    ProviderLoginState::Pending {
                        method: "chatgpt_device_code",
                    },
                );
                serde_json::json!({
                    "operation": PROVIDER_ACCOUNT_LOGIN_START_LABEL,
                    "method": method,
                    "status": "pending",
                    "login_id": login_id,
                    "verification_url": verification_url,
                    "user_code": user_code,
                })
            }
            LoginAccountResponse::ChatgptAuthTokens {} => {
                return sink.send(provider_operation_error(
                    PROVIDER_ACCOUNT_LOGIN_START_LABEL,
                    "upstream_response_unsupported",
                    "unsupported",
                    false,
                    "external ChatGPT token login is not exposed by this provider artifact vocabulary",
                ));
            }
        };
        sink.send(CodexTurnOutput::client(
            PROVIDER_ACCOUNT_LOGIN_STARTED_LABEL,
            value,
        ))
    }

    pub(crate) async fn handle_account_login_cancel(
        &self,
        payload: &[u8],
        sink: &TurnSink<'_>,
    ) -> Result<(), InterfaceDriverError> {
        let Some(request) =
            parse_login_id_request(payload, PROVIDER_ACCOUNT_LOGIN_CANCEL_LABEL, sink)?
        else {
            return Ok(());
        };
        let client_request = ClientRequest::CancelLoginAccount {
            request_id: self.mint_request_id(),
            params: CancelLoginAccountParams {
                login_id: request.login_id.clone(),
            },
        };
        sink.admit_app_server_dispatch(&client_request, None)
            .await?;
        let response = {
            let client = self.client.lock().await;
            client
                .request_typed::<CancelLoginAccountResponse>(client_request)
                .await
        };
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                return sink.send(upstream_operation_error(
                    PROVIDER_ACCOUNT_LOGIN_CANCEL_LABEL,
                    &error,
                    "app-server account login cancel failed",
                ));
            }
        };
        let status = serde_json::to_value(&response.status)
            .ok()
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| "unknown".to_string());
        if status == "canceled" {
            self.login_states
                .lock()
                .await
                .insert(request.login_id.clone(), ProviderLoginState::Cancelled);
        }
        sink.send(CodexTurnOutput::client(
            PROVIDER_ACCOUNT_LOGIN_CANCELLED_LABEL,
            serde_json::json!({
                "operation": PROVIDER_ACCOUNT_LOGIN_CANCEL_LABEL,
                "login_id": request.login_id,
                "status": status,
            }),
        ))
    }

    pub(crate) async fn handle_account_login_status(
        &self,
        payload: &[u8],
        sink: &TurnSink<'_>,
    ) -> Result<(), InterfaceDriverError> {
        let Some(request) =
            parse_login_id_request(payload, PROVIDER_ACCOUNT_LOGIN_STATUS_READ_LABEL, sink)?
        else {
            return Ok(());
        };
        let state = self
            .login_states
            .lock()
            .await
            .get(&request.login_id)
            .cloned();
        let Some(state) = state else {
            return sink.send(provider_operation_error(
                PROVIDER_ACCOUNT_LOGIN_STATUS_READ_LABEL,
                "login_status_unavailable",
                "unsupported",
                false,
                "upstream app-server has no login-status read method and this provider life did not observe that login id",
            ));
        };
        sink.send(CodexTurnOutput::client(
            PROVIDER_ACCOUNT_LOGIN_STATUS_LABEL,
            serde_json::json!({
                "operation": PROVIDER_ACCOUNT_LOGIN_STATUS_READ_LABEL,
                "login_id": request.login_id,
                "state": state,
            }),
        ))
    }

    pub(crate) async fn handle_account_logout(
        &self,
        payload: &[u8],
        sink: &TurnSink<'_>,
    ) -> Result<(), InterfaceDriverError> {
        if !parse_empty_operation(payload, PROVIDER_ACCOUNT_LOGOUT_LABEL, sink)? {
            return Ok(());
        }
        let (auth_manager, auth_store_home) = match (&self.auth_manager, &self.auth_store_home) {
            (Some(auth_manager), Some(auth_store_home)) => {
                (auth_manager.clone(), auth_store_home.clone())
            }
            _ => {
                return sink.send(provider_operation_error(
                    PROVIDER_ACCOUNT_LOGOUT_LABEL,
                    "auth_store_boundary_unavailable",
                    "unsupported",
                    false,
                    "this provider life cannot clear and restore the current auth-store boundary",
                ));
            }
        };
        let app_server_home = {
            let client = self.client.lock().await;
            client.app_server_codex_home().map(PathBuf::from)
        };
        let Some(app_server_home) = app_server_home.filter(|path| path.is_absolute()) else {
            return sink.send(provider_operation_error(
                PROVIDER_ACCOUNT_LOGOUT_LABEL,
                "app_server_auth_home_unavailable",
                "unsupported",
                false,
                "app-server did not expose a local absolute auth-store home",
            ));
        };
        if app_server_home == auth_store_home {
            return sink.send(provider_operation_error(
                PROVIDER_ACCOUNT_LOGOUT_LABEL,
                "auth_store_boundary_not_isolated",
                "unsupported",
                false,
                "app-server auth home is not isolated from the current provider auth store",
            ));
        }
        let request = ClientRequest::LogoutAccount {
            request_id: self.mint_request_id(),
            params: None,
        };
        sink.admit_app_server_dispatch(&request, None).await?;
        let response = {
            let client = self.client.lock().await;
            client.request_typed::<LogoutAccountResponse>(request).await
        };
        if let Err(error) = response {
            return sink.send(upstream_operation_error(
                PROVIDER_ACCOUNT_LOGOUT_LABEL,
                &error,
                "app-server account logout failed",
            ));
        }
        if auth_manager.logout().await.is_err() {
            return sink.send(provider_operation_error(
                PROVIDER_ACCOUNT_LOGOUT_LABEL,
                "auth_store_logout_failed",
                "auth_store",
                false,
                "the current provider auth store could not be cleared after app-server logout",
            ));
        }
        let real_auth = auth_store_home.join("auth.json");
        let curated_auth = app_server_home.join("auth.json");
        let _ = std::fs::remove_file(&curated_auth);
        if crate::config::link_auth_boundary(&real_auth, &curated_auth).is_err() {
            return sink.send(provider_operation_error(
                PROVIDER_ACCOUNT_LOGOUT_LABEL,
                "auth_store_boundary_restore_failed",
                "auth_store",
                false,
                "the isolated app-server auth link could not be restored after logout",
            ));
        }
        self.login_states.lock().await.clear();
        sink.send(CodexTurnOutput::client(
            PROVIDER_ACCOUNT_LOGGED_OUT_LABEL,
            serde_json::json!({
                "operation": PROVIDER_ACCOUNT_LOGOUT_LABEL,
                "status": "completed",
            }),
        ))
    }

    pub(crate) async fn handle_account_read(
        &self,
        payload: &[u8],
        sink: &TurnSink<'_>,
    ) -> Result<(), InterfaceDriverError> {
        let request: AccountReadRequest = match serde_json::from_slice(payload) {
            Ok(request) => request,
            Err(_) => {
                return sink.send(provider_operation_error(
                    PROVIDER_ACCOUNT_READ_LABEL,
                    "operation_payload_malformed",
                    "protocol",
                    false,
                    "account read payload must contain only a boolean refresh_token field",
                ));
            }
        };
        let refresh_token = request.refresh_token;
        let request = ClientRequest::GetAccount {
            request_id: self.mint_request_id(),
            params: GetAccountParams { refresh_token },
        };
        if refresh_token {
            sink.admit_app_server_dispatch(&request, None).await?;
        }
        let response = {
            let client = self.client.lock().await;
            client.request_typed::<GetAccountResponse>(request).await
        };
        match response {
            Ok(response) => sink.send(serialized_client_output(
                PROVIDER_ACCOUNT_SNAPSHOT_LABEL,
                &response,
            )?),
            Err(error) => sink.send(upstream_operation_error(
                PROVIDER_ACCOUNT_READ_LABEL,
                &error,
                "app-server account read failed",
            )),
        }
    }

    pub(crate) async fn handle_account_rate_limits(
        &self,
        payload: &[u8],
        sink: &TurnSink<'_>,
    ) -> Result<(), InterfaceDriverError> {
        if !parse_empty_operation(payload, PROVIDER_ACCOUNT_RATE_LIMITS_READ_LABEL, sink)? {
            return Ok(());
        }
        let request = ClientRequest::GetAccountRateLimits {
            request_id: self.mint_request_id(),
            params: None,
        };
        let response = {
            let client = self.client.lock().await;
            client
                .request_typed::<GetAccountRateLimitsResponse>(request)
                .await
        };
        match response {
            Ok(response) => sink.send(serialized_client_output(
                PROVIDER_ACCOUNT_RATE_LIMITS_LABEL,
                &response,
            )?),
            Err(error) => sink.send(upstream_operation_error(
                PROVIDER_ACCOUNT_RATE_LIMITS_READ_LABEL,
                &error,
                "app-server account rate-limit read failed",
            )),
        }
    }

    pub(crate) async fn handle_account_usage(
        &self,
        payload: &[u8],
        sink: &TurnSink<'_>,
    ) -> Result<(), InterfaceDriverError> {
        let request: AccountUsageReadRequest = match serde_json::from_slice(payload) {
            Ok(request) => request,
            Err(_) => {
                return sink.send(provider_operation_error(
                    PROVIDER_ACCOUNT_USAGE_READ_LABEL,
                    "operation_payload_malformed",
                    "protocol",
                    false,
                    "account usage read accepts only an optional non-empty thread_id",
                ));
            }
        };
        if request
            .thread_id
            .as_deref()
            .is_some_and(|thread_id| thread_id.trim().is_empty() || thread_id.trim() != thread_id)
        {
            return sink.send(provider_operation_error(
                PROVIDER_ACCOUNT_USAGE_READ_LABEL,
                "thread_id_invalid",
                "protocol",
                false,
                "account usage read thread_id must be a non-empty trimmed string",
            ));
        }
        let request = ClientRequest::GetAccountTokenUsage {
            request_id: self.mint_request_id(),
            params: GetAccountTokenUsageParams {
                thread_id: request.thread_id,
            },
        };
        let response = {
            let client = self.client.lock().await;
            client
                .request_typed::<GetAccountTokenUsageResponse>(request)
                .await
        };
        match response {
            Ok(response) => sink.send(serialized_client_output(
                PROVIDER_ACCOUNT_USAGE_LABEL,
                &response,
            )?),
            Err(error) => sink.send(upstream_operation_error(
                PROVIDER_ACCOUNT_USAGE_READ_LABEL,
                &error,
                "app-server account usage read failed",
            )),
        }
    }

    pub(crate) async fn handle_account_workspace_messages(
        &self,
        payload: &[u8],
        sink: &TurnSink<'_>,
    ) -> Result<(), InterfaceDriverError> {
        if !parse_empty_operation(
            payload,
            PROVIDER_ACCOUNT_WORKSPACE_MESSAGES_READ_LABEL,
            sink,
        )? {
            return Ok(());
        }
        let request = ClientRequest::GetWorkspaceMessages {
            request_id: self.mint_request_id(),
            params: None,
        };
        let response = {
            let client = self.client.lock().await;
            client
                .request_typed::<GetWorkspaceMessagesResponse>(request)
                .await
        };
        match response {
            Ok(response) => sink.send(serialized_client_output(
                PROVIDER_ACCOUNT_WORKSPACE_MESSAGES_LABEL,
                &response,
            )?),
            Err(error) => sink.send(upstream_operation_error(
                PROVIDER_ACCOUNT_WORKSPACE_MESSAGES_READ_LABEL,
                &error,
                "app-server workspace-message read failed",
            )),
        }
    }

    pub(crate) async fn handle_account_rate_limit_reset_consume(
        &self,
        payload: &[u8],
        sink: &TurnSink<'_>,
    ) -> Result<(), InterfaceDriverError> {
        let request: AccountRateLimitResetConsumeRequest = match serde_json::from_slice(payload) {
            Ok(request) => request,
            Err(_) => {
                return sink.send(provider_operation_error(
                    PROVIDER_ACCOUNT_RATE_LIMIT_RESET_CONSUME_LABEL,
                    "operation_payload_malformed",
                    "protocol",
                    false,
                    "rate-limit reset consume requires idempotency_key and optional credit_id",
                ));
            }
        };
        if request.idempotency_key.trim().is_empty()
            || request.idempotency_key.trim() != request.idempotency_key
            || request.credit_id.as_deref().is_some_and(|credit_id| {
                credit_id.trim().is_empty() || credit_id.trim() != credit_id
            })
        {
            return sink.send(provider_operation_error(
                PROVIDER_ACCOUNT_RATE_LIMIT_RESET_CONSUME_LABEL,
                "operation_coordinates_invalid",
                "protocol",
                false,
                "idempotency_key and optional credit_id must be non-empty trimmed strings",
            ));
        }
        let client_request = ClientRequest::ConsumeAccountRateLimitResetCredit {
            request_id: self.mint_request_id(),
            params: ConsumeAccountRateLimitResetCreditParams {
                idempotency_key: request.idempotency_key.clone(),
                credit_id: request.credit_id.clone(),
            },
        };
        sink.admit_app_server_dispatch(&client_request, None)
            .await?;
        let response = {
            let client = self.client.lock().await;
            client
                .request_typed::<ConsumeAccountRateLimitResetCreditResponse>(client_request)
                .await
        };
        match response {
            Ok(response) => sink.send(CodexTurnOutput::client(
                PROVIDER_ACCOUNT_RATE_LIMIT_RESET_CONSUMED_LABEL,
                serde_json::json!({
                    "operation": PROVIDER_ACCOUNT_RATE_LIMIT_RESET_CONSUME_LABEL,
                    "idempotency_key": request.idempotency_key,
                    "credit_id": request.credit_id,
                    "outcome": response.outcome,
                }),
            )),
            Err(error) => sink.send(upstream_operation_error(
                PROVIDER_ACCOUNT_RATE_LIMIT_RESET_CONSUME_LABEL,
                &error,
                "app-server rate-limit reset credit consume failed",
            )),
        }
    }

    pub(crate) async fn handle_provider_capabilities(
        &self,
        payload: &[u8],
        sink: &TurnSink<'_>,
    ) -> Result<(), InterfaceDriverError> {
        if !parse_empty_operation(payload, PROVIDER_CAPABILITIES_READ_LABEL, sink)? {
            return Ok(());
        }
        let request = ClientRequest::ModelProviderCapabilitiesRead {
            request_id: self.mint_request_id(),
            params: ModelProviderCapabilitiesReadParams {},
        };
        let (response, server_version, transport_kind, compatibility) = {
            let client = self.client.lock().await;
            let server_version = client.server_version().map(str::to_string);
            let transport_kind = client.transport_kind();
            let compatibility = client.compatibility().cloned();
            let response = client
                .request_typed::<ModelProviderCapabilitiesReadResponse>(request)
                .await;
            (response, server_version, transport_kind, compatibility)
        };
        match response {
            Ok(response) => {
                let mut output = serialized_client_output(PROVIDER_CAPABILITIES_LABEL, &response)?;
                insert_client_output_field(
                    &mut output,
                    "app_server",
                    serde_json::json!({
                        "transport": transport_kind,
                        "server_version": server_version,
                        "version_source": "initialize.userAgent",
                        "compatibility": compatibility,
                    }),
                );
                sink.send(output)
            }
            Err(error) => sink.send(upstream_operation_error(
                PROVIDER_CAPABILITIES_READ_LABEL,
                &error,
                "app-server provider capability read failed",
            )),
        }
    }

    pub(crate) async fn emit_ready_account_notifications(
        &self,
        sink: &TurnSink<'_>,
    ) -> Result<(), InterfaceDriverError> {
        let account_events = {
            let mut client = self.client.lock().await;
            let mut account_events = Vec::new();
            let mut deferred = Vec::new();
            for event in client.take_ready_events() {
                match event {
                    AppServerEvent::ServerNotification(
                        notification @ (ServerNotification::AccountLoginCompleted(_)
                        | ServerNotification::AccountUpdated(_)
                        | ServerNotification::AccountRateLimitsUpdated(_)),
                    ) => account_events.push(notification),
                    event => deferred.push(event),
                }
            }
            client.restore_ready_events(deferred);
            account_events
        };
        for notification in account_events {
            if let Some(output) = self.account_notification_output(notification).await? {
                sink.send(output)?;
            }
        }
        Ok(())
    }

    pub(crate) async fn account_notification_output(
        &self,
        notification: ServerNotification,
    ) -> Result<Option<CodexTurnOutput>, InterfaceDriverError> {
        match notification {
            ServerNotification::AccountLoginCompleted(notification) => {
                self.record_login_completion(&notification).await;
                Ok(Some(account_login_completed_output(&notification)))
            }
            ServerNotification::AccountUpdated(notification) => {
                Ok(Some(account_updated_output(&notification)?))
            }
            ServerNotification::AccountRateLimitsUpdated(notification) => {
                Ok(Some(serialized_client_output(
                    PROVIDER_ACCOUNT_RATE_LIMITS_UPDATED_LABEL,
                    &notification,
                )?))
            }
            _ => Ok(None),
        }
    }

    pub(crate) async fn record_login_completion(&self, notification: &AccountLoginCompletedNotification) {
        if let Some(login_id) = notification.login_id.as_ref() {
            self.login_states.lock().await.insert(
                login_id.clone(),
                ProviderLoginState::Completed {
                    success: notification.success,
                },
            );
        }
    }

    pub(crate) fn model_catalog_operation(&self) -> Result<ModelCatalogOperation, InterfaceDriverError> {
        let value = self
            .interface_private_config
            .get("model_catalog_operation")
            .cloned()
            .ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "codex_app_server descriptor declares no model_catalog_operation".to_string(),
                )
            })?;
        let operation: ModelCatalogOperation = serde_json::from_value(value).map_err(|error| {
            InterfaceDriverError::Protocol(format!(
                "codex_app_server model_catalog_operation is malformed: {error}"
            ))
        })?;
        if operation.request_label != PROVIDER_MODELS_LIST_LABEL
            || operation.output_label != PROVIDER_MODELS_CATALOG_LABEL
        {
            return Err(InterfaceDriverError::Protocol(format!(
                "codex_app_server model catalog labels must be '{PROVIDER_MODELS_LIST_LABEL}' and '{PROVIDER_MODELS_CATALOG_LABEL}'"
            )));
        }
        if operation.default_limit == 0
            || operation.max_limit == 0
            || operation.default_limit > operation.max_limit
        {
            return Err(InterfaceDriverError::Protocol(
                "codex_app_server model catalog limits are invalid".to_string(),
            ));
        }
        Ok(operation)
    }

    pub(crate) async fn emit_model_catalog(
        &self,
        payload: &[u8],
        sink: &TurnSink<'_>,
        formed_models: &BTreeSet<String>,
    ) -> Result<(), InterfaceDriverError> {
        let operation = self.model_catalog_operation()?;
        let request: ModelCatalogArtifactRequest =
            serde_json::from_slice(payload).map_err(|error| {
                InterfaceDriverError::Protocol(format!(
                    "provider.models.list payload is malformed: {error}"
                ))
            })?;
        let cursor = match request.cursor {
            Some(cursor) if !cursor.is_empty() && cursor.trim() == cursor => Some(cursor),
            Some(_) => {
                return Err(InterfaceDriverError::Protocol(
                    "provider.models.list cursor must be non-empty and trimmed".to_string(),
                ));
            }
            None => None,
        };
        let limit = request.limit.unwrap_or(operation.default_limit);
        if limit == 0 || limit > operation.max_limit {
            return Err(InterfaceDriverError::Protocol(format!(
                "provider.models.list limit must be within 1..={} for this interface",
                operation.max_limit
            )));
        }
        let request = ClientRequest::ModelList {
            request_id: self.mint_request_id(),
            params: ModelListParams {
                cursor,
                limit: Some(limit),
                include_hidden: Some(operation.include_hidden),
            },
        };
        let response = {
            let client = self.client.lock().await;
            client
                .request_typed::<ModelListResponse>(request)
                .await
                .map_err(typed_error)?
        };
        let models = response
            .data
            .into_iter()
            .filter(|model| catalog_model_allowed(&model.model, formed_models))
            .collect::<Vec<_>>();
        sink.send(CodexTurnOutput::client(
            PROVIDER_MODELS_CATALOG_LABEL,
            serde_json::json!({
                "label": PROVIDER_MODELS_CATALOG_LABEL,
                "models": models,
                "next_cursor": response.next_cursor,
            }),
        ))
    }
}
