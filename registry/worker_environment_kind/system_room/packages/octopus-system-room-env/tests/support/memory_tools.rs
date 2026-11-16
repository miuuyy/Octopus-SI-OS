use super::*;
use octopus_descriptors::{ArtifactId, CheckpointId, EeId, InterfaceId, RunId};
use octopus_interfaces::{
    ArtifactBindingPayload, GrantProjection, InterfaceArtifactFile, InvocationMetadata,
    RuntimeInvocationMetadata,
};
use octopus_ledger::ArtifactRef;

struct Fixture {
    root: PathBuf,
    artifacts: PathBuf,
    ees: PathBuf,
    source: ArtifactBindingPayload,
    calls: std::cell::Cell<usize>,
    blobs: PathBuf,
    blobs_granted: std::cell::Cell<bool>,
}

impl Fixture {
    fn new(root: &Path) -> Self {
        let artifacts = root.join("artifact");
        let ees = root.join("ee");
        let descriptor = json!({"schema_version":1,"label":"context.collection","id":"artifact-memory-test","description":"Remembered notes",
            "directory":"entries","limits":{"max_entries":64,"max_entry_bytes":65536,"max_page_bytes":65536,"max_scan_bytes":1048576}});
        std::fs::create_dir_all(artifacts.join("artifact-memory-test")).unwrap();
        std::fs::write(artifacts.join("artifact-memory-test/artifact.json"), serde_json::to_vec(&descriptor).unwrap()).unwrap();
        std::fs::create_dir_all(&ees).unwrap();
        for (ee, parent) in [("ee_home", None), ("ee_project", Some("ee_home")), ("ee_other", None)] {
            std::fs::write(ees.join(format!("{ee}.json")), serde_json::to_vec(&json!({"ee_id":ee,"parent_ee_id":parent})).unwrap()).unwrap();
        }
        let source = ArtifactBindingPayload::new(
            ArtifactRef { id: ArtifactId::new("artifact-memory-test"), content: ContentHash::of_canonical_json(&descriptor) },
            octopus_descriptors::canonical_bytes(&descriptor),
        );
        let blobs = root.join("blobs");
        std::fs::create_dir_all(&blobs).unwrap();
        Self { root: root.to_path_buf(), artifacts, ees, source, calls: std::cell::Cell::new(0), blobs, blobs_granted: std::cell::Cell::new(true) }
    }

    fn envelope(&self, tool: &str, arguments: &Value, ee: &str, verbs: &[&str]) -> InterfaceInvocationEnvelope {
        self.calls.set(self.calls.get() + 1);
        let payload = serde_json::to_vec(&json!({"tool":tool,"arguments":arguments})).unwrap();
        let file = InterfaceArtifactFile::write(self.root.join(format!("call-{}", self.calls.get())), "tool.call", &payload).unwrap();
        let manifest = std::fs::read(file.manifest_path()).unwrap();
        let content = ContentHash::of_bytes(&manifest);
        for bytes in [&manifest, &payload] {
            std::fs::write(self.blobs.join(ContentHash::of_bytes(bytes).to_hex()), bytes).unwrap();
        }
        let grant = GrantProjection {
            tools: vec![tool.to_string()],
            worker_environment_kinds: vec!["system_room".into()],
            artifact_verbs: verbs.iter().map(|verb| verb.to_string()).collect(),
            artifact_access: serde_json::from_value(json!({"artifact-memory-test":{"read":true}})).unwrap(),
            ..Default::default()
        };
        let metadata = InvocationMetadata::new(grant).with_runtime(RuntimeInvocationMetadata {
            run: RunId::new(format!("run_{ee}")), ee: EeId::new(ee), worker: InterfaceId::new("system_worker_memory_test"),
            checkpoint: CheckpointId::new("tool"), interface: InterfaceId::new("system_memory"), epoch: 0, run_record_hash: None,
        });
        InterfaceInvocationEnvelope {
            artifact: ArtifactRef { id: ArtifactId::new(format!("tool.call:{}", content.to_hex())), content },
            artifact_file: file,
            artifact_binding_payloads: vec![self.source.clone()],
            metadata: Some(InvocationMetadata { artifact_binding_refs: vec![self.source.artifact.clone()], ..metadata }),
        }
    }

