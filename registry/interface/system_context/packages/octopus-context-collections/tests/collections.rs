use std::fs;
use std::path::PathBuf;

use octopus_context_collections::*;
use octopus_descriptors::ArtifactId;
use octopus_ledger::{ArtifactRef, ContentHash};
use serde_json::{Value, json};
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    folder: PathBuf,
    descriptor: Value,
    source: ArtifactRef,
}

impl Fixture {
    fn new() -> Self {
        Self::limits(4096, 262144, 65536, 16 * 1024 * 1024)
    }
    fn limits(count: u32, entry_bytes: u64, page_bytes: u64, scan_bytes: u64) -> Self {
        let temp = tempfile::tempdir().unwrap();
        Self::with_temp(temp, count, entry_bytes, page_bytes, scan_bytes)
    }
    fn with_temp(
        temp: TempDir,
        count: u32,
        entry_bytes: u64,
        page_bytes: u64,
        scan_bytes: u64,
    ) -> Self {
        let root = temp.path().canonicalize().unwrap().join("artifact");
        let folder = root.join("source");
        fs::create_dir_all(&folder).unwrap();
        let descriptor = json!({"id":"source","label":"context.collection","schema_version":1,"description":"Project publications","directory":"entries","limits":{"max_entries":count,"max_entry_bytes":entry_bytes,"max_page_bytes":page_bytes,"max_scan_bytes":scan_bytes}});
        fs::write(
            folder.join("artifact.json"),
            serde_json::to_vec_pretty(&descriptor).unwrap(),
        )
        .unwrap();
        let source = ArtifactRef {
            id: ArtifactId::new("source"),
            content: ContentHash::of_canonical_json(&descriptor),
        };
        Self {
            _temp: temp,
            root,
            folder,
            descriptor,
            source,
        }
    }
    fn open(&self) -> BoundCollection {
        BoundCollection::open(&self.root, &self.source, &self.descriptor).unwrap()
    }
    fn store(&self, value: &Value) -> CollectionSelection {
        fs::create_dir_all(self.folder.join("entries")).unwrap();
        let id = value["id"].as_str().unwrap();
        fs::write(
            self.folder.join("entries").join(format!("{id}.json")),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
        CollectionSelection {
            source: self.source.clone(),
            entry_id: id.to_string(),
            revision: entry_revision(value).unwrap(),
        }
    }
}

fn scope() -> ScopeContext {
    ScopeContext {
        ee_id: "ee_project".into(),
        worker_interface: "system_worker_task".into(),
    }
}
fn request(limit: u32) -> ListRequest {
    ListRequest {
        query: None,
        limit,
        cursor: None,
    }
}
fn entry(id: &str) -> Value {
    json!({"schema_version":1,"id":id,"title":format!("Procedure {id}"),"description":"A reusable project procedure","applicability":"Use when reviewing this project","state":"active","scope":{},"content":{"text":"Authored body, available through explicit read."},"selection":{}})
}
fn apply(plan: &PublicationPlan) {
    for write in &plan.file_write.writes {
        let before = fs::read_to_string(&write.path).ok();
        assert_eq!(before, write.expected_before_content);
        assert_eq!(
            before
                .as_ref()
                .map(|bytes| ContentHash::of_bytes(bytes.as_bytes()).to_hex()),
            write.expected_before_hash
        );
        fs::create_dir_all(write.path.parent().unwrap()).unwrap();
        fs::write(&write.path, &write.content).unwrap();
    }
}

#[test]
fn empty_bootstrap_is_read_only_and_only_missing_entries_is_empty() {
    let f = Fixture::new();
    let c = f.open();
    let page = c.list(&scope(), &request(10)).unwrap();
    assert!(page.entries.is_empty());
    assert!(!page.has_more);
    assert!(page.cursor.is_none());
    let plan = c
        .prepare_publication(&parse_entry(&entry("first")).unwrap(), None, &scope())
        .unwrap();
    assert!(!f.folder.join("entries").exists());
    let wire = serde_json::to_value(&plan.file_write).unwrap();
    assert_eq!(wire["ordered"], true);
    assert_eq!(wire["writes"][0]["expected_before_hash"], Value::Null);
    assert_eq!(wire["writes"][0]["expected_before_content"], Value::Null);
    apply(&plan);
    assert_eq!(
        c.read(&plan.pin, &scope()).unwrap().entry["content"]["text"],
        entry("first")["content"]["text"]
    );
    fs::remove_file(f.folder.join("artifact.json")).unwrap();
    assert!(c.list(&scope(), &request(10)).is_err());
}

#[test]
fn source_requires_bound_and_disk_canonical_identity() {
    let f = Fixture::new();
    assert_eq!(read_descriptor(&f.root, &f.source).unwrap(), f.descriptor);
    let mut forged = f.source.clone();
    forged.content = ContentHash::of_bytes(b"forged");
    assert!(matches!(
        BoundCollection::open(&f.root, &forged, &f.descriptor),
        Err(CollectionError::SourceMismatch)
    ));
    assert!(matches!(
        read_descriptor(&f.root, &forged),
        Err(CollectionError::SourceMismatch)
    ));
    let c = f.open();
    let mut changed = f.descriptor.clone();
    changed["description"] = json!("Changed");
    fs::write(
        f.folder.join("artifact.json"),
        serde_json::to_vec(&changed).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        c.list(&scope(), &request(10)),
        Err(CollectionError::SourceMismatch)
    ));
    assert!(matches!(
        BoundCollection::open(&f.root, &f.source, &f.descriptor),
        Err(CollectionError::SourceMismatch)
    ));
    assert!(matches!(
        read_descriptor(&f.root, &f.source),
        Err(CollectionError::SourceMismatch)
    ));
}

