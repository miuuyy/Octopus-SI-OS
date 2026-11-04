use super::*;

#[test]
fn model_text_keeps_selected_evidence_without_duplicating_unselected_refs() {
    let mut output = json!({"selected_input":{"fields":{"/text":"original task"}},
        "evidence":[{"source_artifact":"selected reference","fields":{"/ok":true}}],
        "omission":{"selected_fields_trimmed":false,"selected_evidence_trimmed":false,
            "unselected_entries":[{"source_artifact":"unselected reference","source_movement_seq":7}]}});
    let original = output.clone();
    render_text("Capture knowledge", &mut output).unwrap();
    let rendered: Value =
        serde_json::from_str(output["text"].as_str().unwrap().lines().last().unwrap()).unwrap();
    assert_eq!(rendered["selected_input"], original["selected_input"]);
    assert_eq!(rendered["evidence"], original["evidence"]);
    assert_eq!(rendered["omission"]["unselected_entry_count"], 1);
    assert!(rendered["omission"].get("unselected_entries").is_none());
    assert_eq!(output["omission"], original["omission"]);
    let first = output.clone();
    render_text("Capture knowledge", &mut output).unwrap();
    assert_eq!(output, first);
}

#[test]
fn oversized_fields_keep_their_beginning_and_end_within_the_bound() {
    let text = format!("start-{}-end", "x".repeat(4096));
    let payload = json!({"text": text, "list": vec!["y"; 1024], "small": "ok"});
    let pointers = ["/text", "/list", "/small"].map(String::from);
    let (projected, trimmed) = fields(&payload, &pointers, 256, true).unwrap();
    let kept = projected["/text"].as_str().unwrap();
    assert!(kept.starts_with("start-") && kept.ends_with("-end") && kept.contains("chars truncated"));
    for pointer in ["/text", "/list"] {
        assert!(serde_json::to_vec(&projected[pointer]).unwrap().len() <= 256);
    }
    assert_eq!(projected["/small"], "ok");
    assert_eq!(trimmed, vec![json!({"pointer":"/text","bytes":serde_json::to_vec(&payload["text"]).unwrap().len()}),
        json!({"pointer":"/list","bytes":serde_json::to_vec(&payload["list"]).unwrap().len()})]);
    assert!(fields(&payload, &["/absent".to_string()], 256, true).is_err());
    assert_eq!(fields(&payload, &["/absent".to_string()], 256, false).unwrap().0, json!({}));
}

#[test]
fn complete_experience_fits_after_original_input_and_provenance_are_attached() {
    let text = format!("first-{}-last", "\"\\\nя".repeat(9000));
    let mut output = json!({"source":{"run":"source","terminal_artifact":{"id":"terminal"}},
        "selected_input":{"source_artifact":{"id":"input"},"source_movement_seq":7,
            "fields":{"/text":text},"original_fields":{"/text":text},"provenance":{"seq":7}},
        "terminal_fields":{"/text":text}, "evidence":[{"fields":{"/text":text}}],
        "omission":{"unselected_entries":(0..4096).map(|seq| json!({"source_movement_seq":seq,"source_artifact":{"id":format!("unselected-{seq}")}})).collect::<Vec<_>>(),
            "trimmed_evidence_entries":0,"selected_fields_trimmed":false}});
    let provenance = output["selected_input"]["provenance"].clone();
    fit_rendered_context("Capture knowledge", &mut output, 131072, 32768).unwrap();
    assert!(serde_json::to_vec(&output).unwrap().len() <= 131072);
    assert_eq!(output["selected_input"]["source_artifact"]["id"], "input");
    assert_eq!(output["selected_input"]["provenance"], provenance);
    assert_eq!(output["source"]["terminal_artifact"]["id"], "terminal");
    assert_eq!(output["omission"]["trimmed_unselected_entries"], 4096);
    assert_eq!(output["omission"]["selected_fields_trimmed"], true);
    for pointer in ["/selected_input/fields/~1text", "/selected_input/original_fields/~1text", "/terminal_fields/~1text"] {
        let kept = output.pointer(pointer).unwrap().as_str().unwrap();
        assert!(kept.starts_with("first-") && kept.ends_with("-last"));
    }
    let rendered: Value = serde_json::from_str(output["text"].as_str().unwrap().lines().last().unwrap()).unwrap();
    assert_eq!(rendered["selected_input"], output["selected_input"]);
    assert_eq!(rendered["omission"]["unselected_entry_count"], 4096);
    assert_eq!(rendered["omission"]["selected_fields_trimmed"], true);
    let first = output.clone();
    fit_rendered_context("Capture knowledge", &mut output, 131072, 32768).unwrap();
    assert_eq!(output, first);
    assert!(fit_rendered_context("Capture knowledge", &mut output, 64, 32768).unwrap_err().contains("provenance"));
}

