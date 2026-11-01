use std::ffi::OsString;
use std::path::Path;

use octopus_provider_claude_native::{Event, ItemKind, SdkChild, SessionConfig, TextInput, TurnStatus};

fn fake_child(dir: &Path) -> std::path::PathBuf {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake_sdk_child.py");
    let launcher = dir.join("fake-sdk-child");
    std::fs::write(&launcher, format!("#!/bin/sh\nexec python3 '{}'\n", script.display())).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    launcher
}

fn path_env() -> Vec<(OsString, OsString)> {
    vec![("PATH".into(), std::env::var_os("PATH").unwrap_or_default())]
}

fn config(cwd: &Path) -> SessionConfig {
    SessionConfig {
        model: "m".into(),
        system_prompt: String::new(),
        cwd: cwd.display().to_string(),
        effort: None,
        resume: None,
        max_turns: None,
        tools: Vec::new(),
    }
}

#[tokio::test]
async fn a_turn_streams_its_items_and_ends_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = SdkChild::spawn(&fake_child(dir.path()), "claude", path_env(), dir.path()).await.unwrap();
    child.start("s", config(dir.path())).await.unwrap();
    child.turn("s", "t", vec![TextInput::new("hello")]).await.unwrap();
    let mut texts = Vec::new();
    let mut deltas = Vec::new();
    let mut cost = None;
    let end = loop {
        match child.next_event().await.unwrap() {
            Event::Item { item, .. } if item.kind == ItemKind::Text => {
                assert_eq!(item.message_id.as_deref(), Some("msg_fake"));
                texts.push(item.text)
            }
            Event::TextDelta { message_id, text, .. } => deltas.push((message_id, text)),
            Event::Usage { total_cost_usd, .. } => cost = total_cost_usd.as_f64(),
            Event::TurnEnd { status, claude_session_id, .. } => break (status, claude_session_id),
            _ => {}
        }
    };
    assert_eq!(texts, ["echo: hello"]);
    assert_eq!(deltas, [("msg_fake".to_string(), "echo: ".to_string()), ("msg_fake".to_string(), "hello".to_string())]);
    assert_eq!(cost, Some(0.0009085));
    assert_eq!(end, (TurnStatus::Completed, Some("c-1".to_string())));
    let refused = child.tool_result("s", "t", "c", Vec::new(), false).await.unwrap_err();
    assert_eq!(refused.code, "unknown_tool_call");
    child.close("s").await.unwrap();
    child.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_child_that_dies_fails_the_pending_request_and_reports_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = SdkChild::spawn(&fake_child(dir.path()), "claude", path_env(), dir.path()).await.unwrap();
    child.start("s", config(dir.path())).await.unwrap();
    child.turn("s", "t", vec![TextInput::new("die")]).await.unwrap();
    match child.next_event().await.unwrap() {
        Event::Error { code, fatal: true, .. } => assert_eq!(code, "child_exited"),
        other => panic!("expected a fatal child error, got {other:?}"),
    }
    let after = child.close("s").await.unwrap_err();
    assert!(matches!(after.code.as_str(), "child_write_failed" | "child_exited"), "{after}");
}