#[test]
fn descriptor_helper_has_its_own_byte_bound_and_never_scans_entries() {
    let f = Fixture::new();
    fs::create_dir(f.folder.join("entries")).unwrap();
    fs::write(f.folder.join("entries/invalid.json"), b"not JSON").unwrap();
    assert_eq!(read_descriptor(&f.root, &f.source).unwrap(), f.descriptor);
    let mut descriptor = f.descriptor.clone();
    descriptor["text"] = json!("x".repeat(MAX_DESCRIPTOR_BYTES as usize));
    fs::write(
        f.folder.join("artifact.json"),
        serde_json::to_vec(&descriptor).unwrap(),
    )
    .unwrap();
    let source = ArtifactRef {
        content: ContentHash::of_canonical_json(&descriptor),
        ..f.source
    };
    assert!(matches!(
        read_descriptor(&f.root, &source),
        Err(CollectionError::LimitExceeded("file bytes"))
    ));
}

#[test]
fn held_source_refuses_replaced_owner_directory() {
    let f = Fixture::new();
    let pin = f.store(&entry("retained"));
    let collection = f.open();
    fs::rename(&f.folder, f.root.join("retired")).unwrap();
    fs::create_dir(&f.folder).unwrap();
    fs::write(
        f.folder.join("artifact.json"),
        serde_json::to_vec(&f.descriptor).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        collection.list(&scope(), &request(10)),
        Err(CollectionError::ChangedDuringRead)
    ));
    assert!(matches!(
        collection.read(&pin, &scope()),
        Err(CollectionError::ChangedDuringRead)
    ));
    assert!(
        f.open()
            .list(&scope(), &request(10))
            .unwrap()
            .entries
            .is_empty()
    );
}

