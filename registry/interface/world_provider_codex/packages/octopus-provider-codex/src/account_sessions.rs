use super::*;
use octopus_appserver_proto::{AccountSessionsListParams, AccountSessionsResponse};

pub(crate) const PROVIDER_ACCOUNT_SESSIONS_READ_LABEL: &str = "provider.account.sessions.read";
pub(crate) const PROVIDER_ACCOUNT_SESSIONS_LABEL: &str = "provider.account.sessions";

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccountSessionsReadRequest {
    pub(crate) refresh_workspace_metadata: bool,
}

impl CodexAppServerInterfaceDriver {
    pub(crate) async fn handle_account_sessions_read(
        &self,
        payload: &[u8],
        sink: &TurnSink<'_>,
    ) -> Result<(), InterfaceDriverError> {
        let request: AccountSessionsReadRequest = match serde_json::from_slice(payload) {
            Ok(request) => request,
            Err(_) => {
                return sink.send(provider_operation_error(
                    PROVIDER_ACCOUNT_SESSIONS_READ_LABEL,
                    "operation_payload_malformed",
                    "protocol",
                    false,
                    "account sessions read payload must contain only a boolean refresh_workspace_metadata field",
                ));
            }
        };
        let refresh_workspace_metadata = request.refresh_workspace_metadata;
        let request = ClientRequest::AccountSessionsList {
            request_id: self.mint_request_id(),
            params: AccountSessionsListParams {
                refresh_workspace_metadata,
            },
        };
        if refresh_workspace_metadata {
            sink.admit_app_server_dispatch(&request, None).await?;
        }
        let response = {
            let client = self.client.lock().await;
            client.request_typed::<AccountSessionsResponse>(request).await
        };
        match response {
            Ok(response) => sink.send(serialized_client_output(
                PROVIDER_ACCOUNT_SESSIONS_LABEL,
                &response,
            )?),
            Err(error) => sink.send(upstream_operation_error(
                PROVIDER_ACCOUNT_SESSIONS_READ_LABEL,
                &error,
                "app-server account session list failed",
            )),
        }
    }
}
