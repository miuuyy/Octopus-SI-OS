use super::*;
use crate::auth::storage::FileAuthStorage;
use base64::Engine;

fn jwt(payload: serde_json::Value) -> String {
    let encode = |value: &serde_json::Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string())
    };
    format!(
        "{}.{}.sig",
        encode(&serde_json::json!({ "alg": "none" })),
        encode(&payload)
    )
}

fn chatgpt_auth_json(account_id: &str, refresh_token: &str) -> serde_json::Value {
    serde_json::json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": null,
        "tokens": {
            "id_token": jwt(serde_json::json!({
                "https://api.openai.com/auth": { "chatgpt_user_id": "user-1" }
            })),
            "access_token": "access-old",
            "refresh_token": refresh_token,
            "oauth_provenance": { "issuer": "https://auth.example.test", "client_id": "client-1" },
            "login_owner_id": "login-1",
            "account_id": account_id,
            "future_token_field": { "kept": true }
        },
        "last_refresh": "2026-10-01T00:00:00Z",
        "agent_identity": { "agent_runtime_id": "runtime-1" },
        "future_top_level_field": [1, 2, 3]
    })
}

#[test]
fn refresh_persistence_rotates_tokens_and_keeps_fields_it_does_not_interpret() {
    let home = tempfile::tempdir().expect("tempdir");
    let auth_file = home.path().join("auth.json");
    std::fs::write(
        &auth_file,
        chatgpt_auth_json("acct-1", "refresh-old").to_string(),
    )
    .expect("seed auth.json");
    let storage: Arc<dyn AuthStorageBackend> =
        Arc::new(FileAuthStorage::new(home.path().to_path_buf()));
    let expected = storage
        .load()
        .expect("load")
        .and_then(|auth| auth.tokens)
        .expect("seeded tokens");

    persist_tokens(
        &storage,
        &expected,
        expected.oauth_provenance.clone(),
        RefreshResponse {
            id_token: None,
            access_token: Some("access-new".to_string()),
            refresh_token: Some("refresh-new".to_string()),
        },
    )
    .expect("same login persists");

    let stored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&auth_file).expect("read")).expect("json");
    let mut expected_json = chatgpt_auth_json("acct-1", "refresh-new");
    expected_json["tokens"]["access_token"] = "access-new".into();
    expected_json["last_refresh"] = stored["last_refresh"].clone();
    assert_ne!(stored["last_refresh"], "2026-10-01T00:00:00Z");
    assert_eq!(stored, expected_json);

    std::fs::write(
        &auth_file,
        chatgpt_auth_json("acct-2", "refresh-other").to_string(),
    )
    .expect("another login");
    persist_tokens(
        &storage,
        &expected,
        None,
        RefreshResponse {
            id_token: None,
            access_token: Some("access-stale".to_string()),
            refresh_token: None,
        },
    )
    .expect_err("a refresh for a replaced login must not overwrite it");
    let stored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&auth_file).expect("read")).expect("json");
    assert_eq!(stored, chatgpt_auth_json("acct-2", "refresh-other"));
}

#[tokio::test]
async fn an_unreadable_store_keeps_the_cached_credential_and_publishes_no_sign_out() {
    let home = tempfile::tempdir().expect("tempdir");
    let auth_file = home.path().join("auth.json");
    std::fs::write(&auth_file, r#"{"OPENAI_API_KEY":"sk-cached"}"#).expect("seed auth.json");
    let open = || {
        AuthManager::new(
            home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        )
    };
    let manager = open().await;
    let revision = *manager.auth_change_receiver().borrow();

    std::fs::write(&auth_file, r#"{"OPENAI_API_KEY":"#).expect("torn auth.json");
    assert!(!manager.reload().await);
    assert_eq!(
        manager
            .auth()
            .await
            .and_then(|auth| auth.get_token().ok())
            .as_deref(),
        Some("sk-cached")
    );
    assert_eq!(*manager.auth_change_receiver().borrow(), revision);
    assert!(manager.auth_store_failure().is_some());

    let unreadable = open().await;
    assert!(unreadable.auth().await.is_none());
    assert!(unreadable.auth_store_failure().is_some());
}