#[test]
fn strict_schema_has_no_path_source_or_extra_registry_axis() {
    for field in ["source", "path", "directory", "provider"] {
        let mut value = entry("valid");
        value[field] = json!("forged");
        assert!(parse_entry(&value).is_err(), "{field}");
    }
    for field in ["title", "description", "applicability"] {
        let mut value = entry("valid");
        value[field] = json!(" \n ");
        assert!(parse_entry(&value).is_err());
    }
    for id in ["..", "a/b", "a\\b", " leading", "trailing ", "bad\0id"] {
        let mut value = entry("valid");
        value["id"] = json!(id);
        assert!(parse_entry(&value).is_err());
    }
    let mut value = entry("valid");
    value["scope"]["ee_ids"] = Value::Null;
    assert!(parse_entry(&value).is_err());
    value = entry("valid");
    value["content"]["text"] = json!("\n");
    assert!(parse_entry(&value).is_err());
    value = entry("valid");
    value["selection"]["registry_axis"] = json!([]);
    assert!(parse_entry(&value).is_err());
    value = entry("valid");
    value["requires"] = json!([{"kind":"skill","id":"recipe"}]);
    assert!(parse_entry(&value).is_err());
    value = entry("metadata-only");
    value.as_object_mut().unwrap().remove("content");
    assert!(parse_entry(&value).is_ok());
    let f = Fixture::new();
    let mut descriptor = f.descriptor.clone();
    descriptor["description"] = json!(" ");
    assert!(parse_descriptor(&descriptor).is_err());
}

#[test]
fn metadata_discovery_does_not_follow_external_refs_or_expose_authored_body() {
    let f = Fixture::new();
    let mut value = entry("procedure");
    value["content"]["text"] = json!("BodyOnlyNeedle");
    value["content"]["evidence"] = json!([ArtifactRef {
        id: ArtifactId::new("absent-external"),
        content: ContentHash::of_bytes(b"unavailable")
    }]);
    value["selection"] = json!({"context_artifact_bindings":[{"id":"absent-external"}],"door_host_slots":{"future_door":{"future_slot":"/explicit/authoring/path"}},"model_outputs":[]});
    value["requires"] = json!([{"kind":"tool","id":"uninstalled-tool"}]);
    let pin = f.store(&value);
    let c = f.open();
    let page = c.list(&scope(), &request(10)).unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].requires[0].id, "uninstalled-tool");
    assert!(
        !serde_json::to_string(&page)
            .unwrap()
            .contains("BodyOnlyNeedle")
    );
    assert_eq!(c.read(&pin, &scope()).unwrap().entry, value);
    assert_eq!(
        c.resolve_metadata(&pin, &scope()).unwrap().selection["external_inputs"],
        json!([])
    );
    let mut query = request(10);
    query.query = Some("BodyOnlyNeedle".into());
    assert!(c.list(&scope(), &query).unwrap().entries.is_empty());
    value["content"]["evidence"][0]["path"] = json!("/forged");
    assert!(parse_entry(&value).is_err());
}

#[test]
fn scope_narrows_source_and_hides_revision_before_staleness() {
    let f = Fixture::new();
    let c = f.open();
    let mut hidden = entry("hidden");
    hidden["scope"] = json!({"ee_ids":["ee_other"]});
    let mut pin = f.store(&hidden);
    pin.revision = ContentHash::of_bytes(b"wrong-revision");
    assert!(matches!(
        c.read(&pin, &scope()),
        Err(CollectionError::NotVisible(_))
    ));
    assert!(matches!(
        c.resolve_metadata(&pin, &scope()),
        Err(CollectionError::NotVisible(_))
    ));
    assert!(matches!(
        c.prepare_publication(
            &parse_entry(&entry("hidden")).unwrap(),
            Some(pin.revision),
            &scope()
        ),
        Err(CollectionError::NotVisible(_))
    ));
    let mut denied = entry("denied");
    denied["scope"] = json!({"ee_ids":["another-ee"]});
    f.store(&denied);
    let mut worker = entry("worker-private");
    worker["scope"] = json!({"worker_interfaces":["another-worker"]});
    f.store(&worker);
    f.store(&entry("unrestricted"));
    assert_eq!(
        c.list(&scope(), &request(10))
            .unwrap()
            .entries
            .iter()
            .map(|entry| entry.pin.entry_id.as_str())
            .collect::<Vec<_>>(),
        vec!["unrestricted"]
    );
    let mut destination = entry("new-other-scope");
    destination["scope"] = json!({"ee_ids":["ee_other"]});
    assert!(
        c.prepare_publication(&parse_entry(&destination).unwrap(), None, &scope())
            .is_ok()
    );
}