    fn run(&self, input: &InterfaceInvocationEnvelope, tool: &str, arguments: Value) -> Result<Value, String> {
        self.run_for(input, tool, arguments, None)
    }

    fn run_for(&self, input: &InterfaceInvocationEnvelope, tool: &str, arguments: Value, observed_run: Option<&str>) -> Result<Value, String> {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../..");
        let descriptor = octopus_tool_registry::load_tool_descriptor_from_dir(&repository.join("registry/tool"), tool).unwrap().unwrap();
        octopus_tool_registry::ToolArgumentValidator::compile(&descriptor.args_schema).unwrap()
            .validate(&arguments).map_err(|error| error.to_string())?;
        let effects = self.root.join("effects");
        let host = MemoryHost { collection: "artifact-memory-test", artifact_root: &self.artifacts, ee_instances_root: &self.ees,
            effect_root: Some(&effects), maximum_bytes: 1024 * 1024, observed_run,
            artifact_blobs_root: self.blobs_granted.get().then_some(self.blobs.as_path()) };
        let call = ToolCall { tool_id: tool.into(), arguments, handler_config: descriptor.handler_config.clone() };
        execute(input, &call, &host).map(|result| result.body).map_err(|error| error.to_string())
    }

    fn call(&self, tool: &str, arguments: Value, ee: &str, verbs: &[&str]) -> Result<Value, String> {
        let input = self.envelope(tool, &arguments, ee, verbs);
        self.run(&input, tool, arguments)
    }

    fn entry_path(&self, id: &str) -> PathBuf {
        self.artifacts.join(format!("artifact-memory-test/entries/{id}.json"))
    }

    /// Stores one note as the collection holds it, under an id whose time is chosen by the test.
    fn plant(&self, id: &str, title: &str, text: &str, ee: &str) {
        let entry = json!({"schema_version":1,"id":id,"title":title,"description":headline(text, MAX_DESCRIPTION_CHARS),
            "applicability":APPLICABILITY,"state":"active","scope":{"ee_ids":[ee]},"content":{"text":text},"selection":{}});
        std::fs::create_dir_all(self.entry_path(id).parent().unwrap()).unwrap();
        std::fs::write(self.entry_path(id), serde_json::to_vec(&entry).unwrap()).unwrap();
    }
}

fn row<'a>(rows: &'a Value, id: &str) -> &'a Value {
    rows.as_array().unwrap().iter().find(|row| row["id"] == id).unwrap_or_else(|| panic!("'{id}' is listed: {rows}"))
}

fn ids(rows: &Value) -> Vec<&str> {
    rows.as_array().unwrap().iter().map(|row| row["id"].as_str().unwrap()).collect()
}

