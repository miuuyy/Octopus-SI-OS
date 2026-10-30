use std::path::{Path, PathBuf};

use octopus_interfaces::{InterfaceArtifactFile, InterfaceArtifactManifest};
use octopus_provider_claude_native::{LiveTurn, retire_open_streams, stream_identity_hash};
use serde_json::Value;

const PROVIDER: &str = "world_provider_claude_native_home_test";
const RUN: &str = "run_home_test";

fn turn_dir(streams: &Path, thread: &str, turn: &str) -> PathBuf {
    streams.join(stream_identity_hash(&[PROVIDER, RUN])).join(stream_identity_hash(&[thread, turn]))
}

fn manifest(dir: &Path, file: &str) -> InterfaceArtifactManifest {
    serde_json::from_slice(&std::fs::read(dir.join(file)).unwrap()).unwrap()
}

fn rows(dir: &Path) -> Vec<Value> {
    std::fs::read_to_string(dir.join("turn-stream.body.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn a_turn_streams_its_text_and_seals_at_its_terminal() {
    let temp = tempfile::tempdir().unwrap();
    let streams = temp.path().join("streams");
    let emitted = temp.path().join("emitted");
    let mut live = LiveTurn::new(streams.clone(), PROVIDER, RUN, Some("run-thread"), "source-7", 7);
    live.delta("msg_a", "Let me ").unwrap();
    live.delta("msg_a", "check.").unwrap();
    assert!(live.flush_due().is_some(), "deltas inside the window wait to be coalesced");
    let dir = turn_dir(&streams, "run-thread", "source-7");
    assert!(!dir.exists(), "nothing is opened before text is written");
    live.flush().unwrap();
    assert!(live.flush_due().is_none());
    assert_eq!(manifest(&dir, "turn-stream.artifact.json").payload_content_hash, None, "the open stream is unsealed");

    let call = InterfaceArtifactFile::write(&emitted, "tool.call", br#"{"tool":"artifact.list"}"#).unwrap();
    live.boundary("tool.call", &call).unwrap();
    live.source_movement_seq = 9;
    live.delta("msg_b", "You have 3 events.").unwrap();
    assert_eq!(live.reply_item_id("You have 3 events."), Some("msg_b".into()), "the reply completes the streamed item");
    assert_eq!(live.reply_item_id("Something else."), None);

    let terminal = InterfaceArtifactFile::write(&emitted, "interface.artifact.output", br#"{"text":"You have 3 events."}"#).unwrap();
    let terminal_hash = terminal.read_manifest().unwrap().payload_content_hash.unwrap();
    let sealed = live.seal("interface.artifact.output", &terminal).unwrap().expect("a streamed turn seals");
    let sealed_manifest = sealed.read_manifest().unwrap();
    let body = std::fs::read(&sealed_manifest.payload_path).unwrap();
    assert_eq!(sealed_manifest.label, "turn.stream");
    assert_eq!(sealed_manifest.payload_content_hash.as_deref(), Some(blake3::hash(&body).to_hex().as_str()));

    let rows = rows(&dir);
    let kinds: Vec<_> = rows.iter().map(|row| row["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["header", "event", "semantic_boundary", "event", "semantic_boundary", "seal"]);
    for (seq, row) in rows.iter().enumerate() {
        assert_eq!(row["seq"], seq as u64);
    }
    assert_eq!(rows[0]["provider_interface"], PROVIDER);
    assert_eq!(rows[0]["opened_source_movement_seq"], 7);
    assert_eq!(rows[1]["label"], "agent.message.delta");
    assert_eq!(rows[1]["payload"]["text"], "Let me check.");
    assert_eq!(rows[1]["payload"]["item_id"], "msg_a");
    assert_eq!(rows[1]["payload"]["turn_id"], "source-7");
    assert_eq!(rows[3]["payload"]["item_id"], "msg_b");
    assert_eq!(rows[3]["source_movement_seq"], 9);
    assert_eq!(rows[4]["artifact_payload_blake3"], terminal_hash.as_str());
    assert_eq!(rows[5]["terminal_label"], "interface.artifact.output");
}

#[test]
fn a_turn_without_text_has_no_stream_and_a_stale_open_stream_is_withdrawn() {
    let temp = tempfile::tempdir().unwrap();
    let streams = temp.path().join("streams");
    let terminal = InterfaceArtifactFile::write(temp.path().join("emitted"), "turn.error", br#"{"message":"x"}"#).unwrap();
    let quiet = LiveTurn::new(streams.clone(), PROVIDER, RUN, Some("run-thread"), "source-1", 1);
    assert!(quiet.seal("turn.error", &terminal).unwrap().is_none());

    let mut sealed = LiveTurn::new(streams.clone(), PROVIDER, RUN, Some("run-thread"), "source-2", 2);
    sealed.delta("msg_a", "done").unwrap();
    sealed.seal("turn.error", &terminal).unwrap().unwrap();
    let mut stale = LiveTurn::new(streams.clone(), PROVIDER, RUN, Some("run-thread"), "source-3", 3);
    stale.delta("msg_b", "interrupted").unwrap();
    stale.flush().unwrap();
    drop(stale);

    assert_eq!(retire_open_streams(&streams).unwrap(), 1);
    let stale_dir = turn_dir(&streams, "run-thread", "source-3");
    assert!(!stale_dir.join("turn-stream.artifact.json").exists(), "the client no longer sees it as live");
    assert!(stale_dir.join("turn-stream.body.jsonl").exists(), "its body is kept");
    assert!(turn_dir(&streams, "run-thread", "source-2").join("turn-stream.artifact.json").exists());
    assert_eq!(retire_open_streams(&streams).unwrap(), 0);
}

#[test]
fn a_steer_is_recorded_in_the_stream_under_the_claude_session() {
    let temp = tempfile::tempdir().unwrap();
    let streams = temp.path().join("streams");
    let mut live = LiveTurn::new(streams.clone(), PROVIDER, RUN, None, "source-9", 9);
    assert!(live.delta("msg_a", "x").is_ok(), "deltas wait in memory until the session is known");
    assert!(live.flush().unwrap_err().contains("before Claude reports its session"));
    live.set_thread_id("claude-1").unwrap();
    live.flush().unwrap();
    live.input_recorded("msg_steer_1", "steer-msg_steer_1").unwrap();
    assert!(live.set_thread_id("claude-2").is_err(), "an open stream keeps its thread");
    let rows = rows(&turn_dir(&streams, "claude-1", "source-9"));
    let recorded = rows.iter().find(|row| row["type"] == "input_recorded").expect("recorded row");
    for (field, expected) in [
        ("provider_interface", PROVIDER),
        ("run", RUN),
        ("thread_id", "claude-1"),
        ("turn_id", "source-9"),
        ("client_id", "msg_steer_1"),
        ("item_id", "steer-msg_steer_1"),
        ("stage", "input_recorded_in_turn"),
        ("source_event", "item/started"),
    ] {
        assert_eq!(recorded[field], expected, "{field}");
    }
    assert!(recorded["native_event_at_ms"].as_u64().is_some());
    assert_eq!(live.streamed_text().as_deref(), Some("x"));
}