#[test]
fn pagination_pins_visible_query_scope_snapshot_and_survives_hidden_changes() {
    let f = Fixture::new();
    for id in ["a", "b", "c"] {
        f.store(&entry(id));
    }
    let c = f.open();
    let first = c.list(&scope(), &request(1)).unwrap();
    assert!(first.has_more);
    let mut next = request(1);
    next.cursor = first.cursor.clone();
    let mut hidden = entry("hidden");
    hidden["scope"] = json!({"ee_ids":["another-ee"]});
    f.store(&hidden);
    let second = c.list(&scope(), &next).unwrap();
    assert_eq!(second.snapshot, first.snapshot);
    assert_eq!(second.entries[0].pin.entry_id, "b");
    hidden["content"]["text"] = json!("Private update");
    f.store(&hidden);
    assert!(c.list(&scope(), &next).is_ok());
    let mut query = next.clone();
    query.query = Some("a".into());
    assert!(matches!(
        c.list(&scope(), &query),
        Err(CollectionError::StaleCursor)
    ));
    let other = ScopeContext {
        ee_id: "another-ee".into(),
        ..scope()
    };
    assert!(matches!(
        c.list(&other, &next),
        Err(CollectionError::StaleCursor)
    ));
    let mut changed = entry("b");
    changed["content"]["text"] = json!("Public revision changed");
    f.store(&changed);
    assert!(matches!(
        c.list(&scope(), &next),
        Err(CollectionError::StaleCursor)
    ));
}

#[test]
fn revision_is_canonical_whole_entry_and_updates_guard_exact_preimage() {
    let f = Fixture::new();
    let value = entry("update");
    let pin = f.store(&value);
    let c = f.open();
    let path = f.folder.join("entries/update.json");
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(
        c.read(&pin, &scope()).unwrap().pin.revision,
        ContentHash::of_canonical_json(&value)
    );
    let mut updated = value.clone();
    updated["applicability"] = json!("Updated applicability");
    let typed = parse_entry(&updated).unwrap();
    assert!(matches!(
        c.prepare_publication(&typed, None, &scope()),
        Err(CollectionError::AlreadyExists(_))
    ));
    assert!(matches!(
        c.prepare_publication(&typed, Some(ContentHash::of_bytes(b"stale")), &scope()),
        Err(CollectionError::StaleSelection)
    ));
    let plan = c
        .prepare_publication(&typed, Some(pin.revision), &scope())
        .unwrap();
    assert_eq!(
        plan.file_write.writes[0].expected_before_content.as_deref(),
        Some(fs::read_to_string(&path).unwrap().as_str())
    );
    assert_eq!(
        fs::read(&path).unwrap(),
        serde_json::to_vec(&value).unwrap()
    );
    apply(&plan);
    assert!(matches!(
        c.read(&pin, &scope()),
        Err(CollectionError::StaleSelection)
    ));
    assert!(c.read(&plan.pin, &scope()).is_ok());
    let wire = serde_json::to_value(&plan.pin).unwrap();
    assert_eq!(wire["revision"], plan.pin.revision.to_hex());
    assert_eq!(
        serde_json::from_value::<CollectionSelection>(wire).unwrap(),
        plan.pin
    );
}