#[test]
fn one_save_is_found_by_later_runs_within_its_scope_lineage() {
    let temporary = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(&temporary.path().canonicalize().unwrap());
    let writer = ["read", "create", "change"];
    let remember = json!({"note":"The user prefers pnpm over npm for JavaScript projects."});
    let save = fixture.envelope("memory.save", &remember, "ee_home", &writer);
    let saved = fixture.run(&save, "memory.save", remember.clone()).unwrap();
    assert_eq!(saved["result"], "memory_saved");
    assert_eq!(fixture.run(&save, "memory.save", remember.clone()).unwrap(), saved);
    let id = saved["memory"]["id"].as_str().unwrap().to_string();
    let stored: Value = serde_json::from_slice(&std::fs::read(fixture.artifacts.join(format!("artifact-memory-test/entries/{id}.json"))).unwrap()).unwrap();
    assert_eq!(stored["scope"], json!({"ee_ids":["ee_home"]}));
    assert_eq!(stored["content"]["evidence"], json!([save.artifact]));
    let repeated = fixture.call("memory.save", json!({"note":"the user prefers pnpm over npm for JavaScript projects"}), "ee_home", &writer).unwrap();
    assert_eq!((repeated["result"].as_str(), repeated["memory"]["id"].as_str()), (Some("memory_already_saved"), Some(id.as_str())));

    let found = fixture.call("memory.search", json!({"query":"which package manager, pnpm or npm?"}), "ee_project", &["read"]).unwrap();
    assert_eq!(found["matches"][0]["id"], id);
    let read = fixture.call("memory.read", json!({"id":id}), "ee_project", &["read"]).unwrap();
    assert_eq!(read["memory"]["text"], remember["note"]);
    assert_eq!(read["memory"]["usage"]["days_used"], 1);
    assert_eq!(fixture.call("memory.search", json!({"query":"pnpm"}), "ee_other", &["read"]).unwrap()["matches"], json!([]));
    assert!(fixture.call("memory.read", json!({"id":id}), "ee_other", &["read"]).is_err());
    assert!(fixture.call("memory.save", json!({"note":"Unauthorized note."}), "ee_home", &["read"]).unwrap_err().contains("create artifact verb"));
    assert!(fixture.call("memory.save", json!({"note":"Use yarn.","supersedes":[id]}), "ee_project", &writer).unwrap_err().contains("wider or different scope"));

    let correction = fixture.call("memory.save", json!({"note":"The user now uses yarn instead of pnpm.","title":"Package manager: yarn","supersedes":[id]}), "ee_home", &writer).unwrap();
    assert_eq!(correction["withdrawn"][0]["id"], id);
    let current = fixture.call("memory.search", json!({"query":"pnpm yarn"}), "ee_project", &["read"]).unwrap();
    assert_eq!(current["matches"].as_array().unwrap().len(), 1);
    assert_eq!(current["matches"][0]["title"], "Package manager: yarn");
    let history = fixture.call("memory.list", json!({"include_withdrawn":true}), "ee_home", &["read"]).unwrap();
    let states = history["memories"].as_array().unwrap().iter().map(|row| row["state"].as_str().unwrap()).collect::<Vec<_>>();
    assert_eq!((history["total"].as_u64(), states.contains(&"withdrawn")), (Some(2), true));
    let names = std::fs::read_dir(fixture.artifacts.join("artifact-memory-test/entries")).unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap()).collect::<Vec<_>>();
    assert!(names.len() == 2 && names.iter().all(|name| name.ends_with(".json") && !name.starts_with('.')), "{names:?}");
}

#[test]
fn observation_keeps_its_admitted_scope_across_moves_and_retries() {
    let temporary = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(&temporary.path().canonicalize().unwrap());
    let writer = ["read", "create", "change"];
    let persist = |label: &str, value: &Value| {
        let bytes = serde_json::to_vec(value).unwrap();
        let file = InterfaceArtifactFile::write(fixture.root.join("carriers"), label, &bytes).unwrap();
        let manifest = std::fs::read(file.manifest_path()).unwrap();
        let content = ContentHash::of_bytes(&manifest);
        ArtifactBindingPayload::new(ArtifactRef { id: ArtifactId::new(format!("{label}:{}", content.to_hex())), content }, bytes)
    };
    let bind = |input: &mut InterfaceInvocationEnvelope, source: &ArtifactBindingPayload| {
        let metadata = input.metadata.as_mut().unwrap();
        metadata.grant.artifact_access.insert(source.artifact.id.to_string(), serde_json::from_value(json!({"read":true})).unwrap());
        metadata.artifact_binding_refs.push(source.artifact.clone());
        input.artifact_binding_payloads.push(source.clone());
    };
    let mut experience = json!({"schema_version":1,"label":"memory.experience.input","collection_source":"artifact-memory-test",
        "source":{"run":"observed","ee":"ee_project","terminal_source_movement_seq":10,"projection_source_movement_seq":12,"terminal_artifact":fixture.source.artifact}});
    let source = persist("memory.experience.input", &experience);
    let note = json!({"note":"Private project deployment procedure."});
    let mut input = fixture.envelope("memory.save", &note, "ee_home", &writer);
    assert!(fixture.run_for(&input, "memory.save", note.clone(), Some("observed")).unwrap_err().contains("no bound experience"));
    bind(&mut input, &source);
    let saved = fixture.run_for(&input, "memory.save", note.clone(), Some("observed")).unwrap();
    assert_eq!(saved["memory"]["scope_ee_ids"], json!(["ee_project"]));
    assert_eq!(fixture.call("memory.search", json!({"query":"private project"}), "ee_home", &["read"]).unwrap()["matches"], json!([]));
    assert!(fixture.run_for(&input, "memory.save", note.clone(), Some("another_run")).is_err());
    assert_eq!(fixture.run_for(&input, "memory.save", note.clone(), Some("observed")).unwrap(), saved);
    experience["source"]["ee"] = json!("ee_home");
    experience["source"]["projection_source_movement_seq"] = json!(30);
    let later = persist("memory.experience.input", &experience);
    let future_note = json!({"note":"New Home experience after the move."});
    let mut future = fixture.envelope("memory.save", &future_note, "ee_other", &writer);
    bind(&mut future, &source);
    bind(&mut future, &later);
    assert_eq!(fixture.run_for(&future, "memory.save", future_note, Some("observed")).unwrap()["memory"]["scope_ee_ids"], json!(["ee_home"]));
    let mut denied = input.clone();
    denied.metadata.as_mut().unwrap().grant.artifact_access.remove(source.artifact.id.as_str());
    assert!(fixture.run_for(&denied, "memory.save", note.clone(), Some("observed")).is_err());
    let plan = fixture.root.join("effects/memory-save").join(input.artifact.content.to_hex()).join("plan.json");
    let mut poisoned: Value = serde_json::from_slice(&std::fs::read(&plan).unwrap()).unwrap();
    poisoned["memory"]["scope_ee_ids"] = json!(["ee_home"]);
    std::fs::write(plan, serde_json::to_vec(&poisoned).unwrap()).unwrap();
    assert!(fixture.run_for(&input, "memory.save", note, Some("observed")).unwrap_err().contains("differs from its admitted experience scope"));
}