#[test]
fn dropping_trimmed_evidence_recomputes_the_remaining_field_omission() {
    let mut output = json!({"selected_input":{"fields":{"/text":"task"}},
        "terminal_fields":{"/text":"answer"},
        "evidence":[{"fields":{"/text":"x".repeat(4096)},
            "trimmed_fields":[{"pointer":"/text","bytes":8192}]}],
        "omission":{"selected_fields_trimmed":true,"selected_evidence_trimmed":false,
            "trimmed_evidence_entries":0,"unselected_entries":[]}});
    fit_rendered_context("Capture knowledge", &mut output, 2000, 1024).unwrap();
    assert_eq!(output["evidence"], json!([]));
    assert_eq!(output["omission"]["selected_fields_trimmed"], false);
    assert_eq!(output["omission"]["trimmed_evidence_entries"], 1);
    assert_eq!(output["omission"]["selected_evidence_trimmed"], true);
    let rendered: Value = serde_json::from_str(output["text"].as_str().unwrap().lines().last().unwrap()).unwrap();
    assert_eq!(rendered["omission"]["selected_fields_trimmed"], false);
}

#[test]
fn an_input_consumed_after_later_outputs_keeps_its_window() {
    let config: ContextProjectionConfig = serde_json::from_value(json!({
        "schema_version":1,"observer_run":"observer","projection_run":"projection","source_run":"run",
        "source_worker_interface":"worker","source_provider_interface":"provider","source_client_interface":"client",
        "source_relay_interface":"relay","terminal_label":"interface.artifact.output","snapshot_labels":["interface.artifact.output"],
        "input_rules":[{"labels":["operator.message"],"pointers":["/text"]}],"input_routes":[],
        "evidence_rules":[{"labels":["email.event"],"pointers":["/message/subject"]}],"terminal_pointers":["/text"],
        "collection_source":"memory","publication_tool":"memory.save","task":"Remember what matters.",
        "bounds":{"ancestry":{"max_carriers":32,"max_entries":8192,"max_total_bytes":33554432},"max_field_bytes":32768,
            "max_input_bytes":16777216,"max_output_bytes":131072,"max_projected_entries":64,"max_snapshot_entries":4096}})).unwrap();
    let entry = |label: &str, seq: u64, payload: Value| SessionContextArtifact {label:label.into(),payload,
        source_artifact:serde_json::from_value(json!({"id":format!("{label}:{seq}"),"content":vec![seq as u8;32]})).unwrap(),source_movement_seq:Some(seq)};
    let carrier: ArtifactRef = serde_json::from_value(json!({"id":"interface.artifact.output:terminal","content":vec![9u8;32]})).unwrap();
    let mut entries = vec![entry("operator.message",1,json!({"text":"look around"})),
        entry("interface.artifact.output",111,json!({"text":"done"})),
        entry("email.event",3,json!({"message":{"subject":"queued while the turn worked"}})),
        entry("turn.agent_message",157,json!({"text":"read the letter"}))];
    let projected = project_window(&config, "ee", &carrier, 160, &json!({"text":"read the letter"}), None, &entries).unwrap();
    assert_eq!(projected["evidence"][0]["source_movement_seq"], 3);
    entries.push(entries[2].clone());
    assert!(project_window(&config, "ee", &carrier, 160, &json!({"text":"read the letter"}), None, &entries).is_err());
}