#[test]
fn withdrawal_is_publication_without_removing_old_evidence() {
    let f = Fixture::new();
    let value = entry("withdraw");
    let old = f.store(&value);
    let c = f.open();
    let mut withdrawn = parse_entry(&value).unwrap();
    withdrawn.state = PublicationState::Withdrawn;
    let plan = c
        .prepare_publication(&withdrawn, Some(old.revision), &scope())
        .unwrap();
    apply(&plan);
    assert!(c.list(&scope(), &request(10)).unwrap().entries.is_empty());
    assert!(matches!(
        c.read(&old, &scope()),
        Err(CollectionError::NotVisible(_))
    ));
    assert!(f.folder.join("entries/withdraw.json").is_file());
}

#[test]
fn scan_count_byte_and_page_limits_refuse_without_partial_success() {
    let count = Fixture::limits(1, 262144, 65536, 16 * 1024 * 1024);
    count.store(&entry("a"));
    count.store(&entry("b"));
    assert!(matches!(
        count.open().list(&scope(), &request(1)),
        Err(CollectionError::LimitExceeded("entry count"))
    ));
    let byte = Fixture::limits(20, 1024, 65536, 16 * 1024 * 1024);
    let mut large = entry("large");
    large["content"]["text"] = json!("x".repeat(2048));
    byte.store(&large);
    assert!(matches!(
        byte.open().list(&scope(), &request(10)),
        Err(CollectionError::LimitExceeded(_))
    ));
    let scan = Fixture::limits(20, 262144, 65536, 500);
    scan.store(&entry("a"));
    scan.store(&entry("b"));
    assert!(matches!(
        scan.open().list(&scope(), &request(10)),
        Err(CollectionError::LimitExceeded("scan bytes"))
    ));
    let page = Fixture::limits(20, 262144, 1500, 16 * 1024 * 1024);
    for id in ["a", "b", "c"] {
        page.store(&entry(id));
    }
    let first = page.open().list(&scope(), &request(10)).unwrap();
    assert!(first.has_more);
    assert!(!first.entries.is_empty());
    assert!(serde_json::to_vec(&first).unwrap().len() <= 1500);
    let tiny = Fixture::limits(20, 262144, 100, 16 * 1024 * 1024);
    tiny.store(&entry("a"));
    assert!(matches!(
        tiny.open().list(&scope(), &request(10)),
        Err(CollectionError::LimitExceeded(_))
    ));
    let mut descriptor = scan.descriptor.clone();
    descriptor["limits"]["max_scan_bytes"] = json!(MAX_SCAN_BYTES + 1);
    assert!(parse_descriptor(&descriptor).is_err());
}

#[test]
fn publication_postimages_remain_scannable_and_pageable() {
    let base = parse_entry(&entry("a")).unwrap();
    let bytes =
        octopus_descriptors::canonical_bytes(&serde_json::to_value(&base).unwrap()).len() as u64;
    let too_small = Fixture::limits(8, 262144, 65536, bytes - 1);
    assert!(matches!(
        too_small.open().prepare_publication(&base, None, &scope()),
        Err(CollectionError::LimitExceeded("scan bytes"))
    ));
    assert!(!too_small.folder.join("entries").exists());
    let full = Fixture::limits(8, 262144, 65536, bytes * 2);
    let c = full.open();
    let first = c.prepare_publication(&base, None, &scope()).unwrap();
    apply(&first);
    let second = c
        .prepare_publication(&parse_entry(&entry("b")).unwrap(), None, &scope())
        .unwrap();
    apply(&second);
    assert!(matches!(
        c.prepare_publication(&parse_entry(&entry("c")).unwrap(), None, &scope()),
        Err(CollectionError::LimitExceeded("scan bytes"))
    ));
    let mut grown = base.clone();
    grown.content.as_mut().unwrap().text = Some("x".repeat(500));
    assert!(matches!(
        c.prepare_publication(&grown, Some(first.pin.revision), &scope()),
        Err(CollectionError::LimitExceeded("scan bytes"))
    ));
    grown.content.as_mut().unwrap().text = Some("Short".into());
    let smaller = c
        .prepare_publication(&grown, Some(first.pin.revision), &scope())
        .unwrap();
    apply(&smaller);
    assert!(c.read(&smaller.pin, &scope()).is_ok());
    assert_eq!(c.list(&scope(), &request(8)).unwrap().entries.len(), 2);

    let pages = Fixture::limits(8, 262144, 1500, 16 * 1024 * 1024);
    let c = pages.open();
    let mut candidate = base;
    let mut last = None;
    for length in 1..1500 {
        candidate.title = "x".repeat(length);
        match c.prepare_publication(&candidate, None, &scope()) {
            Ok(plan) => last = Some(plan),
            Err(CollectionError::LimitExceeded("one summary exceeds page bytes")) => break,
            other => panic!("unexpected preparation outcome: {other:?}"),
        }
    }
    let boundary = last.unwrap();
    apply(&boundary);
    let next = c
        .prepare_publication(&parse_entry(&entry("b")).unwrap(), None, &scope())
        .unwrap();
    apply(&next);
    let first = c.list(&scope(), &request(1)).unwrap();
    assert!(first.has_more && first.cursor.is_some());
    assert_eq!(first.entries[0].pin, boundary.pin);
    assert!(serde_json::to_vec(&first).unwrap().len() <= 1500);
    let tail = c
        .list(
            &scope(),
            &ListRequest {
                cursor: first.cursor,
                ..request(1)
            },
        )
        .unwrap();
    assert_eq!(tail.entries[0].pin, next.pin);
    assert!(c.read(&boundary.pin, &scope()).is_ok());
}