#[test]
fn a_note_is_traced_to_its_own_saving_call_and_to_nothing_else() {
    let temporary = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(&temporary.path().canonicalize().unwrap());
    let writer = ["read", "create", "change"];
    let remember = json!({"title":"Staging gateway port",
        "note":format!("Zażółć gęślą jaźń: the staging gateway listens on port 8443. {}", "€".repeat(900))});
    let save = fixture.envelope("memory.save", &remember, "ee_home", &writer);
    let id = fixture.run(&save, "memory.save", remember.clone()).unwrap()["memory"]["id"].as_str().unwrap().to_string();
    let payload = save.artifact_file.read_payload_bytes().unwrap();
    assert!(payload.len() > 2048);

    let traced = fixture.call("memory.inspect_evidence", json!({"id":id}), "ee_project", &["read"]).unwrap();
    assert_eq!((traced["result"].as_str(), traced["memory"]["id"].as_str()), (Some("memory_evidence"), Some(id.as_str())));
    assert_eq!(traced["memory"]["usage"]["days_used"], 0, "tracing a note is not a use of it");
    assert_eq!(traced["evidence"]["omitted"], 0);
    let found = &traced["evidence"]["artifacts"][0];
    assert_eq!(found["artifact"], json!(save.artifact));
    assert_eq!((found["status"].as_str(), found["label"].as_str(), found["tool"].as_str()), (Some("available"), Some("tool.call"), Some("memory.save")));
    assert_eq!((found["payload"]["bytes"].as_u64(), found["truncated"].as_bool()), (Some(payload.len() as u64), Some(true)));
    assert_eq!(found["payload"]["content_hash"], ContentHash::of_bytes(&payload).to_hex());
    let preview = found["preview"].as_str().unwrap();
    assert!(preview.len() <= 2048 && preview.len() > 2044 && payload.starts_with(preview.as_bytes()));
    assert_eq!(found["preview_bytes"], preview.len());
    assert!(!traced.to_string().contains(fixture.root.to_str().unwrap()), "no host path leaves the door");

    for limit in 16..=40_usize {
        let bounded = fixture.call("memory.inspect_evidence", json!({"id":id,"max_bytes":limit}), "ee_project", &["read"]).unwrap();
        let preview = bounded["evidence"]["artifacts"][0]["preview"].as_str().unwrap().to_string();
        assert!(preview.len() <= limit && limit - preview.len() < 4 && payload.starts_with(preview.as_bytes()), "{limit}: {preview:?}");
    }
    let whole = fixture.call("memory.inspect_evidence", json!({"id":id,"max_bytes":8192}), "ee_project", &["read"]).unwrap();
    assert_eq!(whole["evidence"]["artifacts"][0]["truncated"], false);
    assert_eq!(whole["evidence"]["artifacts"][0]["preview"].as_str().unwrap().as_bytes(), payload);
    for refused in [15, 8193] {
        assert!(fixture.call("memory.inspect_evidence", json!({"id":id,"max_bytes":refused}), "ee_project", &["read"]).is_err());
    }

    let read = fixture.call("memory.read", json!({"id":id,"include_evidence":true}), "ee_project", &["read"]).unwrap();
    assert_eq!(read["memory"]["evidence"], json!([save.artifact]));
    assert_eq!(read["memory"]["evidence_details"]["artifacts"][0]["status"], "available");
    assert_eq!(read["memory"]["evidence_details"]["artifacts"][0]["preview"].as_str().unwrap().len(), preview.len());
    assert!(fixture.call("memory.read", json!({"id":id}), "ee_project", &["read"]).unwrap()["memory"].get("evidence_details").is_none());

    for tool in ["memory.inspect_evidence", "memory.read"] {
        let refused = fixture.call(tool, json!({"id":id}), "ee_other", &["read"]).unwrap_err();
        assert!(refused.contains("not an active memory visible here"), "{refused}");
    }
    assert!(fixture.call("memory.inspect_evidence", json!({"id":save.artifact.id}), "ee_project", &["read"]).is_err(),
        "the caller names a note, never an artifact");

    fixture.blobs_granted.set(false);
    let blind = fixture.call("memory.inspect_evidence", json!({"id":id}), "ee_project", &["read"]).unwrap();
    assert_eq!(blind["evidence"]["artifacts"][0]["status"], "unavailable");
    assert!(blind["evidence"]["artifacts"][0].get("preview").is_none());
    fixture.blobs_granted.set(true);

    let secret = json!({"note":"Sibling secret: the vault passphrase is heron-42."});
    let sibling = fixture.envelope("memory.save", &secret, "ee_other", &writer);
    fixture.run(&sibling, "memory.save", secret).unwrap();
    let loose = ContentHash::of_bytes(b"a blob that is no call manifest");
    std::fs::write(fixture.blobs.join(loose.to_hex()), b"a blob that is no call manifest").unwrap();
    let mut stored: Value = serde_json::from_slice(&std::fs::read(fixture.entry_path(&id)).unwrap()).unwrap();
    stored["content"]["evidence"] = json!([
        sibling.artifact,
        ArtifactRef { id: ArtifactId::new("note:loose"), content: loose },
        ArtifactRef { id: ArtifactId::new("tool.call:absent"), content: ContentHash::of_bytes(b"absent") },
        save.artifact,
    ]);
    std::fs::write(fixture.entry_path(&id), serde_json::to_vec(&stored).unwrap()).unwrap();
    let rewritten = fixture.call("memory.read", json!({"id":id,"include_evidence":true}), "ee_project", &["read"]).unwrap();
    let rows = rewritten["memory"]["evidence_details"]["artifacts"].as_array().unwrap();
    let statuses = rows.iter().map(|row| row["status"].as_str().unwrap()).collect::<Vec<_>>();
    assert_eq!(statuses, ["withheld", "withheld", "unavailable", "available"]);
    assert!(rows[..3].iter().all(|row| row.get("preview").is_none() && row.get("tool").is_none() && row["reason"].is_string()));
    assert!(!rewritten.to_string().contains("heron-42"), "a note's evidence list does not open another note's call");

    let tampered = fixture.blobs.join(ContentHash::of_bytes(&payload).to_hex());
    std::fs::write(&tampered, b"{\"arguments\":{\"note\":\"replaced\"}}").unwrap();
    let damaged = fixture.call("memory.inspect_evidence", json!({"id":id}), "ee_project", &["read"]).unwrap();
    assert_eq!(damaged["evidence"]["artifacts"][3]["status"], "unavailable");
    assert!(damaged["evidence"]["artifacts"][3]["reason"].as_str().unwrap().contains("differs from the recorded content"));
}

#[test]
fn a_narrower_scope_corrects_a_wider_note_without_withdrawing_it() {
    let temporary = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(&temporary.path().canonicalize().unwrap());
    let writer = ["read", "create", "change"];
    let saved_id = |saved: &Value| saved["memory"]["id"].as_str().unwrap().to_string();
    let old = saved_id(&fixture.call("memory.save", json!({"note":"Deployments go out on Fridays.","title":"Deploy day"}), "ee_home", &writer).unwrap());
    let foreign = saved_id(&fixture.call("memory.save", json!({"note":"A sibling's private deployments note."}), "ee_other", &writer).unwrap());
    assert!(fixture.call("memory.save", json!({"note":"Tuesdays.","supersedes":[old]}), "ee_project", &writer).unwrap_err().contains("wider or different scope"));
    assert!(fixture.call("memory.save", json!({"note":"Wrong target.","corrects":[foreign]}), "ee_project", &writer).unwrap_err().contains("not an active memory visible here"));
    assert!(fixture.call("memory.save", json!({"note":"Both at once.","corrects":[old],"supersedes":[old]}), "ee_home", &writer).unwrap_err().contains("none of them also superseded"));

    let correction = json!({"note":"Deployments moved to Tuesdays in this project.","title":"Deploy day: Tuesday","corrects":[old]});
    let input = fixture.envelope("memory.save", &correction, "ee_project", &writer);
    let saved = fixture.run(&input, "memory.save", correction.clone()).unwrap();
    assert_eq!(fixture.run(&input, "memory.save", correction).unwrap(), saved, "the same call repeats its exact result");
    let new = saved_id(&saved);
    assert_eq!((saved["corrects"][0]["id"].as_str(), &saved["corrects"][0]["scope_ee_ids"]), (Some(old.as_str()), &json!(["ee_home"])));
    assert_eq!((saved["withdrawn"].clone(), saved["writes"].as_array().unwrap().len()), (json!([]), 1));
    let stored: Value = serde_json::from_slice(&std::fs::read(fixture.entry_path(&old)).unwrap()).unwrap();
    assert_eq!(stored["state"], "active", "the corrected note is not written");
    let by = json!([{"id":new,"revision":saved["memory"]["revision"],"title":"Deploy day: Tuesday",
        "saved_at":saved["memory"]["saved_at"],"scope_ee_ids":["ee_project"]}]);

    let read = fixture.call("memory.read", json!({"id":old}), "ee_project", &["read"]).unwrap();
    assert_eq!((read["memory"]["state"].as_str(), &read["memory"]["corrected_by"]), (Some("active"), &by));
    assert_eq!(fixture.call("memory.read", json!({"id":new}), "ee_project", &["read"]).unwrap()["memory"]["corrected_by"], json!([]));
    let found = fixture.call("memory.search", json!({"query":"deployments"}), "ee_project", &["read"]).unwrap();
    assert_eq!((&row(&found["matches"], &old)["corrected_by"], &row(&found["matches"], &new)["corrected_by"]), (&by, &json!([])));
    let listed = fixture.call("memory.list", json!({}), "ee_project", &["read"]).unwrap();
    assert_eq!(row(&listed["memories"], &old)["corrected_by"], by);
    assert_eq!(fixture.call("memory.inspect_evidence", json!({"id":old}), "ee_project", &["read"]).unwrap()["memory"]["corrected_by"], by);

    let above = fixture.call("memory.read", json!({"id":old}), "ee_home", &["read"]).unwrap();
    assert_eq!(above["memory"]["corrected_by"], json!([]), "a reader who cannot see the correcting note is told nothing of it");
    assert_eq!(row(&fixture.call("memory.search", json!({"query":"deployments"}), "ee_home", &["read"]).unwrap()["matches"], &old)["corrected_by"], json!([]));

    let reverted = fixture.call("memory.save", json!({"note":"Deployments are back on Fridays in this project.","supersedes":[new]}), "ee_project", &writer).unwrap();
    assert_eq!(reverted["withdrawn"][0]["id"], new);
    assert_eq!(fixture.call("memory.read", json!({"id":old}), "ee_project", &["read"]).unwrap()["memory"]["corrected_by"], json!([]),
        "a withdrawn note corrects nothing");
    let history = fixture.call("memory.list", json!({"include_withdrawn":true}), "ee_project", &["read"]).unwrap();
    assert_eq!((row(&history["memories"], &new)["state"].as_str(), &row(&history["memories"], &old)["corrected_by"]), (Some("withdrawn"), &json!([])));
}