#[test]
fn hidden_integrity_errors_do_not_return_private_metadata_or_entry_paths() {
    let f = Fixture::new();
    let mut hidden = entry("hidden");
    hidden["scope"] = json!({"ee_ids":["another-ee"]});
    f.store(&hidden);
    hidden["PrivateMarker"] = json!("PrivateBody");
    fs::write(
        f.folder.join("entries/hidden.json"),
        serde_json::to_vec(&hidden).unwrap(),
    )
    .unwrap();
    let error = f.open().list(&scope(), &request(10)).unwrap_err();
    assert!(matches!(error, CollectionError::SourceUnavailable));
    for private in ["PrivateMarker", "PrivateBody", "hidden.json"] {
        assert!(!error.to_string().contains(private));
    }
}

#[cfg(unix)]
#[test]
fn no_symlink_root_collection_directory_leaf_or_foreign_subdirectory() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let other = f.root.parent().unwrap().join("foreign");
    fs::create_dir(&other).unwrap();
    symlink(&other, f.folder.join("entries")).unwrap();
    assert!(matches!(
        f.open().list(&scope(), &request(10)),
        Err(CollectionError::SourceUnavailable)
    ));
    fs::remove_file(f.folder.join("entries")).unwrap();
    fs::create_dir(f.folder.join("entries")).unwrap();
    fs::write(
        other.join("external.json"),
        serde_json::to_vec(&entry("external")).unwrap(),
    )
    .unwrap();
    symlink(
        other.join("external.json"),
        f.folder.join("entries/external.json"),
    )
    .unwrap();
    assert!(matches!(
        f.open().list(&scope(), &request(10)),
        Err(CollectionError::SourceUnavailable)
    ));
    fs::remove_file(f.folder.join("entries/external.json")).unwrap();
    fs::create_dir(f.folder.join("entries/subdirectory.json")).unwrap();
    assert!(matches!(
        f.open().list(&scope(), &request(10)),
        Err(CollectionError::SourceUnavailable)
    ));
    let alias = f.root.parent().unwrap().join("alias");
    symlink(&f.root, &alias).unwrap();
    assert!(matches!(
        BoundCollection::open(&alias, &f.source, &f.descriptor),
        Err(CollectionError::UnsafePath(_))
    ));
    fs::rename(&f.folder, f.root.join("original")).unwrap();
    symlink(f.root.join("original"), &f.folder).unwrap();
    assert!(matches!(
        BoundCollection::open(&f.root, &f.source, &f.descriptor),
        Err(CollectionError::UnsafePath(_))
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn sandboxed_collection_requires_no_ancestor_directory_read() {
    use std::os::unix::fs::{OpenOptionsExt, symlink};
    use std::process::Command;

    const CHILD: &str = "OCTOPUS_COLLECTION_SANDBOX_INPUT";
    const PROOF: &str = "collection sandbox: ancestor read denied; discovery/read/prepare passed";
    if let Ok(input) = std::env::var(CHILD) {
        let input: Value = serde_json::from_str(&input).unwrap();
        let root = PathBuf::from(input["root"].as_str().unwrap());
        let source: ArtifactRef = serde_json::from_value(input["source"].clone()).unwrap();
        let payload = &input["payload"];
        let parent = root.parent().unwrap();
        let denied = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(parent)
            .unwrap_err();
        assert_eq!(denied.raw_os_error(), Some(libc::EPERM));
        assert_eq!(
            fs::read_dir(parent).unwrap_err().raw_os_error(),
            Some(libc::EPERM)
        );
        assert_eq!(
            fs::read(parent.join("outside.txt"))
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EPERM)
        );
        assert_eq!(read_descriptor(&root, &source).unwrap(), *payload);
        let collection = BoundCollection::open(&root, &source, payload).unwrap();
        let page = collection.list(&scope(), &request(8)).unwrap();
        assert_eq!(page.entries.len(), 1);
        assert_eq!(
            collection
                .read(&page.entries[0].pin, &scope())
                .unwrap()
                .entry,
            entry("original")
        );
        let plan = collection
            .prepare_publication(&parse_entry(&entry("next")).unwrap(), None, &scope())
            .unwrap();
        assert_eq!(
            plan.file_write.writes[0].path,
            root.join("source/entries/next.json")
        );
        assert!(!plan.file_write.writes[0].path.exists());
        assert_eq!(
            fs::write(root.join("source/forbidden.txt"), b"no")
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EPERM)
        );
        assert!(matches!(
            BoundCollection::open(&root.join("alias"), &source, payload),
            Err(CollectionError::UnsafePath(_))
        ));
        println!("{PROOF}");
        return;
    }

    let temp = tempfile::Builder::new()
        .prefix("collection-sandbox-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .unwrap();
    let f = Fixture::with_temp(temp, 8, 16384, 32768, 131072);
    f.store(&entry("original"));
    fs::write(
        f.root.parent().unwrap().join("outside.txt"),
        b"outside the granted root",
    )
    .unwrap();
    symlink(&f.root, f.root.join("alias")).unwrap();
    let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
    let profile = format!(
        "{}\n{}\n(allow file-read* file-test-existence (literal {}) (subpath {}))",
        include_str!(
            "../../../../../worker_environment_kind/process_session/packages/octopus-sandbox-substrate/src/seatbelt_base_policy.sbpl"
        ),
        include_str!(
            "../../../../../worker_environment_kind/process_session/packages/octopus-sandbox-substrate/src/restricted_read_only_platform_defaults.sbpl"
        ),
        serde_json::to_string(executable.to_str().unwrap()).unwrap(),
        serde_json::to_string(f.root.to_str().unwrap()).unwrap(),
    );
    let output = Command::new("/usr/bin/sandbox-exec")
        .args(["-p", &profile])
        .arg(&executable)
        .args([
            "--exact",
            "sandboxed_collection_requires_no_ancestor_directory_read",
            "--nocapture",
            "--test-threads=1",
        ])
        .current_dir(&f.root)
        .env_clear()
        .env(
            CHILD,
            json!({"root":f.root,"source":f.source,"payload":f.descriptor}).to_string(),
        )
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains(PROOF), "{stdout}");
    assert!(!f.folder.join("entries/next.json").exists());
    assert!(!f.folder.join("forbidden.txt").exists());
}