#[test]
fn search_ranks_whole_words_by_bm25_above_words_that_only_contain_them() {
    let temporary = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(&temporary.path().canonicalize().unwrap());
    let (report, portal, gateway, cyrillic) = ("memory-20260104t000000z-report-aaaaaaaa", "memory-20260103t000000z-portal-bbbbbbbb",
        "memory-20260102t000000z-gateway-cccccccc", "memory-20260101t000000z-server-dddddddd");
    fixture.plant(report, "Weekly report", "The weekly report report report is sent on Friday; report delays at once.", "ee_home");
    fixture.plant(portal, "Customer portal", "The customer portal is where portal users sign in.", "ee_home");
    fixture.plant(gateway, "Staging gateway", "A long note about the staging gateway and many other things: behind the proxy it listens on port 8443.", "ee_home");
    fixture.plant(cyrillic, "Сервер", "Порт сервера — 8443, żółw-test.", "ee_home");
    let search = |query: &str| fixture.call("memory.search", json!({"query":query}), "ee_project", &["read"]).unwrap()["matches"].clone();

    let port = search("port");
    assert_eq!(ids(&port), [gateway, portal, report], "one whole word outranks any count of longer words, and a prefix outranks an inner part");
    assert_eq!(port.as_array().unwrap().iter().map(|row| row["match"].as_str().unwrap()).collect::<Vec<_>>(), ["word", "partial", "partial"]);
    assert!(port.as_array().unwrap().iter().all(|row| row["score"].as_f64().unwrap() > 0.0 && row["preview"].is_string()));
    assert_eq!(ids(&search("ПОРТ")), [cyrillic], "words are compared without case in any script");
    assert_eq!(ids(&search("żółw")), [cyrillic]);
    assert_eq!(ids(&search("сервера?")), [cyrillic]);
    let number = search("8443");
    assert_eq!((number.as_array().unwrap().len(), row(&number, gateway)["match"].as_str(), row(&number, cyrillic)["match"].as_str()), (2, Some("word"), Some("word")));
    assert_eq!(search("nothing-like-this"), json!([]));
    assert!(fixture.call("memory.search", json!({"query":"— …"}), "ee_project", &["read"]).unwrap_err().contains("searchable words"));

    for (index, subject) in ["build cache", "test runner", "release notes", "crash upload"].iter().enumerate() {
        fixture.plant(&format!("memory-2026020{}t000000z-common-eeeeeee{index}", index + 1), "Tooling", &format!("The octopus {subject} is configured per project."), "ee_home");
    }
    let (rare, both) = ("memory-20260210t000000z-quorum-ffffffff", "memory-20260211t000000z-octopus-quorum-99999999");
    fixture.plant(rare, "Recovery", "Recovery after a quorum loss takes one manual step today.", "ee_home");
    fixture.plant(both, "Tooling", "The octopus quorum setting is configured per project.", "ee_home");
    let weighted = search("octopus quorum");
    assert_eq!(&ids(&weighted)[..2], [both, rare], "a rare word weighs more than a common one");
    assert_eq!(weighted.as_array().unwrap().len(), 6);
    assert!(row(&weighted, both)["score"].as_f64() > row(&weighted, rare)["score"].as_f64());

    let (older, newer) = ("memory-20260301t000000z-zeta-11111111", "memory-20260302t000000z-alpha-22222222");
    fixture.plant(older, "Zeta host", "Zeta host uses tunnel one.", "ee_home");
    fixture.plant(newer, "Alpha host", "Alpha host uses tunnel two.", "ee_home");
    let tied = search("tunnel");
    assert_eq!((ids(&tied), tied[0]["score"] == tied[1]["score"]), (vec![newer, older], true), "equal scores list the newer note first");
    assert_eq!(ids(&search("host tunnel"))[..2], [newer, older]);
    let titled = search("gateway");
    assert_eq!(ids(&titled), [gateway]);

    assert_eq!(row(&search("tunnel"), newer)["usage"], json!({"days_used":0,"last_used":null}));
    fixture.call("memory.read", json!({"id":newer}), "ee_project", &["read"]).unwrap();
    let used = search("tunnel");
    let usage = &row(&used, newer)["usage"];
    assert_eq!(usage["days_used"], 1);
    assert!(usage["last_used"].as_str().is_some_and(is_day), "{usage}");
    assert_eq!(row(&used, older)["usage"]["days_used"], 0);
}
