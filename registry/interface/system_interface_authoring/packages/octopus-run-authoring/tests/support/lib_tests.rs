    use super::*;
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/ordered_source_replay.rs"));
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/independent_provider.rs"));
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/one_shot_progress.rs"));
    use octopus_descriptors::{EeId, VerifierId};
    use octopus_interfaces::{GrantProjection, RuntimeInvocationMetadata};
    use octopus_ledger::{MovementDraft, MovementLog, Seq};
    use octopus_ledger_index::LedgerSqliteIndex;

    #[test]
    fn reconciled_result_input_keeps_its_place_before_its_outputs() {
        let entry = |label: &str, seq: u64| SessionContextArtifact {
            label: label.into(),
            payload: serde_json::json!({"label": label}),
            source_artifact: ArtifactRef { id: ArtifactId::new(format!("{label}:{seq}")), content: ContentHash::of_bytes(label.as_bytes()) },
            source_movement_seq: Some(seq),
        };
        let mut state = HandoffState {
            validated_withdrawal_output: None,
            context_artifacts: Vec::new(),
            static_context_artifacts: Vec::new(),
            session_context: ResolvedSessionContext {
                provider_interface: InterfaceId::new("world_provider_test"),
                artifacts: vec![entry("operator.message", 3), entry("tool.result", 10), entry("turn.reasoning", 12), entry("turn.agent_message", 14)],
            },
            session_snapshot_head: None,
            run_record_bytes: Vec::new(),
            run_record_evidence_seq: None,
            consumed_input: true,
            consumed_context_free_independent_input: None,
            consumed_verified_input: false,
            deferred_context_generations: 0,
        };
        reattach_unpublished_context_inputs(&mut state, 10, vec![ArtifactId::new("operator.message:3")]).unwrap();
        assert_eq!(state.session_context.artifacts.iter().map(|entry| entry.source_movement_seq.unwrap()).collect::<Vec<_>>(), vec![3, 10, 12, 14]);
        assert_eq!(state.context_artifacts, vec![ArtifactId::new("operator.message:3"), ArtifactId::new("tool.result:10")]);
        assert!(state.consumed_input);
    }

    fn test_verifier_package_pin() -> octopus_ledger::VerifierPackagePin {
        octopus_ledger::VerifierPackagePin {
            descriptor_content: octopus_ledger::ContentHash::of_bytes(b"test verifier descriptor"),
            script_content: octopus_ledger::ContentHash::of_bytes(b"test verifier script"),
        }
    }

    fn all_binding(interface: impl Into<String>) -> AuthoredInterfaceBinding {
        AuthoredInterfaceBinding::all(InterfaceId::new(interface))
    }

    fn names_binding(interface: impl Into<String>, names: &[&str]) -> AuthoredInterfaceBinding {
        AuthoredInterfaceBinding {
            interface: InterfaceId::new(interface),
            config: octopus_policy::InterfaceBindingConfig {
                artifact_policy: BindingArtifactPolicy::Allow {
                    names: names.iter().map(|name| (*name).to_string()).collect(),
                    namespaces: Vec::new(),
                },
            },
        }
    }

    struct SnapshotMovementLog {
        rows: Vec<Movement>,
        durable_end_offset: u64,
        row_ranges: Vec<(u64, u64)>,
    }

    impl MovementLog for SnapshotMovementLog {
        fn append(&mut self, _draft: MovementDraft) -> Movement {
            unreachable!()
        }

        fn read_from(&self, from: Seq) -> Vec<Movement> {
            self.rows
                .iter()
                .filter(|movement| movement.seq >= from)
                .cloned()
                .collect()
        }

        fn len(&self) -> usize {
            self.rows.len()
        }

        fn durable_end_offset(&self) -> Option<u64> {
            Some(self.durable_end_offset)
        }

        fn durable_row_byte_range(&self, seq: Seq) -> Option<(u64, u64)> {
            self.row_ranges.get(seq.0 as usize).copied()
        }
    }

    fn typed_invocation_metadata(
        run: &RunId,
        checkpoint: &str,
        interface: &InterfaceId,
        epoch: u64,
    ) -> InvocationMetadata {
        InvocationMetadata::new(GrantProjection::default()).with_runtime(
            RuntimeInvocationMetadata {
                run: run.clone(),
                ee: EeId::new("ee_test"),
                worker: InterfaceId::new("system_worker_test"),
                checkpoint: CheckpointId::new(checkpoint),
                interface: interface.clone(),
                epoch,
                run_record_hash: None,
            },
        )
    }

    #[test]
    fn checked_prefix_stream_preserves_cursor_and_requires_every_row() {
        let dir = tempfile::tempdir().unwrap();
        let movements = dir.path().join("movements.jsonl");
        let index = dir.path().join("absent-index.sqlite");
        let blobs = dir.path().join("blobs");
        let bytes = (0..15_000).map(|seq| format!("{}\n", serde_json::to_string(&ledger_row(seq)).unwrap())).collect::<String>();
        std::fs::write(&movements, &bytes).unwrap();
        let evidence = CheckedRunEvidence { movements_path:&movements, ledger_index_path:&index, blobs_dir:&blobs,
            max_control_frame_bytes:1<<20, max_manifest_bytes:1<<20, max_payload_bytes:1<<26 };
        let mut reader = FileCheckedRunReader::open(&evidence, None).unwrap();
        reader.movement(0).unwrap().unwrap();
        let position = reader.reader.stream_position().unwrap();
        let started = std::time::Instant::now();
        let mut count = 0;
        reader.visit_admitted_prefix(15_000, &mut |row| { assert_eq!(row.seq.0, count); count += 1; Ok(()) }).unwrap();
        eprintln!("checked prefix: {count} rows without SQLite in {:?}", started.elapsed());
        assert_eq!((count, reader.next_sequence(), reader.reader.stream_position().unwrap(), reader.snapshot_bytes), (15_000, 1, position, 0));
        assert_eq!(reader.movement(1).unwrap().unwrap().seq.0, 1);
        for suffix in ["".to_string(), "{".to_string(), format!("{}\n", serde_json::to_string(&ledger_row(2)).unwrap())] {
            std::fs::write(&movements, format!("{}\n{suffix}", serde_json::to_string(&ledger_row(0)).unwrap())).unwrap();
            reader.visit_admitted_prefix(1, &mut |_| Ok(())).unwrap();
            assert!(reader.visit_admitted_prefix(2, &mut |_| Ok(())).is_err());
            assert_eq!(reader.next_sequence(), 2);
        }
    }

    fn transport_dispatch_payload(
        method: &str,
        transport: &str,
        source_movement_seq: u64,
        bytes: u64,
        digest: String,
        fingerprint: Option<&str>,
    ) -> Vec<u8> {
        let (scheme, host) = match transport {
            "websocket" => ("wss", "example.test"),
            "process" => ("process", "codex_app_server_cli"),
            "local_ipc" => ("unix", ""),
            _ => ("https", "example.test"),
        };
        serde_json::to_vec(&serde_json::json!({
            "label": PROVIDER_TRANSPORT_DISPATCH_LABEL,
            "provider_interface": "world_provider_test",
            "source_movement_seq": source_movement_seq,
            "attempt": 1,
            "method": method,
            "transport": transport,
            "endpoint": {
                "scheme": scheme,
                "host": host,
                "path": "/v1/dispatch"
            },
            "request": {
                "bytes": bytes,
                "blake3": digest
            },
            "credential_handle_fingerprint": fingerprint
        }))
        .unwrap()
    }

    #[test]
    fn transport_dispatch_contract_is_generic_across_network_process_and_local_ipc() {
        let provider = InterfaceId::new("world_provider_test");
        let empty = blake3::hash(&[]).to_hex().to_string();
        let get = transport_dispatch_payload("GET", "http", 7, 0, empty, Some(&"a".repeat(64)));
        validate_transport_dispatch_payload(&get, &provider, 7).unwrap();

        let request = br#"{"type":"response.create"}"#;
        let websocket = transport_dispatch_payload(
            "response.create",
            "websocket",
            7,
            request.len() as u64,
            blake3::hash(request).to_hex().to_string(),
            None,
        );
        validate_transport_dispatch_payload(&websocket, &provider, 7).unwrap();
        let malformed_websocket = transport_dispatch_payload(
            "response..create",
            "websocket",
            7,
            request.len() as u64,
            blake3::hash(request).to_hex().to_string(),
            None,
        );
        assert!(
            validate_transport_dispatch_payload(&malformed_websocket, &provider, 7)
                .err()
                .unwrap()
                .contains("canonical lowercase event token")
        );
        let alternate_websocket = transport_dispatch_payload(
            "response.cancel",
            "websocket",
            7,
            request.len() as u64,
            blake3::hash(request).to_hex().to_string(),
            None,
        );
        validate_transport_dispatch_payload(&alternate_websocket, &provider, 7).unwrap();
        let process = transport_dispatch_payload(
            "thread/start",
            "process",
            7,
            request.len() as u64,
            blake3::hash(request).to_hex().to_string(),
            None,
        );
        validate_transport_dispatch_payload(&process, &provider, 7).unwrap();
        let local_ipc = transport_dispatch_payload(
            "session/prompt",
            "local_ipc",
            7,
            request.len() as u64,
            blake3::hash(request).to_hex().to_string(),
            None,
        );
        validate_transport_dispatch_payload(&local_ipc, &provider, 7).unwrap();

        let bad_get = transport_dispatch_payload(
            "GET",
            "http",
            7,
            0,
            blake3::hash(b"not empty").to_hex().to_string(),
            None,
        );
        assert!(
            validate_transport_dispatch_payload(&bad_get, &provider, 7)
                .err()
                .unwrap()
                .contains("canonical empty request body")
        );
        let bad_fingerprint = transport_dispatch_payload(
            "POST",
            "http",
            7,
            1,
            blake3::hash(b"x").to_hex().to_string(),
            Some("raw-secret-handle"),
        );
        assert!(
            validate_transport_dispatch_payload(&bad_fingerprint, &provider, 7)
                .err()
                .unwrap()
                .contains("credential_handle_fingerprint")
        );
    }

    fn ledger_row(seq: u64) -> Movement {
        Movement {
            seq: octopus_ledger::Seq(seq),
            lamport: octopus_ledger::Lamport(seq + 1),
            recorded_at_unix_ms: seq + 1,
            trace: octopus_ledger::TraceId(format!("trace-index-{seq}")),
            kind: MovementKind::ArtifactMoved,
            from: Endpoint::Interface(InterfaceId::new("world_provider_test")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_test")),
            artifact: None,
            invocation_metadata: None,
        }
    }

    #[test]
    fn writer_edge_candidates_keep_unattributed_rows_and_validate_ledger_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("movements.jsonl");
        let index_path = dir.path().join("ledger-index.sqlite");
        let source = InterfaceId::new("source");
        let door = InterfaceId::new("door");
        let mut rows: Vec<_> = (0..4).map(ledger_row).collect();
        rows[1].kind = MovementKind::InterfaceOutbound;
        rows[1].from = Endpoint::Interface(source.clone());
        rows[1].to = Endpoint::Interface(door.clone());
        rows[2].kind = MovementKind::InterfaceOutbound;
        rows[2].to = Endpoint::Interface(door.clone());
        let mut ledger = String::new();
        let mut row_ranges = Vec::new();
        for row in &rows {
            let start = ledger.len() as u64;
            ledger.push_str(&format!("{}\n", serde_json::to_string(row).unwrap()));
            row_ranges.push((start, ledger.len() as u64));
        }
        std::fs::write(&path, &ledger).unwrap();
        let mut index = LedgerSqliteIndex::open(&index_path).unwrap();
        index.ingest_from(&SnapshotMovementLog { rows, durable_end_offset: ledger.len() as u64, row_ranges }).unwrap();
        drop(index);
        let blobs = dir.path().join("blobs");
        let evidence = CheckedRunEvidence { movements_path: &path, ledger_index_path: &index_path, blobs_dir: &blobs,
            max_control_frame_bytes: 4096, max_manifest_bytes: 4096, max_payload_bytes: 4096 };
        let mut reader = FileCheckedRunReader::open(&evidence, None).unwrap();
        let selected = reader.indexed_outbound_edges(&[(&source, &door)]).unwrap().unwrap();
        assert_eq!(selected.iter().map(|row| row.seq).collect::<Vec<_>>(), [1, 3]);
        reader.reader.seek(std::io::SeekFrom::Start(selected[0].start_offset)).unwrap();
        reader.next_seq = selected[0].seq;
        assert!(reader.movement(1).unwrap().unwrap().invocation_metadata.is_none());
        let mut bytes = ledger.into_bytes();
        bytes[selected[0].start_offset as usize] = b'!';
        std::fs::write(&path, bytes).unwrap();
        let mut reader = FileCheckedRunReader::open(&evidence, None).unwrap();
        assert!(reader.indexed_outbound_edges(&[(&source, &door)]).unwrap().is_none());
        reader.movement(0).unwrap();
        assert!(reader.movement(1).is_err());
        std::fs::remove_file(&index_path).unwrap();
        let mut reader = FileCheckedRunReader::open(&evidence, None).unwrap();
        assert!(reader.indexed_outbound_edges(&[(&source, &door)]).unwrap().is_none());
    }

    #[test]
    fn selected_ledger_grants_preserve_authority_and_validate_unselected_content() {
        let dir = tempfile::tempdir().unwrap();
        let metadata = dir.path().join("metadata");
        std::fs::create_dir(&metadata).unwrap();
        let mut movement = ledger_row(0);
        movement.invocation_metadata = Some(InvocationMetadata::new(GrantProjection {
            tools: vec!["file.write".into()], interfaces: vec!["system_room".into()], ..Default::default()
        }).into());
        let row = octopus_ledger::encode_durable_movement_row(&movement, 0, |_, bytes| {
            let hash = ContentHash::of_bytes(bytes);
            std::fs::write(metadata.join(hash.to_hex()), bytes).unwrap(); Ok(hash)
        }).unwrap();
        let rows = ledger_rows::LedgerRows::detached(&dir.path().join("movements.jsonl"));
        assert_eq!(rows.decode_with_grant_if(row.as_bytes(), 0, |_| true).unwrap(), movement);
        let projected = rows.decode_with_grant_if(row.as_bytes(), 0, |_| false).unwrap();
        let grant = &projected.invocation_metadata.as_ref().unwrap().invocation().unwrap().grant;
        assert!(grant.tools.is_empty());
        assert_eq!(grant.interfaces, ["system_room"]);
        let blob = std::fs::read_dir(&metadata).unwrap().next().unwrap().unwrap().path();
        std::fs::remove_file(blob).unwrap();
        assert!(rows.decode_with_grant_if(row.as_bytes(), 0, |_| false).is_err());
    }

    #[test]
    fn retained_delivery_reads_own_grants_and_checks_the_global_sequence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("movements.jsonl");
        let metadata = dir.path().join("metadata");
        std::fs::create_dir(&metadata).unwrap();
        let source = InterfaceId::new("own");
        let mut foreign = ledger_row(0);
        foreign.from = Endpoint::Interface("other".into());
        foreign.invocation_metadata = Some(InvocationMetadata::new(Default::default()).into());
        let foreign_row = octopus_ledger::encode_durable_movement_row(&foreign, 0,
            |_, bytes| Ok(ContentHash::of_bytes(bytes))).unwrap();
        let mut own = ledger_row(1);
        own.from = Endpoint::Interface(source.clone());
        own.invocation_metadata = Some(InvocationMetadata::new(GrantProjection {
            tools: vec!["file.write".into()], ..Default::default()
        }).into());
        let own_row = octopus_ledger::encode_durable_movement_row(&own, 1, |_, bytes| {
            let hash = ContentHash::of_bytes(bytes);
            std::fs::write(metadata.join(hash.to_hex()), bytes).unwrap(); Ok(hash)
        }).unwrap();
        std::fs::write(&path, format!("{foreign_row}\n{own_row}\n")).unwrap();
        let mut projection = pending_delivery::Ledger::new(source.clone(), path.clone(), 1_048_576);
        assert_eq!(projection.watermark().unwrap(), 2);
        assert_eq!(projection.watermark().unwrap(), 2);
        let mut sparse = octopus_ledger_index::InterfaceArrivalProjection::new(source.clone());
        assert!(sparse.observe(&foreign).is_err());
        sparse.observe(&own).unwrap();
        assert!(sparse.observe(&own).is_err());
        let mut gap = serde_json::from_str::<serde_json::Value>(&foreign_row).unwrap();
        gap["seq"] = serde_json::json!(3);
        use std::io::Write as _;
        writeln!(std::fs::OpenOptions::new().append(true).open(&path).unwrap(), "{gap}").unwrap();
        assert!(projection.watermark().unwrap_err().contains("not contiguous"));
        std::fs::write(&path, format!("{foreign_row}\n{own_row}\n")).unwrap();
        let blob = std::fs::read_dir(metadata).unwrap().next().unwrap().unwrap().path();
        std::fs::write(blob, b"corrupt").unwrap();
        assert!(pending_delivery::Ledger::new(source, path, 1_048_576).watermark().is_err());
    }

    #[test]
    fn ledger_index_advances_only_complete_delta_and_poison_is_permanent() {
        use std::io::Write as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("movements.jsonl");
        let first = format!("{}\n", serde_json::to_string(&ledger_row(0)).unwrap());
        std::fs::write(&path, first.as_bytes()).unwrap();
        let mut index = ProviderLedgerEvidenceIndex::new(
            InterfaceId::new("world_provider_test"),
            path.clone(),
            1_048_576,
        );
        index.catch_up().unwrap();
        assert_eq!(index.next_seq, 1);
        assert_eq!(index.byte_offset, first.len() as u64);

        let second = serde_json::to_string(&ledger_row(1)).unwrap();
        let mut ledger = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        ledger.write_all(second.as_bytes()).unwrap();
        ledger.sync_all().unwrap();
        index.catch_up().unwrap();
        assert_eq!(index.next_seq, 1);
        assert_eq!(index.byte_offset, first.len() as u64);
        ledger.write_all(b"\n").unwrap();
        ledger.sync_all().unwrap();
        index.catch_up().unwrap();
        assert_eq!(index.next_seq, 2);

        drop(ledger);
        std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        let first_error = index.catch_up().unwrap_err();
        assert!(first_error.contains("shrank behind observed watermark"));
        assert_eq!(index.catch_up().unwrap_err(), first_error);

        let gap_path = dir.path().join("gap.jsonl");
        std::fs::write(
            &gap_path,
            format!("{}\n", serde_json::to_string(&ledger_row(1)).unwrap()),
        )
        .unwrap();
        let mut gap = ProviderLedgerEvidenceIndex::new(
            InterfaceId::new("world_provider_test"),
            gap_path,
            1_048_576,
        );
        assert!(
            gap.catch_up()
                .unwrap_err()
                .contains("expected movement seq 0, found 1")
        );

        let duplicate_path = dir.path().join("duplicate.jsonl");
        std::fs::write(
            &duplicate_path,
            format!(
                "{}\n{}\n",
                serde_json::to_string(&ledger_row(0)).unwrap(),
                serde_json::to_string(&ledger_row(0)).unwrap()
            ),
        )
        .unwrap();
        let mut duplicate = ProviderLedgerEvidenceIndex::new(
            InterfaceId::new("world_provider_test"),
            duplicate_path,
            1_048_576,
        );
        assert!(
            duplicate
                .catch_up()
                .unwrap_err()
                .contains("expected movement seq 1, found 0")
        );
    }

    #[test]
    fn provider_session_index_accepts_only_exact_interface_movement_provenance() {
        use octopus_ledger::{Lamport, Seq, TraceId};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("movements.jsonl");
        let artifact = ArtifactRef {
            id: ArtifactId::new("provider.output:exact"),
            content: ContentHash::of_bytes(b"provider output"),
        };
        let metadata = typed_invocation_metadata(
            &RunId::new("run_test"),
            "client_output",
            &InterfaceId::new("world_provider_test"),
            0,
        );
        let rows = [
            Movement {
                seq: Seq(0),
                lamport: Lamport(1),
                recorded_at_unix_ms: 1,
                trace: TraceId("trace-legacy-outbound".to_string()),
                kind: MovementKind::InterfaceOutbound,
                from: Endpoint::Interface(InterfaceId::new("world_provider_test")),
                to: Endpoint::Interface(InterfaceId::new("world_client_test")),
                artifact: Some(artifact.clone()),
                invocation_metadata: Some(metadata.clone().into()),
            },
            Movement {
                seq: Seq(1),
                lamport: Lamport(2),
                recorded_at_unix_ms: 2,
                trace: TraceId("x".repeat(8_192)),
                kind: MovementKind::InterfaceOutbound,
                from: Endpoint::Interface(InterfaceId::new("world_foreign")),
                to: Endpoint::Interface(InterfaceId::new("world_foreign_target")),
                artifact: Some(artifact.clone()),
                invocation_metadata: Some(DeliveryVerdict::Admitted { seq: 1 }.into()),
            },
            Movement {
                seq: Seq(2),
                lamport: Lamport(3),
                recorded_at_unix_ms: 3,
                trace: TraceId("trace-exact-outbound".to_string()),
                kind: MovementKind::InterfaceOutbound,
                from: Endpoint::Interface(InterfaceId::new("world_client_test")),
                to: Endpoint::Interface(InterfaceId::new("world_provider_test")),
                artifact: Some(artifact.clone()),
                invocation_metadata: Some(metadata.into()),
            },
        ];
        let mut ledger = String::new();
        let mut row_ranges = Vec::new();
        for row in &rows {
            let start = ledger.len() as u64;
            ledger.push_str(&serde_json::to_string(row).unwrap());
            ledger.push('\n');
            row_ranges.push((start, ledger.len() as u64));
        }
        std::fs::write(&path, &ledger).unwrap();
        let mut index = ProviderLedgerEvidenceIndex::new(
            InterfaceId::new("world_provider_test"),
            path.clone(),
            1_048_576,
        );
        let key = artifact_ref_key(&artifact);
        let evidence = index
            .session_evidence(
                &RunId::new("run_test"),
                &InterfaceId::new("world_provider_test"),
                std::iter::once(key.clone()),
            )
            .unwrap();
        assert_eq!(evidence.get(&key).unwrap().movements, BTreeSet::from([2]));
        let provenance = index
            .bound_provenance(
                &RunId::new("run_test"),
                &InterfaceId::new("world_provider_test"),
                std::iter::once(&artifact),
            )
            .unwrap();
        assert_eq!(provenance.get(&key).unwrap().movement_seq, Some(2));

        let spotlight_path = dir.path().join("ledger-index.sqlite");
        let mut spotlight = LedgerSqliteIndex::open(&spotlight_path).unwrap();
        spotlight
            .ingest_from(&SnapshotMovementLog {
                rows: rows.to_vec(),
                durable_end_offset: ledger.len() as u64,
                row_ranges,
            })
            .unwrap();
        drop(spotlight);
        let mut scoped = ProviderLedgerEvidenceIndex::with_spotlight(
            InterfaceId::new("world_provider_test"),
            path,
            4_096,
            &spotlight_path,
        );
        let evidence = scoped
            .session_evidence(
                &RunId::new("run_test"),
                &InterfaceId::new("world_provider_test"),
                std::iter::once(key.clone()),
            )
            .unwrap();
        assert_eq!(evidence.get(&key).unwrap().movements, BTreeSet::from([2]));
        assert_eq!(scoped.next_seq, 3);
        assert_eq!(scoped.byte_offset, ledger.len() as u64);

        let provider = InterfaceId::new("world_provider_test");
        let run = RunId::new("run_test");
        let worker = InterfaceId::new("system_worker_test");
        let checkpoint = CheckpointId::new("replacement");
        let mark_id = "provider.output.admitted";
        let mut prefix = one_shot_movement(0, &artifact, checkpoint.as_str(), MovementKind::InterfaceArrival);
        let mut metadata = prefix.invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
        metadata.runtime.as_mut().unwrap().run_record_hash = Some(ContentHash::of_bytes(b"original snapshot").to_hex());
        metadata.required_verified_artifacts = vec![serde_json::from_value(serde_json::json!({
            "mark":mark_id,"artifact_label":"provider.output","verifier":"artifact_exists","params":{}
        })).unwrap()];
        metadata.checkpoint_admission = Some(octopus_ledger::CheckpointAdmissionMetadata {
            artifact_label:"provider.output".into(), required_artifact_labels:vec!["provider.output".into()], complete:false,
        });
        prefix.invocation_metadata = Some(metadata.clone().into());
        let query = |index: &mut ProviderLedgerEvidenceIndex| index.admitted_handoff_seq(PendingHandoffLedgerCoordinate {
            interface:&provider, run:&run, worker:&worker, artifact:&artifact, required_evidence:Some((mark_id,"artifact_exists")),
            checkpoint:&checkpoint, epoch:0,
        });
        let mark = |sequence: u64, pin: Option<u64>| {
            let mut movement = one_shot_movement(sequence, &artifact, checkpoint.as_str(),
                MovementKind::MarkOutcomeAdmitted(mark_id.into()));
            movement.trace = prefix.trace.clone();
            let mut proof = movement.invocation_metadata.as_ref().unwrap().mark_outcome().unwrap().clone();
            proof.arrival_seq = pin;
            movement.invocation_metadata = Some(proof.into());
            movement
        };
        for mode in ["replacement", "chain", "unmarked", "legacy", "old_pin", "missing_pin", "grant", "bindings",
            "requirements", "artifact", "predecessor", "repeat", "marked_prior", "ambiguous"] {
            let path = dir.path().join(format!("{mode}.jsonl"));
            std::fs::write(&path, format!("{}\n", serde_json::to_string(&prefix).unwrap())).unwrap();
            let mut index = ProviderLedgerEvidenceIndex::new(provider.clone(), path.clone(), 1_048_576);
            assert_eq!(query(&mut index).unwrap(), None);
            let mut rows = vec![prefix.clone()];
            if mode == "marked_prior" { rows.push(mark(1, Some(0))); }
            if mode == "ambiguous" {
                let mut competing = prefix.clone(); competing.seq.0 = 1; competing.lamport.0 = 2;
                competing.artifact.as_mut().unwrap().content = ContentHash::of_bytes(b"competing candidate");
                rows.push(competing);
            }
            let expected = if mode == "legacy" {
                rows.push(mark(1, None)); Some(0)
            } else {
                let mut replacement = prefix.clone(); replacement.seq.0 = rows.len() as u64; replacement.lamport.0 = replacement.seq.0 + 1;
                let mut revised = metadata.clone(); revised.replaces_arrival_seq = Some(0);
                revised.runtime.as_mut().unwrap().run_record_hash = Some(ContentHash::of_bytes(b"current snapshot").to_hex());
                revised.checkpoint_admission.as_mut().unwrap().complete = true;
                match mode {
                    "grant" => revised.grant.interfaces.push("ungranted".into()),
                    "bindings" => revised.artifact_binding_refs.push(artifact.clone()),
                    "requirements" => revised.required_verified_artifacts[0].params = serde_json::json!({"changed":true}),
                    "artifact" => replacement.artifact.as_mut().unwrap().content = ContentHash::of_bytes(b"different candidate"),
                    "predecessor" => revised.replaces_arrival_seq = Some(99),
                    "repeat" => revised.replaces_arrival_seq = None,
                    _ => (),
                }
                replacement.invocation_metadata = Some(revised.clone().into()); rows.push(replacement.clone());
                if mode == "chain" {
                    revised.replaces_arrival_seq = Some(1);
                    revised.runtime.as_mut().unwrap().run_record_hash = Some(ContentHash::of_bytes(b"next snapshot").to_hex());
                    replacement.seq.0 = 2; replacement.lamport.0 = 3; replacement.invocation_metadata = Some(revised.into()); rows.push(replacement);
                }
                let current = rows.len() as u64 - 1;
                if mode != "unmarked" {
                    rows.push(mark(current + 1, if mode == "old_pin" {Some(0)} else if mode == "missing_pin" {None} else {Some(current)}));
                }
                (mode != "unmarked").then_some(current)
            };
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
            for movement in &rows[1..] { writeln!(file, "{}", serde_json::to_string(movement).unwrap()).unwrap(); }
            file.sync_all().unwrap();
            let actual = query(&mut index);
            if !matches!(mode, "replacement" | "chain" | "unmarked" | "legacy") { assert!(actual.is_err(), "{mode}: {actual:?}"); continue; }
            assert_eq!(actual.unwrap(), expected, "{mode}");
            let mut cold = ProviderLedgerEvidenceIndex::new(provider.clone(), path, 1_048_576);
            assert_eq!(query(&mut cold).unwrap(), expected, "cold {mode}");
            let evidence = index.session_evidence(&run, &provider, std::iter::once(key.clone())).unwrap();
            assert_eq!(evidence[&key].provider_arrivals, BTreeSet::from([if mode == "chain" {2} else if mode == "legacy" {0} else {1}]));
            let proved = index.admitted_slot_artifacts(&RuntimeCoordinateKey::new(&run, &worker, &provider, &checkpoint, 0), mark_id, "artifact_exists").unwrap();
            assert_eq!(proved.iter().map(|proof| proof.arrival_seq).collect::<Vec<_>>(), expected.into_iter().collect::<Vec<_>>());
        }
    }

    fn one_shot_movement(
        seq: u64,
        artifact: &ArtifactRef,
        checkpoint: &str,
        kind: MovementKind,
    ) -> Movement {
        let invocation_metadata = match &kind {
            MovementKind::MarkOutcomeAdmitted(mark) => octopus_interfaces::Mark::passed(
                mark.clone(),
                CheckpointId::new(checkpoint),
                RunId::new("run_test"),
                0,
                artifact.clone(),
                VerifierId::new("artifact_exists"),
                test_verifier_package_pin(),
                serde_json::json!({}),
            )
            .into(),
            _ => typed_invocation_metadata(
                &RunId::new("run_test"),
                checkpoint,
                &InterfaceId::new("world_provider_test"),
                0,
            )
            .into(),
        };
        let from = Endpoint::Interface(InterfaceId::new("world_provider_test"));
        let to = Endpoint::Interface(InterfaceId::new("system_worker_test"));
        Movement {
            seq: octopus_ledger::Seq(seq),
            lamport: octopus_ledger::Lamport(seq + 1),
            recorded_at_unix_ms: seq + 1,
            trace: octopus_ledger::TraceId(format!("trace-one-shot-{seq}")),
            kind,
            from,
            to,
            artifact: Some(artifact.clone()),
            invocation_metadata: Some(invocation_metadata),
        }
    }

    fn pin_artifact(file: &InterfaceArtifactFile, blobs_dir: &Path) -> ArtifactRef {
        let identity = artifact_identity(file, file.manifest_path().parent().unwrap())
            .unwrap()
            .artifact;
        let manifest_bytes = std::fs::read(file.manifest_path()).unwrap();
        let manifest: InterfaceArtifactManifest = serde_json::from_slice(&manifest_bytes).unwrap();
        let payload = file.read_payload_bytes().unwrap();
        std::fs::write(blobs_dir.join(identity.content.to_hex()), manifest_bytes).unwrap();
        std::fs::write(
            blobs_dir.join(manifest.payload_content_hash.unwrap()),
            payload,
        )
        .unwrap();
        identity
    }

    #[test]
    fn foreign_snapshot_evidence_preserves_the_complete_source_payload() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = dir.path().join("blobs");
        std::fs::create_dir(&blobs).unwrap();
        let payload = serde_json::json!({
            "label": "interface.artifact.output", "text": "peer output",
            "session_context": {"schema_version":3,"provider_interface":"world_provider_peer","artifacts":[]}
        });
        let file = InterfaceArtifactFile::write(&dir.path().join("emitted"), "interface.artifact.output", &serde_json::to_vec(&payload).unwrap()).unwrap();
        let mut entry = SessionContextArtifact {
            label: "interface.artifact.output".into(), payload,
            source_artifact: pin_artifact(&file, &blobs), source_movement_seq: Some(8),
        };
        let limits = InterfaceHostIoLimits { max_control_frame_bytes: 1_048_576, max_stderr_bytes: 262_144, max_manifest_bytes: 1_048_576, max_payload_bytes: 1_048_576 };
        let labels = BTreeSet::from(["interface.artifact.output".into()]);
        let local = InterfaceId::new("world_provider_local");
        let peer = InterfaceId::new("world_provider_peer");
        validate_session_source_payload(&entry, &blobs, limits, &labels, 0, &local).unwrap();
        assert!(validate_session_source_payload(&entry, &blobs, limits, &labels, 0, &peer).is_err());
        entry.payload.as_object_mut().unwrap().remove(SESSION_CONTEXT_FIELD);
        assert!(validate_session_source_payload(&entry, &blobs, limits, &labels, 0, &local).is_err());
        validate_session_source_payload(&entry, &blobs, limits, &labels, 0, &peer).unwrap();
    }

    #[tokio::test]
    async fn one_shot_recovery_reuses_terminal_evidence_after_multiple_dispatches() {
        use std::io::Write as _;
        use tokio_util::sync::CancellationToken;

        let dir = tempfile::tempdir().unwrap();
        let var_dir = dir.path().join("var/provider");
        let blobs_dir = dir.path().join("ledger/blobs");
        let movements_path = dir.path().join("ledger/movements.jsonl");
        std::fs::create_dir_all(&blobs_dir).unwrap();
        let request = b"request";
        let dispatch_payload = transport_dispatch_payload(
            "POST",
            "http",
            0,
            request.len() as u64,
            blake3::hash(request).to_hex().to_string(),
            None,
        );
        let dispatch_file = InterfaceArtifactFile::write(
            var_dir.join("dispatch"),
            PROVIDER_TRANSPORT_DISPATCH_LABEL,
            &dispatch_payload,
        )
        .unwrap();
        let dispatch = pin_artifact(&dispatch_file, &blobs_dir);
        let mut dispatch_two_value: serde_json::Value =
            serde_json::from_slice(&dispatch_payload).unwrap();
        dispatch_two_value["attempt"] = serde_json::json!(2);
        let dispatch_two_file = InterfaceArtifactFile::write(
            var_dir.join("dispatch-2"),
            PROVIDER_TRANSPORT_DISPATCH_LABEL,
            &serde_json::to_vec(&dispatch_two_value).unwrap(),
        )
        .unwrap();
        let dispatch_two = pin_artifact(&dispatch_two_file, &blobs_dir);
        let dispatch_arrival =
            one_shot_movement(1, &dispatch, "dispatch_1", MovementKind::InterfaceArrival);
        let dispatch_mark = one_shot_movement(
            2,
            &dispatch,
            "dispatch_1",
            MovementKind::MarkOutcomeAdmitted(octopus_descriptors::MarkId::new(
                "provider.dispatch.admitted",
            )),
        );
        let dispatch_two_arrival = one_shot_movement(
            3,
            &dispatch_two,
            "dispatch_2",
            MovementKind::InterfaceArrival,
        );
        let dispatch_two_mark = one_shot_movement(
            4,
            &dispatch_two,
            "dispatch_2",
            MovementKind::MarkOutcomeAdmitted(octopus_descriptors::MarkId::new(
                "provider.dispatch.admitted",
            )),
        );
        std::fs::write(
            &movements_path,
            format!(
                "{}\n{}\n{}\n{}\n{}\n",
                serde_json::to_string(&ledger_row(0)).unwrap(),
                serde_json::to_string(&dispatch_arrival).unwrap(),
                serde_json::to_string(&dispatch_mark).unwrap(),
                serde_json::to_string(&dispatch_two_arrival).unwrap(),
                serde_json::to_string(&dispatch_two_mark).unwrap()
            ),
        )
        .unwrap();
        let (arrivals, mut arrival_rx) = tokio::sync::mpsc::channel(1);
        let ctx = InterfaceLiftContext::unprivileged(
            InterfaceId::new("world_provider_test"),
            dir.path().join("registry/interface/world_provider_test"),
            None,
            var_dir.clone(),
            movements_path.clone(),
            blobs_dir.clone(),
            dir.path().join("ledger/ledger-index.sqlite"),
            Default::default(),
            arrivals,
            CancellationToken::new(),
        )
        .with_host_io_limits(InterfaceHostIoLimits {
            max_control_frame_bytes: 1_048_576,
            max_stderr_bytes: 262_144,
            max_manifest_bytes: 1_048_576,
            max_payload_bytes: 1_048_576,
        })
        .unwrap();
        let session = ProviderHandoffSession::new(&ctx).unwrap();
        let result_reader = LedgerTailReader::open_named(&ctx, "run-authoring").unwrap();
        let handoff = ProviderOutputHandoff {
            invocation_metadata: InvocationMetadata::new(Default::default()),
            ctx,
            config: config_with_tool_routes(&[]),
            run_path: dir.path().join("runs/run_test.json"),
            hand_in_root: dir.path().join("hand-in"),
            var_dir: var_dir.clone(),
            source_movement_seq: 0,
            source_carries_context: true,
            failed_source: false,
            checked_settlement_output: None,
            context_output_labels: BTreeSet::new(),
            session_snapshot_labels: BTreeSet::new(),
            standing_candidate_labels: BTreeSet::new(),
            granted_tools: BTreeSet::new(),
            granted_interfaces: BTreeSet::new(),
            granted_mark_outcomes: BTreeSet::new(),
            bound_topology: test_bound_topology(&config_with_tool_routes(&[])),
            delivery_mode: HandoffDeliveryMode::OneShot(OneShotHandoffPlan {
                terminal: BTreeMap::from([(
                    "interface.artifact.output".to_string(),
                    OneShotHandoffSlot {
                        checkpoint: CheckpointId::new("terminal"),
                        epoch: 0,
                        required_mark: "provider.output.admitted".to_string(),
                        required_verifier: "artifact_exists".to_string(),
                    },
                )]),
                dispatch: BTreeMap::from([
                    (
                        1,
                        OneShotHandoffSlot {
                            checkpoint: CheckpointId::new("dispatch_1"),
                            epoch: 0,
                            required_mark: "provider.dispatch.admitted".to_string(),
                            required_verifier: "artifact_exists".to_string(),
                        },
                    ),
                    (
                        2,
                        OneShotHandoffSlot {
                            checkpoint: CheckpointId::new("dispatch_2"),
                            epoch: 0,
                            required_mark: "provider.dispatch.admitted".to_string(),
                            required_verifier: "artifact_exists".to_string(),
                        },
                    ),
                    (
                        3,
                        OneShotHandoffSlot {
                            checkpoint: CheckpointId::new("dispatch_3"),
                            epoch: 0,
                            required_mark: "provider.dispatch.admitted".to_string(),
                            required_verifier: "artifact_exists".to_string(),
                        },
                    ),
                ]),
                completion: OneShotHandoffSlot {
                    checkpoint: CheckpointId::new("completion"),
                    epoch: 0,
                    required_mark: "provider.handoff.completed".to_string(),
                    required_verifier: "artifact_exists".to_string(),
                },
            }),
            session,
            invocation_session_snapshot: None,
            state: Arc::new(tokio::sync::Mutex::new(HandoffState {
            validated_withdrawal_output: None,
                context_artifacts: Vec::new(),
                static_context_artifacts: Vec::new(),
                session_context: ResolvedSessionContext {
                    provider_interface: InterfaceId::new("world_provider_test"),
                    artifacts: Vec::new(),
                },
                session_snapshot_head: None,
                run_record_bytes: Vec::new(),
                run_record_evidence_seq: None,
                consumed_input: false,
                consumed_context_free_independent_input: None,
                consumed_verified_input: false,
                deferred_context_generations: 0,
            })),
            result_reader: Arc::new(tokio::sync::Mutex::new(result_reader)),
        };
        assert_eq!(
            handoff.recover_source().await.unwrap(),
            ProviderSourceRecovery::Interrupted {
                last_artifact_label: PROVIDER_TRANSPORT_DISPATCH_LABEL.to_string()
            }
        );

        let terminal_file = InterfaceArtifactFile::write(
            var_dir.join("terminal"),
            "interface.artifact.output",
            br#"{"text":"done"}"#,
        )
        .unwrap();
        let terminal = pin_artifact(&terminal_file, &blobs_dir);
        let terminal_arrival =
            one_shot_movement(5, &terminal, "terminal", MovementKind::InterfaceArrival);
        let terminal_mark = one_shot_movement(
            6,
            &terminal,
            "terminal",
            MovementKind::MarkOutcomeAdmitted(octopus_descriptors::MarkId::new(
                "provider.output.admitted",
            )),
        );
        let mut ledger = std::fs::OpenOptions::new()
            .append(true)
            .open(&movements_path)
            .unwrap();
        writeln!(
            ledger,
            "{}",
            serde_json::to_string(&terminal_arrival).unwrap()
        )
        .unwrap();
        writeln!(ledger, "{}", serde_json::to_string(&terminal_mark).unwrap()).unwrap();
        ledger.sync_all().unwrap();
        drop(ledger);
        let terminal_for_completion = terminal.clone();
        let movements_for_completion = movements_path.clone();
        let blobs_for_completion = blobs_dir.clone();
        let completion_delivery = async {
            let delivery = arrival_rx.recv().await.unwrap();
            let manifest = delivery.artifact.read_manifest().unwrap();
            assert_eq!(manifest.label, PROVIDER_HANDOFF_COMPLETE_LABEL);
            let completion = validate_handoff_completion_payload(
                &delivery.artifact.read_payload_bytes().unwrap(),
                &InterfaceId::new("world_provider_test"),
                0,
            )
            .unwrap();
            assert_eq!(completion.artifacts, vec![terminal_for_completion]);
            let completion_artifact = pin_artifact(&delivery.artifact, &blobs_for_completion);
            let completion_arrival = one_shot_movement(
                7,
                &completion_artifact,
                "completion",
                MovementKind::InterfaceArrival,
            );
            let completion_mark = one_shot_movement(
                8,
                &completion_artifact,
                "completion",
                MovementKind::MarkOutcomeAdmitted(octopus_descriptors::MarkId::new(
                    "provider.handoff.completed",
                )),
            );
            let mut ledger = std::fs::OpenOptions::new()
                .append(true)
                .open(&movements_for_completion)
                .unwrap();
            writeln!(
                ledger,
                "{}",
                serde_json::to_string(&completion_arrival).unwrap()
            )
            .unwrap();
            writeln!(
                ledger,
                "{}",
                serde_json::to_string(&completion_mark).unwrap()
            )
            .unwrap();
            ledger.sync_all().unwrap();
            delivery
                .verdict_tx
                .unwrap()
                .send(DeliveryVerdict::Admitted { seq: 7 })
                .unwrap();
        };
        let (recovery, ()) = tokio::join!(handoff.recover_source(), completion_delivery);
        assert_eq!(recovery.unwrap(), ProviderSourceRecovery::Completed);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), handoff.recover_source())
                .await
                .unwrap()
                .unwrap(),
            ProviderSourceRecovery::Completed
        );
        assert!(matches!(
            arrival_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        assert!(
            handoff
                .hand_transport_dispatch(dispatch_file)
                .await
                .unwrap_err()
                .contains("follows completion evidence")
        );

        let mut late_dispatch_value: serde_json::Value =
            serde_json::from_slice(&dispatch_payload).unwrap();
        late_dispatch_value["attempt"] = serde_json::json!(3);
        let late_dispatch_file = InterfaceArtifactFile::write(
            var_dir.join("late-dispatch"),
            PROVIDER_TRANSPORT_DISPATCH_LABEL,
            &serde_json::to_vec(&late_dispatch_value).unwrap(),
        )
        .unwrap();
        let late_dispatch = pin_artifact(&late_dispatch_file, &blobs_dir);
        let mut ledger = std::fs::OpenOptions::new()
            .append(true)
            .open(&movements_path)
            .unwrap();
        writeln!(
            ledger,
            "{}",
            serde_json::to_string(&one_shot_movement(
                9,
                &late_dispatch,
                "dispatch_3",
                MovementKind::InterfaceArrival,
            ))
            .unwrap()
        )
        .unwrap();
        writeln!(
            ledger,
            "{}",
            serde_json::to_string(&one_shot_movement(
                10,
                &late_dispatch,
                "dispatch_3",
                MovementKind::MarkOutcomeAdmitted(octopus_descriptors::MarkId::new(
                    "provider.dispatch.admitted",
                )),
            ))
            .unwrap()
        )
        .unwrap();
        ledger.sync_all().unwrap();
        assert!(
            handoff
                .recover_source()
                .await
                .unwrap_err()
                .contains("does not precede every terminal arrival")
        );
    }

    #[tokio::test]
    async fn one_shot_progress_then_terminal_batch_never_authors_the_run() {
        use std::io::Write as _;
        use tokio_util::sync::CancellationToken;

        let dir = tempfile::tempdir().unwrap();
        let var_dir = dir.path().join("var/provider");
        let movements_path = dir.path().join("ledger/movements.jsonl");
        let blobs_dir = dir.path().join("ledger/blobs");
        std::fs::create_dir_all(&blobs_dir).unwrap();
        std::fs::write(
            &movements_path,
            format!("{}\n", serde_json::to_string(&ledger_row(0)).unwrap()),
        )
        .unwrap();
        let answer = InterfaceArtifactFile::write(
            var_dir.join("terminal-answer"),
            "interface.artifact.output",
            br#"{"text":"done"}"#,
        )
        .unwrap();
        let reasoning = InterfaceArtifactFile::write(
            var_dir.join("terminal-reasoning"),
            "provider.reasoning",
            br#"{"summary":"because"}"#,
        )
        .unwrap();
        let expected_answer = artifact_identity(&answer, &var_dir).unwrap().artifact;
        let expected_reasoning = artifact_identity(&reasoning, &var_dir).unwrap().artifact;
        let (arrivals, mut arrival_rx) = tokio::sync::mpsc::channel(3);
        let ctx = InterfaceLiftContext::unprivileged(
            InterfaceId::new("world_provider_test"),
            dir.path().join("registry/interface/world_provider_test"),
            None,
            var_dir.clone(),
            movements_path.clone(),
            blobs_dir.clone(),
            dir.path().join("ledger/ledger-index.sqlite"),
            Default::default(),
            arrivals,
            CancellationToken::new(),
        )
        .with_host_io_limits(InterfaceHostIoLimits {
            max_control_frame_bytes: 1_048_576,
            max_stderr_bytes: 262_144,
            max_manifest_bytes: 1_048_576,
            max_payload_bytes: 1_048_576,
        })
        .unwrap();
        let session = ProviderHandoffSession::new(&ctx).unwrap();
        let result_reader = LedgerTailReader::open_named(&ctx, "run-authoring").unwrap();
        let run_path = dir.path().join("runs/run_test.json");
        let mut handoff = ProviderOutputHandoff {
            invocation_metadata: InvocationMetadata::new(Default::default()),
            ctx,
            config: config_with_tool_routes(&[]),
            run_path: run_path.clone(),
            hand_in_root: dir.path().join("hand-in"),
            var_dir,
            source_movement_seq: 0,
            source_carries_context: true,
            failed_source: false,
            checked_settlement_output: None,
            context_output_labels: BTreeSet::new(),
            session_snapshot_labels: BTreeSet::new(),
            standing_candidate_labels: BTreeSet::new(),
            granted_tools: BTreeSet::new(),
            granted_interfaces: BTreeSet::new(),
            granted_mark_outcomes: BTreeSet::new(),
            bound_topology: test_bound_topology(&config_with_tool_routes(&[])),
            delivery_mode: HandoffDeliveryMode::OneShot(OneShotHandoffPlan {
                terminal: BTreeMap::from([
                    (
                        "interface.artifact.output".to_string(),
                        OneShotHandoffSlot {
                            checkpoint: CheckpointId::new("terminal_answer"),
                            epoch: 0,
                            required_mark: "provider.output.admitted".to_string(),
                            required_verifier: "artifact_exists".to_string(),
                        },
                    ),
                    (
                        "provider.reasoning".to_string(),
                        OneShotHandoffSlot {
                            checkpoint: CheckpointId::new("terminal_reasoning"),
                            epoch: 0,
                            required_mark: "provider.reasoning.admitted".to_string(),
                            required_verifier: "artifact_exists".to_string(),
                        },
                    ),
                ]),
                dispatch: BTreeMap::from([(
                    1,
                    OneShotHandoffSlot {
                        checkpoint: CheckpointId::new("dispatch_1"),
                        epoch: 0,
                        required_mark: "provider.dispatch.admitted".to_string(),
                        required_verifier: "artifact_exists".to_string(),
                    },
                )]),
                completion: OneShotHandoffSlot {
                    checkpoint: CheckpointId::new("completion"),
                    epoch: 0,
                    required_mark: "provider.handoff.completed".to_string(),
                    required_verifier: "artifact_exists".to_string(),
                },
            }),
            session,
            invocation_session_snapshot: None,
            state: Arc::new(tokio::sync::Mutex::new(HandoffState {
            validated_withdrawal_output: None,
                context_artifacts: Vec::new(),
                static_context_artifacts: Vec::new(),
                session_context: ResolvedSessionContext {
                    provider_interface: InterfaceId::new("world_provider_test"),
                    artifacts: Vec::new(),
                },
                session_snapshot_head: None,
                run_record_bytes: b"not authored".to_vec(),
                run_record_evidence_seq: None,
                consumed_input: false,
                consumed_context_free_independent_input: None,
                consumed_verified_input: false,
                deferred_context_generations: 0,
            })),
            result_reader: Arc::new(tokio::sync::Mutex::new(result_reader)),
        };
        let first_terminal_seq = exercise_one_shot_progress(&mut handoff, &mut arrival_rx).await;
        let movements_for_arrival = movements_path.clone();
        let blobs_for_arrival = blobs_dir.clone();
        let answer_for_arrival = expected_answer.clone();
        let reasoning_for_arrival = expected_reasoning.clone();
        let arrival = tokio::spawn(async move {
            for index in 0..3u64 {
                let delivery = arrival_rx.recv().await.unwrap();
                let manifest = delivery.artifact.read_manifest().unwrap();
                let artifact = artifact_identity(
                    &delivery.artifact,
                    delivery.artifact.manifest_path().parent().unwrap(),
                )
                .unwrap()
                .artifact;
                assert_eq!(
                    pin_artifact(&delivery.artifact, &blobs_for_arrival),
                    artifact
                );
                let (checkpoint, mark) = match index {
                    0 => {
                        assert_eq!(manifest.label, "interface.artifact.output");
                        assert_eq!(artifact, answer_for_arrival);
                        ("terminal_answer", "provider.output.admitted")
                    }
                    1 => {
                        assert_eq!(manifest.label, "provider.reasoning");
                        assert_eq!(artifact, reasoning_for_arrival);
                        ("terminal_reasoning", "provider.reasoning.admitted")
                    }
                    2 => {
                        assert_eq!(manifest.label, PROVIDER_HANDOFF_COMPLETE_LABEL);
                        let completion = validate_handoff_completion_payload(
                            &delivery.artifact.read_payload_bytes().unwrap(),
                            &InterfaceId::new("world_provider_test"),
                            0,
                        )
                        .unwrap();
                        assert_eq!(
                            completion.artifacts,
                            vec![answer_for_arrival.clone(), reasoning_for_arrival.clone()]
                        );
                        ("completion", "provider.handoff.completed")
                    }
                    _ => unreachable!(),
                };
                let seq = first_terminal_seq + index * 2;
                let admitted =
                    one_shot_movement(seq, &artifact, checkpoint, MovementKind::InterfaceArrival);
                let mark = one_shot_movement(
                    seq + 1,
                    &artifact,
                    checkpoint,
                    MovementKind::MarkOutcomeAdmitted(octopus_descriptors::MarkId::new(mark)),
                );
                let mut ledger = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&movements_for_arrival)
                    .unwrap();
                writeln!(ledger, "{}", serde_json::to_string(&admitted).unwrap()).unwrap();
                writeln!(ledger, "{}", serde_json::to_string(&mark).unwrap()).unwrap();
                ledger.sync_all().unwrap();
                delivery
                    .verdict_tx
                    .unwrap()
                    .send(DeliveryVerdict::Admitted { seq })
                    .unwrap();
            }
        });
        assert_eq!(
            handoff
                .hand_batch(vec![
                    ProviderEmission {
                        artifact: answer,
                        route: ProviderEmissionRoute::ClientProjection,
                    },
                    ProviderEmission {
                        artifact: reasoning,
                        route: ProviderEmissionRoute::ClientProjection,
                    },
                ])
                .await
                .unwrap(),
            vec![
                ProviderHandoffOutcome::Admitted {
                    seq: first_terminal_seq,
                    artifact: expected_answer,
                },
                ProviderHandoffOutcome::Admitted {
                    seq: first_terminal_seq + 2,
                    artifact: expected_reasoning,
                }
            ]
        );
        arrival.await.unwrap();
        assert_eq!(
            handoff.recover_source().await.unwrap(),
            ProviderSourceRecovery::Completed
        );
        let late = InterfaceArtifactFile::write(
            handoff.var_dir.join("late-progress"),
            "hook.catalog",
            br#"{"hooks":[]}"#,
        )
        .unwrap();
        assert!(
            handoff
                .hand(ProviderEmission {
                    artifact: late,
                    route: ProviderEmissionRoute::ClientProjection
                })
                .await
                .unwrap_err()
                .contains("progress follows terminal evidence")
        );
        assert!(!run_path.exists());
        assert_eq!(
            handoff.state.lock().await.run_record_bytes,
            b"not authored".to_vec()
        );
    }

    fn session_entry(
        label: &str,
        payload: serde_json::Value,
        source: &str,
    ) -> SessionContextArtifact {
        let content = ContentHash::of_bytes(source.as_bytes());
        SessionContextArtifact {
            label: label.to_string(),
            payload,
            source_artifact: ArtifactRef {
                id: ArtifactId::new(format!("{label}:{}", content.to_hex())),
                content,
            },
            source_movement_seq: None,
        }
    }

    #[test]
    fn recovered_snapshot_preserves_checked_publication_order_and_rejects_conflicts() {
        let entry = |label, sequence| {
            let mut entry = session_entry(label, serde_json::json!({"sequence": sequence}), label);
            entry.source_movement_seq = Some(sequence);
            entry
        };
        let prior = vec![entry("input", 1), entry("tool.call", 5), entry("tool.result", 7)];
        let snapshot = vec![prior[0].clone(), prior[1].clone(), entry("reasoning", 9),
            prior[2].clone(), entry("output", 10)];
        let source = snapshot[4].source_artifact.clone();
        let publications = BTreeMap::from([(artifact_ref_key(&snapshot[2].source_artifact), BTreeSet::from([9]))]);
        let mut recovered = snapshot.clone();
        assert!(reconcile_recovered_session_snapshot(&prior, &mut recovered, &publications, &source).unwrap());
        assert_eq!(recovered.iter().map(|entry| entry.source_movement_seq.unwrap()).collect::<Vec<_>>(), [1, 5, 9, 7, 10]);
        let exact = session_artifacts_fingerprint(&recovered).unwrap();
        assert!(reconcile_recovered_session_snapshot(&recovered.clone(), &mut recovered, &publications, &source).unwrap());
        assert_eq!(session_artifacts_fingerprint(&recovered).unwrap(), exact);
        assert!(reconcile_recovered_session_snapshot(&prior, &mut snapshot.clone(), &BTreeMap::new(), &source).is_err());
        let wrong_publication = BTreeMap::from([(artifact_ref_key(&snapshot[2].source_artifact), BTreeSet::from([8]))]);
        assert!(reconcile_recovered_session_snapshot(&prior, &mut snapshot.clone(), &wrong_publication, &source).is_err());
        for corruption in ["payload", "source", "sequence", "label", "order"] {
            let mut invalid = snapshot.clone();
            match corruption {
                "payload" => invalid[3].payload = serde_json::json!({"corrupt": true}),
                "source" => invalid[3].source_artifact = entry("other", 7).source_artifact,
                "sequence" => invalid[3].source_movement_seq = Some(8),
                "label" => invalid[3].label = "other".into(),
                "order" => invalid.swap(1, 3),
                _ => unreachable!(),
            }
            assert!(reconcile_recovered_session_snapshot(&prior, &mut invalid, &publications, &source).is_err(), "{corruption}");
        }
        let mut ungrounded = snapshot.clone();
        ungrounded[2].source_movement_seq = None;
        assert!(reconcile_recovered_session_snapshot(&prior, &mut ungrounded, &publications, &source).unwrap());
        assert_eq!(ungrounded[2].source_movement_seq, Some(9));
        let mut extended = snapshot.clone();
        extended.push(entry("next.input", 11));
        assert!(!reconcile_recovered_session_snapshot(&extended, &mut snapshot.clone(), &BTreeMap::new(), &source).unwrap());
        let mut admitted = prior.clone();
        admitted.insert(2, entry("peer.input", 6));
        assert!(reconcile_recovered_session_snapshot(&admitted, &mut snapshot.clone(), &publications, &source).is_err());
        let mut checked = publications.clone();
        checked.insert(artifact_ref_key(&source), BTreeSet::from([10]));
        let mut repaired = snapshot.clone();
        assert!(reconcile_recovered_session_snapshot(&admitted, &mut repaired, &checked, &source).unwrap());
        assert_eq!(repaired.iter().map(|entry| entry.source_movement_seq.unwrap()).collect::<Vec<_>>(), [1, 5, 6, 9, 7, 10]);
        let retained = repaired.clone();
        assert!(!reconcile_recovered_session_snapshot(&retained, &mut snapshot.clone(), &BTreeMap::new(), &source).unwrap());
        let mut ambiguous = admitted.clone();
        ambiguous[2].source_movement_seq = Some(11);
        assert!(reconcile_recovered_session_snapshot(&ambiguous, &mut snapshot.clone(), &checked, &source).is_err());
        let missing = (20..54).map(|seq| {
            let label = format!("history.{seq}");
            let mut item = session_entry(&label, serde_json::json!({"seq":seq}), &label);
            item.source_movement_seq = Some(seq);
            item
        }).collect::<Vec<_>>();
        let result = entry("completed.result", 68);
        let mut full = prior.clone();
        full.extend(missing);
        full.push(result.clone());
        let mut bound = prior.clone();
        bound.extend([entry("continuity", 80), entry("later.input", 98)]);
        let restored = merge_failed_source_context(&bound, &full, 68, Some(98)).unwrap();
        assert_eq!(session_artifacts_fingerprint(&restored[..full.len()]).unwrap(), session_artifacts_fingerprint(&full).unwrap());
        assert_eq!(session_artifacts_fingerprint(&restored[full.len()..]).unwrap(), session_artifacts_fingerprint(&bound[prior.len()..]).unwrap());
        assert_eq!(restored.iter().filter(|item| item.source_artifact == result.source_artifact).count(), 1);
        let mut published = restored.clone();
        let result_position = published.iter().position(|item| item.source_artifact == result.source_artifact).unwrap();
        published.insert(result_position, entry("provider.tool.call", 67));
        let retained_publication = merge_failed_source_context(&published, &full, 68, Some(98)).unwrap();
        assert_eq!(session_artifacts_fingerprint(&retained_publication).unwrap(), session_artifacts_fingerprint(&published).unwrap());
        assert_eq!(session_artifacts_fingerprint(&merge_failed_source_context(&restored, &full, 68, Some(98)).unwrap()).unwrap(), session_artifacts_fingerprint(&restored).unwrap());
        let mut conflicting = bound.clone();
        conflicting[1].payload = serde_json::json!({"wrong":true});
        assert!(merge_failed_source_context(&conflicting, &full, 68, Some(98)).is_err());
        let mut reversed = full.clone();
        reversed.swap(0, 1);
        assert!(merge_failed_source_context(&bound, &reversed, 68, Some(98)).is_err());
        let mut unproven = full.clone();
        unproven[3].source_movement_seq = None;
        assert!(merge_failed_source_context(&bound, &unproven, 68, Some(98)).is_err());
        let mut early_input = bound.clone();
        early_input.last_mut().unwrap().source_movement_seq = Some(60);
        let late_result = merge_failed_source_context(&early_input, &full, 68, Some(60)).unwrap();
        assert_eq!(session_artifacts_fingerprint(&late_result[..full.len()]).unwrap(), session_artifacts_fingerprint(&full).unwrap());
        assert_eq!(late_result.last().unwrap().source_movement_seq, Some(60));
        for (mut completed, input) in [(restored.clone(), 98), (late_result, 60)] {
            completed.push(entry("current.reply", 100));
            let replayed = merge_failed_source_context(&completed, &full, 68, Some(input)).unwrap();
            assert_eq!(session_artifacts_fingerprint(&replayed).unwrap(), session_artifacts_fingerprint(&completed).unwrap());
        }
        let mut queued = bound.clone();
        queued.insert(queued.len() - 1, entry("previous.queued.input", 60));
        let merged = merge_failed_source_context(&queued, &full, 68, Some(98)).unwrap();
        assert_eq!(session_artifacts_fingerprint(&merged[full.len()..]).unwrap(), session_artifacts_fingerprint(&queued[prior.len()..]).unwrap());
        queued.insert(prior.len(), entry("previous.result", 66));
        let merged = merge_failed_source_context(&queued, &full, 68, Some(98)).unwrap();
        let position = |label| merged.iter().position(|item| item.label == label).unwrap();
        assert!(position("previous.result") < position("completed.result"));
        assert!(position("completed.result") < position("continuity"));
        assert!(position("continuity") < position("previous.queued.input"));
        assert!(position("previous.queued.input") < position("later.input"));
        let dir = tempfile::tempdir().unwrap();
        let output = InterfaceArtifactFile::write(dir.path(),
            "interface.context.updated", br#"{"label":"interface.context.updated"}"#).unwrap();
        let context = ResolvedSessionContext { provider_interface: InterfaceId::new("provider"), artifacts: repaired };
        let rebased = materialize_session_snapshot(&output, &context, None).unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&rebased.read_payload_bytes().unwrap()).unwrap();
        let rebased: SessionContextSnapshot = serde_json::from_value(payload[SESSION_CONTEXT_FIELD].clone()).unwrap();
        assert!(rebased.parent.is_none());
        assert_eq!(session_artifacts_fingerprint(&rebased.artifacts).unwrap(), session_artifacts_fingerprint(&retained).unwrap());
    }

    #[test]
    fn recovered_snapshot_keeps_checked_source_order_after_concurrent_progress() {
        let entry = |label, sequence| {
            let mut entry = session_entry(label, serde_json::json!({"sequence":sequence}), label);
            entry.source_movement_seq = Some(sequence);
            entry
        };
        let prior = vec![entry("earlier", 1), entry("progress", 10)];
        let snapshot = vec![prior[0].clone(), entry("call", 12), entry("queued.input", 9), entry("error", 14)];
        let source = snapshot.last().unwrap().source_artifact.clone();
        let proof = snapshot.iter().map(|entry| (artifact_ref_key(&entry.source_artifact), BTreeSet::from([entry.source_movement_seq.unwrap()]))).collect::<BTreeMap<_,_>>();
        let mut recovered = snapshot.clone();
        assert!(reconcile_recovered_session_snapshot(&prior, &mut recovered, &proof, &source).unwrap());
        assert_eq!(recovered.iter().map(|entry| entry.source_movement_seq.unwrap()).collect::<Vec<_>>(), [1,10,12,9,14]);
        for missing in &snapshot[1..] {
            let mut incomplete = proof.clone();
            incomplete.remove(&artifact_ref_key(&missing.source_artifact));
            assert!(reconcile_recovered_session_snapshot(&prior, &mut snapshot.clone(), &incomplete, &source).is_err());
        }
        let conflicting = vec![prior[0].clone(), entry("later.progress", 13)];
        assert!(reconcile_recovered_session_snapshot(&conflicting, &mut snapshot.clone(), &proof, &source).is_err());
    }

    #[test]
    fn session_snapshot_round_trip_replaces_dynamic_bindings_without_losing_history() {
        let dir = tempfile::tempdir().unwrap();
        let blobs_dir = dir.path().join("blobs");
        let movements_path = dir.path().join("movements.jsonl");
        std::fs::create_dir_all(&blobs_dir).unwrap();
        std::fs::write(&movements_path, b"").unwrap();
        let (arrivals, _arrival_rx) = tokio::sync::mpsc::channel(1);
        let ctx = InterfaceLiftContext::unprivileged(
            InterfaceId::new("world_provider_test"),
            dir.path().join("registry/interface/world_provider_test"),
            None,
            dir.path().join("var"),
            movements_path,
            blobs_dir.clone(),
            dir.path().join("ledger-index.sqlite"),
            Default::default(),
            arrivals,
            tokio_util::sync::CancellationToken::new(),
        )
        .with_host_io_limits(InterfaceHostIoLimits {
            max_control_frame_bytes: 1_048_576,
            max_stderr_bytes: 262_144,
            max_manifest_bytes: 1_048_576,
            max_payload_bytes: 1_048_576,
        })
        .unwrap();
        let session_context = ResolvedSessionContext {
            provider_interface: InterfaceId::new("world_provider_test"),
            artifacts: vec![
                session_entry(
                    "interface.artifact.input",
                    serde_json::json!({"text": "first"}),
                    "one",
                ),
                session_entry(
                    "tool.call",
                    serde_json::json!({"tool": "file.write"}),
                    "two",
                ),
                session_entry("tool.result", serde_json::json!({"status": "ok"}), "three"),
                session_entry(
                    "interface.artifact.output",
                    serde_json::json!({"text": "first answer"}),
                    "four",
                ),
                session_entry(
                    "interface.artifact.input",
                    serde_json::json!({"text": "second"}),
                    "five",
                ),
            ],
        };
        let output = InterfaceArtifactFile::write(
            dir.path(),
            "interface.artifact.output",
            br#"{"text":"second answer"}"#,
        )
        .unwrap();
        let snapshot = materialize_session_snapshot(&output, &session_context, None).unwrap();
        let snapshot_payload = snapshot.read_payload_bytes().unwrap();
        let snapshot_value: serde_json::Value = serde_json::from_slice(&snapshot_payload).unwrap();
        assert_eq!(
            snapshot_value[SESSION_CONTEXT_FIELD]["artifacts"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
        let snapshot_ref = pin_artifact(&snapshot, &blobs_dir);
        let reference_ref = ArtifactRef {
            id: ArtifactId::new("context.reference:system"),
            content: ContentHash::of_bytes(b"system"),
        };
        let mut static_context = Vec::new();
        let mut restored = ResolvedSessionContext {
            provider_interface: InterfaceId::new("world_provider_test"),
            artifacts: Vec::new(),
        };
        let mut snapshot_head = None;
        let snapshot_labels = BTreeSet::from(["interface.artifact.output".to_string()]);

        collect_bound_context(
            &ctx,
            &mut static_context,
            &mut restored,
            &mut snapshot_head,
            &reference_ref,
            br#"{"label":"context.reference","text":"system"}"#,
            BoundArtifactProvenance::default(),
            &snapshot_labels,
            &InterfaceId::new("world_provider_test"),
        )
        .unwrap();
        collect_bound_context(
            &ctx,
            &mut static_context,
            &mut restored,
            &mut snapshot_head,
            &snapshot_ref,
            &snapshot_payload,
            BoundArtifactProvenance {
                movement_seq: Some(9),
                provider_arrival_seq: Some(9),
            },
            &snapshot_labels,
            &InterfaceId::new("world_provider_test"),
        )
        .unwrap();

        assert_eq!(static_context, vec![reference_ref.id.clone()]);
        assert_eq!(snapshot_head.as_ref().unwrap().covered_artifact_count, 5);
        assert_eq!(restored.artifacts.len(), 6);
        let recorded = restored.artifacts[0].clone();
        let fingerprint = session_artifacts_fingerprint(&restored.artifacts).unwrap();
        assert!(!append_session_payload(&mut restored, &recorded.label,
            &serde_json::to_vec(&recorded.payload).unwrap(), &recorded.source_artifact,
            recorded.source_movement_seq, "replayed input").unwrap());
        assert_eq!(session_artifacts_fingerprint(&restored.artifacts).unwrap(), fingerprint);
        for (label, payload, sequence) in [
            ("different", recorded.payload.clone(), recorded.source_movement_seq),
            (recorded.label.as_str(), serde_json::json!({"changed": true}), recorded.source_movement_seq),
        ] {
            assert!(append_session_payload(&mut restored, label, &serde_json::to_vec(&payload).unwrap(),
                &recorded.source_artifact, sequence, "conflicting input").is_err());
        }
        let mut foreign = ResolvedSessionContext {
            provider_interface: InterfaceId::new("world_provider_other"),
            artifacts: Vec::new(),
        };
        collect_bound_context(
            &ctx,
            &mut Vec::new(),
            &mut foreign,
            &mut None,
            &snapshot_ref,
            &snapshot_payload,
            BoundArtifactProvenance::default(),
            &snapshot_labels,
            &InterfaceId::new("world_provider_other"),
        )
        .unwrap();
        assert!(foreign.artifacts.is_empty());
        assert_eq!(
            restored
                .artifacts
                .iter()
                .map(|artifact| artifact.label.as_str())
                .collect::<Vec<_>>(),
            vec![
                "interface.artifact.input",
                "tool.call",
                "tool.result",
                "interface.artifact.output",
                "interface.artifact.input",
                "interface.artifact.output"
            ]
        );

        let mut changed_prefix = restored.clone();
        changed_prefix.artifacts[0].payload = serde_json::json!({"text": "changed payload"});
        let rejected_output = InterfaceArtifactFile::write(
            dir.path().join("rejected"),
            "interface.artifact.output",
            br#"{"text":"must fail"}"#,
        )
        .unwrap();
        assert!(materialize_session_snapshot(
            &rejected_output,
            &changed_prefix,
            snapshot_head.as_ref(),
        )
        .unwrap_err()
        .contains("pinned snapshot prefix"));

        let mut normalized = restored.clone();
        let mut normalized_head = snapshot_head.clone();
        for (index, artifact) in normalized.artifacts[..5].iter_mut().enumerate() {
            artifact.source_movement_seq = Some(index as u64 + 1);
        }
        reconcile_resolved_session_snapshot_head(&normalized, &mut normalized_head).unwrap();
        assert!(normalized_head.is_some());
        let rebased_output = InterfaceArtifactFile::write(
            dir.path().join("rebased"),
            "interface.artifact.output",
            br#"{"text":"rebased answer"}"#,
        )
        .unwrap();
        let rebased_snapshot =
            materialize_session_snapshot(&rebased_output, &normalized, normalized_head.as_ref())
                .unwrap();
        let rebased_payload = rebased_snapshot.read_payload_bytes().unwrap();
        let rebased_value: serde_json::Value = serde_json::from_slice(&rebased_payload).unwrap();
        assert_eq!(
            serde_json::from_value::<ArtifactRef>(rebased_value[SESSION_CONTEXT_FIELD]["parent"].clone()).unwrap(),
            snapshot_ref
        );
        assert_eq!(
            rebased_value[SESSION_CONTEXT_FIELD]["artifacts"]
                .as_array()
                .unwrap()
                .len(),
            normalized.artifacts.len() - normalized_head.as_ref().unwrap().covered_artifact_count
        );

        restored.artifacts.push(session_entry(
            "interface.artifact.input",
            serde_json::json!({"text": "third"}),
            "six",
        ));
        let next_output = InterfaceArtifactFile::write(
            dir.path().join("next"),
            "interface.artifact.output",
            br#"{"text":"third answer"}"#,
        )
        .unwrap();
        let next_snapshot =
            materialize_session_snapshot(&next_output, &restored, snapshot_head.as_ref()).unwrap();
        let next_payload = next_snapshot.read_payload_bytes().unwrap();
        let next_value: serde_json::Value = serde_json::from_slice(&next_payload).unwrap();
        assert_eq!(
            serde_json::from_value::<ArtifactRef>(
                next_value[SESSION_CONTEXT_FIELD]["parent"].clone(),
            )
            .unwrap(),
            snapshot_ref
        );
        assert_eq!(
            next_value[SESSION_CONTEXT_FIELD]["artifacts"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let next_ref = artifact_identity(&next_snapshot, dir.path())
            .unwrap()
            .artifact;
        let mut chained = ResolvedSessionContext {
            provider_interface: InterfaceId::new("world_provider_test"),
            artifacts: Vec::new(),
        };
        let mut chained_head = None;
        collect_bound_context(
            &ctx,
            &mut Vec::new(),
            &mut chained,
            &mut chained_head,
            &next_ref,
            &next_payload,
            BoundArtifactProvenance {
                movement_seq: Some(10),
                provider_arrival_seq: Some(10),
            },
            &snapshot_labels,
            &InterfaceId::new("world_provider_test"),
        )
        .unwrap();
        assert_eq!(chained.artifacts.len(), 8);
        assert_eq!(chained_head.unwrap().covered_artifact_count, 7);
        let active_bindings = [reference_ref.id, snapshot_ref.id.clone()];
        assert_eq!(active_bindings.len(), 2);
        let latest = &restored.artifacts[5];
        assert_eq!(latest.source_artifact, snapshot_ref);
        assert_eq!(latest.source_movement_seq, Some(9));
    }

    #[test]
    fn admitted_context_output_is_grounded_in_its_ledger_movement() {
        let entry = session_entry(
            "interface.artifact.output",
            serde_json::json!({"text": "ground me"}),
            "grounded-output",
        );
        let source = entry.source_artifact.clone();
        let mut context = ResolvedSessionContext {
            provider_interface: InterfaceId::new("world_provider_test"),
            artifacts: vec![entry],
        };
        let pending = PendingHandoff {
            source_movement_seq: 41,
            context_publication: None,
            deferred_publication: false,
            source_without_context: false,
            settlement_of: None,
            dispatch_fenced: true,
            withdrawal: None, replacement: None,
            standing: Vec::new(),
            items: vec![PendingHandoffItem {
                artifact_id: source.id.clone(),
                artifact_content_hash: source.content.to_hex(),
                artifact_label: "interface.artifact.output".to_string(),
                manifest_relative_path: PathBuf::from("unused-in-grounding-test"),
                checkpoint: PendingHandoffCheckpoint {
                    id: CheckpointId::new("output"),
                    epoch: 0,
                    interface_binding: all_binding("world_provider_test"),
                    outbound_interface_bindings: vec![all_binding("world_client_test")],
                    artifact_bindings: Vec::new(),
                    required_verified_artifacts: Vec::new(),
                },
            }],
            terminal: false,
        };

        ProviderOutputHandoff::ground_admitted_context_outputs(
            &mut context,
            &pending,
            &[ProviderHandoffOutcome::Admitted {
                seq: 42,
                artifact: source.clone(),
            }],
            &BTreeSet::from(["interface.artifact.output".to_string()]),
        )
        .unwrap();

        assert_eq!(context.artifacts[0].source_movement_seq, Some(42));
    }

    #[test]
    fn session_context_requires_ledger_and_blob_proof_for_each_embedded_payload() {
        use octopus_ledger::{Lamport, Seq, TraceId};

        let dir = tempfile::tempdir().unwrap();
        let source_root = dir.path().join("source");
        let blobs_dir = dir.path().join("blobs");
        std::fs::create_dir_all(&blobs_dir).unwrap();
        let source = InterfaceArtifactFile::write(
            &source_root,
            "interface.artifact.output",
            br#"{"text":"proved"}"#,
        )
        .unwrap();
        let identity = artifact_identity(&source, dir.path()).unwrap().artifact;
        let manifest_bytes = std::fs::read(source.manifest_path()).unwrap();
        let manifest: InterfaceArtifactManifest = serde_json::from_slice(&manifest_bytes).unwrap();
        let payload_bytes = source.read_payload_bytes().unwrap();
        std::fs::write(blobs_dir.join(identity.content.to_hex()), &manifest_bytes).unwrap();
        std::fs::write(
            blobs_dir.join(manifest.payload_content_hash.unwrap()),
            &payload_bytes,
        )
        .unwrap();
        let movements_path = dir.path().join("movements.jsonl");
        let movement = Movement {
            seq: Seq(0),
            lamport: Lamport(8),
            recorded_at_unix_ms: 1,
            trace: TraceId("trace-session-proof".to_string()),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(InterfaceId::new("world_provider_test")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_test")),
            artifact: Some(identity.clone()),
            invocation_metadata: Some(
                typed_invocation_metadata(
                    &RunId::new("run_test"),
                    "output",
                    &InterfaceId::new("world_provider_test"),
                    0,
                )
                .into(),
            ),
        };
        std::fs::write(
            &movements_path,
            format!("{}\n", serde_json::to_string(&movement).unwrap()),
        )
        .unwrap();
        let mut context = ResolvedSessionContext {
            provider_interface: InterfaceId::new("world_provider_test"),
            artifacts: vec![SessionContextArtifact {
                label: "interface.artifact.output".to_string(),
                payload: serde_json::json!({"text": "proved"}),
                source_artifact: identity,
                source_movement_seq: None,
            }],
        };
        let limits = InterfaceHostIoLimits {
            max_control_frame_bytes: 1_048_576,
            max_stderr_bytes: 262_144,
            max_manifest_bytes: 1_048_576,
            max_payload_bytes: 1_048_576,
        };
        let session = ProviderHandoffSession {
            interface: InterfaceId::new("world_provider_test"),
            movements_path: movements_path.canonicalize().unwrap(),
            writer_history: Arc::new(Mutex::new(BTreeMap::new())),
            writer_call_generations: Arc::new(Mutex::new(writer_run::WriterCallIndex::default())),
            evidence: Arc::new(Mutex::new(ProviderLedgerEvidenceIndex::new(
                InterfaceId::new("world_provider_test"),
                movements_path.canonicalize().unwrap(),
                limits.max_control_frame_bytes,
            ))),
        };
        validate_session_context_evidence(
            &mut context,
            SessionContextEvidenceScope {
                blobs_dir: &blobs_dir,
                limits,
                run: &RunId::new("run_test"),
                provider: &InterfaceId::new("world_provider_test"),
                context_output_labels: &BTreeSet::from(["interface.artifact.output".to_string()]),
                session_snapshot_labels: &BTreeSet::new(),
            },
            &session,
        )
        .unwrap();
        assert_eq!(context.artifacts[0].source_movement_seq, Some(0));
        for corruption in ["payload", "source", "sequence"] {
            let mut invalid = context.clone();
            match corruption {
                "payload" => invalid.artifacts[0].payload = serde_json::json!({"text": "forged"}),
                "source" => invalid.artifacts[0].source_artifact.id = ArtifactId::new("forged"),
                "sequence" => invalid.artifacts[0].source_movement_seq = Some(1),
                _ => unreachable!(),
            }
            let error = validate_session_context_evidence(
                &mut invalid,
                SessionContextEvidenceScope {
                    blobs_dir: &blobs_dir,
                    limits,
                    run: &RunId::new("run_test"),
                    provider: &InterfaceId::new("world_provider_test"),
                    context_output_labels: &BTreeSet::from(["interface.artifact.output".to_string()]),
                    session_snapshot_labels: &BTreeSet::new(),
                },
                &session,
            ).unwrap_err();
            assert!(error.contains(match corruption {
                "payload" => "payload differs from its pinned source artifact",
                "source" => "source id is not the pinned manifest identity",
                "sequence" => "does not prove its artifact",
                _ => unreachable!(),
            }), "{corruption}: {error}");
        }

        let input_file = InterfaceArtifactFile::write(&source_root, "tool.call", br#"{"tool":"new.tool","arguments":{}}"#).unwrap();
        let input_ref = pin_artifact(&input_file, &blobs_dir);
        let input_context = ResolvedSessionContext {
            provider_interface: context.provider_interface.clone(),
            artifacts: vec![SessionContextArtifact {
                label: "tool.call".into(), payload: serde_json::json!({"tool":"new.tool","arguments":{}}),
                source_artifact: input_ref.clone(), source_movement_seq: Some(0),
            }],
        };
        let mut incoming = movement.clone();
        incoming.kind = MovementKind::InterfaceOutbound;
        incoming.from = Endpoint::Interface(InterfaceId::new("system_worker_test"));
        incoming.to = Endpoint::Interface(context.provider_interface.clone());
        incoming.artifact = Some(input_ref.clone());
        let mut metadata = typed_invocation_metadata(&RunId::new("run_test"), "input", &context.provider_interface, 0);
        metadata.checkpoint_admission = Some(serde_json::from_value(serde_json::json!({"artifact_label":"tool.call","complete":true,"required_artifact_labels":[]})).unwrap());
        metadata.checkpoint_binding_policy = Some(CheckpointBindingPolicySnapshot {
            inbound: all_binding("system_worker_test"), outbound: vec![all_binding("world_provider_test")],
        });
        metadata.outbound_interface_bindings = vec![context.provider_interface.clone()];
        incoming.invocation_metadata = Some(metadata.into());
        let from_rows = |rows: &[Movement]| {
            std::fs::write(&movements_path, rows.iter().map(|row| format!("{}\n", serde_json::to_string(row).unwrap())).collect::<String>()).unwrap();
            ProviderHandoffSession {
                evidence: Arc::new(Mutex::new(ProviderLedgerEvidenceIndex::new(context.provider_interface.clone(), movements_path.clone(), limits.max_control_frame_bytes))),
                ..session.clone()
            }
        };
        let check = |context: &mut ResolvedSessionContext, session: &ProviderHandoffSession| validate_session_context_evidence(
            context, SessionContextEvidenceScope {
                blobs_dir: &blobs_dir, limits, run: &RunId::new("run_test"), provider: &InterfaceId::new("world_provider_test"),
                context_output_labels: &BTreeSet::from(["tool.call".into(), "interface.artifact.output".into()]), session_snapshot_labels: &BTreeSet::new(),
            }, session,
        );
        for case in ["valid", "unadmitted", "incomplete", "binding", "recipient", "run", "label", "verification", "sequence", "unpinned", "output_without_arrival", "outbound_binding"] {
            let mut row = serde_json::to_value(&incoming).unwrap();
            let mut candidate = input_context.clone();
            match case {
                "unadmitted" => row["invocation_metadata"]["checkpoint_admission"] = serde_json::Value::Null,
                "incomplete" => row["invocation_metadata"]["checkpoint_admission"]["complete"] = serde_json::json!(false),
                "binding" => row["invocation_metadata"]["checkpoint_binding_policy"]["inbound"]["interface"] = serde_json::json!("other_source"),
                "recipient" => row["to"] = serde_json::json!({"Interface":"other_provider"}),
                "run" => row["invocation_metadata"]["runtime"]["run"] = serde_json::json!("other_run"),
                "label" => row["invocation_metadata"]["checkpoint_admission"]["artifact_label"] = serde_json::json!("other.label"),
                "verification" => row["invocation_metadata"]["verification_failure"] = serde_json::json!({"error":"verification_failed","verifier":"check","details":{}}),
                "sequence" => candidate.artifacts[0].source_movement_seq = Some(1),
                "unpinned" => candidate.artifacts[0].source_movement_seq = None,
                "output_without_arrival" => {
                    row["from"] = serde_json::json!({"Interface":"world_provider_test"});
                    row["invocation_metadata"]["checkpoint_binding_policy"]["inbound"]["interface"] = serde_json::json!("world_provider_test");
                }
                "outbound_binding" => row["invocation_metadata"]["checkpoint_binding_policy"]["outbound"] = serde_json::json!([]),
                _ => {}
            }
            let session = from_rows(&[serde_json::from_value(row).unwrap()]);
            let result = check(&mut candidate, &session);
            assert_eq!(result.is_ok(), case == "valid", "{case}: {result:?}");
        }
        let later = one_shot_movement(1, &input_ref, "output", MovementKind::InterfaceArrival);
        let session = from_rows(&[incoming, later]);
        for sequence in [0, 2] {
            let mut candidate = input_context.clone();
            candidate.artifacts[0].source_movement_seq = Some(sequence);
            ground_session_context_movements(&mut candidate, &session, &RunId::new("run_test"), &config_with_tool_routes(&[])).unwrap();
            assert_eq!(candidate.artifacts[0].source_movement_seq, Some(sequence));
            assert_eq!(check(&mut candidate, &session).is_ok(), sequence == 0);
        }
    }

    fn config_with_tool_routes(routes: &[(&str, &[&str])]) -> RunAuthoringConfig {
        let mut checkpoints = vec![
            serde_json::json!({
                "name": "terminal_input",
                "role": "stable",
                "interface_binding": all_binding("world_input_test"),
                "outbound_interface_bindings": [all_binding("world_provider_test")],
                "include_context": true,
                "required_verified_artifacts": []
            }),
            serde_json::json!({
                "name": "client_output",
                "role": "provider_artifact_output",
                "interface_binding": all_binding("world_provider_test"),
                "outbound_interface_bindings": [all_binding("world_client_test")],
                "include_context": false,
                "required_verified_artifacts": []
            }),
            serde_json::json!({
                "name": "authoring_call",
                "role": "authoring_call",
                "interface_binding": all_binding("system_interface_authoring_test"),
                "outbound_interface_bindings": [all_binding("system_context_authoring_test")],
                "include_context": false,
                "required_verified_artifacts": []
            }),
            serde_json::json!({
                "name": "authoring_result",
                "role": "authoring_result",
                "interface_binding": all_binding("system_context_authoring_test"),
                "outbound_interface_bindings": [all_binding("world_provider_test")],
                "include_context": false,
                "required_verified_artifacts": []
            }),
        ];
        for (name, tools) in routes {
            checkpoints.push(serde_json::json!({
                "name": name,
                "role": "provider_tool_output",
                "interface_binding": all_binding("world_provider_test"),
                "outbound_interface_bindings": [all_binding(format!("system_{name}"))],
                "include_context": false,
                "required_verified_artifacts": [{
                    "mark": "tool.call.allowed",
                    "artifact_label": "tool.call",
                    "verifier": "tool_call_allowed",
                    "params": { "tools": tools }
                }]
            }));
        }
        serde_json::from_value(serde_json::json!({
            "run": "run_test",
            "worker": "system_worker_test",
            "provider_interface": "world_provider_test",
            "authoring_source_interface": "system_interface_authoring_test",
            "file_write_tool": "file.write",
            "file_write_label": "tool.call",
            "file_write_result_label": "file.write.result",
            "checkpoints": checkpoints,
            "result_timeout_ms": 1000
        }))
        .unwrap()
    }

    fn test_bound_topology(config: &RunAuthoringConfig) -> BoundRunAuthoringTopology {
        BoundRunAuthoringTopology {
            schema_version: 2,
            run: config.run.clone(),
            worker: config.worker.clone(),
            provider_interface: config.provider_interface.clone(),
            authoring_source_interface: config.authoring_source_interface.clone(),
            payload_artifact_bindings: config.payload_artifact_bindings.clone(),
            writer_run: config.writer_run.clone(),
            checkpoints: config.checkpoints.clone(),
        }
    }

    #[test]
    fn recorded_output_is_explicit_and_disjoint_from_return_bindings() {
        let mut config = config_with_tool_routes(&[]);
        let output = config.checkpoints.iter_mut().find(|cp| cp.role == RunAuthoringCheckpointRole::ProviderArtifactOutput).unwrap();
        output.interface_binding = names_binding("world_provider_test", &["interface.artifact.output", "turn.error"]);
        let mut recorded = output.clone();
        recorded.name = "recorded_output".into();
        recorded.interface_binding = names_binding("world_provider_test", &["turn.stream"]);
        recorded.outbound_interface_bindings.clear();
        config.checkpoints.push(recorded);
        config.validate(&config.provider_interface, &config.run).unwrap();
        let selected = config.output_checkpoint(&ProviderHandoffRoute::ClientProjection, &BTreeSet::new(), &BTreeSet::new(), "turn.stream").unwrap();
        assert!(selected.outbound_interface_bindings.is_empty());
        let error = config.output_checkpoint(&ProviderHandoffRoute::ClientProjection, &BTreeSet::new(), &BTreeSet::new(), "turn.error").unwrap();
        assert_eq!(error.outbound_interface_bindings.len(), 1);
        config.checkpoints.last_mut().unwrap().interface_binding = AuthoredInterfaceBinding::all(config.provider_interface.clone());
        assert!(config.validate(&config.provider_interface, &config.run).unwrap_err().contains("overlapping"));
    }

    #[test]
    fn private_run_authoring_recovery_owns_only_its_exact_correlation_namespace() {
        let prefix = "provider-run-authoring:run_test:";
        let foreign = br#"{"tool":"file.write","arguments":{"writes":[]},"correlation_id":"provider-other:run_test:batch"}"#;
        assert!(
            decode_owned_run_authoring_call(foreign, prefix)
                .unwrap()
                .is_none()
        );
        let owned = br#"{"tool":"file.write","arguments":{"path":"/tmp/run.json","patch":[],"expected_before_hash":"before","expected_after_hash":"after"},"correlation_id":"provider-run-authoring:run_test:1"}"#;
        assert!(
            decode_owned_run_authoring_call(owned, prefix)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn delivery_mode_rejects_partial_authority_and_accepts_strict_one_shot_slots() {
        use octopus_descriptors::EeId;
        use octopus_interfaces::{GrantProjection, RuntimeInvocationMetadata};

        let mut config = config_with_tool_routes(&[]);
        config
            .checkpoints
            .iter_mut()
            .find(|checkpoint| {
                checkpoint.role == RunAuthoringCheckpointRole::ProviderArtifactOutput
            })
            .unwrap()
            .required_verified_artifacts = serde_json::from_value(serde_json::json!([{
            "mark": "provider.output.admitted",
            "artifact_label": "interface.artifact.output",
            "verifier": "artifact_exists",
            "params": {}
        }]))
        .unwrap();
        config.checkpoints.push(
            serde_json::from_value(serde_json::json!({
                "name": "dispatch_attempt_1",
                "role": "provider_transport_dispatch",
                "interface_binding": all_binding("world_provider_test"),
                "outbound_interface_bindings": [all_binding("world_dispatch_sink_test")],
                "include_context": false,
                "required_verified_artifacts": [{
                    "mark": "provider.dispatch.admitted",
                    "artifact_label": "provider.transport.dispatch",
                    "verifier": "artifact_exists",
                    "params": {"attempt": 1}
                }]
            }))
            .unwrap(),
        );
        config.checkpoints.push(
            serde_json::from_value(serde_json::json!({
                "name": "handoff_completion",
                "role": "provider_handoff_completion",
                "interface_binding": all_binding("world_provider_test"),
                "outbound_interface_bindings": [],
                "include_context": false,
                "required_verified_artifacts": [{
                    "mark": "provider.handoff.completed",
                    "artifact_label": "provider.handoff.complete",
                    "verifier": "artifact_exists",
                    "params": {}
                }]
            }))
            .unwrap(),
        );
        config
            .validate(
                &InterfaceId::new("world_provider_test"),
                &RunId::new("run_test"),
            )
            .unwrap();
        let checkpoints = config
            .checkpoints
            .iter()
            .map(|checkpoint| {
                materialize_checkpoint(checkpoint, &checkpoint.name, &RunId::new("run_test"), &[])
            })
            .collect::<Vec<_>>();
        let active = config
            .checkpoints
            .iter()
            .map(|checkpoint| checkpoint.name.clone())
            .collect::<Vec<_>>();
        let record = RunRecord::from_bytes(
            &serde_json::to_vec(&serde_json::json!({
                "id": "run_test",
                "ee": "ee_test",
                "worker": "system_worker_test",
                "trace": "trace:run_test",
                "status": "running",
                "interface_state": {},
                "checkpoints": checkpoints,
                "active_checkpoints": active
            }))
            .unwrap(),
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let artifact_file = InterfaceArtifactFile::write(
            dir.path(),
            "interface.artifact.input",
            br#"{"text":"one shot"}"#,
        )
        .unwrap();
        let artifact = artifact_identity(&artifact_file, dir.path())
            .unwrap()
            .artifact;
        let runtime = RuntimeInvocationMetadata {
            run: RunId::new("run_test"),
            ee: EeId::new("ee_test"),
            worker: InterfaceId::new("system_worker_test"),
            checkpoint: CheckpointId::new("terminal_input"),
            interface: InterfaceId::new("world_provider_test"),
            epoch: 0,
            run_record_hash: None,
        };
        let mut input = InterfaceInvocationEnvelope {
            artifact,
            artifact_file,
            artifact_binding_payloads: Vec::new(),
            metadata: Some(
                InvocationMetadata::new(GrantProjection::default()).with_runtime(runtime),
            ),
        };
        let mode = derive_handoff_delivery_mode(
            &input,
            &config,
            &record,
            &CheckpointId::new("terminal_input"),
            &BTreeSet::new(),
        )
        .unwrap();
        let HandoffDeliveryMode::OneShot(plan) = mode else {
            panic!("expected one-shot delivery mode")
        };
        assert!(plan.terminal.contains_key("interface.artifact.output"));
        assert!(plan.dispatch.contains_key(&1));
        assert_eq!(
            plan.completion.checkpoint,
            CheckpointId::new("handoff_completion")
        );

        input.metadata.as_mut().unwrap().grant.tools = vec!["coder.exec".to_string()];
        let error = derive_handoff_delivery_mode(
            &input,
            &config,
            &record,
            &CheckpointId::new("terminal_input"),
            &BTreeSet::from(["coder.exec".to_string()]),
        )
        .unwrap_err();
        assert!(error.contains("tool continuation without run re-arm authority"));

        let grant = &mut input.metadata.as_mut().unwrap().grant;
        grant.tools = vec!["file.write".to_string()];
        grant.interfaces = vec![
            "world_provider_test".to_string(),
            "system_interface_authoring_test".to_string(),
            "system_context_authoring_test".to_string(),
        ];
        let unrestricted = derive_handoff_delivery_mode(
            &input,
            &config,
            &record,
            &CheckpointId::new("terminal_input"),
            &BTreeSet::from(["file.write".to_string()]),
        ).unwrap();
        assert_eq!(unrestricted, HandoffDeliveryMode::Rearm);
        input.metadata.as_mut().unwrap().grant.ee_root_slot_allowance = Some(Vec::new());
        let error = derive_handoff_delivery_mode(
            &input, &config, &record, &CheckpointId::new("terminal_input"),
            &BTreeSet::from(["file.write".to_string()]),
        ).unwrap_err();
        assert!(error.contains("does not admit run_records_root"));
        input
            .metadata
            .as_mut()
            .unwrap()
            .grant
            .ee_root_slot_allowance = Some(vec!["run_records_root".to_string()]);
        assert_eq!(
            derive_handoff_delivery_mode(
                &input,
                &config,
                &record,
                &CheckpointId::new("terminal_input"),
                &BTreeSet::from(["file.write".to_string()]),
            )
            .unwrap(),
            HandoffDeliveryMode::Rearm
        );
        let dispatch = config
            .output_checkpoint(
                &ProviderHandoffRoute::TransportDispatch { attempt: 2 },
                &BTreeSet::from(["file.write".to_string()]),
                &BTreeSet::new(),
                PROVIDER_TRANSPORT_DISPATCH_LABEL,
            )
            .unwrap();
        assert_eq!(
            dispatch.role,
            RunAuthoringCheckpointRole::ProviderTransportDispatch
        );
        assert!(dispatch.outbound_interface_bindings.is_empty());
        assert!(dispatch.required_verified_artifacts.is_empty());
        assert!(
            pending_output_checkpoint(
                &dispatch,
                "transport_dispatch_2",
                PROVIDER_TRANSPORT_DISPATCH_LABEL,
                &[ArtifactId::new("interface.context.updated:dynamic")],
                &[ArtifactId::new("context.reference:role")],
            )
            .artifact_bindings
                == vec![ArtifactId::new("context.reference:role")]
        );
    }

    fn provider_descriptor(folder: &str) -> InterfaceDescriptor {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join(folder)
            .join("interface.json");
        let bytes = std::fs::read(path).unwrap();
        octopus_descriptors::parse_strict(&bytes).unwrap()
    }

    fn local_profile_descriptor() -> InterfaceDescriptor {
        provider_descriptor("world_provider_ollama")
    }

    fn profile_run_value(provider: &InterfaceId, run: &RunId) -> serde_json::Value {
        let mut value = serde_json::json!({
            "id": run,
            "ee": "ee_profile_test",
            "worker": "system_worker_profile_test",
            "trace": format!("trace:{}", run.as_str()),
            "status": "running",
            "interface_state": {
                provider.as_str(): {
                    "context_generation": 0,
                    "authoring_generation": 0,
                    "frontier_checkpoint_ids": [
                        "client_output",
                        "tool_output",
                        "authoring_call",
                        "authoring_result"
                    ]
                }
            },
            "checkpoints": [
                {
                    "id": "stable_input",
                    "run": run,
                    "trace": format!("trace:{}:stable_input", run.as_str()),
                    "epoch": 4,
                    "interface_binding": all_binding("world_input_profile_test"),
                    "outbound_interface_bindings": [all_binding(provider.as_str())],
                    "artifact_bindings": [{"id": "artifact-context"}],
                    "required_verified_artifacts": []
                },
                {
                    "id": "client_output",
                    "run": run,
                    "trace": format!("trace:{}:client_output", run.as_str()),
                    "epoch": 0,
                    "interface_binding": all_binding(provider.as_str()),
                    "outbound_interface_bindings": [all_binding("world_client_profile_test")],
                    "artifact_bindings": [],
                    "required_verified_artifacts": []
                },
                {
                    "id": "tool_output",
                    "run": run,
                    "trace": format!("trace:{}:tool_output", run.as_str()),
                    "epoch": 0,
                    "interface_binding": all_binding(provider.as_str()),
                    "outbound_interface_bindings": [all_binding("system_tool_profile_test")],
                    "artifact_bindings": [],
                    "required_verified_artifacts": [{
                        "mark": "tool.call.allowed",
                        "artifact_label": "tool.call",
                        "verifier": "tool_call_allowed",
                        "params": {"tools": ["coder.exec"]}
                    }]
                },
                {
                    "id": "authoring_call",
                    "run": run,
                    "trace": format!("trace:{}:authoring_call", run.as_str()),
                    "epoch": 0,
                    "interface_binding": all_binding("system_interface_authoring_profile_test"),
                    "outbound_interface_bindings": [all_binding("system_context_authoring_profile_test")],
                    "artifact_bindings": [],
                    "required_verified_artifacts": []
                },
                {
                    "id": "authoring_result",
                    "run": run,
                    "trace": format!("trace:{}:authoring_result", run.as_str()),
                    "epoch": 0,
                    "interface_binding": all_binding("system_context_authoring_profile_test"),
                    "outbound_interface_bindings": [all_binding(provider.as_str())],
                    "artifact_bindings": [],
                    "required_verified_artifacts": []
                }
            ],
            "active_checkpoints": [
                "stable_input",
                "client_output",
                "tool_output",
                "authoring_call",
                "authoring_result"
            ]
        });
        let record = RunRecord::from_bytes(&serde_json::to_vec(&value).unwrap()).unwrap();
        let topology = derive_bound_run_authoring_topology(
            &record,
            provider,
            &CheckpointId::new("stable_input"),
        )
        .unwrap();
        value["interface_state"][provider.as_str()][RUN_AUTHORING_BINDING] =
            serde_json::to_value(topology).unwrap();
        value
    }

    fn pending_movement(seq: u64, item: &PendingHandoffItem, artifact: &ArtifactRef) -> Movement {
        Movement {
            seq: octopus_ledger::Seq(seq),
            lamport: octopus_ledger::Lamport(seq + 1),
            recorded_at_unix_ms: seq + 1,
            trace: octopus_ledger::TraceId(format!("trace-pending-{seq}")),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(InterfaceId::new("world_provider_test")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_test")),
            artifact: Some(artifact.clone()),
            invocation_metadata: Some(
                typed_invocation_metadata(
                    &RunId::new("run_test"),
                    item.checkpoint.id.as_str(),
                    &InterfaceId::new("world_provider_test"),
                    item.checkpoint.epoch,
                )
                .into(),
            ),
        }
    }

    fn retained_pending_movements(delivery: &octopus_interface_transport::InterfaceActivationArrival,
        mut arrival: Movement, snapshot: &str) -> [Movement; 2] {
        let scope_seq = arrival.seq.0;
        let mut metadata = arrival.invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
        metadata.runtime.as_mut().unwrap().run_record_hash = Some(snapshot.to_string());
        metadata.delivery_scope_seq = Some(scope_seq);
        let mut scope = arrival.clone();
        scope.kind = MovementKind::ArtifactMoved;
        scope.to = scope.from.clone();
        scope.invocation_metadata = Some(octopus_ledger::MovementMetadata::ArrivalFanout(octopus_ledger::ArrivalFanoutMetadata {
            arrival_fanout: octopus_ledger::ArrivalFanoutScope {
                identity: delivery.retained.as_ref().unwrap().id.clone(), targets: vec![metadata.runtime.clone().unwrap()],
            },
        }));
        arrival.seq.0 += 1;
        arrival.lamport.0 += 1;
        arrival.invocation_metadata = Some(metadata.into());
        [scope, arrival]
    }

    #[test]
    fn run_authoring_result_identity_requires_the_private_correlation_shape() {
        let descriptor = local_profile_descriptor();
        assert_eq!(
            RunAuthoringResultIdentity::from_descriptor(&descriptor)
                .unwrap()
                .artifact_label(),
            "file.write.result"
        );
        assert_eq!(
            correlation_generation(&RunId::new("run_test"), "provider-run-authoring:run_test:7")
                .unwrap(),
            7
        );
        assert!(correlation_generation(&RunId::new("run_test"), "call_model_123").is_err());
        assert!(
            correlation_generation(
                &RunId::new("run_test"),
                "provider-run-authoring:run_test:07"
            )
            .is_err()
        );
    }

    #[test]
    fn run_authoring_profile_requires_its_file_tool_to_be_hidden_from_the_model() {
        let mut descriptor = local_profile_descriptor();
        descriptor
            .interface_driver_config
            .as_mut()
            .unwrap()
            .private_config
            .insert(
                "model_hidden_tools".to_string(),
                serde_json::json!(["coder.exec"]),
            );
        let error = run_authoring_profile(&descriptor).unwrap_err();
        assert!(error.contains("exposes run-authoring tool 'file.write' to the model"));
    }

    #[test]
    fn context_free_provider_outputs_keep_only_mandatory_role_bindings() {
        let config = config_with_tool_routes(&[("coder", &["coder.exec"])]);
        let mut seeded = config.clone();
        let stable = seeded.checkpoints.iter().position(|checkpoint| checkpoint.role == RunAuthoringCheckpointRole::Stable).unwrap();
        seeded.checkpoints[stable].static_artifact_bindings = vec![ArtifactId::new("declared-seed")];
        seeded.validate(&seeded.provider_interface, &seeded.run).unwrap();
        seeded.checkpoints[stable].static_artifact_bindings.push(ArtifactId::new("declared-seed"));
        assert!(seeded.validate(&seeded.provider_interface, &seeded.run).is_err());
        for role in [RunAuthoringCheckpointRole::ProviderArtifactOutput, RunAuthoringCheckpointRole::AuthoringCall, RunAuthoringCheckpointRole::AuthoringResult] {
            let mut invalid = config.clone();
            invalid.checkpoints.iter_mut().find(|checkpoint| checkpoint.role == role).unwrap().static_artifact_bindings = vec![ArtifactId::new("forbidden-seed")];
            assert!(invalid.validate(&invalid.provider_interface, &invalid.run).is_err());
        }
        let tool = config
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.role == RunAuthoringCheckpointRole::ProviderToolOutput)
            .unwrap();
        let full = vec![
            ArtifactId::new("context.reference:role"),
            ArtifactId::new("interface.context.updated:dynamic"),
        ];
        let mandatory_role = vec![ArtifactId::new("context.reference:role")];
        let pending =
            pending_output_checkpoint(tool, "tool_output", "tool.call", &full, &mandatory_role);
        assert_eq!(pending.artifact_bindings, mandatory_role);
        let mut declared = config.clone();
        let output = declared.checkpoints.iter_mut().find(|checkpoint| checkpoint.role == RunAuthoringCheckpointRole::ProviderToolOutput).unwrap();
        output.include_context = true;
        output.static_artifact_bindings = vec![ArtifactId::new("explicit-object")];
        declared.validate(&declared.provider_interface, &declared.run).unwrap();
        let output = declared.checkpoints.iter().find(|checkpoint| checkpoint.role == RunAuthoringCheckpointRole::ProviderToolOutput).unwrap();
        let bound = pending_output_checkpoint(output, "bound_output", "tool.call", &full, &mandatory_role);
        assert_eq!(bound.artifact_bindings, [full.clone(), vec![ArtifactId::new("explicit-object")]].concat());
        let roles = vec![ArtifactBinding { id: mandatory_role[0].clone(), access: Default::default() }];
        let mut projected: octopus_run_record::CheckpointRecord = serde_json::from_value(
            materialize_pending_checkpoint(&bound, &config.run),
        ).unwrap();
        assert!(checkpoint_role_bindings_match(&projected, &roles, output));
        projected.artifact_bindings.swap(1, 2);
        assert!(checkpoint_role_bindings_match(&projected, &roles, output));
        projected.artifact_bindings[1].access.read = false;
        assert!(!checkpoint_role_bindings_match(&projected, &roles, output));
        projected.artifact_bindings.remove(1);
        assert!(!checkpoint_role_bindings_match(&projected, &roles, output));
        let mut overlap = output.clone();
        overlap.static_artifact_bindings.push(mandatory_role[0].clone());
        let mut roles = roles;
        roles[0].access.change = true;
        let mut projected: octopus_run_record::CheckpointRecord = serde_json::from_value(
            materialize_pending_checkpoint(&bound, &config.run),
        ).unwrap();
        projected.artifact_bindings[0] = roles[0].clone();
        assert!(checkpoint_role_bindings_match(&projected, &roles, &overlap));
        projected.artifact_bindings[2].access.change = true;
        assert!(!checkpoint_role_bindings_match(&projected, &roles, &overlap));
        projected.artifact_bindings[2].access.change = false;
        projected.artifact_bindings[0].access.change = false;
        assert!(!checkpoint_role_bindings_match(&projected, &roles, &overlap));
        projected.artifact_bindings[0] = roles[0].clone();
        projected.artifact_bindings.push(roles[0].clone());
        assert!(!checkpoint_role_bindings_match(&projected, &roles, &overlap));
        projected.artifact_bindings.pop();
        projected.artifact_bindings.swap(0, 1);
        assert!(!checkpoint_role_bindings_match(&projected, &roles, &overlap));
        assert_eq!(
            pending.interface_binding.config.artifact_policy,
            BindingArtifactPolicy::Allow {
                names: vec!["tool.call".to_string()],
                namespaces: Vec::new(),
            }
        );

        let client = config
            .checkpoints
            .iter()
            .find(|checkpoint| {
                checkpoint.role == RunAuthoringCheckpointRole::ProviderArtifactOutput
            })
            .unwrap();
        assert_eq!(
            pending_output_checkpoint(
                client,
                "client_output",
                "interface.artifact.output",
                &full,
                &mandatory_role,
            )
            .artifact_bindings,
            mandatory_role,
        );
    }

    #[test]
    fn rearm_batches_preserve_order_and_isolate_repeated_names_and_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let emitted = dir.path().join("emitted");
        let emissions = [
            ("agent.message.delta", br#"{"text":"one"}"#.as_slice()),
            ("account.rate_limits", br#"{"limit":1}"#.as_slice()),
            ("agent.message.delta", br#"{"text":"two"}"#.as_slice()),
        ]
        .into_iter()
        .map(|(label, payload)| ProviderHandoffEmission {
            artifact: InterfaceArtifactFile::write(&emitted, label, payload).unwrap(),
            route: ProviderHandoffRoute::ClientProjection,
        })
        .collect();
        let batches = split_handoff_batches_by_artifact_name(emissions, &BTreeSet::new()).unwrap();
        assert_eq!(
            batches.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![2, 1]
        );
        assert_eq!(batches.iter().flatten().map(|emission| emission.artifact.read_manifest().unwrap().label).collect::<Vec<_>>(),
            vec!["agent.message.delta", "account.rate_limits", "agent.message.delta"]);
        let emissions = ["tool.call", "turn.completed", "turn.stream", "operator.input.request", "provider.account.updated"]
            .into_iter().map(|label| ProviderHandoffEmission {
                artifact: InterfaceArtifactFile::write(&emitted, label, br#"{}"#).unwrap(),
                route: if label == "operator.input.request" {
                    ProviderHandoffRoute::ClientProjectionWithVerifiedInputs { inputs: Vec::new() }
                } else { ProviderHandoffRoute::ClientProjection },
            }).collect();
        let batches = split_handoff_batches_by_artifact_name(emissions, &BTreeSet::from(["turn.completed".into()])).unwrap();
        assert_eq!(batches.iter().map(Vec::len).collect::<Vec<_>>(), vec![1, 1, 1, 1, 1]);
        let names = batches.iter().flatten().map(|emission| emission.artifact.read_manifest().unwrap().label).collect::<Vec<_>>();
        assert_eq!(names, vec!["tool.call", "turn.completed", "turn.stream", "operator.input.request", "provider.account.updated"]);
    }

    #[test]
    fn verified_operator_inputs_are_exact_granted_one_shot_edges() {
        let config = config_with_tool_routes(&[]);
        let output = config
            .checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput)
            .unwrap();
        let requirement = |mark: &str, verifier: &str| RequiredVerifiedArtifact {
            mark: octopus_descriptors::MarkId::new(mark),
            artifact_label: "operator.approval.response".to_string(),
            verifier: octopus_descriptors::VerifierId::new(verifier),
            params: serde_json::json!({"request_id": "request-7"}),
        };
        let route = ProviderHandoffRoute::ClientProjectionWithVerifiedInputs {
            inputs: vec![
                ProviderVerifiedInput {
                    also_deliver_to_tool: None,
                    also_deliver_to_interface: None,
                    artifact_label: "operator.approval.response".to_string(),
                    required_verified_artifact: requirement(
                        "operator.approval",
                        "operator_approval",
                    ),
                },
                ProviderVerifiedInput {
                    also_deliver_to_tool: None,
                    also_deliver_to_interface: None,
                    artifact_label: "operator.approval.response".to_string(),
                    required_verified_artifact: requirement(
                        "operator.input.provided",
                        "operator_input",
                    ),
                },
            ],
        };
        let marks = BTreeSet::from([
            "operator.approval".to_string(),
            "operator.input.provided".to_string(),
        ]);
        let static_context = vec![ArtifactId::new("artifact-static-reference")];
        let verified = config
            .verified_input_checkpoints(&route, output, &static_context, &marks, &BTreeSet::new(), &BTreeSet::new())
            .unwrap();
        assert_eq!(verified.len(), 2);
        assert!(verified.iter().all(|checkpoint| {
            checkpoint.role == RunAuthoringCheckpointRole::VerifiedInput
                && checkpoint.static_artifact_bindings == static_context
                && checkpoint.include_context
                && binding_admits_exact_name(
                    &checkpoint.interface_binding,
                    "operator.approval.response",
                )
                && binding_admits_exact_name(
                    &checkpoint.outbound_interface_bindings[0],
                    "operator.approval.response",
                )
        }));

        let mut next = config.clone();
        next.checkpoints.extend(verified);
        next.validate(&next.provider_interface, &next.run).unwrap();
        let run = next.run.clone();
        let mut root = serde_json::json!({
            "checkpoints": config.checkpoints.iter().map(|checkpoint| {
                materialize_checkpoint(checkpoint, &checkpoint.name, &run, &[])
            }).collect::<Vec<_>>(),
            "active_checkpoints": config.checkpoints.iter().map(|checkpoint| {
                checkpoint.name.clone()
            }).collect::<Vec<_>>()
        })
        .as_object()
        .unwrap()
        .clone();
        replace_verified_input_checkpoints(&mut root, &config, &next, &run, "request", &[])
            .unwrap();
        assert_eq!(
            root["active_checkpoints"].as_array().unwrap().len(),
            config.checkpoints.len() + 2
        );
        replace_verified_input_checkpoints(&mut root, &next, &config, &run, "response", &[])
            .unwrap();
        assert_eq!(
            root["active_checkpoints"].as_array().unwrap().len(),
            config.checkpoints.len()
        );

        let error = config
            .verified_input_checkpoints(
                &route,
                output,
                &static_context,
                &BTreeSet::from(["operator.approval".to_string()]),
                &BTreeSet::new(),
                &BTreeSet::new(),
            )
            .unwrap_err();
        assert!(error.contains("outside the formed invocation grant"));

        let ProviderHandoffRoute::ClientProjectionWithVerifiedInputs { mut inputs } = route else {
            unreachable!()
        };
        let recipient = InterfaceId::new("world_input_test");
        for input in &mut inputs {
            input.also_deliver_to_interface = Some(recipient.clone());
        }
        let route = ProviderHandoffRoute::ClientProjectionWithVerifiedInputs { inputs };
        let unadmitted = config
            .verified_input_checkpoints(&route, output, &static_context, &marks, &BTreeSet::new(), &BTreeSet::new())
            .unwrap_err();
        assert!(unadmitted.contains("not a distinct granted interface"));
        let delivered = config
            .verified_input_checkpoints(&route, output, &static_context, &marks, &BTreeSet::new(), &BTreeSet::from([recipient.clone()]))
            .unwrap();
        assert!(delivered.iter().all(|checkpoint| {
            checkpoint
                .outbound_interfaces()
                .eq([&config.provider_interface, &recipient])
        }));
        let mut next = config.clone();
        next.checkpoints.extend(delivered);
        next.validate(&next.provider_interface, &next.run).unwrap();
    }

    #[test]
    fn base_provider_profile_lifts_derives_and_persists_exact_run_handoff() {
        use octopus_descriptors::EeId;
        use octopus_interfaces::{GrantProjection, RuntimeInvocationMetadata};
        use octopus_ledger::{Lamport, Seq, TraceId};
        use tokio_util::sync::CancellationToken;

        let descriptor = local_profile_descriptor();
        let result_identity = RunAuthoringResultIdentity::from_descriptor(&descriptor).unwrap();
        assert_eq!(result_identity.artifact_label(), "file.write.result");
        let profile = run_authoring_profile(&descriptor).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let provider = descriptor.id.clone();
        let run = RunId::new("run_profile_test");
        let runs_root = dir.path().join("runs");
        let hand_in_root = dir.path().join("hand-in");
        let control_root = dir.path().join("control");
        let var_dir = dir.path().join("var/provider");
        let blobs_dir = dir.path().join("ledger/blobs");
        let movements_path = dir.path().join("ledger/movements.jsonl");
        std::fs::create_dir_all(&runs_root).unwrap();
        std::fs::create_dir_all(&blobs_dir).unwrap();
        std::fs::create_dir_all(&control_root).unwrap();
        std::fs::write(&movements_path, b"").unwrap();
        let run_value = profile_run_value(&provider, &run);
        let mut run_bytes = serde_json::to_vec_pretty(&run_value).unwrap();
        run_bytes.push(b'\n');
        let run_path = runs_root.join(format!("{}.json", run.as_str()));
        std::fs::write(&run_path, &run_bytes).unwrap();
        let input_file = InterfaceArtifactFile::write(
            var_dir.join("input"),
            "interface.artifact.input",
            br#"{"text":"profile input"}"#,
        )
        .unwrap();
        let input_ref = artifact_identity(&input_file, &var_dir).unwrap().artifact;
        let metadata = InvocationMetadata::new(GrantProjection {
            tools: vec!["coder.exec".to_string(), "file.write".to_string()],
            interfaces: vec![
                provider.as_str().to_string(),
                "system_interface_authoring_profile_test".to_string(),
                "system_context_authoring_profile_test".to_string(),
            ],
            ee_root_slot_allowance: Some(vec!["run_records_root".to_string()]),
            ..GrantProjection::default()
        })
        .with_runtime(RuntimeInvocationMetadata {
            run: run.clone(),
            ee: EeId::new("ee_profile_test"),
            worker: InterfaceId::new("system_worker_profile_test"),
            checkpoint: CheckpointId::new("stable_input"),
            interface: provider.clone(),
            epoch: 4,
            run_record_hash: Some(ContentHash::of_bytes(&run_bytes).to_hex()),
        });
        let input = InterfaceInvocationEnvelope {
            artifact: input_ref,
            artifact_file: input_file,
            artifact_binding_payloads: Vec::new(),
            metadata: Some(metadata),
        };
        let (arrivals, _arrival_rx) = tokio::sync::mpsc::channel(1);
        let ctx = InterfaceLiftContext::unprivileged(
            provider.clone(),
            dir.path()
                .join(format!("registry/interface/{}", provider.as_str())),
            Some(descriptor.clone()),
            var_dir.clone(),
            movements_path,
            blobs_dir,
            dir.path().join("ledger/ledger-index.sqlite"),
            BTreeMap::from([
                ("run_records_root".to_string(), runs_root.clone()),
                ("interface_hand_in_root".to_string(), hand_in_root.clone()),
                ("app_server_control_root".to_string(), control_root),
            ]),
            arrivals,
            CancellationToken::new(),
        )
        .with_host_io_limits(InterfaceHostIoLimits {
            max_control_frame_bytes: 1_048_576,
            max_stderr_bytes: 262_144,
            max_manifest_bytes: 1_048_576,
            max_payload_bytes: 268_435_456,
        })
        .unwrap();
        let mut mismatched_ctx = ctx.clone();
        mismatched_ctx.interface = InterfaceId::new("world_provider_profile_mismatch");
        let error = ProviderHandoffSession::new(&mismatched_ctx)
            .unwrap()
            .from_invocation(&mismatched_ctx, &descriptor, &run, &input, 23, &run_bytes)
            .err()
            .unwrap();
        assert!(error.contains("does not match lifted interface"));
        let session = ProviderHandoffSession::new(&ctx).unwrap();
        {
            let mut plain: serde_json::Value = serde_json::from_slice(&run_bytes).unwrap();
            plain["interface_state"].as_object_mut().unwrap().remove(provider.as_str());
            let plain_bytes = serde_json::to_vec(&plain).unwrap();
            std::fs::write(&run_path, &plain_bytes).unwrap();
            for (name, pinned, expected_error) in [("plain", &plain_bytes, false), ("lost", &run_bytes, true)] {
                let mut probe_ctx = ctx.clone();
                probe_ctx.movements_path = ctx.movements_path.with_file_name(format!("candidate-{name}.jsonl"));
                probe_ctx.ledger_index_path = ctx.ledger_index_path.with_file_name(format!("candidate-{name}.sqlite"));
                let hash = ContentHash::of_bytes(pinned);
                std::fs::write(ctx.blobs_dir.join(hash.to_hex()), pinned).unwrap();
                let mut metadata = input.metadata().unwrap().clone();
                metadata.runtime.as_mut().unwrap().run_record_hash = Some(hash.to_hex());
                let row = Movement { seq: Seq(0), lamport: Lamport(0), recorded_at_unix_ms: 0,
                    trace: TraceId("candidate".into()), kind: MovementKind::InterfaceOutbound,
                    from: Endpoint::Interface(InterfaceId::new("world_input_profile_test")), to: Endpoint::Interface(provider.clone()),
                    artifact: Some(input.artifact.clone()), invocation_metadata: Some(metadata.into()) };
                std::fs::write(&probe_ctx.movements_path, format!("{}\n", serde_json::to_string(&row).unwrap())).unwrap();
                let candidates = ProviderHandoffSession::new(&probe_ctx).unwrap().completed_source_candidates(&probe_ctx);
                if expected_error { assert!(candidates.unwrap_err().contains("lost its declared authoring state")); }
                else { assert!(candidates.unwrap().is_empty()); }
            }
            std::fs::write(&run_path, &run_bytes).unwrap();
        }
        let mut handoff = session
            .from_invocation(&ctx, &descriptor, &run, &input, 23, &run_bytes)
            .unwrap();
        assert_eq!(
            handoff.hand_in_root,
            hand_in_root.join("system_interface_authoring_profile_test")
        );
        let independent_run = exercise_independent_provider_owner(&handoff, &run_bytes);
        {
            let mut probe: serde_json::Value = serde_json::from_slice(&independent_run).unwrap();
            let mut topology: BoundRunAuthoringTopology = serde_json::from_value(
                probe["interface_state"][provider.as_str()][RUN_AUTHORING_BINDING].clone(),
            ).unwrap();
            let mut independent = topology.checkpoints.iter().find(|cp| cp.role == RunAuthoringCheckpointRole::Stable).unwrap().clone();
            independent.name = "independent_probe".into();
            independent.role = RunAuthoringCheckpointRole::IndependentInput;
            independent.include_context = false;
            independent.interface_binding = all_binding("world_peer_probe");
            probe["checkpoints"].as_array_mut().unwrap().push(materialize_checkpoint(&independent, &independent.name, &run, &[]));
            probe["active_checkpoints"].as_array_mut().unwrap().push(serde_json::json!(independent.name));
            topology.checkpoints.push(independent.clone());
            probe["interface_state"][provider.as_str()][RUN_AUTHORING_BINDING] = serde_json::json!(topology);
            let probe_bytes = serde_json::to_vec(&probe).unwrap();
            std::fs::write(&run_path, &probe_bytes).unwrap();
            let mut probe_ctx = ctx.clone();
            probe_ctx.movements_path = ctx.movements_path.with_file_name("consumed-inputs.jsonl");
            probe_ctx.ledger_index_path = ctx.ledger_index_path.with_file_name("unused-inputs.sqlite");
            let rows = [(0, "stable_input", "world_input_profile_test", 4), (1, "independent_probe", "world_peer_probe", 0)]
                .into_iter().map(|(seq, checkpoint, source, epoch)| {
                    let mut metadata = input.metadata().unwrap().clone();
                    metadata.runtime.as_mut().unwrap().checkpoint = CheckpointId::new(checkpoint);
                    metadata.runtime.as_mut().unwrap().epoch = epoch;
                    metadata.runtime.as_mut().unwrap().run_record_hash = Some(ContentHash::of_bytes(&probe_bytes).to_hex());
                    if checkpoint == "independent_probe" {
                        metadata = metadata.with_checkpoint_binding_policy(independent.binding_policy_snapshot())
                            .with_outbound_interface_bindings(vec![provider.clone()]);
                    }
                    Movement { seq: Seq(seq), lamport: Lamport(seq), recorded_at_unix_ms: 0,
                        trace: TraceId("probe".into()), kind: MovementKind::InterfaceOutbound,
                        from: Endpoint::Interface(InterfaceId::new(source)), to: Endpoint::Interface(provider.clone()),
                        artifact: Some(input.artifact.clone()), invocation_metadata: Some(metadata.into()), }
                }).collect::<Vec<_>>();
            std::fs::write(&probe_ctx.movements_path, rows.iter().map(|row| format!("{}\n", serde_json::to_string(row).unwrap())).collect::<String>()).unwrap();
            let probe_session = ProviderHandoffSession::new(&probe_ctx).unwrap();
            assert_eq!(probe_session.consumed_context_input_sources(&probe_ctx, &descriptor).unwrap(),
                vec![(run.clone(), 0), (run.clone(), 1)]);
            let mut independent_input = input.clone();
            independent_input.metadata = Some(rows[1].invocation_metadata.as_ref().unwrap().invocation().unwrap().clone());
            let mut changed = probe.clone();
            changed["interface_state"]["system_neighbor"] = serde_json::json!({"revision":7});
            let changed_bytes = serde_json::to_vec(&changed).unwrap();
            std::fs::write(&run_path, &changed_bytes).unwrap();
            let independent_handoff = probe_session.from_invocation(
                &probe_ctx, &descriptor, &run, &independent_input, 1, &probe_bytes,
            ).unwrap();
            assert!(!independent_handoff.source_carries_context);
            let restored = probe_session.failed_source_handoff(&probe_ctx, &descriptor, &run, &independent_input, 1).unwrap();
            assert_eq!(restored.state.try_lock().unwrap().consumed_context_free_independent_input.as_ref(), Some(&independent));
            assert!(independent_handoff.invocation_session_snapshot().is_none());
            let state = independent_handoff.state.try_lock().unwrap();
            assert!(state.session_context.artifacts.is_empty());
            assert_eq!(state.run_record_bytes, changed_bytes);
            assert_eq!(state.consumed_context_free_independent_input.as_ref(), Some(&independent));
            assert!(probe_session.from_invocation(&probe_ctx, &descriptor, &run, &input, 0, &probe_bytes).is_err());
            assert!(probe_session.from_invocation(&probe_ctx, &descriptor, &run, &independent_input, 0, &probe_bytes).is_err());
            let cp_index = changed["checkpoints"].as_array().unwrap().iter().position(|cp| cp["id"] == "independent_probe").unwrap();
            let template_index = topology.checkpoints.len() - 1;
            for (pointer, value) in [
                (format!("/checkpoints/{cp_index}/epoch"), serde_json::json!(1)),
                (format!("/checkpoints/{cp_index}/interface_binding/interface"), serde_json::json!("other_source")),
                (format!("/interface_state/{provider}/run_authoring_binding/checkpoints/{template_index}/include_context"), serde_json::json!(true)),
            ] {
                let mut drift = changed.clone();
                *drift.pointer_mut(&pointer).unwrap() = value;
                std::fs::write(&run_path, serde_json::to_vec(&drift).unwrap()).unwrap();
                assert!(probe_session.from_invocation(&probe_ctx, &descriptor, &run, &independent_input, 1, &probe_bytes).is_err(), "{pointer}");
            }
            std::fs::write(&run_path, &changed_bytes).unwrap();
            let mut forged = independent_input.clone();
            forged.metadata.as_mut().unwrap().runtime.as_mut().unwrap().run_record_hash = Some(ContentHash::of_bytes(b"wrong").to_hex());
            assert!(probe_session.from_invocation(&probe_ctx, &descriptor, &run, &forged, 1, &probe_bytes).is_err());
            forged = independent_input.clone();
            forged.artifact.content = ContentHash::of_bytes(b"wrong");
            assert!(probe_session.from_invocation(&probe_ctx, &descriptor, &run, &forged, 1, &probe_bytes).is_err());
            let mut rearmed = probe;
            rearmed["checkpoints"].as_array_mut().unwrap().iter_mut().find(|cp| cp["id"] == "independent_probe").unwrap()["epoch"] = serde_json::json!(1);
            std::fs::write(&run_path, serde_json::to_vec(&rearmed).unwrap()).unwrap();
            assert_eq!(probe_session.consumed_context_input_sources(&probe_ctx, &descriptor).unwrap(), vec![(run.clone(), 0)]);
            std::fs::write(&run_path, &run_bytes).unwrap();
        }
        let topology = handoff.bound_topology.clone();
        assert_eq!(topology.run, run);
        assert_eq!(topology.provider_interface, provider);
        assert_eq!(topology.checkpoints.len(), 5);
        {
            let failed = session
                .failed_source_handoff(&ctx, &descriptor, &run, &input, 23)
                .unwrap();
            assert!(failed.failed_source && !failed.source_carries_context);
            assert!(failed.context_output_labels.is_empty());
            assert!(failed.session_snapshot_labels.is_empty());
            let error_file = InterfaceArtifactFile::write(
                var_dir.join("emitted"),
                "turn.error",
                br#"{"label":"turn.error","message":"turn stopped before dispatch"}"#,
            )
            .unwrap();
            let state = failed.state.try_lock().unwrap();
            assert!(state.consumed_input);
            assert_eq!(state.context_artifacts, vec![ArtifactId::new("artifact-context")]);
            let rearmed = failed
                .author_run_with_bindings(
                    &state.run_record_bytes,
                    &state.context_artifacts,
                    &[PreparedHandoff {
                        identity: artifact_identity(&error_file, &var_dir).unwrap(),
                        output_checkpoint: failed
                            .config
                            .checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput)
                            .unwrap()
                            .clone(),
                        verified_inputs: Vec::new(),
                    }],
                    state.consumed_input,
                    None,
                    false,
                    true,
                    &[],
                )
                .unwrap();
            let rearmed_record = RunRecord::from_bytes(rearmed.content.as_bytes()).unwrap();
            assert_eq!(rearmed_record.status, serde_json::json!("idle"));
            assert!(rearmed.pending_handoff.terminal);
            assert!(rearmed.pending_handoff.source_without_context);
            assert_eq!(
                rearmed_record.interface_state[&provider]["context_generation"],
                RunRecord::from_bytes(&state.run_record_bytes).unwrap().interface_state
                    [&provider]["context_generation"]
            );
            assert_eq!(rearmed.pending_handoff.source_movement_seq, 23);
            let mut probe = handoff.clone();
            probe.run_path = var_dir.join("failed-context-probe.json");
            probe.ctx.movements_path = var_dir.join("failed-context-probe.jsonl");
            probe.ctx.ledger_index_path = var_dir.join("failed-context-probe.sqlite");
            std::fs::write(&probe.run_path, rearmed.content.as_bytes()).unwrap();
            std::fs::write(ctx.blobs_dir.join(ContentHash::of_bytes(&run_bytes).to_hex()), &run_bytes).unwrap();
            let source_ref = pin_artifact(&input.artifact_file, &ctx.blobs_dir);
            let source_template = handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::Stable).unwrap();
            let metadata = input.metadata().unwrap().clone()
                .with_checkpoint_binding_policy(source_template.binding_policy_snapshot())
                .with_outbound_interface_bindings(vec![provider.clone()]);
            let mut rows = (0..24).map(ledger_row).collect::<Vec<_>>();
            rows[23].kind = MovementKind::InterfaceOutbound;
            rows[23].from = Endpoint::Interface(source_template.interface().clone());
            rows[23].to = Endpoint::Interface(provider.clone());
            rows[23].artifact = Some(source_ref);
            rows[23].invocation_metadata = Some(metadata.into());
            std::fs::write(&probe.ctx.movements_path, rows.iter()
                .map(|row| format!("{}\n", serde_json::to_string(row).unwrap())).collect::<String>()).unwrap();
            probe.session = ProviderHandoffSession::new(&probe.ctx).unwrap();
            let mut probe_state = handoff.state.try_lock().unwrap().clone();
            probe_state.run_record_bytes = rearmed.content.as_bytes().to_vec();
            let before = session_artifacts_fingerprint(&probe_state.session_context.artifacts).unwrap();
            let error = probe.restore_failed_context(&mut probe_state, &[]).unwrap_err();
            assert!(error.contains("no declared checked owner writers"), "{error}");
            assert_eq!(session_artifacts_fingerprint(&probe_state.session_context.artifacts).unwrap(), before);
            probe.source_carries_context = false;
            assert!(probe.checked_failed_source_contexts(&probe_state.run_record_bytes, &[], None).unwrap().is_empty());
            assert!(!rearmed_record
                .active_checkpoints
                .contains(&CheckpointId::new("stable_input")));
            let rearmed_input = rearmed_record
                .checkpoints
                .iter()
                .find(|checkpoint| {
                    rearmed_record.active_checkpoints.contains(&checkpoint.id)
                        && checkpoint.inbound_interface().map(InterfaceId::as_str)
                            == Some("world_input_profile_test")
                })
                .unwrap();
            assert_eq!(
                rearmed_input
                    .artifact_bindings
                    .iter()
                    .map(|binding| binding.id.as_str())
                    .collect::<Vec<_>>(),
                ["artifact-context"]
            );
            let tool_file = InterfaceArtifactFile::write(var_dir.join("emitted"), "tool.call",
                br#"{"tool":"coder.exec","arguments":{},"correlation_id":"refused-tool"}"#).unwrap();
            let tool = handoff.author_run(&run_bytes, &state.context_artifacts,
                &[PreparedHandoff {
                    identity: artifact_identity(&tool_file, &var_dir).unwrap(),
                    output_checkpoint: handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderToolOutput).unwrap().clone(),
                    verified_inputs: Vec::new(),
                }], true, None, false, true).unwrap();
            let tool_record = RunRecord::from_bytes(tool.content.as_bytes()).unwrap();
            assert!(tool_record.checkpoints.iter().filter(|checkpoint| checkpoint.outbound_interfaces()
                .any(|interface| interface.as_str() == "world_client_profile_test"))
                .all(|checkpoint| checkpoint.id.as_str().contains("_standing_")));
            assert!(tool.pending_handoff.items.iter().all(|item| item.artifact_label == "tool.call"));
            let mut recovery = failed.clone();
            recovery.ctx.movements_path = var_dir.join("refused-pending.jsonl");
            recovery.ctx.ledger_index_path = var_dir.join("refused-pending.sqlite");
            let hash = ContentHash::of_bytes(tool.content.as_bytes());
            std::fs::write(ctx.blobs_dir.join(hash.to_hex()), tool.content.as_bytes()).unwrap();
            let mut rows = (0..24).map(ledger_row).collect::<Vec<_>>();
            let save = |rows: &[Movement]| std::fs::write(&recovery.ctx.movements_path,
                rows.iter().map(|row| format!("{}\n", serde_json::to_string(row).unwrap())).collect::<String>()).unwrap();
            save(&rows);
            assert!(recovery.validate_refused_source_handoff(&tool_record, &tool.pending_handoff).is_err());
            let mut intent_handoff = recovery.clone();
            let mut intent_state = state.clone();
            intent_state.run_record_bytes = tool.content.as_bytes().to_vec();
            intent_handoff.state = Arc::new(tokio::sync::Mutex::new(intent_state));
            intent_handoff.failed_source = false;
            let intent_runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            assert!(intent_runtime.block_on(intent_handoff.pending_source_intent()).unwrap().is_some());
            let mut refusal = ledger_row(24);
            refusal.kind = MovementKind::DeliveryRefused;
            refusal.from = Endpoint::Interface(provider.clone());
            refusal.to = Endpoint::Interface(tool_record.worker.clone());
            refusal.artifact = Some(artifact_identity(&tool_file, &var_dir).unwrap().artifact);
            refusal.invocation_metadata = Some(octopus_ledger::MovementMetadata::Delivery(octopus_ledger::DeliveryMetadata {
                delivery_verdict: DeliveryVerdict::Refused { reason: octopus_ledger::RefusedReason::Policy, detail: serde_json::json!({"message":"descriptor unavailable"}) },
                runtime: Some(RuntimeInvocationMetadata {
                    run: run.clone(), ee: tool_record.ee.clone(), worker: tool_record.worker.clone(),
                    checkpoint: tool.pending_handoff.items[0].checkpoint.id.clone(), interface: provider.clone(),
                    epoch: tool.pending_handoff.items[0].checkpoint.epoch, run_record_hash: Some(hash.to_hex()),
                }), source_movement_seq: None, delivery_scope_seq: None,
            }));
            let mut transient = refusal.clone();
            if let Some(octopus_ledger::MovementMetadata::Delivery(metadata)) = transient.invocation_metadata.as_mut() {
                metadata.delivery_verdict = DeliveryVerdict::refused(octopus_ledger::RefusedReason::RegistryUnstable, serde_json::json!({}));
            }
            rows.push(transient); save(&rows);
            assert!(intent_runtime.block_on(intent_handoff.pending_source_intent()).unwrap().is_some());
            assert!(recovery.validate_refused_source_handoff(&tool_record, &tool.pending_handoff).is_err());
            rows.pop(); rows.push(refusal.clone()); save(&rows);
            recovery.validate_refused_source_handoff(&tool_record, &tool.pending_handoff).unwrap();
            assert!(intent_runtime.block_on(intent_handoff.pending_source_intent()).unwrap().is_none());
            let error_projection = recovery.author_run_with_bindings(tool.content.as_bytes(), &state.context_artifacts,
                &[PreparedHandoff {
                    identity: artifact_identity(&error_file, &var_dir).unwrap(),
                    output_checkpoint: recovery.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput).unwrap().clone(),
                    verified_inputs: Vec::new(),
                }], true, None, false, true, &[]).unwrap();
            assert!(error_projection.pending_handoff.source_without_context);
            assert_eq!(error_projection.pending_handoff.items[0].artifact_label, "turn.error");
            assert_eq!(error_projection.pending_handoff.items[0].checkpoint.outbound_interface_bindings[0].interface.as_str(), "world_client_profile_test");
            assert_eq!(error_projection.before_hash, hash.to_hex());
            let mut drift = tool.pending_handoff.clone(); drift.items[0].checkpoint.epoch += 1;
            assert!(recovery.validate_refused_source_handoff(&tool_record, &drift).is_err());
            let mut admitted = refusal.clone(); admitted.seq = Seq(25); admitted.kind = MovementKind::InterfaceArrival;
            rows.push(admitted); save(&rows);
            assert!(recovery.validate_refused_source_handoff(&tool_record, &tool.pending_handoff).is_err());
            assert!(!recovery.pending_source_fully_refused(&tool_record, &tool.pending_handoff).unwrap());
            rows.pop(); rows[24].artifact.as_mut().unwrap().content = ContentHash::of_bytes(b"other"); save(&rows);
            assert!(recovery.validate_refused_source_handoff(&tool_record, &tool.pending_handoff).is_err());
        }
        let output_file = InterfaceArtifactFile::write(
            var_dir.join("emitted"),
            "interface.artifact.output",
            br#"{"text":"profile output"}"#,
        )
        .unwrap();
        let output_identity = artifact_identity(&output_file, &var_dir).unwrap();
        let output_checkpoint = handoff
            .config
            .checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput)
            .unwrap()
            .clone();
        let authored = handoff
            .author_run(
                &run_bytes,
                &[],
                &[PreparedHandoff {
                    identity: output_identity,
                    output_checkpoint,
                    verified_inputs: Vec::new(),
                }],
                true,
                None,
                false,
                false,
            )
            .unwrap();
        let authored_record = RunRecord::from_bytes(authored.content.as_bytes()).unwrap();
        assert_eq!(authored_record.status, serde_json::json!("running"));
        validate_persisted_profile_binding(&authored_record, &handoff.config, &topology).unwrap();
        let extra_role = ArtifactId::new("artifact-connected-worker-role");
        let connected = handoff.author_run_with_bindings(
            &run_bytes, std::slice::from_ref(&extra_role),
            &[PreparedHandoff {
                identity: artifact_identity(&output_file, &var_dir).unwrap(),
                output_checkpoint: handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput).unwrap().clone(),
                verified_inputs: Vec::new(),
            }], true, None, false, false,
            &[ProviderOutgoingBinding {
                interface: InterfaceId::new("system_worker_peer"),
                artifact_label: "worker.message".into(),
                artifact_bindings: vec![extra_role.clone()],
            }],
        ).unwrap();
        let connected_record = RunRecord::from_bytes(connected.content.as_bytes()).unwrap();
        let connected_config = config_from_persisted_profile(&connected_record, &provider, &profile).unwrap();
        let connected_topology: BoundRunAuthoringTopology = serde_json::from_value(
            connected_record.interface_state[&provider][RUN_AUTHORING_BINDING].clone()
        ).unwrap();
        assert!(preserves_provider_topology(&topology, &connected_topology));
        assert_eq!(current_mandatory_role_artifacts(&connected_record, &connected_config).unwrap(), vec![extra_role.clone()]);
        assert!(connected_record.checkpoints.iter().any(|checkpoint| {
            connected_record.active_checkpoints.contains(&checkpoint.id)
                && checkpoint.outbound_interface_bindings.iter().any(|binding| {
                    binding.interface.as_str() == "system_worker_peer"
                        && binding.admits("worker.message") && !binding.admits("interface.artifact.output")
                })
        }));
        let mut stolen = connected_topology.clone();
        stolen.checkpoints[0].outbound_interface_bindings[0].interface = InterfaceId::new("system_stolen");
        assert!(!preserves_provider_topology(&topology, &stolen));
        {
            let identity = artifact_identity(&output_file, &var_dir).unwrap();
            let authored = handoff.author_run(authored.content.as_bytes(), std::slice::from_ref(&identity.artifact.id.clone()),
                &[PreparedHandoff { identity, output_checkpoint: handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput).unwrap().clone(), verified_inputs: Vec::new() }],
                true, None, false, true).unwrap();
            let authored_record = RunRecord::from_bytes(authored.content.as_bytes()).unwrap();
            let mut route = connected_topology.checkpoints.last().unwrap().clone();
            route.static_artifact_bindings.clear();
            let mut observed: serde_json::Value = serde_json::from_str(&authored.content).unwrap();
            let id = "new_outgoing_connection";
            let roles = current_mandatory_role_bindings(&authored_record, &handoff.config).unwrap()
                .iter().map(serde_json::to_value).collect::<Result<Vec<_>, _>>().unwrap();
            observed["checkpoints"].as_array_mut().unwrap().push(materialize_checkpoint(&route, id, &run, &roles));
            observed["active_checkpoints"].as_array_mut().unwrap().push(serde_json::json!(id));
            observed["interface_state"][provider.as_str()][FRONTIER_CHECKPOINT_IDS].as_array_mut().unwrap().push(serde_json::json!(id));
            observed["interface_state"][provider.as_str()][RUN_AUTHORING_BINDING]["checkpoints"].as_array_mut().unwrap().push(serde_json::json!(route));
            let encode = |value: &serde_json::Value| { let mut bytes = serde_json::to_vec_pretty(value).unwrap(); bytes.push(b'\n'); bytes };
            let projection = output_connections::protected_record(authored.content.as_bytes(), &encode(&observed), &handoff.config).unwrap().unwrap();
            assert_eq!(projection, authored.content.as_bytes(), "routing projection must end at the exact evidenced bytes");
            let connected_bytes = encode(&observed);
            assert_eq!(output_connections::protected_record(&connected_bytes, authored.content.as_bytes(), &handoff.config).unwrap().unwrap(), connected_bytes);
            assert!(checked_selected_run_advance_state(&ctx, &run_path, &connected_bytes, authored.content.as_bytes(), 23, &handoff.config).unwrap().is_empty());
            let mut changed_history: serde_json::Value = serde_json::from_str(&authored.content).unwrap();
            changed_history["interface_state"][provider.as_str()]["context_generation"] = serde_json::json!(200);
            assert!(output_connections::protected_record(&connected_bytes, &encode(&changed_history), &handoff.config).unwrap().is_none());
            let mut two_routes = observed.clone();
            let mut second = route.clone();
            second.name = "second_outgoing".into();
            second.outbound_interface_bindings[0].interface = InterfaceId::new("system_worker_peer_two");
            two_routes["checkpoints"].as_array_mut().unwrap().push(materialize_checkpoint(&second, "second_outgoing", &run, &roles));
            two_routes["active_checkpoints"].as_array_mut().unwrap().push(serde_json::json!("second_outgoing"));
            two_routes["interface_state"][provider.as_str()][FRONTIER_CHECKPOINT_IDS].as_array_mut().unwrap().push(serde_json::json!("second_outgoing"));
            two_routes["interface_state"][provider.as_str()][RUN_AUTHORING_BINDING]["checkpoints"].as_array_mut().unwrap().push(serde_json::json!(second));
            let mut middle_removed = two_routes.clone();
            middle_removed["checkpoints"].as_array_mut().unwrap().retain(|cp| cp["id"] != id);
            middle_removed["active_checkpoints"].as_array_mut().unwrap().retain(|cp| cp != id);
            middle_removed["interface_state"][provider.as_str()][FRONTIER_CHECKPOINT_IDS].as_array_mut().unwrap().retain(|cp| cp != id);
            middle_removed["interface_state"][provider.as_str()][RUN_AUTHORING_BINDING]["checkpoints"].as_array_mut().unwrap().retain(|cp| cp["name"] != route.name);
            assert_eq!(output_connections::protected_record(&encode(&two_routes), &encode(&middle_removed), &handoff.config).unwrap().unwrap(), encode(&two_routes));
            let mut pending_route = observed.clone();
            pending_route["interface_state"][provider.as_str()]["pending_handoff"]["items"][0]["checkpoint"]["id"] = serde_json::json!(id);
            assert!(output_connections::protected_record(&encode(&pending_route), authored.content.as_bytes(), &handoff.config).is_err());
            let mut contextual_route = observed.clone();
            contextual_route["interface_state"][provider.as_str()][RUN_AUTHORING_BINDING]["checkpoints"].as_array_mut().unwrap().last_mut().unwrap()["include_context"] = serde_json::json!(true);
            assert!(output_connections::protected_record(&encode(&contextual_route), authored.content.as_bytes(), &handoff.config).unwrap().is_none());
            let publication = handoff.author_context_publication(authored.content.as_bytes(), &authored.pending_handoff).unwrap();
            let rebased = authoring_retry::rebase_authored_run(authored.content.as_bytes(), &encode(&observed), &publication, &handoff.config, Some(JsonFormat::Compact)).unwrap();
            let rebased_record = RunRecord::from_bytes(rebased.content.as_bytes()).unwrap();
            assert_eq!(rebased.pending_handoff, publication.pending_handoff);
            assert_eq!(rebased_record.checkpoint(&CheckpointId::new(id)), RunRecord::from_bytes(&encode(&observed)).unwrap().checkpoint(&CheckpointId::new(id)));
            assert_eq!(output_connections::protected_record(publication.content.as_bytes(), rebased.content.as_bytes(), &handoff.config).unwrap().unwrap(), publication.content.as_bytes());
            assert!(checked_selected_run_advance_state(&ctx, &run_path, authored.content.as_bytes(), &encode(&observed), 23, &handoff.config).unwrap().is_empty());
            let mut history = observed.clone();
            history["interface_state"][provider.as_str()]["context_generation"] = serde_json::json!(200);
            assert!(checked_selected_run_advance_state(&ctx, &run_path, authored.content.as_bytes(), &encode(&history), 23, &handoff.config).is_err(), "an empty writer set cannot prove changed history");
            assert!(authoring_retry::rebase_authored_run(authored.content.as_bytes(), &encode(&history), &publication, &handoff.config, Some(JsonFormat::Compact)).is_err());
            for mutation in 0..4 {
                let mut invalid = observed.clone();
                match mutation {
                    0 => invalid["interface_state"][provider.as_str()][RUN_AUTHORING_BINDING]["checkpoints"].as_array_mut().unwrap().last_mut().unwrap()["static_artifact_bindings"] = serde_json::json!(["ungranted-context"]),
                    1 => invalid["checkpoints"].as_array_mut().unwrap().last_mut().unwrap()["artifact_bindings"] = serde_json::json!([{"id":"ungranted-context"}]),
                    2 => { invalid["active_checkpoints"].as_array_mut().unwrap().pop(); },
                    _ => invalid["interface_state"][provider.as_str()]["pending_handoff"]["items"][0]["checkpoint"]["id"] = serde_json::json!(id),
                }
                assert!(output_connections::protected_record(authored.content.as_bytes(), &encode(&invalid), &handoff.config).is_err(), "invalid outgoing mutation {mutation}");
                assert!(authoring_retry::rebase_authored_run(authored.content.as_bytes(), &encode(&invalid), &publication, &handoff.config, Some(JsonFormat::Compact)).is_err(), "invalid outgoing retry {mutation}");
            }
            let mut input_change = observed;
            input_change["interface_state"][provider.as_str()][RUN_AUTHORING_BINDING]["checkpoints"][0]["include_context"] = serde_json::json!(false);
            assert!(output_connections::protected_record(authored.content.as_bytes(), &encode(&input_change), &handoff.config).unwrap().is_none(), "input changes are not routing additions");
        }
        std::fs::write(&run_path, authored.content.as_bytes()).unwrap();
        let replay = session
            .from_invocation(&ctx, &descriptor, &run, &input, 23, &run_bytes)
            .unwrap();
        assert_eq!(replay.bound_topology, topology);
        let authored_frontier = frontier_checkpoint_ids(authored_record.interface_state.get(&provider).unwrap()).unwrap();
        assert_eq!(authored_frontier.len(), 5);
        assert_eq!(authored_frontier.iter().filter(|id| id.contains("_standing_")).count(), 2,
            "the write leaves one standing client route and one standing transport dispatch");
        assert_eq!(
            config_from_persisted_profile(&authored_record, &provider, &profile).unwrap(),
            handoff.config
        );
        let result_payload = serde_json::to_vec(&serde_json::json!({
            "label": "file.write.result",
            "tool": "file.write",
            "correlation_id": authored.correlation_id,
            "ok": true,
            "body": {"result": "files_written", "writes": []}
        }))
        .unwrap();
        let result_file = InterfaceArtifactFile::write(
            var_dir.join("result"),
            "file.write.result",
            &result_payload,
        )
        .unwrap();
        let result_ref = artifact_identity(&result_file, &var_dir).unwrap().artifact;
        let result_binding_policy = handoff
            .config
            .checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)
            .unwrap()
            .binding_policy_snapshot();
        let result_envelope = InterfaceInvocationEnvelope {
            artifact: result_ref.clone(),
            artifact_file: result_file,
            artifact_binding_payloads: Vec::new(),
            metadata: Some(
                InvocationMetadata::new(GrantProjection::default())
                    .with_runtime(RuntimeInvocationMetadata {
                        run: run.clone(),
                        ee: EeId::new("ee_profile_test"),
                        worker: InterfaceId::new("system_worker_profile_test"),
                        checkpoint: authored.authoring_result_checkpoint.clone(),
                        interface: provider.clone(),
                        epoch: 0,
                        run_record_hash: Some(authored.after_hash.clone()),
                    })
                    .with_outbound_interface_bindings(vec![provider.clone()])
                    .with_checkpoint_binding_policy(result_binding_policy),
            ),
        };
        let movement = Movement {
            seq: Seq(31),
            lamport: Lamport(32),
            recorded_at_unix_ms: 33,
            trace: TraceId("trace-profile-result".to_string()),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(authored.authoring_result_interface.clone()),
            to: Endpoint::Interface(provider.clone()),
            artifact: Some(result_ref),
            invocation_metadata: result_envelope.metadata.clone().map(Into::into),
        };
        result_identity
            .validate(
                &movement,
                &result_envelope,
                authored.content.as_bytes(),
                &result_payload,
            )
            .unwrap();
        let mut without_binding: serde_json::Value =
            serde_json::from_str(&authored.content).unwrap();
        without_binding["interface_state"][provider.as_str()]
            .as_object_mut()
            .unwrap()
            .remove(RUN_AUTHORING_BINDING);
        let error = result_identity
            .validate(
                &movement,
                &result_envelope,
                &serde_json::to_vec(&without_binding).unwrap(),
                &result_payload,
            )
            .unwrap_err();
        assert!(error.contains("no durable run_authoring_binding"));
        let mut stale_pending: serde_json::Value = serde_json::from_str(&authored.content).unwrap();
        stale_pending["interface_state"][provider.as_str()]["pending_handoff"]["items"][0]["checkpoint"]
            ["id"] = serde_json::json!("forged_pending_checkpoint");
        let error = result_identity
            .validate(
                &movement,
                &result_envelope,
                &serde_json::to_vec(&stale_pending).unwrap(),
                &result_payload,
            )
            .unwrap_err();
        assert!(error.contains("matches 0 active provider checkpoints"), "{error}");
        let original = RunRecord::from_bytes(&run_bytes).unwrap();
        let call_file = handoff.materialize_file_write_call(&authored).unwrap();
        std::fs::write(ctx.blobs_dir.join(authored.before_hash.clone()), &run_bytes).unwrap();
        let mut rejection_ctx = ctx.clone();
        rejection_ctx.movements_path = dir.path().join("ledger/rejected-write.jsonl");
        rejection_ctx.ledger_index_path = dir.path().join("ledger/rejected-write.sqlite");
        let result_template = handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult).unwrap();
        let rejected_file = InterfaceArtifactFile::write(var_dir.join("rejected"), "file.write.result",
            &serde_json::to_vec(&serde_json::json!({
                "label":"file.write.result", "tool":"file.write", "ok":false,
                "correlation_id":authored.correlation_id,
                "error":{"code":"tool_execution_failed","message":"ordinary refusal"}
            })).unwrap()).unwrap();
        let mut rejected_envelope = result_envelope.clone();
        rejected_envelope.artifact = pin_artifact(&rejected_file, &ctx.blobs_dir);
        rejected_envelope.artifact_file = rejected_file;
        let rejected_metadata = rejected_envelope.metadata.as_mut().unwrap();
        rejected_metadata.runtime.as_mut().unwrap().checkpoint = active_checkpoint_for_role(
            &original, &handoff.config, RunAuthoringCheckpointRole::AuthoringResult).unwrap().id.clone();
        rejected_metadata.runtime.as_mut().unwrap().run_record_hash = Some(authored.before_hash.clone());
        let mut rejected_movement = movement.clone();
        rejected_movement.seq = Seq(25);
        rejected_movement.artifact = Some(rejected_envelope.artifact.clone());
        rejected_movement.invocation_metadata = rejected_envelope.metadata.clone().map(Into::into);
        let mut call_movement = ledger_row(24);
        call_movement.kind = MovementKind::InterfaceOutbound;
        call_movement.from = Endpoint::Interface(handoff.config.authoring_source_interface.clone());
        call_movement.to = Endpoint::Interface(result_template.interface().clone());
        call_movement.artifact = Some(pin_artifact(&call_file, &ctx.blobs_dir));
        let mut call_metadata = rejected_envelope.metadata.clone().unwrap();
        call_metadata.runtime.as_mut().unwrap().interface = result_template.interface().clone();
        call_metadata.grant.tools = vec![handoff.config.file_write_tool.clone()];
        call_metadata.grant.interfaces = vec![result_template.interface().to_string()];
        call_movement.invocation_metadata = Some(call_metadata.into());
        let mut rows = (0..24).map(ledger_row).collect::<Vec<_>>();
        rows.extend([call_movement, rejected_movement.clone()]);
        let journal = rows.iter().map(|row| format!("{}\n", serde_json::to_string(row).unwrap())).collect::<String>();
        std::fs::write(&rejection_ctx.movements_path, &journal).unwrap();
        let rejected: ToolResultArtifact = serde_json::from_slice(&rejected_envelope.read_artifact_payload().unwrap()).unwrap();
        authoring_retry::validate_rejected_result(&rejection_ctx, &handoff.config, &rejected_movement, &rejected_envelope, &original, &rejected).unwrap();
        let consumed = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(
            result_identity.consume_if_matches(&rejection_ctx, &ProviderHandoffSession::new(&rejection_ctx).unwrap(), &rejected_movement, &rejected_envelope, "file.write.result")
        ).unwrap();
        assert!(consumed);
        let wrong_ledger = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(
            result_identity.consume_if_matches(&rejection_ctx, &ProviderHandoffSession::new(&ctx).unwrap(), &rejected_movement, &rejected_envelope, "file.write.result")
        ).unwrap_err();
        assert!(wrong_ledger.contains("handoff session belongs to interface"));
        assert_eq!(std::fs::read_to_string(&rejection_ctx.movements_path).unwrap(), journal);
        {
            let mut concurrent: serde_json::Value = serde_json::from_slice(&run_bytes).unwrap();
            concurrent["interface_state"]["system_tool_profile_test"] = serde_json::json!({"generation": 2});
            let current = serde_json::to_vec(&concurrent).unwrap();
            let hash = ContentHash::of_bytes(&current);
            std::fs::write(ctx.blobs_dir.join(hash.to_hex()), &current).unwrap();
            let mut refused = rejected_movement.clone();
            refused.kind = MovementKind::DeliveryRefused;
            refused.to = Endpoint::Interface(original.worker.clone());
            let mut runtime = rejected_envelope.metadata.as_ref().unwrap().runtime.clone().unwrap();
            runtime.interface = result_template.interface().clone();
            runtime.run_record_hash = Some(hash.to_hex());
            refused.invocation_metadata = Some(octopus_ledger::MovementMetadata::Delivery(octopus_ledger::DeliveryMetadata {
                delivery_verdict: DeliveryVerdict::Refused { reason: octopus_ledger::RefusedReason::CheckpointSpent, detail: serde_json::json!({}) },
                runtime: Some(runtime), source_movement_seq: None, delivery_scope_seq: None,
            }));
            let mut refused_rows = rows.clone();
            let save = |rows: &[Movement]| std::fs::write(&rejection_ctx.movements_path,
                rows.iter().map(|row| format!("{}\n", serde_json::to_string(row).unwrap())).collect::<String>()).unwrap();
            refused_rows[25] = refused.clone(); save(&refused_rows);
            assert!(authoring_retry::rejected_write(&rejection_ctx, &handoff.config, &authored).err().unwrap()
                .contains("complete admitted checked file.write chain"));
            if let Some(octopus_ledger::MovementMetadata::Delivery(metadata)) = refused.invocation_metadata.as_mut() {
                metadata.runtime.as_mut().unwrap().run_record_hash = Some(authored.before_hash.clone());
            }
            refused_rows[25] = refused.clone(); save(&refused_rows);
            let rejection = authoring_retry::rejected_write(&rejection_ctx, &handoff.config, &authored).unwrap().unwrap();
            assert_eq!(rejection.result_seq, 25);
            assert!(matches!(rejection.evidence, authoring_retry::RejectionEvidence::ToolResult { call_seq: 24, .. }));
            let rebased = authoring_retry::rebase_authored_run(&run_bytes, &current, &authored, &handoff.config, Some(JsonFormat::Compact)).unwrap();
            assert_eq!(rebased.before_hash, hash.to_hex());
            assert_eq!(rebased.pending_handoff, authored.pending_handoff);
            assert_eq!(serde_json::from_str::<serde_json::Value>(&rebased.content).unwrap()["interface_state"]["system_tool_profile_test"], concurrent["interface_state"]["system_tool_profile_test"]);
            let mut saved_handoff = handoff.clone();
            saved_handoff.hand_in_root = var_dir.join("saved-retries");
            let mut old = authored.clone();
            old.json_format = None;
            old.content = String::from_utf8(JsonFormat::Pretty.encode(&serde_json::from_str::<serde_json::Value>(&old.content).unwrap()).unwrap()).unwrap();
            old.after_hash = ContentHash::of_bytes(old.content.as_bytes()).to_hex();
            let cold_read = |before: &[u8], planned: &AuthoredRun| -> Result<AuthoredRun, String> {
                let file = saved_handoff.materialize_file_write_call(planned)?;
                validate_authoring_attempt_source(AuthoringAttemptSourceValidation {
                    ctx: &ctx, record: &RunRecord::from_bytes(before).unwrap(), record_bytes: before,
                    config: &handoff.config,
                    call: serde_json::from_slice(&file.read_payload_bytes().unwrap()).unwrap(),
                    manifest_path: file.manifest_path().to_path_buf(),
                }).map(|attempt| attempt.authored)
            };
            let old = cold_read(&run_bytes, &old).unwrap();
            for format in [None, Some(JsonFormat::Compact)] {
                let planned = authoring_retry::rebase_authored_run(&run_bytes, &current, &old, &handoff.config, format).unwrap();
                let saved = cold_read(&current, &planned).unwrap();
                assert_eq!(saved, planned);
                assert_eq!(saved.json_format, format);
                authoring_retry::validate_rebased_authored_run(&run_bytes, &current, &old, &saved, &handoff.config).unwrap();
                let mut altered = saved.clone();
                altered.json_format = if format.is_none() { Some(JsonFormat::Compact) } else { None };
                assert!(cold_read(&current, &altered).is_err());
                assert!(authoring_retry::validate_rebased_authored_run(&run_bytes, &current, &old, &altered, &handoff.config).is_err());
                altered = saved.clone();
                altered.content.push(' ');
                assert!(authoring_retry::validate_rebased_authored_run(&run_bytes, &current, &old, &altered, &handoff.config).is_err());
                altered.after_hash = ContentHash::of_bytes(altered.content.as_bytes()).to_hex();
                assert!(cold_read(&current, &altered).is_err());
                assert!(authoring_retry::validate_rebased_authored_run(&run_bytes, &current, &old, &altered, &handoff.config).is_err());
            }
            if let Some(octopus_ledger::MovementMetadata::Delivery(metadata)) = refused_rows[25].invocation_metadata.as_mut() {
                metadata.runtime.as_mut().unwrap().epoch += 1;
            }
            save(&refused_rows);
            assert!(authoring_retry::rejected_write(&rejection_ctx, &handoff.config, &authored).is_err());
            refused_rows[25] = refused;
            refused_rows[24].artifact = Some(rejected_envelope.artifact.clone()); save(&refused_rows);
            assert!(authoring_retry::rejected_write(&rejection_ctx, &handoff.config, &authored).unwrap().is_none());
            save(&rows);
        }
        let mut malformed = rejected.clone();
        malformed.body = Some(serde_json::json!({"result":"files_written"}));
        assert!(authoring_retry::validate_rejected_result(&rejection_ctx, &handoff.config, &rejected_movement, &rejected_envelope, &original, &malformed).is_err());
        malformed = rejected.clone();
        malformed.correlation_id = Some("provider-run-authoring:run_profile_test:999".into());
        assert!(authoring_retry::validate_rejected_result(&rejection_ctx, &handoff.config, &rejected_movement, &rejected_envelope, &original, &malformed).is_err());
        std::fs::write(&rejection_ctx.movements_path, rows[..24].iter().map(|row| format!("{}\n", serde_json::to_string(row).unwrap())).collect::<String>()).unwrap();
        assert!(authoring_retry::validate_rejected_result(&rejection_ctx, &handoff.config, &rejected_movement, &rejected_envelope, &original, &rejected).is_err());
        exercise_checked_verification_failure(&ctx, &handoff.config, &run_bytes, &run_path);
        exercise_ordered_verified_tool_recovery(&mut handoff, &run_bytes, &profile);
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
            .block_on(exercise_interrupted_source_order(&ctx, &descriptor, &run_bytes, &input));
    }

    fn exercise_checked_verification_failure(
        ctx: &InterfaceLiftContext,
        config: &RunAuthoringConfig,
        initial: &[u8],
        path: &Path,
    ) {
        let requirement: RequiredVerifiedArtifact = serde_json::from_value(serde_json::json!({
            "mark":"tool.call.allowed", "artifact_label":config.file_write_label, "verifier":"tool_call_allowed",
            "params":{"tools":[config.file_write_tool],"metadata_matches":[{"payload":"/arguments/expected_before_hash","metadata":"/runtime/run_record_hash"}]}
        })).unwrap();
        let mut original = RunRecord::from_bytes(initial).unwrap();
        let call_id = active_checkpoint_for_role(
            &original,
            config,
            RunAuthoringCheckpointRole::AuthoringCall,
        )
        .unwrap()
        .id
        .clone();
        original
            .checkpoints
            .iter_mut()
            .find(|cp| cp.id == call_id)
            .unwrap()
            .required_verified_artifacts = vec![requirement.clone()];
        let before = serde_json::to_vec(&original).unwrap();
        let mut target = serde_json::to_value(&original).unwrap();
        target["interface_state"][config.provider_interface.as_str()]["fixture_checked_intent"] =
            serde_json::json!(true);
        let edit = CheckedRunEdit::prepare(
            path.to_path_buf(),
            &before,
            target,
            "checked-gate-attempt".into(),
        )
        .unwrap();
        let writer = CheckedRunWriter {
            owner: config.provider_interface.clone(),
            source: config.authoring_source_interface.clone(),
            door: config
                .checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)
                .unwrap()
                .interface()
                .clone(),
            tool: config.file_write_tool.clone(),
            call_label: config.file_write_label.clone(),
            result_label: config.file_write_result_label.clone(),
        };
        let profile = RunAuthoringProfile {
            file_write_tool: writer.tool.clone(),
            file_write_label: writer.call_label.clone(),
            file_write_result_label: writer.result_label.clone(),
            result_timeout_ms: 1000,
        };
        let file = edit
            .materialize(&ctx.var_dir.join("checked-gate"), &profile)
            .unwrap();
        let reference = pin_artifact(&file, &ctx.blobs_dir);
        std::fs::write(ctx.blobs_dir.join(&edit.before_hash), &before).unwrap();
        let mut concurrent = serde_json::to_value(&original).unwrap();
        concurrent["interface_state"]["system_independent_fixture"] =
            serde_json::json!({"generation":1});
        let concurrent = serde_json::to_vec(&concurrent).unwrap();
        let hash = ContentHash::of_bytes(&concurrent);
        std::fs::write(ctx.blobs_dir.join(hash.to_hex()), &concurrent).unwrap();
        let checkpoint = original.checkpoint(&call_id).unwrap();
        let mut metadata = InvocationMetadata::new(GrantProjection {
            tools: vec![writer.tool.clone()],
            interfaces: vec![writer.door.to_string()],
            mark_outcomes: vec![requirement.mark.to_string()],
            ..Default::default()
        })
        .with_runtime(octopus_interfaces::RuntimeInvocationMetadata {
            run: original.id.clone(),
            ee: original.ee.clone(),
            worker: original.worker.clone(),
            checkpoint: call_id.clone(),
            interface: writer.source.clone(),
            epoch: checkpoint.epoch.0,
            run_record_hash: Some(hash.to_hex()),
        })
        .with_outbound_interface_bindings(vec![writer.door.clone()])
        .with_checkpoint_binding_policy(CheckpointBindingPolicySnapshot {
            inbound: checkpoint.interface_binding.clone().unwrap(),
            outbound: checkpoint.outbound_interface_bindings.clone(),
        })
        .with_required_verified_artifacts(vec![requirement.clone()])
        .with_checkpoint_admission(octopus_ledger::CheckpointAdmissionMetadata {
            artifact_label: writer.call_label.clone(),
            required_artifact_labels: vec![writer.call_label.clone()],
            complete: true,
        });
        metadata.verification_failure = Some(octopus_ledger::VerificationFailureMetadata {
            error: "verifier_rejected".into(),
            verifier: Some(requirement.verifier),
            mark: Some(requirement.mark),
            artifact_label: None,
            run: None,
            checkpoint: None,
            verifier_package: None,
            details: Some(serde_json::json!({"reason":"metadata_mismatch"})),
        });
        let mut arrival = ledger_row(24);
        arrival.kind = MovementKind::InterfaceArrival;
        arrival.trace = original.trace.clone();
        arrival.from = Endpoint::Interface(writer.source.clone());
        arrival.to = Endpoint::Interface(original.worker.clone());
        arrival.artifact = Some(reference);
        arrival.invocation_metadata = Some(metadata.clone().into());
        let mut failure = arrival.clone();
        failure.seq = octopus_ledger::Seq(25);
        failure.kind = MovementKind::VerificationFailed;
        let mut failure_ctx = ctx.clone();
        failure_ctx.movements_path = ctx.var_dir.join("checked-gate-ledger.jsonl");
        failure_ctx.ledger_index_path = ctx.var_dir.join("checked-gate-ledger.sqlite");
        let mut rows = (0..24).map(ledger_row).collect::<Vec<_>>();
        rows.extend([arrival, failure.clone()]);
        let save = |rows: &[Movement]| {
            std::fs::write(
                &failure_ctx.movements_path,
                rows.iter()
                    .map(|row| format!("{}\n", serde_json::to_string(row).unwrap()))
                    .collect::<String>(),
            )
            .unwrap()
        };
        save(&rows);
        assert_eq!(
            checked_edit_verification_failure(&failure_ctx, &writer, &edit, 23).unwrap(),
            Some(failure)
        );
        let mut different = edit.clone();
        different.correlation_id.push_str("other");
        assert!(
            checked_edit_verification_failure(&failure_ctx, &writer, &different, 23)
                .unwrap()
                .is_none()
        );
        rows[24].kind = MovementKind::ArtifactMoved;
        save(&rows);
        assert!(checked_edit_verification_failure(&failure_ctx, &writer, &edit, 23).is_err());
        rows[24].kind = MovementKind::InterfaceArrival;
        metadata.runtime.as_mut().unwrap().checkpoint = CheckpointId::new("foreign");
        rows[25].invocation_metadata = Some(metadata.into());
        save(&rows);
        assert!(checked_edit_verification_failure(&failure_ctx, &writer, &edit, 23).is_err());
        let capture_path = ctx.var_dir.join("coherent-capture.json");
        std::fs::write(&capture_path, &before).unwrap();
        let captured = stable_run_observation(
            &capture_path,
            &before,
            ctx.required_host_io_limits().unwrap().max_payload_bytes,
            || -> Result<(), CheckedRunError> {
                std::fs::write(&capture_path, &concurrent).unwrap();
                Err("checked result used an unproven snapshot".into())
            },
        );
        assert!(matches!(
            captured,
            Err(CheckedRunError::ChangedDuringRead { .. })
        ));
        assert!(matches!(
            stable_run_observation(
                &capture_path,
                &concurrent,
                ctx.required_host_io_limits().unwrap().max_payload_bytes,
                || -> Result<(), CheckedRunError> { Err("broken stable evidence".into()) }
            ),
            Err(CheckedRunError::Invalid(_))
        ));
        assert!(
            stable_run_observation(
                &capture_path,
                &concurrent,
                ctx.required_host_io_limits().unwrap().max_payload_bytes,
                || Ok(())
            )
            .is_ok()
        );
    }

    fn exercise_ordered_verified_tool_recovery(
        handoff: &mut ProviderOutputHandoff,
        initial: &[u8],
        profile: &RunAuthoringProfile,
    ) {
        let provider = handoff.config.provider_interface.clone();
        let context_bindings = provider_context_bindings(
            &RunRecord::from_bytes(initial).unwrap(),
            &provider,
        )
        .unwrap();
        let context = context_bindings
            .iter()
            .map(|binding| serde_json::to_value(binding).unwrap())
            .collect::<Vec<_>>();
        let tool_door = InterfaceId::new("system_tool_profile_test");
        let mut consumed = handoff
            .config
            .checkpoint_for_role(RunAuthoringCheckpointRole::Stable)
            .unwrap()
            .clone();
        consumed.name = "tool_result_input".into();
        consumed.interface_binding = AuthoredInterfaceBinding::all(tool_door.clone());
        handoff.config.checkpoints.push(consumed.clone());
        handoff.bound_topology.checkpoints.push(consumed.clone());
        handoff.source_movement_seq = 0;
        let mut initial: serde_json::Value = serde_json::from_slice(initial).unwrap();
        initial["checkpoints"]
            .as_array_mut()
            .unwrap()
            .push(materialize_checkpoint(
                &consumed,
                &consumed.name,
                &handoff.config.run,
                &context,
            ));
        initial["active_checkpoints"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!(consumed.name));
        initial["interface_state"][provider.as_str()][RUN_AUTHORING_BINDING] =
            serde_json::to_value(&handoff.bound_topology).unwrap();
        let initial = serde_json::to_vec(&initial).unwrap();
        assert_eq!(
            provider_context_bindings(&RunRecord::from_bytes(&initial).unwrap(), &provider).unwrap(),
            context_bindings,
        );
        let output = handoff
            .config
            .checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput)
            .unwrap()
            .clone();
        let route = ProviderHandoffRoute::ClientProjectionWithVerifiedInputs {
            inputs: [
                ("operator.approval", "operator_approval"),
                ("operator.input.provided", "operator_input"),
            ]
            .into_iter()
            .map(|(mark, verifier)| ProviderVerifiedInput {
                also_deliver_to_tool: Some("coder.exec".into()),
                also_deliver_to_interface: None,
                artifact_label: "operator.approval.response".into(),
                required_verified_artifact: RequiredVerifiedArtifact {
                    mark: octopus_descriptors::MarkId::new(mark),
                    verifier: octopus_descriptors::VerifierId::new(verifier),
                    artifact_label: "operator.approval.response".into(),
                    params: serde_json::json!({"request_id":"request-1","call_id":"call-1"}),
                },
            })
            .collect(),
        };
        let marks = BTreeSet::from(["operator.approval".into(), "operator.input.provided".into()]);
        let interfaces = BTreeSet::from([provider.clone(), tool_door.clone()]);
        let tools = BTreeSet::from(["coder.exec".into()]);
        let verified = handoff
            .config
            .verified_input_checkpoints(&route, &output, &[], &marks, &tools, &interfaces)
            .unwrap();
        assert!(
            verified
                .iter()
                .all(|checkpoint| checkpoint.outbound_interfaces().eq([&provider, &tool_door]))
        );
        assert!(
            handoff
                .config
                .verified_input_checkpoints(&route, &output, &[], &marks, &BTreeSet::new(), &interfaces)
                .is_err()
        );
        let request = InterfaceArtifactFile::write(
            handoff.var_dir.join("approval-request"),
            "operator.approval.request",
            br#"{"request_id":"request-1"}"#,
        )
        .unwrap();
        let authored_request = handoff
            .author_run(
                &initial,
                &[],
                &[PreparedHandoff {
                    identity: artifact_identity(&request, &handoff.var_dir).unwrap(),
                    output_checkpoint: output.clone(),
                    verified_inputs: verified.clone(),
                }],
                true,
                None,
                false,
                true,
            )
            .unwrap();
        let request_record = RunRecord::from_bytes(authored_request.content.as_bytes()).unwrap();
        let request_identity = artifact_identity(&request, &handoff.var_dir).unwrap();
        let question_context = handoff.author_run(&initial, &[request_identity.artifact.id.clone()],
            &[PreparedHandoff { identity: request_identity.clone(), output_checkpoint: output.clone(), verified_inputs: verified.clone() }],
            true, None, false, true).unwrap();
        let question_record = RunRecord::from_bytes(question_context.content.as_bytes()).unwrap();
        assert!(question_record.checkpoints.iter().filter(|checkpoint| checkpoint.id.as_str().contains("_verified_input_"))
            .all(|checkpoint| checkpoint.artifact_bindings.iter().any(|binding|
                binding.id == request_identity.artifact.id && binding.access.read)));
        assert!(question_context.pending_handoff.context_publication.as_ref().unwrap().contains(&request_identity.artifact.id));
        let request_config =
            config_from_persisted_profile(&request_record, &provider, profile).unwrap();
        let request_topology: BoundRunAuthoringTopology = serde_json::from_value(
            request_record.interface_state[&provider][RUN_AUTHORING_BINDING].clone(),
        )
        .unwrap();
        handoff.config = request_config.clone();
        handoff.bound_topology = request_topology.clone();
        handoff.source_movement_seq = 1;
        let carrier = InterfaceArtifactFile::write(
            handoff.var_dir.join("approval-phase"),
            "interface.context.updated",
            br#"{"phase":"awaiting_resume"}"#,
        )
        .unwrap();
        let authored_response = handoff
            .author_run(
                authored_request.content.as_bytes(),
                &[],
                &[PreparedHandoff {
                    identity: artifact_identity(&carrier, &handoff.var_dir).unwrap(),
                    output_checkpoint: output,
                    verified_inputs: vec![],
                }],
                true,
                None,
                true,
                true,
            )
            .unwrap();
        let response_record = RunRecord::from_bytes(authored_response.content.as_bytes()).unwrap();
        let response_config =
            config_from_persisted_profile(&response_record, &provider, profile).unwrap();
        let response_topology: BoundRunAuthoringTopology = serde_json::from_value(
            response_record.interface_state[&provider][RUN_AUTHORING_BINDING].clone(),
        )
        .unwrap();
        assert!(
            !response_config
                .checkpoints
                .iter()
                .any(|checkpoint| checkpoint.role == RunAuthoringCheckpointRole::VerifiedInput)
        );
        let source_checkpoint = request_record
            .checkpoints
            .iter()
            .find(|checkpoint| {
                checkpoint_record_matches_template(checkpoint, &verified[0], &request_record.id)
            })
            .unwrap();
        assert!(response_record.checkpoint(&source_checkpoint.id).is_none());
        let decision = InterfaceArtifactFile::write(handoff.var_dir.join("approval-decision"), "operator.approval.response", br#"{"operator_approval_response":{"request_id":"request-1","call_id":"call-1","decision":"accept"}}"#).unwrap();
        let artifact = pin_artifact(&decision, &handoff.ctx.blobs_dir);
        let pinned_hash = ContentHash::of_bytes(authored_request.content.as_bytes());
        std::fs::write(
            handoff.ctx.blobs_dir.join(pinned_hash.to_hex()),
            authored_request.content.as_bytes(),
        )
        .unwrap();
        let runtime = octopus_interfaces::RuntimeInvocationMetadata {
            run: request_record.id.clone(),
            ee: request_record.ee.clone(),
            worker: request_record.worker.clone(),
            checkpoint: source_checkpoint.id.clone(),
            interface: provider.clone(),
            epoch: source_checkpoint.epoch.0,
            run_record_hash: Some(pinned_hash.to_hex()),
        };
        let metadata = InvocationMetadata::new(Default::default())
            .with_runtime(runtime)
            .with_outbound_interface_bindings(vec![provider.clone(), tool_door.clone()])
            .with_checkpoint_binding_policy(verified[0].binding_policy_snapshot())
            .with_required_verified_artifacts(verified[0].required_verified_artifacts.clone())
            .with_checkpoint_admission(octopus_ledger::CheckpointAdmissionMetadata {artifact_label:"operator.approval.response".into(),required_artifact_labels:vec!["operator.approval.response".into()],complete:true});
        let mut movement = ledger_row(1);
        movement.kind = MovementKind::InterfaceOutbound;
        movement.from = Endpoint::Interface(verified[0].interface().clone());
        movement.to = Endpoint::Interface(provider.clone());
        movement.artifact = Some(artifact);
        movement.invocation_metadata = Some(metadata.into());
        std::fs::write(
            &handoff.ctx.movements_path,
            format!(
                "{}\n{}\n",
                serde_json::to_string(&ledger_row(0)).unwrap(),
                serde_json::to_string(&movement).unwrap()
            ),
        )
        .unwrap();
        let evidence = CheckedRunEvidence { movements_path:&handoff.ctx.movements_path,ledger_index_path:&handoff.ctx.ledger_index_path,blobs_dir:&handoff.ctx.blobs_dir,max_control_frame_bytes:1_048_576,max_manifest_bytes:1_048_576,max_payload_bytes:268_435_456 };
        let mut reader = FileCheckedRunReader::open(&evidence, None).unwrap();
        let writer = CheckedRunWriter {owner:provider.clone(),source:handoff.config.authoring_source_interface.clone(),door:handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult).unwrap().interface().clone(),tool:profile.file_write_tool.clone(),call_label:profile.file_write_label.clone(),result_label:profile.file_write_result_label.clone()};
        let mut call = ledger_row(100);
        call.invocation_metadata = Some(InvocationMetadata::new(GrantProjection { mark_outcomes:marks.iter().cloned().collect(), ..Default::default() }).into());
        for (before, after) in [(&initial, authored_request.content.as_bytes()), (&authored_request.content.as_bytes().to_vec(), authored_response.content.as_bytes())] {
            let change = CheckedRunTransition {writer:writer.clone(),before:before.to_vec(),after:after.to_vec(),call:call.clone(),result:ledger_row(101),placement:false};
            validate_checked_provider_transition(&change).unwrap();
            validate_verified_input_transition(&mut reader, &change).unwrap();
        }
        let mut wrong:serde_json::Value=serde_json::from_str(&authored_response.content).unwrap();
        wrong["interface_state"][provider.as_str()]["pending_handoff"]["source_movement_seq"]=serde_json::json!(0);
        let forged=CheckedRunTransition {writer,before:authored_request.content.as_bytes().to_vec(),after:serde_json::to_vec(&wrong).unwrap(),call,result:ledger_row(101),placement:false};
        assert!(validate_verified_input_transition(&mut reader, &forged).is_err());
        let session = ProviderHandoffSession::new(&handoff.ctx).unwrap();
        validate_retired_verified_input(
            &handoff.ctx,
            &session,
            &request_record,
            &response_record,
            &request_config,
            &response_config,
            &request_topology,
            &response_topology,
            &consumed,
            2,
        )
        .unwrap();
        let mut wrong_source = consumed.clone();
        wrong_source.interface_binding =
            AuthoredInterfaceBinding::all(InterfaceId::new("other_door"));
        assert!(
            validate_retired_verified_input(
                &handoff.ctx,
                &session,
                &request_record,
                &response_record,
                &request_config,
                &response_config,
                &request_topology,
                &response_topology,
                &wrong_source,
                2
            )
            .is_err()
        );
        assert!(
            validate_retired_verified_input(
                &handoff.ctx,
                &session,
                &request_record,
                &response_record,
                &request_config,
                &response_config,
                &request_topology,
                &response_topology,
                &consumed,
                1
            )
            .is_err()
        );
        let mut changed_neighbor = response_config.clone();
        changed_neighbor.checkpoints[0].interface_binding =
            AuthoredInterfaceBinding::all(InterfaceId::new("unrelated_change"));
        assert!(
            validate_retired_verified_input(
                &handoff.ctx,
                &session,
                &request_record,
                &response_record,
                &request_config,
                &changed_neighbor,
                &request_topology,
                &response_topology,
                &consumed,
                2
            )
            .is_err()
        );

        let tool_checkpoint = request_record
            .checkpoints
            .iter()
            .find(|checkpoint| {
                checkpoint_record_matches_template(checkpoint, &consumed, &request_record.id)
            })
            .unwrap();
        let result_file = InterfaceArtifactFile::write(handoff.var_dir.join("queued-tool-result"), "coder.exec.result", br#"{"label":"coder.exec.result","tool":"coder.exec","ok":true,"correlation_id":"resume-1","body":{}}"#).unwrap();
        let result_ref = pin_artifact(&result_file, &handoff.ctx.blobs_dir);
        let queued_metadata = InvocationMetadata::new(octopus_interfaces::GrantProjection {
            tools: vec!["coder.exec".into(), "file.write".into()],
            interfaces: vec![
                provider.as_str().into(),
                tool_door.as_str().into(),
                handoff.config.authoring_source_interface.as_str().into(),
                "system_context_authoring_profile_test".into(),
            ],
            ee_root_slot_allowance: Some(vec!["run_records_root".into()]),
            ..Default::default()
        })
        .with_runtime(octopus_interfaces::RuntimeInvocationMetadata {
            run: request_record.id.clone(),
            ee: request_record.ee.clone(),
            worker: request_record.worker.clone(),
            interface: provider.clone(),
            checkpoint: tool_checkpoint.id.clone(),
            epoch: tool_checkpoint.epoch.0,
            run_record_hash: Some(pinned_hash.to_hex()),
        })
        .with_checkpoint_binding_policy(consumed.binding_policy_snapshot())
        .with_outbound_interface_bindings(vec![provider.clone()]);
        let queued = InterfaceInvocationEnvelope {
            artifact: result_ref.clone(),
            artifact_file: result_file,
            artifact_binding_payloads: vec![],
            metadata: Some(queued_metadata.clone()),
        };
        let mut queued_movement = ledger_row(2);
        queued_movement.kind = MovementKind::InterfaceOutbound;
        queued_movement.from = Endpoint::Interface(tool_door);
        queued_movement.to = Endpoint::Interface(provider.clone());
        queued_movement.artifact = Some(result_ref);
        queued_movement.invocation_metadata = Some(queued_metadata.into());
        let mut log = format!(
            "{}\n{}\n{}\n",
            serde_json::to_string(&ledger_row(0)).unwrap(),
            serde_json::to_string(&movement).unwrap(),
            serde_json::to_string(&queued_movement).unwrap()
        );
        std::fs::write(&handoff.ctx.movements_path, &log).unwrap();
        std::fs::write(&handoff.run_path, authored_response.content.as_bytes()).unwrap();
        let descriptor = local_profile_descriptor();
        let before = ProviderHandoffSession::new(&handoff.ctx)
            .unwrap()
            .from_invocation(
                &handoff.ctx,
                &descriptor,
                &request_record.id,
                &queued,
                2,
                authored_request.content.as_bytes(),
            )
            .err()
            .unwrap();
        assert!(
            before.contains("precedes the admitted provider decision handoff"),
            "{before}"
        );
        let phase_ref = pin_artifact(&carrier, &handoff.ctx.blobs_dir);
        let phase_item = &authored_response.pending_handoff.items[0];
        let mut admitted_phase = ledger_row(3);
        admitted_phase.kind = MovementKind::InterfaceArrival;
        admitted_phase.from = Endpoint::Interface(provider.clone());
        admitted_phase.to = Endpoint::Interface(request_record.worker.clone());
        admitted_phase.artifact = Some(phase_ref);
        admitted_phase.invocation_metadata = Some(
            InvocationMetadata::new(Default::default())
                .with_runtime(octopus_interfaces::RuntimeInvocationMetadata {
                    run: request_record.id.clone(),
                    ee: request_record.ee.clone(),
                    worker: request_record.worker.clone(),
                    interface: provider.clone(),
                    checkpoint: phase_item.checkpoint.id.clone(),
                    epoch: phase_item.checkpoint.epoch,
                    run_record_hash: Some(authored_response.after_hash.clone()),
                })
                .into(),
        );
        log.push_str(&format!(
            "{}\n",
            serde_json::to_string(&admitted_phase).unwrap()
        ));
        std::fs::write(&handoff.ctx.movements_path, &log).unwrap();
        let resumed = ProviderHandoffSession::new(&handoff.ctx)
            .unwrap()
            .from_invocation(
                &handoff.ctx,
                &descriptor,
                &request_record.id,
                &queued,
                2,
                authored_request.content.as_bytes(),
            )
            .unwrap();
        assert_eq!(resumed.config, response_config);
        let state = resumed.state.try_lock().unwrap();
        assert!(state.session_context.artifacts.iter().any(|entry| {
            entry
                .payload
                .get("phase")
                .and_then(serde_json::Value::as_str)
                == Some("awaiting_resume")
        }));
        assert!(state.context_artifacts.contains(&queued.artifact.id));
        assert_eq!(
            queued.runtime().unwrap().run_record_hash.as_deref(),
            Some(pinned_hash.to_hex().as_str())
        );
    }

    #[test]
    fn bind_time_separates_context_free_control_from_conversation_inputs() {
        let provider = InterfaceId::new("world_provider_ollama");
        let run = RunId::new("run_profile_test");
        let profile = run_authoring_profile(&local_profile_descriptor()).unwrap();
        let mut value = profile_run_value(&provider, &run);
        value["checkpoints"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": "turn_control_input",
                "run": run,
                "trace": format!("trace:{}:turn_control_input", run.as_str()),
                "epoch": 0,
                "interface_binding": names_binding(
                    "world_operator_chat_profile_test",
                    &["operator.turn.control"],
                ),
                "outbound_interface_bindings": [all_binding(provider.as_str())],
                "artifact_bindings": [],
                "required_verified_artifacts": []
            }));
        value["active_checkpoints"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("turn_control_input"));
        let record = RunRecord::from_bytes(&serde_json::to_vec(&value).unwrap()).unwrap();

        let topology = derive_bound_run_authoring_topology(
            &record,
            &provider,
            &CheckpointId::new("stable_input"),
        )
        .unwrap();
        let stable = topology
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.interface().as_str() == "world_input_profile_test")
            .unwrap();
        assert_eq!(stable.role, RunAuthoringCheckpointRole::Stable);
        assert!(stable.include_context);
        let control = topology
            .checkpoints
            .iter()
            .find(|checkpoint| {
                checkpoint.interface().as_str() == "world_operator_chat_profile_test"
            })
            .unwrap();
        assert_eq!(control.role, RunAuthoringCheckpointRole::IndependentInput);
        assert!(!control.include_context);
        topology.config(&profile).validate(&provider, &run).unwrap();
    }

    #[test]
    fn explicit_topology_keeps_role_bindings_out_of_context_semantics() {
        let provider = InterfaceId::new("world_provider_ollama");
        let run = RunId::new("run_profile_test");
        let profile = run_authoring_profile(&local_profile_descriptor()).unwrap();
        let mut value = profile_run_value(&provider, &run);
        for checkpoint in value["checkpoints"].as_array_mut().unwrap() {
            checkpoint["artifact_bindings"] = serde_json::json!([{"id": "artifact-role"}]);
        }
        value["checkpoints"][0]["artifact_bindings"] = serde_json::json!([
            {"id": "artifact-role"},
            {"id": "artifact-conversation"}
        ]);
        let record = RunRecord::from_bytes(&serde_json::to_vec(&value).unwrap()).unwrap();
        let (config, _) = resolve_profile_invocation(
            &profile,
            &provider,
            &run,
            &record,
            Some(&CheckpointId::new("stable_input")),
        )
        .unwrap();
        assert!(
            config
                .checkpoint_for_role(RunAuthoringCheckpointRole::Stable)
                .unwrap()
                .include_context
        );
        assert!(
            !config
                .checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringCall)
                .unwrap()
                .include_context
        );

        value["checkpoints"][4]["artifact_bindings"] = serde_json::json!([]);
        let drifted = RunRecord::from_bytes(&serde_json::to_vec(&value).unwrap()).unwrap();
        let error = resolve_profile_invocation(
            &profile,
            &provider,
            &run,
            &drifted,
            Some(&CheckpointId::new("stable_input")),
        )
        .unwrap_err();
        assert!(error.contains("different mandatory role bindings"));
    }

    #[test]
    fn profile_topology_rejects_structural_ambiguity() {
        let provider = InterfaceId::new("world_provider_ollama");
        let run = RunId::new("run_profile_test");
        let profile = run_authoring_profile(&local_profile_descriptor()).unwrap();
        let mut duplicate_client = profile_run_value(&provider, &run);
        duplicate_client["checkpoints"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": "client_output_duplicate",
                "run": run,
                "trace": format!("trace:{}:client_output_duplicate", run.as_str()),
                "epoch": 0,
                "interface_binding": all_binding(provider.as_str()),
                "outbound_interface_bindings": [all_binding("world_client_profile_duplicate")],
                "artifact_bindings": [],
                "required_verified_artifacts": []
            }));
        duplicate_client["active_checkpoints"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("client_output_duplicate"));
        duplicate_client["interface_state"][provider.as_str()][FRONTIER_CHECKPOINT_IDS]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("client_output_duplicate"));
        let record =
            RunRecord::from_bytes(&serde_json::to_vec(&duplicate_client).unwrap()).unwrap();
        let topology = derive_bound_run_authoring_topology(
            &record,
            &provider,
            &CheckpointId::new("stable_input"),
        )
        .unwrap();
        let error = topology
            .config(&profile)
            .validate(&provider, &run)
            .unwrap_err();
        assert!(error.contains("overlapping outbound artifact policies"));

        let mut duplicate_call = profile_run_value(&provider, &run);
        duplicate_call["checkpoints"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": "authoring_call_duplicate",
                "run": run,
                "trace": format!("trace:{}:authoring_call_duplicate", run.as_str()),
                "epoch": 0,
                "interface_binding": all_binding("system_interface_authoring_profile_duplicate"),
                "outbound_interface_bindings": [all_binding("system_context_authoring_profile_test")],
                "artifact_bindings": [],
                "required_verified_artifacts": []
            }));
        duplicate_call["active_checkpoints"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("authoring_call_duplicate"));
        duplicate_call["interface_state"][provider.as_str()][FRONTIER_CHECKPOINT_IDS]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("authoring_call_duplicate"));
        let record = RunRecord::from_bytes(&serde_json::to_vec(&duplicate_call).unwrap()).unwrap();
        let error = derive_bound_run_authoring_topology(
            &record,
            &provider,
            &CheckpointId::new("stable_input"),
        )
        .unwrap_err();
        assert!(error.contains("2 structural authoring-call routes"));

        let mut duplicate_tool = profile_run_value(&provider, &run);
        duplicate_tool["checkpoints"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": "tool_output_duplicate",
                "run": run,
                "trace": format!("trace:{}:tool_output_duplicate", run.as_str()),
                "epoch": 0,
                "interface_binding": all_binding(provider.as_str()),
                "outbound_interface_bindings": [all_binding("system_tool_profile_duplicate")],
                "artifact_bindings": [],
                "required_verified_artifacts": [{
                    "mark": "tool.call.allowed",
                    "artifact_label": "tool.call",
                    "verifier": "tool_call_allowed",
                    "params": {"tools": ["coder.exec"]}
                }]
            }));
        duplicate_tool["active_checkpoints"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("tool_output_duplicate"));
        duplicate_tool["interface_state"][provider.as_str()][FRONTIER_CHECKPOINT_IDS]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("tool_output_duplicate"));
        let record = RunRecord::from_bytes(&serde_json::to_vec(&duplicate_tool).unwrap()).unwrap();
        let error = resolve_profile_invocation(
            &profile,
            &provider,
            &run,
            &record,
            Some(&CheckpointId::new("stable_input")),
        )
        .unwrap_err();
        assert!(error.contains("unclassified checkpoint topology"));

        let mut preseeded = profile_run_value(&provider, &run);
        let initial = RunRecord::from_bytes(&serde_json::to_vec(&preseeded).unwrap()).unwrap();
        let topology = derive_bound_run_authoring_topology(
            &initial,
            &provider,
            &CheckpointId::new("stable_input"),
        )
        .unwrap();
        preseeded["interface_state"][provider.as_str()][RUN_AUTHORING_BINDING] =
            serde_json::to_value(topology).unwrap();
        let record = RunRecord::from_bytes(&serde_json::to_vec(&preseeded).unwrap()).unwrap();
        let (resolved, persisted) = resolve_profile_invocation(
            &profile,
            &provider,
            &run,
            &record,
            Some(&CheckpointId::new("stable_input")),
        )
        .unwrap();
        assert_eq!(resolved.checkpoints, persisted.checkpoints);

        let mut control_collision = profile_run_value(&provider, &run);
        control_collision["checkpoints"][0]["interface_binding"] =
            serde_json::to_value(all_binding("system_context_authoring_profile_test")).unwrap();
        let record =
            RunRecord::from_bytes(&serde_json::to_vec(&control_collision).unwrap()).unwrap();
        let error = resolve_profile_invocation(
            &profile,
            &provider,
            &run,
            &record,
            Some(&CheckpointId::new("stable_input")),
        )
        .unwrap_err();
        assert!(
            error.contains("shares a provider-owned authoring interface outside the frontier"),
            "{error}"
        );
    }

    #[test]
    fn tool_route_requires_exactly_one_descriptor_owner_inside_grant() {
        let config = config_with_tool_routes(&[("coder", &["coder.exec"])]);
        config
            .validate(
                &InterfaceId::new("world_provider_test"),
                &RunId::new("run_test"),
            )
            .unwrap();
        let granted = BTreeSet::from(["coder.exec".to_string(), "file.write".to_string()]);
        let owner = config
            .output_checkpoint(
                &ProviderHandoffRoute::Tool {
                    tool: "coder.exec".to_string(),
                },
                &granted,
                &BTreeSet::new(),
                "tool.call",
            )
            .unwrap();
        assert_eq!(owner.name, "coder");

        let no_owner = config
            .output_checkpoint(
                &ProviderHandoffRoute::Tool {
                    tool: "file.write".to_string(),
                },
                &granted,
                &BTreeSet::new(),
                "tool.call",
            )
            .err()
            .unwrap();
        assert!(no_owner.contains("no descriptor-derived output owner"));

        let duplicate =
            config_with_tool_routes(&[("coder_a", &["coder.exec"]), ("coder_b", &["coder.exec"])]);
        let duplicate_error = duplicate
            .validate(
                &InterfaceId::new("world_provider_test"),
                &RunId::new("run_test"),
            )
            .unwrap_err();
        assert!(duplicate_error.contains("owned by both 'coder_a' and 'coder_b'"));
        let duplicate_owner = duplicate
            .output_checkpoint(
                &ProviderHandoffRoute::Tool {
                    tool: "coder.exec".to_string(),
                },
                &granted,
                &BTreeSet::new(),
                "tool.call",
            )
            .err()
            .unwrap();
        assert!(duplicate_owner.contains("2 descriptor-derived output owners"));

        let outside_grant = config
            .output_checkpoint(
                &ProviderHandoffRoute::Tool {
                    tool: "coder.exec".to_string(),
                },
                &BTreeSet::new(),
                &BTreeSet::new(),
                "tool.call",
            )
            .err()
            .unwrap();
        assert!(outside_grant.contains("outside the formed invocation grant"));
        let metadata = InvocationMetadata::new(octopus_interfaces::GrantProjection {
            tools: vec!["coder.exec".into(), "candidate".into()],
            interfaces: vec!["world_provider_test".into(), "system_coder".into()],
            interface_affordance_specs: ["coder.exec", "candidate"].into_iter().map(|name| octopus_interfaces::InterfaceToolAffordance {
                name:name.into(),description:name.into(),args_schema:serde_json::json!({"type":"object"}),
            }).collect(),
            ..Default::default()
        });
        let before = metadata.clone();
        assert_eq!(config.callable_tool_names(&metadata).unwrap(), BTreeSet::from(["coder.exec".into()]));
        assert_eq!(metadata, before);
        assert!(duplicate.callable_tool_names(&metadata).is_err());
        for case in ["missing", "duplicate", "malformed", "denied", "blocked", "door"] {
            let mut metadata = before.clone();
            match case {
                "missing" => { metadata.grant.interface_affordance_specs.remove(0); }
                "duplicate" => metadata.grant.interface_affordance_specs.push(metadata.grant.interface_affordance_specs[0].clone()),
                "malformed" => metadata.grant.interface_affordance_specs[0].args_schema = serde_json::json!(false),
                "denied" => metadata.grant.tools.retain(|tool| tool != "coder.exec"),
                "blocked" => metadata.grant.hard_blocked_tools.push("coder.exec".into()),
                "door" => metadata.grant.interfaces.retain(|interface| interface != "system_coder"),
                _ => unreachable!(),
            }
            assert!(config.callable_tool_names(&metadata).is_err(), "{case}");
        }
        let mut malformed = config.clone();
        malformed.checkpoints.last_mut().unwrap().required_verified_artifacts[0].params = serde_json::json!({"tools":[]});
        assert!(malformed.callable_tool_names(&before).is_err());
    }

    #[test]
    fn interface_route_requires_one_exact_descriptor_owner_inside_grant() {
        let mut config = config_with_tool_routes(&[]);
        config
            .checkpoints
            .iter_mut()
            .find(|checkpoint| {
                checkpoint.role == RunAuthoringCheckpointRole::ProviderArtifactOutput
            })
            .unwrap()
            .outbound_interface_bindings = vec![names_binding(
            "world_client_test",
            &["interface.artifact.output"],
        )];
        let interface = InterfaceId::new("world_operation_exact");
        config.checkpoints.push(RunAuthoringCheckpoint {
            name: "operation_output".to_string(),
            role: RunAuthoringCheckpointRole::ProviderArtifactOutput,
            result_receiving: BTreeMap::new(),
            interface_binding: all_binding("world_provider_test"),
            outbound_interface_bindings: vec![names_binding(
                interface.as_str(),
                &["operation.request"],
            )],
            include_context: false,
            static_artifact_bindings: Vec::new(),
        owner_authoring: None,
            required_verified_artifacts: Vec::new(),
        });
        config
            .validate(
                &InterfaceId::new("world_provider_test"),
                &RunId::new("run_test"),
            )
            .unwrap();
        let owner = config
            .output_checkpoint(
                &ProviderHandoffRoute::Interface {
                    interface: interface.clone(),
                },
                &BTreeSet::new(),
                &BTreeSet::from([interface.clone()]),
                "operation.request",
            )
            .unwrap();
        assert_eq!(owner.name, "operation_output");
        let refused = config
            .output_checkpoint(
                &ProviderHandoffRoute::Interface { interface },
                &BTreeSet::new(),
                &BTreeSet::new(),
                "operation.request",
            )
            .unwrap_err();
        assert!(refused.contains("outside the formed invocation grant"));
    }

    #[test]
    fn frontier_replacement_preserves_foreign_checkpoint_values() {
        let stable = serde_json::json!({
            "id": "stable_input",
            "run": "run_test",
            "trace": "trace:run_test:stable_input",
            "epoch": 7,
            "interface_binding": all_binding("world_input_test"),
            "outbound_interface_bindings": [all_binding("world_provider_test")],
            "artifact_bindings": [{ "id": "artifact_seed", "extension": [3, 2, 1] }],
            "required_verified_artifacts": [],
            "foreign_extension": { "preserve": true, "nested": { "value": 17 } }
        });
        let stable_bytes = serde_json::to_vec(&stable).unwrap();
        let mut root = serde_json::json!({
            "checkpoints": [
                stable,
                { "id": "old_authoring_call", "foreign": "old-call" },
                { "id": "old_authoring_result", "foreign": "old-result" }
            ],
            "active_checkpoints": [
                "stable_input",
                "old_authoring_call",
                "old_authoring_result"
            ],
            "foreign_root": { "untouched": [1, 4, 9] }
        })
        .as_object()
        .unwrap()
        .clone();
        let previous = BTreeSet::from([
            "old_authoring_call".to_string(),
            "old_authoring_result".to_string(),
        ]);
        let next_ids = vec![
            "new_output".to_string(),
            "new_authoring_call".to_string(),
            "new_authoring_result".to_string(),
        ];
        let next = next_ids
            .iter()
            .map(|id| serde_json::json!({ "id": id }))
            .collect();

        replace_owned_frontier(&mut root, &previous, next, &next_ids).unwrap();

        let checkpoints = root["checkpoints"].as_array().unwrap();
        assert_eq!(serde_json::to_vec(&checkpoints[0]).unwrap(), stable_bytes);
        assert_eq!(
            checkpoints
                .iter()
                .map(|checkpoint| checkpoint["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "stable_input",
                "new_output",
                "new_authoring_call",
                "new_authoring_result"
            ]
        );
        assert_eq!(
            root["active_checkpoints"],
            serde_json::json!([
                "stable_input",
                "new_output",
                "new_authoring_call",
                "new_authoring_result"
            ])
        );
        assert_eq!(
            root["foreign_root"],
            serde_json::json!({ "untouched": [1, 4, 9] })
        );
    }

    #[test]
    fn context_refresh_updates_every_provider_input_and_preserves_artifact_order() {
        let mut config = config_with_tool_routes(&[]);
        config.checkpoints.push(
            serde_json::from_value(serde_json::json!({
                "name": "tool_result_input",
                "role": "stable",
                "interface_binding": all_binding("system_context_test"),
                "outbound_interface_bindings": [all_binding("world_provider_test")],
                "include_context": true,
                "required_verified_artifacts": []
            }))
            .unwrap(),
        );
        config.checkpoints.push(
            serde_json::from_value(serde_json::json!({
                "name": "scheduled_input",
                "role": "independent_input",
                "interface_binding": all_binding("world_schedule_test"),
                "outbound_interface_bindings": [all_binding("world_provider_test")],
                "include_context": true,
                "required_verified_artifacts": []
            }))
            .unwrap(),
        );
        config
            .validate(
                &InterfaceId::new("world_provider_test"),
                &RunId::new("run_test"),
            )
            .unwrap();
        let mut root = serde_json::json!({
            "checkpoints": [
                {
                    "id": "terminal_input_old",
                    "run": "run_test",
                    "trace": "trace:run_test:terminal_input_old",
                    "epoch": 0,
                    "interface_binding": all_binding("world_input_test"),
                    "outbound_interface_bindings": [all_binding("world_provider_test")],
                    "artifact_bindings": [{"id": "artifact_old"}],
                    "required_verified_artifacts": []
                },
                {
                    "id": "tool_result_input_old",
                    "run": "run_test",
                    "trace": "trace:run_test:tool_result_input_old",
                    "epoch": 0,
                    "interface_binding": all_binding("system_context_test"),
                    "outbound_interface_bindings": [all_binding("world_provider_test")],
                    "artifact_bindings": [{"id": "artifact_old"}],
                    "required_verified_artifacts": []
                },
                {
                    "id": "foreign_wait",
                    "run": "run_test",
                    "trace": "trace:run_test:foreign_wait",
                    "epoch": 9,
                    "interface_binding": all_binding("world_schedule_test"),
                    "outbound_interface_bindings": [all_binding("world_provider_test")],
                    "artifact_bindings": [{"id": "artifact_schedule"}],
                    "required_verified_artifacts": [],
                    "foreign": {"preserve": true}
                }
            ],
            "active_checkpoints": [
                "terminal_input_old",
                "tool_result_input_old",
                "foreign_wait"
            ],
            "foreign_root": [1, 2, 3]
        })
        .as_object()
        .unwrap()
        .clone();
        let ordered_bindings = vec![
            serde_json::json!({"id": "artifact_z"}),
            serde_json::json!({"id": "artifact_a"}),
        ];
        replace_context_checkpoints(
            &mut root,
            &config,
            &RunId::new("run_test"),
            "cp_next",
            &ordered_bindings,
        )
        .unwrap();

        let checkpoints = root["checkpoints"].as_array().unwrap();
        assert_eq!(
            checkpoints[0]["foreign"],
            serde_json::json!({"preserve": true})
        );
        assert_eq!(
            checkpoints[0]["artifact_bindings"],
            serde_json::json!([{"id": "artifact_schedule"}])
        );
        assert_eq!(
            checkpoints
                .iter()
                .map(|checkpoint| checkpoint["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "foreign_wait",
                "cp_next_context_terminal_input",
                "cp_next_context_tool_result_input"
            ]
        );
        assert_eq!(
            checkpoints[1]["artifact_bindings"],
            serde_json::Value::Array(ordered_bindings.clone())
        );
        assert_eq!(
            checkpoints[2]["artifact_bindings"],
            serde_json::Value::Array(ordered_bindings)
        );
        assert_eq!(
            root["active_checkpoints"],
            serde_json::json!([
                "foreign_wait",
                "cp_next_context_terminal_input",
                "cp_next_context_tool_result_input"
            ])
        );
        assert_eq!(root["foreign_root"], serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn consumed_context_free_input_rearms_without_touching_external_input() {
        let control: RunAuthoringCheckpoint = serde_json::from_value(serde_json::json!({
            "name": "turn_control",
            "role": "independent_input",
            "interface_binding": names_binding(
                "world_operator_chat_test",
                &["operator.turn.control"],
            ),
            "outbound_interface_bindings": [all_binding("world_provider_test")],
            "include_context": false,
            "static_artifact_bindings": ["instruction-source"],
            "required_verified_artifacts": []
        }))
        .unwrap();
        let mut consumed = materialize_checkpoint(
            &control,
            "turn_control_old",
            &RunId::new("run_test"),
            &[serde_json::json!({"id": "role-context"})],
        );
        consumed["artifact_bindings"] = serde_json::json!([
            {"id": "instruction-source"},
            {
                "id": "role-context",
                "access": {"read": true, "change": false, "create": false, "delete": false}
            }
        ]);
        let mut root = serde_json::json!({
            "checkpoints": [
                consumed,
                {
                    "id": "scheduled_input",
                    "run": "run_test",
                    "trace": "trace:run_test:scheduled_input",
                    "epoch": 7,
                    "interface_binding": all_binding("world_schedule_test"),
                    "outbound_interface_bindings": [all_binding("world_provider_test")],
                    "artifact_bindings": [{"id": "schedule-context"}],
                    "required_verified_artifacts": [],
                    "foreign": {"preserve": true}
                }
            ],
            "active_checkpoints": ["turn_control_old", "scheduled_input"]
        })
        .as_object()
        .unwrap()
        .clone();

        replace_consumed_independent_input_checkpoint(
            &mut root,
            &control,
            &RunId::new("run_test"),
            "cp_next",
            &[serde_json::json!({"id": "role-context"})],
        )
        .unwrap();

        assert_eq!(
            root["active_checkpoints"],
            serde_json::json!(["scheduled_input", "cp_next_independent_input_turn_control"])
        );
        let checkpoints = root["checkpoints"].as_array().unwrap();
        assert_eq!(checkpoints[0]["id"], serde_json::json!("scheduled_input"));
        assert_eq!(
            checkpoints[0]["foreign"],
            serde_json::json!({"preserve": true})
        );
        assert_eq!(
            checkpoints[1]["id"],
            serde_json::json!("cp_next_independent_input_turn_control")
        );
        assert_eq!(
            checkpoints[1]["artifact_bindings"],
            serde_json::json!([{"id": "role-context"}, {"id": "instruction-source"}])
        );
    }

    #[tokio::test]
    async fn batch_frontier_recovers_each_output_without_provider_replay() {
        use tokio_util::sync::CancellationToken;

        let dir = tempfile::tempdir().unwrap();
        let var_dir = dir.path().join("var/provider");
        let emitted = var_dir.join("emitted");
        let first = InterfaceArtifactFile::write(
            &emitted,
            "interface.artifact.output",
            br#"{"text":"answer"}"#,
        )
        .unwrap();
        let second = InterfaceArtifactFile::write(
            &emitted,
            "interface.artifact.reasoning",
            br#"{"text":"reasoning"}"#,
        )
        .unwrap();
        let first_identity = artifact_identity(&first, &var_dir).unwrap();
        let second_identity = artifact_identity(&second, &var_dir).unwrap();
        let config = config_with_tool_routes(&[]);
        let output = config
            .checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput)
            .unwrap()
            .clone();
        let bound_topology = test_bound_topology(&config);
        let run_value = serde_json::json!({
            "id": "run_test",
            "ee": "ee_test",
            "worker": "system_worker_test",
            "trace": "trace:run_test",
            "status": "running",
            "interface_state": {
                "world_provider_test": {
                    "context_generation": 0,
                    "authoring_generation": 0,
                    "frontier_checkpoint_ids": ["output_old", "authoring_call_old", "authoring_result_old"]
                }
            },
            "checkpoints": [
                {
                    "id": "terminal_input",
                    "run": "run_test",
                    "trace": "trace:run_test:terminal_input",
                    "epoch": 0,
                    "interface_binding": all_binding("world_input_test"),
                    "outbound_interface_bindings": [all_binding("world_provider_test")],
                    "artifact_bindings": [],
                    "required_verified_artifacts": []
                },
                {
                    "id": "output_old",
                    "run": "run_test",
                    "trace": "trace:run_test:output_old",
                    "epoch": 0,
                    "interface_binding": all_binding("world_provider_test"),
                    "outbound_interface_bindings": [all_binding("world_client_test")],
                    "artifact_bindings": [],
                    "required_verified_artifacts": []
                },
                {
                    "id": "authoring_call_old",
                    "run": "run_test",
                    "trace": "trace:run_test:authoring_call_old",
                    "epoch": 0,
                    "interface_binding": all_binding("system_interface_authoring_test"),
                    "outbound_interface_bindings": [all_binding("system_context_authoring_test")],
                    "artifact_bindings": [],
                    "required_verified_artifacts": []
                },
                {
                    "id": "authoring_result_old",
                    "run": "run_test",
                    "trace": "trace:run_test:authoring_result_old",
                    "epoch": 0,
                    "interface_binding": all_binding("system_context_authoring_test"),
                    "outbound_interface_bindings": [all_binding("world_provider_test")],
                    "artifact_bindings": [],
                    "required_verified_artifacts": []
                }
            ],
            "active_checkpoints": ["terminal_input", "output_old", "authoring_call_old", "authoring_result_old"]
        });
        let run_bytes = serde_json::to_vec_pretty(&run_value).unwrap();
        let run_path = dir.path().join("runs/run_test.json");
        std::fs::create_dir_all(run_path.parent().unwrap()).unwrap();
        std::fs::write(&run_path, &run_bytes).unwrap();
        let movements_path = dir.path().join("ledger/movements.jsonl");
        std::fs::create_dir_all(movements_path.parent().unwrap()).unwrap();
        std::fs::write(&movements_path, b"").unwrap();
        let (arrivals, mut arrival_rx) = tokio::sync::mpsc::channel(2);
        let descriptor_snapshot = serde_json::from_value(serde_json::json!({
            "id": "world_provider_test",
            "interface_type": "world",
            "role": "provider",
            "direction": "bidirectional",
            "source_names": ["test"],
            "interface_driver_ref": "test_provider"
        }))
        .unwrap();
        let ctx = InterfaceLiftContext::unprivileged(
            InterfaceId::new("world_provider_test"),
            dir.path().join("registry/interface/world_provider_test"),
            Some(descriptor_snapshot),
            var_dir.clone(),
            movements_path.clone(),
            dir.path().join("ledger/blobs"),
            dir.path().join("ledger/ledger-index.sqlite"),
            Default::default(),
            arrivals,
            CancellationToken::new(),
        )
        .with_host_io_limits(InterfaceHostIoLimits {
            max_control_frame_bytes: 1_048_576,
            max_stderr_bytes: 262_144,
            max_manifest_bytes: 1_048_576,
            max_payload_bytes: 268_435_456,
        })
        .unwrap();
        let result_reader = LedgerTailReader::open_named(&ctx, "run-authoring").unwrap();
        let session = ProviderHandoffSession::new(&ctx).unwrap();
        let input = InterfaceArtifactFile::write(&emitted, "operator.input", br#"{"text":"produce a batch"}"#).unwrap();
        std::fs::create_dir_all(&ctx.blobs_dir).unwrap();
        let source_hash = ContentHash::of_bytes(&run_bytes);
        std::fs::write(ctx.blobs_dir.join(source_hash.to_hex()), &run_bytes).unwrap();
        let mut metadata = typed_invocation_metadata(&RunId::new("run_test"), "terminal_input", &ctx.interface, 0);
        metadata.runtime.as_mut().unwrap().run_record_hash = Some(source_hash.to_hex());
        let source = Movement {
            trace: octopus_ledger::TraceId("trace:run_test:terminal_input".into()),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface("world_input_test".into()),
            to: Endpoint::Interface(ctx.interface.clone()),
            artifact: Some(pin_artifact(&input, &ctx.blobs_dir)),
            invocation_metadata: Some(metadata.clone().into()),
            ..ledger_row(0)
        };
        let handoff = ProviderOutputHandoff {
            invocation_metadata: metadata,
            ctx,
            config,
            run_path: run_path.clone(),
            hand_in_root: dir.path().join("hand-in"),
            var_dir: var_dir.clone(),
            source_movement_seq: 0,
            source_carries_context: true,
            failed_source: false,
            checked_settlement_output: None,
            context_output_labels: BTreeSet::new(),
            session_snapshot_labels: BTreeSet::new(),
            standing_candidate_labels: BTreeSet::new(),
            granted_tools: BTreeSet::new(),
            granted_interfaces: BTreeSet::new(),
            granted_mark_outcomes: BTreeSet::new(),
            bound_topology,
            delivery_mode: HandoffDeliveryMode::Rearm,
            session,
            invocation_session_snapshot: None,
            state: Arc::new(tokio::sync::Mutex::new(HandoffState {
            validated_withdrawal_output: None,
                context_artifacts: Vec::new(),
                static_context_artifacts: Vec::new(),
                session_context: ResolvedSessionContext {
                    provider_interface: InterfaceId::new("world_provider_test"),
                    artifacts: Vec::new(),
                },
                session_snapshot_head: None,
                run_record_bytes: run_bytes.clone(),
                run_record_evidence_seq: None,
                consumed_input: false,
                consumed_context_free_independent_input: None,
                consumed_verified_input: false,
                deferred_context_generations: 0,
            })),
            result_reader: Arc::new(tokio::sync::Mutex::new(result_reader)),
        };
        let prepared = vec![
            PreparedHandoff {
                identity: first_identity.clone(),
                output_checkpoint: output.clone(),
                verified_inputs: Vec::new(),
            },
            PreparedHandoff {
                identity: second_identity.clone(),
                output_checkpoint: output,
                verified_inputs: Vec::new(),
            },
        ];
        let initial = handoff
            .author_run(&run_bytes, &[], &prepared, false, None, false, true)
            .unwrap();
        let initial_record = RunRecord::from_bytes(initial.content.as_bytes()).unwrap();
        assert_eq!(initial_record.status, serde_json::json!("idle"));
        let pending = pending_handoff(&initial_record, &InterfaceId::new("world_provider_test"))
            .unwrap()
            .unwrap();
        assert_eq!(pending.items.len(), 2);
        validate_pending_items_are_active(
            &initial_record,
            &InterfaceId::new("world_provider_test"),
            &pending,
        )
        .unwrap();
        let first_movement = pending_movement(1, &pending.items[0], &first_identity.artifact);
        std::fs::write(
            &movements_path,
            format!("{}\n{}\n", serde_json::to_string(&source).unwrap(), serde_json::to_string(&first_movement).unwrap()),
        )
        .unwrap();
        std::fs::write(&run_path, initial.content.as_bytes()).unwrap();
        handoff.state.lock().await.run_record_bytes = initial.content.as_bytes().to_vec();
        let pending_for_arrival = pending.clone();
        let movements_for_arrival = movements_path.clone();
        let expected_label = second_identity.label.clone();
        let expected_ref = second_identity.artifact.clone();
        let pending_snapshot = initial.after_hash.clone();
        let arrival = tokio::spawn(async move {
            let output = arrival_rx.recv().await.unwrap();
            assert_eq!(
                output.artifact.read_manifest().unwrap().label,
                expected_label
            );
            let movements = retained_pending_movements(&output,
                pending_movement(2, &pending_for_arrival.items[1], &expected_ref), &pending_snapshot);
            use std::io::Write as _;
            let mut ledger = std::fs::OpenOptions::new()
                .append(true)
                .open(&movements_for_arrival)
                .unwrap();
            for movement in movements { writeln!(ledger, "{}", serde_json::to_string(&movement).unwrap()).unwrap(); }
            ledger.sync_all().unwrap();
            output
                .retained
                .unwrap()
                .reply.send(Ok(octopus_ledger::ArtifactDelivery::Complete { verdict: DeliveryVerdict::Admitted { seq: 3 } }))
                .unwrap();
            tokio::time::timeout(Duration::from_millis(25), arrival_rx.recv())
                .await
                .is_err()
        });
        assert_eq!(
            handoff.recover_source().await.unwrap(),
            ProviderSourceRecovery::Completed
        );
        assert!(arrival.await.unwrap());
    }

    #[tokio::test]
    async fn standing_route_hands_the_next_output_without_a_run_write() {
        use tokio_util::sync::CancellationToken;

        let dir = tempfile::tempdir().unwrap();
        let var_dir = dir.path().join("var/provider");
        let emitted = var_dir.join("emitted");
        let reasoning = InterfaceArtifactFile::write(&emitted, "turn.reasoning", br#"{"text":"thinking"}"#).unwrap();
        let reasoning_identity = artifact_identity(&reasoning, &var_dir).unwrap();
        let config = config_with_tool_routes(&[]);
        let output = config
            .checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput)
            .unwrap()
            .clone();
        let bound_topology = test_bound_topology(&config);
        let checkpoint = |id: &str, inbound: &str, outbound: &str| serde_json::json!({
            "id": id, "run": "run_test", "trace": format!("trace:run_test:{id}"), "epoch": 0,
            "interface_binding": all_binding(inbound), "outbound_interface_bindings": [all_binding(outbound)],
            "artifact_bindings": [], "required_verified_artifacts": []
        });
        let run_value = serde_json::json!({
            "id": "run_test", "ee": "ee_test", "worker": "system_worker_test", "trace": "trace:run_test", "status": "running",
            "interface_state": {"world_provider_test": {
                "context_generation": 0, "authoring_generation": 0,
                "frontier_checkpoint_ids": ["output_old", "authoring_call_old", "authoring_result_old"]
            }},
            "checkpoints": [
                checkpoint("terminal_input", "world_input_test", "world_provider_test"),
                checkpoint("output_old", "world_provider_test", "world_client_test"),
                checkpoint("authoring_call_old", "system_interface_authoring_test", "system_context_authoring_test"),
                checkpoint("authoring_result_old", "system_context_authoring_test", "world_provider_test"),
            ],
            "active_checkpoints": ["terminal_input", "output_old", "authoring_call_old", "authoring_result_old"]
        });
        let run_bytes = serde_json::to_vec_pretty(&run_value).unwrap();
        let run_path = dir.path().join("runs/run_test.json");
        std::fs::create_dir_all(run_path.parent().unwrap()).unwrap();
        let movements_path = dir.path().join("ledger/movements.jsonl");
        std::fs::create_dir_all(movements_path.parent().unwrap()).unwrap();
        std::fs::write(&movements_path, b"").unwrap();
        let (arrivals, mut arrival_rx) = tokio::sync::mpsc::channel(2);
        let descriptor_snapshot = serde_json::from_value(serde_json::json!({
            "id": "world_provider_test", "interface_type": "world", "role": "provider",
            "direction": "bidirectional", "source_names": ["test"], "interface_driver_ref": "test_provider"
        }))
        .unwrap();
        let ctx = InterfaceLiftContext::unprivileged(
            InterfaceId::new("world_provider_test"),
            dir.path().join("registry/interface/world_provider_test"),
            Some(descriptor_snapshot),
            var_dir.clone(),
            movements_path.clone(),
            dir.path().join("ledger/blobs"),
            dir.path().join("ledger/ledger-index.sqlite"),
            Default::default(),
            arrivals,
            CancellationToken::new(),
        )
        .with_host_io_limits(InterfaceHostIoLimits {
            max_control_frame_bytes: 1_048_576,
            max_stderr_bytes: 262_144,
            max_manifest_bytes: 1_048_576,
            max_payload_bytes: 268_435_456,
        })
        .unwrap();
        std::fs::create_dir_all(&ctx.blobs_dir).unwrap();
        let input = InterfaceArtifactFile::write(&emitted, "operator.input", br#"{"text":"answer"}"#).unwrap();
        let source_hash = ContentHash::of_bytes(&run_bytes);
        std::fs::write(ctx.blobs_dir.join(source_hash.to_hex()), &run_bytes).unwrap();
        let mut metadata = typed_invocation_metadata(&RunId::new("run_test"), "terminal_input", &ctx.interface, 0);
        metadata.runtime.as_mut().unwrap().run_record_hash = Some(source_hash.to_hex());
        let source = Movement {
            trace: octopus_ledger::TraceId("trace:run_test:terminal_input".into()),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface("world_input_test".into()),
            to: Endpoint::Interface(ctx.interface.clone()),
            artifact: Some(pin_artifact(&input, &ctx.blobs_dir)),
            invocation_metadata: Some(metadata.clone().into()),
            ..ledger_row(0)
        };
        let result_reader = LedgerTailReader::open_named(&ctx, "run-authoring").unwrap();
        let session = ProviderHandoffSession::new(&ctx).unwrap();
        let handoff = ProviderOutputHandoff {
            invocation_metadata: metadata,
            ctx,
            config,
            run_path: run_path.clone(),
            hand_in_root: dir.path().join("hand-in"),
            var_dir: var_dir.clone(),
            source_movement_seq: 0,
            source_carries_context: true,
            failed_source: false,
            checked_settlement_output: None,
            context_output_labels: BTreeSet::from(["turn.agent_message".to_string()]),
            session_snapshot_labels: BTreeSet::new(),
            standing_candidate_labels: BTreeSet::from(["turn.agent_message".to_string(), "turn.reasoning".to_string()]),
            granted_tools: BTreeSet::new(),
            granted_interfaces: BTreeSet::new(),
            granted_mark_outcomes: BTreeSet::new(),
            bound_topology,
            delivery_mode: HandoffDeliveryMode::Rearm,
            session,
            invocation_session_snapshot: None,
            state: Arc::new(tokio::sync::Mutex::new(HandoffState {
                validated_withdrawal_output: None,
                context_artifacts: Vec::new(),
                static_context_artifacts: Vec::new(),
                session_context: ResolvedSessionContext {
                    provider_interface: InterfaceId::new("world_provider_test"),
                    artifacts: Vec::new(),
                },
                session_snapshot_head: None,
                run_record_bytes: run_bytes.clone(),
                run_record_evidence_seq: None,
                consumed_input: false,
                consumed_context_free_independent_input: None,
                consumed_verified_input: false,
                deferred_context_generations: 0,
            })),
            result_reader: Arc::new(tokio::sync::Mutex::new(result_reader)),
        };
        let prepared = [PreparedHandoff { identity: reasoning_identity.clone(), output_checkpoint: output, verified_inputs: Vec::new() }];
        let authored = handoff.author_run(&run_bytes, &[], &prepared, false, None, false, false).unwrap();
        let record = RunRecord::from_bytes(authored.content.as_bytes()).unwrap();
        let provider = InterfaceId::new("world_provider_test");
        let pending = pending_handoff(&record, &provider).unwrap().unwrap();
        let standing = record
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.id.as_str().ends_with("_standing_client_output"))
            .expect("the write leaves a standing route for the next client outputs");
        assert_eq!(
            standing.interface_binding.as_ref().unwrap().config.artifact_policy,
            BindingArtifactPolicy::Allow { names: vec!["turn.agent_message".into()], namespaces: Vec::new() },
            "the standing route takes every client name the write's own outputs do not claim"
        );
        assert!(record.checkpoints.iter().any(|checkpoint| checkpoint.id.as_str().ends_with("_standing_transport_dispatch_standing")));
        validate_pending_items_are_active(&record, &provider, &pending).unwrap();
        std::fs::write(&run_path, authored.content.as_bytes()).unwrap();
        std::fs::write(
            &movements_path,
            format!(
                "{}\n{}\n",
                serde_json::to_string(&source).unwrap(),
                serde_json::to_string(&pending_movement(1, &pending.items[0], &reasoning_identity.artifact)).unwrap()
            ),
        )
        .unwrap();
        handoff.state.lock().await.run_record_bytes = authored.content.as_bytes().to_vec();

        let answer = InterfaceArtifactFile::write(&emitted, "turn.agent_message", br#"{"text":"answer"}"#).unwrap();
        let answer_ref = pin_artifact(&answer, &dir.path().join("ledger/blobs"));
        let mut standing_item = pending.items[0].clone();
        standing_item.checkpoint.id = standing.id.clone();
        let movements_for_arrival = movements_path.clone();
        let snapshot = authored.after_hash.clone();
        let arrival = tokio::spawn(async move {
            let delivery = arrival_rx.recv().await.unwrap();
            assert_eq!(delivery.artifact.read_manifest().unwrap().label, "turn.agent_message");
            let rows = retained_pending_movements(&delivery, pending_movement(2, &standing_item, &answer_ref), &snapshot);
            use std::io::Write as _;
            let mut ledger = std::fs::OpenOptions::new().append(true).open(&movements_for_arrival).unwrap();
            for row in rows { writeln!(ledger, "{}", serde_json::to_string(&row).unwrap()).unwrap(); }
            ledger.sync_all().unwrap();
            delivery
                .retained
                .unwrap()
                .reply
                .send(Ok(octopus_ledger::ArtifactDelivery::Complete { verdict: DeliveryVerdict::Admitted { seq: 3 } }))
                .unwrap();
            tokio::time::timeout(Duration::from_millis(25), arrival_rx.recv()).await.is_err()
        });
        let outcome = handoff
            .hand(ProviderEmission { artifact: answer, route: ProviderEmissionRoute::ClientProjection })
            .await
            .unwrap();
        assert!(matches!(outcome, ProviderHandoffOutcome::Admitted { seq: 3, .. }));
        assert!(arrival.await.unwrap(), "the standing delivery hands the output once");
        assert_eq!(
            std::fs::read(&run_path).unwrap(),
            authored.content.as_bytes(),
            "the next ordinary output rides the standing route without rewriting the run"
        );
        assert_eq!(handoff.deferred_context_generations().await, 1);
        assert_eq!(handoff.context_generation().await.unwrap(), 1);
        assert_eq!(
            handoff.recover_source().await.unwrap(),
            ProviderSourceRecovery::Interrupted { last_artifact_label: "turn.agent_message".into() },
            "the private journal carries the standing delivery across a restart"
        );
    }

    #[tokio::test]
    async fn terminal_pending_handoff_recovers_from_ledger_without_provider_replay() {
        use octopus_ledger::{Lamport, Seq, TraceId};
        use tokio_util::sync::CancellationToken;

        let dir = tempfile::tempdir().unwrap();
        let var_dir = dir.path().join("var/provider");
        let emitted = var_dir.join("emitted");
        let artifact = InterfaceArtifactFile::write(
            &emitted,
            "interface.artifact.output",
            br#"{"text":"already handed"}"#,
        )
        .unwrap();
        let artifact = materialize_session_snapshot(
            &artifact,
            &ResolvedSessionContext {
                provider_interface: InterfaceId::new("world_provider_test"),
                artifacts: Vec::new(),
            },
            None,
        )
        .unwrap();
        let blobs_dir = dir.path().join("ledger/blobs");
        std::fs::create_dir_all(&blobs_dir).unwrap();
        pin_artifact(&artifact, &blobs_dir);
        let identity = artifact_identity(&artifact, &var_dir).unwrap();
        let pending = PendingHandoff {
            source_movement_seq: 17,
            context_publication: None,
            deferred_publication: false,
            source_without_context: false,
            settlement_of: None,
            dispatch_fenced: true,
            withdrawal: None, replacement: None,
            standing: Vec::new(),
            items: vec![PendingHandoffItem {
                artifact_id: identity.artifact.id.clone(),
                artifact_content_hash: identity.artifact.content.to_hex(),
                artifact_label: identity.label.clone(),
                manifest_relative_path: identity.manifest_relative_path.clone(),
                checkpoint: PendingHandoffCheckpoint {
                    id: CheckpointId::new("output"),
                    epoch: 0,
                    interface_binding: all_binding("world_provider_test"),
                    outbound_interface_bindings: vec![all_binding("world_client_test")],
                    artifact_bindings: Vec::new(),
                    required_verified_artifacts: Vec::new(),
                },
            }],
            terminal: true,
        };
        let config = config_with_tool_routes(&[]);
        let run_value = serde_json::json!({
            "id": "run_test",
            "ee": "ee_test",
            "worker": "system_worker_test",
            "trace": "trace:run_test",
            "status": "running",
            "interface_state": {
                "world_provider_test": {
                    "context_generation": 1,
                    "authoring_generation": 47,
                    "run_authoring_binding": test_bound_topology(&config),
                    "frontier_checkpoint_ids": ["output", "authoring_call", "authoring_result"],
                    "pending_handoff": pending
                }
            },
            "checkpoints": [
                {
                    "id": "terminal_input",
                    "run": "run_test",
                    "trace": "trace:run_test:terminal_input",
                    "epoch": 0,
                    "interface_binding": all_binding("world_input_test"),
                    "outbound_interface_bindings": [all_binding("world_provider_test")],
                    "artifact_bindings": [{"id": "context.reference:seed"}, {"id": identity.artifact.id}],
                    "required_verified_artifacts": []
                },
                {
                    "id": "output",
                    "run": "run_test",
                    "trace": "trace:run_test:output",
                    "epoch": 0,
                    "interface_binding": all_binding("world_provider_test"),
                    "outbound_interface_bindings": [all_binding("world_client_test")],
                    "artifact_bindings": [],
                    "required_verified_artifacts": []
                },
                {
                    "id": "authoring_call",
                    "run": "run_test",
                    "trace": "trace:run_test:authoring_call",
                    "epoch": 0,
                    "interface_binding": all_binding("system_interface_authoring_test"),
                    "outbound_interface_bindings": [all_binding("system_context_authoring_test")],
                    "artifact_bindings": [],
                    "required_verified_artifacts": []
                },
                {
                    "id": "authoring_result",
                    "run": "run_test",
                    "trace": "trace:run_test:authoring_result",
                    "epoch": 0,
                    "interface_binding": all_binding("system_context_authoring_test"),
                    "outbound_interface_bindings": [all_binding("world_provider_test")],
                    "artifact_bindings": [],
                    "required_verified_artifacts": []
                }
            ],
            "active_checkpoints": ["terminal_input", "output", "authoring_call", "authoring_result"]
        });
        let run_bytes = serde_json::to_vec_pretty(&run_value).unwrap();
        let run_path = dir.path().join("runs/run_test.json");
        std::fs::create_dir_all(run_path.parent().unwrap()).unwrap();
        std::fs::write(&run_path, &run_bytes).unwrap();

        let movements_path = dir.path().join("ledger/movements.jsonl");
        std::fs::create_dir_all(movements_path.parent().unwrap()).unwrap();
        let movement = Movement {
            seq: Seq(18),
            lamport: Lamport(19),
            recorded_at_unix_ms: 19,
            trace: TraceId("trace-recovery".to_string()),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(InterfaceId::new("world_provider_test")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_test")),
            artifact: Some(identity.artifact.clone()),
            invocation_metadata: Some(
                typed_invocation_metadata(
                    &RunId::new("run_test"),
                    "output",
                    &InterfaceId::new("world_provider_test"),
                    0,
                )
                .into(),
            ),
        };
        let mut rows = (0..18).map(|seq| format!("{}\n", serde_json::to_string(&ledger_row(seq)).unwrap())).collect::<String>();
        rows.push_str(&format!("{}\n", serde_json::to_string(&movement).unwrap()));
        std::fs::write(&movements_path, rows).unwrap();

        let (arrivals, mut arrival_rx) = tokio::sync::mpsc::channel(1);
        let descriptor_snapshot = serde_json::from_value(serde_json::json!({
            "id": "world_provider_test",
            "interface_type": "world",
            "role": "provider",
            "direction": "bidirectional",
            "source_names": ["test"],
            "interface_driver_ref": "test_provider"
        }))
        .unwrap();
        let ctx = InterfaceLiftContext::unprivileged(
            InterfaceId::new("world_provider_test"),
            dir.path().join("registry/interface/world_provider_test"),
            Some(descriptor_snapshot),
            var_dir.clone(),
            movements_path.clone(),
            dir.path().join("ledger/blobs"),
            dir.path().join("ledger/ledger-index.sqlite"),
            Default::default(),
            arrivals,
            CancellationToken::new(),
        )
        .with_host_io_limits(InterfaceHostIoLimits {
            max_control_frame_bytes: 1_048_576,
            max_stderr_bytes: 262_144,
            max_manifest_bytes: 1_048_576,
            max_payload_bytes: 268_435_456,
        })
        .unwrap();
        let result_reader = LedgerTailReader::open_named(&ctx, "run-authoring").unwrap();
        let session = ProviderHandoffSession::new(&ctx).unwrap();
        let bound_topology = test_bound_topology(&config);
        let mut handoff = ProviderOutputHandoff {
            invocation_metadata: InvocationMetadata::new(Default::default()),
            ctx,
            config,
            run_path,
            hand_in_root: dir.path().join("hand-in"),
            var_dir,
            source_movement_seq: 17,
            source_carries_context: true,
            failed_source: false,
            checked_settlement_output: None,
            context_output_labels: BTreeSet::from(["interface.artifact.output".to_string()]),
            session_snapshot_labels: BTreeSet::from(["interface.artifact.output".to_string()]),
            standing_candidate_labels: BTreeSet::new(),
            granted_tools: BTreeSet::new(),
            granted_interfaces: BTreeSet::new(),
            granted_mark_outcomes: BTreeSet::new(),
            bound_topology,
            delivery_mode: HandoffDeliveryMode::Rearm,
            session,
            invocation_session_snapshot: None,
            state: Arc::new(tokio::sync::Mutex::new(HandoffState {
            validated_withdrawal_output: None,
                context_artifacts: vec![ArtifactId::new("context.reference:seed")],
                static_context_artifacts: vec![ArtifactId::new("context.reference:seed")],
                session_context: ResolvedSessionContext {
                    provider_interface: InterfaceId::new("world_provider_test"),
                    artifacts: Vec::new(),
                },
                session_snapshot_head: None,
                run_record_bytes: run_bytes.clone(),
                run_record_evidence_seq: None,
                consumed_input: false,
                consumed_context_free_independent_input: None,
                consumed_verified_input: false,
                deferred_context_generations: 0,
            })),
            result_reader: Arc::new(tokio::sync::Mutex::new(result_reader)),
        };

        assert_eq!(
            handoff.recover_source().await.unwrap(),
            ProviderSourceRecovery::Completed
        );
        let mut rearmed_value: serde_json::Value = serde_json::from_slice(&run_bytes).unwrap();
        {
            let mut diagnostic = handoff.clone();
            diagnostic.source_carries_context = false;
            diagnostic.context_output_labels.insert("turn.error".into());
            diagnostic.session_snapshot_labels.insert("turn.error".into());
            let file = InterfaceArtifactFile::write(&emitted, "turn.error", br#"{"label":"turn.error","message":"independent input failed"}"#).unwrap();
            let error = artifact_identity(&file, &handoff.var_dir).unwrap();
            let mut pending = pending.clone();
            pending.items[0].artifact_id = error.artifact.id;
            pending.items[0].artifact_content_hash = error.artifact.content.to_hex();
            pending.items[0].artifact_label = error.label;
            pending.items[0].manifest_relative_path = error.manifest_relative_path;
            let mut state = diagnostic.state.lock().await;
            let before = serde_json::to_value(&state.session_context.artifacts).unwrap();
            let record = RunRecord::from_bytes(&state.run_record_bytes).unwrap();
            diagnostic.restore_pending_context(&mut state, &record, &pending).unwrap();
            assert_eq!(serde_json::to_value(&state.session_context.artifacts).unwrap(), before);
        }
        {
            let mut owner_config = handoff.config.clone();
            let owner_input: RunAuthoringCheckpoint = serde_json::from_value(serde_json::json!({
                "name":"owner_input", "role":"owner_input", "include_context":false,
                "interface_binding":all_binding("system_owner_test"),
                "outbound_interface_bindings":[all_binding("world_provider_test")],
                "required_verified_artifacts":[],
                "owner_authoring":{"owner":"system_owner_test","source":"system_owner_test",
                    "door":"system_owner_writer_test","tool":"file.write.owner",
                    "call_label":"tool.call","result_label":"file.write.owner.result"}
            })).unwrap();
            owner_config.checkpoints.push(owner_input.clone());
            let mut pinned = run_value.clone();
            pinned["interface_state"]["world_provider_test"][RUN_AUTHORING_BINDING] =
                serde_json::to_value(test_bound_topology(&owner_config)).unwrap();
            pinned["checkpoints"].as_array_mut().unwrap().push(serde_json::json!({
                "id":"owner_input", "run":"run_test", "trace":"trace:owner-input", "epoch":3,
                "interface_binding":owner_input.interface_binding,
                "outbound_interface_bindings":owner_input.outbound_interface_bindings,
                "artifact_bindings":[],"required_verified_artifacts":[]
            }));
            pinned["active_checkpoints"].as_array_mut().unwrap().push(serde_json::json!("owner_input"));
            let pinned_bytes = serde_json::to_vec(&pinned).unwrap();
            let pinned_hash = ContentHash::of_bytes(&pinned_bytes);
            std::fs::write(blobs_dir.join(pinned_hash.to_hex()), &pinned_bytes).unwrap();
            let input = InterfaceArtifactFile::write(&emitted, "tool.result", br#"{"output":"31"}"#).unwrap();
            let source = pin_artifact(&input, &blobs_dir);
            let mut metadata = typed_invocation_metadata(&handoff.config.run, "owner_input", &handoff.config.provider_interface, 3);
            metadata.runtime.as_mut().unwrap().run_record_hash = Some(pinned_hash.to_hex());
            metadata.checkpoint_binding_policy = Some(owner_input.binding_policy_snapshot());
            metadata.outbound_interface_bindings = vec![handoff.config.provider_interface.clone()];
            let movement = Movement {
                seq:Seq(19), lamport:Lamport(20), recorded_at_unix_ms:20,
                trace:TraceId("trace:owner-input".into()), kind:MovementKind::InterfaceOutbound,
                from:Endpoint::Interface(owner_input.interface().clone()),
                to:Endpoint::Interface(handoff.config.provider_interface.clone()),
                artifact:Some(source), invocation_metadata:Some(metadata.into()),
            };
            use std::io::Write as _;
            let mut ledger = std::fs::OpenOptions::new().append(true).open(&movements_path).unwrap();
            writeln!(ledger, "{}", serde_json::to_string(&movement).unwrap()).unwrap();
            let mut wrong_epoch = movement.clone();
            wrong_epoch.seq = Seq(20);
            wrong_epoch.lamport = Lamport(21);
            let mut wrong_metadata = wrong_epoch.invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
            wrong_metadata.runtime.as_mut().unwrap().epoch = 4;
            wrong_epoch.invocation_metadata = Some(wrong_metadata.into());
            writeln!(ledger, "{}", serde_json::to_string(&wrong_epoch).unwrap()).unwrap();
            ledger.sync_all().unwrap();
            let mut later = handoff.clone();
            later.source_movement_seq = 21;
            assert!(later.source_is_context_free(19).unwrap());
            assert!(later.source_is_context_free(20).unwrap_err().contains("exact admitted checkpoint"));
            let file = InterfaceArtifactFile::write(&emitted, "interface.artifact.output", br#"{"text":"31"}"#).unwrap();
            let output = artifact_identity(&file, &handoff.var_dir).unwrap();
            let mut context_free = pending.clone();
            context_free.source_movement_seq = 19;
            context_free.items[0].artifact_id = output.artifact.id;
            context_free.items[0].artifact_content_hash = output.artifact.content.to_hex();
            context_free.items[0].manifest_relative_path = output.manifest_relative_path;
            let mut state = later.state.lock().await;
            let before = serde_json::to_value(&state.session_context.artifacts).unwrap();
            let record = RunRecord::from_bytes(&state.run_record_bytes).unwrap();
            later.restore_pending_context(&mut state, &record, &context_free).unwrap();
            assert_eq!(serde_json::to_value(&state.session_context.artifacts).unwrap(), before);
            let mut visible = record.clone();
            visible.checkpoints.iter_mut().find(|cp| cp.id.as_str() == "terminal_input").unwrap()
                .artifact_bindings.push(ArtifactBinding {
                    id:context_free.items[0].artifact_id.clone(), access:octopus_policy::ArtifactAccess::read_only(),
                });
            assert!(later.restore_pending_context(&mut state, &visible, &context_free).is_err());
        }
        {
            let mut later = handoff.clone();
            later.source_movement_seq = pending.source_movement_seq + 1;
            later.context_output_labels.insert("turn.error".into());
            later.session_snapshot_labels.insert("turn.error".into());
            let payload = br#"{"label":"turn.error","message":"turn stopped before dispatch"}"#;
            assert!(!payload_carries_session_context(payload).unwrap());
            let file = InterfaceArtifactFile::write(&emitted, "turn.error", payload).unwrap();
            let error = artifact_identity(&file, &handoff.var_dir).unwrap();
            let mut failed = pending.clone();
            failed.items[0].artifact_id = error.artifact.id;
            failed.items[0].artifact_content_hash = error.artifact.content.to_hex();
            failed.items[0].artifact_label = error.label;
            failed.items[0].manifest_relative_path = error.manifest_relative_path;
            let mut state = later.state.lock().await;
            let before = serde_json::to_value(&state.session_context.artifacts).unwrap();
            let record = RunRecord::from_bytes(&state.run_record_bytes).unwrap();
            assert!(later.restore_pending_context(&mut state, &record, &failed).is_err());
            failed.source_without_context = true;
            assert_eq!(
                serde_json::to_value(&failed).unwrap()["source_without_context"],
                serde_json::json!(true)
            );
            later.restore_pending_context(&mut state, &record, &failed).unwrap();
            assert_eq!(serde_json::to_value(&state.session_context.artifacts).unwrap(), before);
        }
        for checkpoint in rearmed_value["checkpoints"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
        {
            let Some(id) = checkpoint["id"].as_str() else {
                continue;
            };
            if ["output", "authoring_call", "authoring_result"].contains(&id) {
                checkpoint["id"] = serde_json::json!(format!("{id}_next"));
            }
        }
        rearmed_value["interface_state"]["world_provider_test"]["frontier_checkpoint_ids"] =
            serde_json::json!([
                "output_next",
                "authoring_call_next",
                "authoring_result_next"
            ]);
        rearmed_value["active_checkpoints"] = serde_json::json!([
            "terminal_input",
            "output_next",
            "authoring_call_next",
            "authoring_result_next"
        ]);
        let rearmed_bytes = serde_json::to_vec(&rearmed_value).unwrap();
        let rearmed_record = RunRecord::from_bytes(&rearmed_bytes).unwrap();
        validate_bound_run_authoring_topology(
            &rearmed_record,
            &handoff.config,
            Some(&CheckpointId::new("terminal_input")),
        )
        .unwrap();
        let mut rearmed_handoff = handoff.clone();
        rearmed_handoff.source_movement_seq = 18;
        rearmed_handoff.state = Arc::new(tokio::sync::Mutex::new(HandoffState {
            validated_withdrawal_output: None,
            context_artifacts: vec![ArtifactId::new("context.reference:seed")],
            static_context_artifacts: vec![ArtifactId::new("context.reference:seed")],
            session_context: ResolvedSessionContext {
                provider_interface: InterfaceId::new("world_provider_test"),
                artifacts: Vec::new(),
            },
            session_snapshot_head: None,
            run_record_bytes: rearmed_bytes,
                run_record_evidence_seq: None,
            consumed_input: false,
            consumed_context_free_independent_input: None,
            consumed_verified_input: false,
            deferred_context_generations: 0,
        }));
        assert_eq!(
            rearmed_handoff.recover_source().await.unwrap(),
            ProviderSourceRecovery::Process
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(25), arrival_rx.recv())
                .await
                .is_err()
        );
        {
            let mut state = handoff.state.lock().await;
            let mut value: serde_json::Value =
                serde_json::from_slice(&state.run_record_bytes).unwrap();
            value["interface_state"]["world_provider_test"]["pending_handoff"]["terminal"] =
                serde_json::Value::Bool(false);
            state.run_record_bytes = serde_json::to_vec(&value).unwrap();
            state.session_context.artifacts = vec![SessionContextArtifact {
                label: identity.label.clone(),
                payload: serde_json::json!({"text": "already handed"}),
                source_artifact: identity.artifact.clone(),
                source_movement_seq: Some(18),
            }];
            state.session_snapshot_head = None;
        }
        assert_eq!(
            handoff.recover_source().await.unwrap(),
            ProviderSourceRecovery::Interrupted {
                last_artifact_label: "interface.artifact.output".to_string()
            }
        );
        assert_eq!(
            handoff.state.lock().await.session_context.artifacts[0].source_movement_seq,
            Some(18)
        );

        use std::io::Write as _;
        handoff.run_path = handoff.run_path.canonicalize().unwrap();
        handoff.ctx.root_slots.insert(
            "run_records_root".into(),
            handoff.run_path.parent().unwrap().to_path_buf(),
        );
        let control_dir = tempfile::tempdir_in("/tmp").unwrap();
        handoff.ctx.root_slots.insert(
            "app_server_control_root".into(),
            control_dir.path().to_path_buf(),
        );
        handoff
            .context_output_labels
            .insert("interface.artifact.reasoning".into());
        handoff
            .session_snapshot_labels
            .insert("interface.artifact.reasoning".into());
        handoff
            .context_output_labels
            .insert("interface.artifact.progress".into());
        for (generation, terminal) in [(48, false), (50, true)] {
            let label = if terminal {
                "interface.artifact.progress"
            } else {
                "interface.artifact.reasoning"
            };
            let next_file = InterfaceArtifactFile::write(
                &handoff.var_dir.join("emitted"),
                label,
                &serde_json::to_vec(&serde_json::json!({"text": format!("output {generation}")}))
                    .unwrap(),
            )
            .unwrap();
            let mut state = handoff.state.lock().await;
            let next_file = if terminal {
                next_file
            } else {
                materialize_session_snapshot(
                    &next_file,
                    &state.session_context,
                    state.session_snapshot_head.as_ref(),
                )
                .unwrap()
            };
            let next_identity = artifact_identity(&next_file, &handoff.var_dir).unwrap();
            let mut next_context = if terminal {
                state.context_artifacts.clone()
            } else {
                state.static_context_artifacts.clone()
            };
            next_context.push(next_identity.artifact.id.clone());
            let next = handoff
                .author_run(
                    &state.run_record_bytes,
                    &next_context,
                    &[PreparedHandoff {
                        identity: next_identity.clone(),
                        output_checkpoint: handoff
                            .config
                            .checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput)
                            .unwrap()
                            .clone(),
                        verified_inputs: Vec::new(),
                    }],
                    true,
                    None,
                    false,
                    terminal,
                )
                .unwrap();
            assert_eq!(
                next.correlation_id,
                format!("provider-run-authoring:run_test:{generation}")
            );
            let call_file = handoff.materialize_file_write_call(&next).unwrap();
            let call_bytes = std::fs::read(call_file.manifest_path()).unwrap();
            let staged_record = RunRecord::from_bytes(next.content.as_bytes()).unwrap();
            assert!(!current_context_artifacts(&staged_record, &handoff.config).unwrap().contains(&next_identity.artifact.id));
            let publication = handoff.author_context_publication(next.content.as_bytes(), &next.pending_handoff).unwrap();
            let published_record = RunRecord::from_bytes(publication.content.as_bytes()).unwrap();
            let staged_state = &staged_record.interface_state[&handoff.config.provider_interface];
            let published_state = &published_record.interface_state[&handoff.config.provider_interface];
            assert_eq!(published_state["context_generation"], staged_state["context_generation"]);
            assert_eq!(published_state["authoring_generation"].as_u64(), staged_state["authoring_generation"].as_u64().map(|generation| generation + 1));
            let publication_file = handoff.materialize_file_write_call(&publication).unwrap();
            let publication_bytes = std::fs::read(publication_file.manifest_path()).unwrap();
            let before = state.run_record_bytes.clone();
            let mut concurrent: serde_json::Value = serde_json::from_slice(&before).unwrap();
            let mut independent = concurrent["checkpoints"][0].clone();
            independent["id"] = serde_json::json!("independent_input");
            independent["interface_binding"]["interface"] = serde_json::json!("system_independent_input");
            independent["outbound_interface_bindings"] = serde_json::json!([]);
            concurrent["checkpoints"].as_array_mut().unwrap().push(independent.clone());
            concurrent["active_checkpoints"].as_array_mut().unwrap().push(serde_json::json!("independent_input"));
            concurrent["interface_state"]["system_independent_input"] = serde_json::json!({"owned_checkpoint_ids":["independent_input"]});
            let mut current_bytes = serde_json::to_vec_pretty(&concurrent).unwrap(); current_bytes.push(b'\n');
            let rebased = authoring_retry::rebase_authored_run(&before, &current_bytes, &next, &handoff.config, Some(JsonFormat::Compact)).unwrap();
            let merged: serde_json::Value = serde_json::from_str(&rebased.content).unwrap();
            assert_eq!(merged["interface_state"]["system_independent_input"], concurrent["interface_state"]["system_independent_input"]);
            assert!(merged["checkpoints"].as_array().unwrap().contains(&independent));
            assert!(merged["active_checkpoints"].as_array().unwrap().contains(&serde_json::json!("independent_input")));
            assert_eq!(rebased.pending_handoff, next.pending_handoff);
            assert_ne!(rebased.after_hash, next.after_hash);
            concurrent["interface_state"][handoff.config.provider_interface.as_str()]["context_generation"] = serde_json::json!(9999);
            assert!(authoring_retry::rebase_authored_run(&before, &serde_json::to_vec(&concurrent).unwrap(), &next, &handoff.config, Some(JsonFormat::Compact)).is_err());
            std::fs::write(&handoff.run_path, &before).unwrap();
            let mut different = next.clone();
            different.content.push(' ');
            let error = handoff.install_authored_run(&mut state, &different).await.err().unwrap();
            assert!(error.contains("different exact attempt"), "{error}");
            let mut previous = handoff.clone();
            previous.state = Arc::new(tokio::sync::Mutex::new(state.clone()));
            drop(state);
            let mut wrong_source = previous.clone();
            wrong_source.source_movement_seq += 1;
            let mut foreign_state = previous.state.lock().await.clone();
            assert!(wrong_source.install_authored_run(&mut foreign_state, &next).await.err().unwrap()
                .contains("private attempt belongs to source movement"));
            assert_eq!(std::fs::read(call_file.manifest_path()).unwrap(), call_bytes);
            assert_eq!(std::fs::read(&handoff.run_path).unwrap(), before);
            let mut unadmitted = previous.clone();
            unadmitted.ctx.movements_path =
                dir.path().join(format!("unadmitted-{generation}.jsonl"));
            std::fs::write(&unadmitted.ctx.movements_path, b"").unwrap();
            unadmitted.session = ProviderHandoffSession::new(&unadmitted.ctx).unwrap();
            let socket = octopus_app_server_interface::interface_control_socket_path_from_root(
                control_dir.path(), &handoff.config.authoring_source_interface,
            );
            std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
            let listener = tokio::net::UnixListener::bind(&socket).unwrap();
            let journal_prefix = std::fs::read(&movements_path).unwrap();
            let call_seq = journal_prefix
                .split(|b| *b == b'\n')
                .filter(|row| !row.is_empty())
                .count() as u64;
            let result_checkpoint = handoff
                .config
                .checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)
                .unwrap()
                .clone();
            let writes_for_host = [(next.clone(), call_file, call_bytes, call_seq),
                (publication.clone(), publication_file, publication_bytes, call_seq + 4)];
            let config_for_host = handoff.config.clone();
            let blobs = blobs_dir.clone();
            let journal = movements_path.clone();
            let path = handoff.run_path.clone();
            let host = tokio::spawn(async move {
              for (next_for_host, call_file, call_bytes, call_seq) in writes_for_host {
                let (stream, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                let received = ws.next().await.unwrap().unwrap().into_text().unwrap();
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&received).unwrap(),
                    serde_json::from_slice::<serde_json::Value>(&call_bytes).unwrap()
                );
                let call: FileWriteCallEnvelope =
                    serde_json::from_slice(&call_file.read_payload_bytes().unwrap()).unwrap();
                let current = std::fs::read(&path).unwrap();
                assert_eq!(
                    ContentHash::of_bytes(&current).to_hex(),
                    call.arguments.expected_before_hash
                );
                let content = apply_run_json_patch(
                    &current,
                    &call.arguments.patch,
                    &call.arguments.expected_after_hash, call.arguments.json_format
                )
                .unwrap();
                assert_eq!(content, next_for_host.content);
                let call_ref = pin_artifact(&call_file, &blobs);
                let mut arrival = ledger_row(call_seq);
                arrival.kind = MovementKind::InterfaceArrival;
                arrival.from =
                    Endpoint::Interface(InterfaceId::new("system_interface_authoring_test"));
                arrival.artifact = Some(call_ref);
                let current_record = RunRecord::from_bytes(&current).unwrap();
                let checkpoint = active_checkpoint_for_role(&current_record, &config_for_host,
                    RunAuthoringCheckpointRole::AuthoringCall).unwrap();
                let snapshot = ContentHash::of_bytes(&current);
                std::fs::write(blobs.join(snapshot.to_hex()), &current).unwrap();
                let mut metadata = typed_invocation_metadata(&current_record.id, checkpoint.id.as_str(),
                    checkpoint.inbound_interface().unwrap(), checkpoint.epoch.0);
                let runtime = metadata.runtime.as_mut().unwrap();
                runtime.run_record_hash = Some(snapshot.to_hex());
                runtime.worker = current_record.worker.clone();
                runtime.ee = current_record.ee.clone();
                metadata.checkpoint_binding_policy = Some(CheckpointBindingPolicySnapshot {
                    inbound: checkpoint.interface_binding.clone().unwrap(),
                    outbound: checkpoint.outbound_interface_bindings.clone(),
                });
                metadata.outbound_interface_bindings = checkpoint.outbound_interfaces().cloned().collect();
                arrival.to = Endpoint::Interface(current_record.worker.clone());
                arrival.trace = checkpoint.trace.clone();
                arrival.invocation_metadata = Some(metadata.into());
                let mut journal = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&journal)
                    .unwrap();
                writeln!(journal, "{}", serde_json::to_string(&arrival).unwrap()).unwrap();
                journal.sync_all().unwrap();
                std::fs::write(&path, &content).unwrap();
                let result = InterfaceArtifactFile::write(blobs.join("results"), "file.write.result", &serde_json::to_vec(&serde_json::json!({
                    "label": "file.write.result", "tool": "file.write", "ok": true,
                    "correlation_id": call.correlation_id,
                    "body": {"result": "files_written", "writes": [{"path": path, "before_hash": call.arguments.expected_before_hash,
                        "after_hash": call.arguments.expected_after_hash, "bytes": content.len()}]}
                })).unwrap()).unwrap();
                let mut result_movement = ledger_row(call_seq + 1);
                result_movement.kind = MovementKind::InterfaceOutbound;
                result_movement.from = Endpoint::Interface(result_checkpoint.interface().clone());
                result_movement.to = Endpoint::Interface(InterfaceId::new("world_provider_test"));
                result_movement.artifact = Some(pin_artifact(&result, &blobs));
                let mut metadata = typed_invocation_metadata(
                    &RunId::new("run_test"),
                    next_for_host.authoring_result_checkpoint.as_str(),
                    &InterfaceId::new("world_provider_test"),
                    0,
                );
                metadata.runtime.as_mut().unwrap().run_record_hash = Some(next_for_host.after_hash);
                metadata.outbound_interface_bindings =
                    result_checkpoint.outbound_interfaces().cloned().collect();
                metadata.checkpoint_binding_policy =
                    Some(result_checkpoint.binding_policy_snapshot());
                result_movement.invocation_metadata = Some(metadata.into());
                writeln!(
                    journal,
                    "{}",
                    serde_json::to_string(&result_movement).unwrap()
                )
                .unwrap();
                journal.sync_all().unwrap();
                ws.send(WsMessage::Text(
                    serde_json::to_string(&DeliveryVerdict::Admitted { seq: call_seq })
                        .unwrap()
                        .into(),
                ))
                .await
                .unwrap();
                let _ = ws.next().await;
              }
            });
            let deliver = async {
                let output = arrival_rx.recv().await.unwrap();
                assert_eq!(
                    pin_artifact(&output.artifact, &blobs_dir),
                    next_identity.artifact
                );
                let movements = retained_pending_movements(&output, pending_movement(
                    call_seq + 2,
                    &next.pending_handoff.items[0],
                    &next_identity.artifact,
                ), &next.after_hash);
                let mut journal = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&movements_path)
                    .unwrap();
                for movement in movements { writeln!(journal, "{}", serde_json::to_string(&movement).unwrap()).unwrap(); }
                journal.sync_all().unwrap();
                output
                    .retained
                    .unwrap()
                    .reply.send(Ok(octopus_ledger::ArtifactDelivery::Complete { verdict: DeliveryVerdict::Admitted { seq: call_seq + 3 } }))
                    .unwrap();
            };
            let expected_recovery = if terminal {
                ProviderSourceRecovery::Completed
            } else {
                ProviderSourceRecovery::Interrupted {
                    last_artifact_label: next_identity.label.clone(),
                }
            };
            let recover = async {
                let recovered = handoff.recover_source_with_outcomes().await.unwrap();
                assert_eq!(
                    recovered.0, expected_recovery,
                    "persisted next generation {generation} was skipped"
                );
                Ok::<_, String>(recovered)
            };
            let (recovery, ()) = tokio::time::timeout(Duration::from_secs(3), async {
                tokio::join!(recover, deliver)
            })
            .await
            .unwrap();
            let (recovery, outcomes) = recovery.unwrap();
            assert_eq!(
                recovery,
                if terminal {
                    ProviderSourceRecovery::Completed
                } else {
                    ProviderSourceRecovery::Interrupted {
                        last_artifact_label: next_identity.label.clone(),
                    }
                }
            );
            assert_eq!(
                outcomes,
                vec![ProviderHandoffOutcome::Admitted {
                    seq: call_seq + 3,
                    artifact: next_identity.artifact.clone()
                }]
            );
            host.await.unwrap();
            std::fs::remove_file(&socket).unwrap();
            assert!(
                std::fs::read(&movements_path)
                    .unwrap()
                    .starts_with(&journal_prefix)
            );
            let state = handoff.state.lock().await;
            assert_eq!(state.run_record_bytes, publication.content.as_bytes());
            assert_eq!(state.context_artifacts, next_context);
            assert_eq!(
                state
                    .session_context
                    .artifacts
                    .last()
                    .unwrap()
                    .source_movement_seq,
                Some(call_seq + 3)
            );
            if !terminal {
                assert_eq!(
                    state.session_snapshot_head.as_ref().unwrap().artifact,
                    next_identity.artifact
                );
            }
            drop(state);
            let settled_journal = std::fs::read(&movements_path).unwrap();
            assert_eq!(handoff.recover_source().await.unwrap(), recovery);
            assert_eq!(std::fs::read(&movements_path).unwrap(), settled_journal);
            if terminal {
                let mut next_source = handoff.clone();
                next_source.source_movement_seq = (call_seq + 6).max(handoff.source_movement_seq + 1);
                let mut early_context = handoff.state.lock().await.clone();
                let selected = ArtifactId::new("admitted.read");
                early_context.static_context_artifacts.push(selected.clone());
                early_context.context_artifacts.push(selected.clone());
                assert_eq!(early_context.session_context.artifacts.pop().unwrap().source_artifact, next_identity.artifact);
                let published_record = RunRecord::from_bytes(&early_context.run_record_bytes).unwrap();
                let mut observation_record = published_record.clone();
                observation_record.interface_state.get_mut(&handoff.config.provider_interface).unwrap()
                    .remove("pending_handoff");
                let mut recovered_after_observation = early_context.clone();
                handoff.restore_published_context(&mut recovered_after_observation, &observation_record, &published_record, &BTreeMap::new()).unwrap();
                assert_eq!(recovered_after_observation.session_context.artifacts.last().unwrap().source_artifact, next_identity.artifact);
                let recovered_input = recovered_after_observation.session_context.artifacts.last().unwrap().clone();
                let fingerprint = session_artifacts_fingerprint(&recovered_after_observation.session_context.artifacts).unwrap();
                restore_consumed_context_inputs(&mut recovered_after_observation, vec![recovered_input.clone(), recovered_input.clone()]).unwrap();
                assert_eq!(session_artifacts_fingerprint(&recovered_after_observation.session_context.artifacts).unwrap(), fingerprint);
                let mut corrupt_input = recovered_input;
                corrupt_input.source_movement_seq = Some(u64::MAX);
                assert!(restore_consumed_context_inputs(&mut recovered_after_observation, vec![corrupt_input]).is_err());
                assert!(unadmitted.restore_published_context(&mut early_context.clone(), &observation_record, &published_record, &BTreeMap::new()).is_err());
                next_source.state = Arc::new(tokio::sync::Mutex::new(early_context));
                assert_eq!(next_source.recover_source().await.unwrap(), ProviderSourceRecovery::Process);
                assert_eq!(next_source.current_session_context_artifacts().await.last().unwrap().source_artifact, next_identity.artifact);
                assert!(!next_source.current_session_context_artifacts().await.iter().any(|entry| entry.source_artifact.id == selected));
                assert_eq!(std::fs::read(&movements_path).unwrap(), settled_journal);
                let later = next_source.state.lock().await.clone();
                let next_attempt = next_source.author_run(&later.run_record_bytes, &later.context_artifacts,
                    &[PreparedHandoff { identity: next_identity.clone(), output_checkpoint: handoff.config
                        .checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput).unwrap().clone(), verified_inputs: vec![] }],
                    true, None, false, true).unwrap();
                assert!(current_context_artifacts(&RunRecord::from_bytes(next_attempt.content.as_bytes()).unwrap(), &next_source.config).unwrap().contains(&selected));
                let private = next_source.materialize_file_write_call(&next_attempt).unwrap();
                let private_bytes = std::fs::read(private.manifest_path()).unwrap();
                assert_eq!(handoff.recover_source().await.unwrap(), ProviderSourceRecovery::Completed);
                assert_eq!(std::fs::read(private.manifest_path()).unwrap(), private_bytes);
                assert!(matches!(next_source.recover_authoring_attempt(&RunRecord::from_bytes(&later.run_record_bytes).unwrap(),
                    &later.run_record_bytes).unwrap(), AuthoringAttemptRecovery::InFlight(_)));
                assert_eq!(std::fs::read(&movements_path).unwrap(), settled_journal);
            }
            assert!(
                wrong_source
                    .recover_source()
                    .await
                    .err()
                    .unwrap()
                    .contains("private attempt belongs to source movement")
            );
            assert!(
                unadmitted
                    .recover_source()
                    .await
                    .err()
                    .unwrap()
                    .contains("without admitted ledger evidence")
            );
            if !terminal {
                let mut state = handoff.state.lock().await;
                state.session_context.artifacts.clear();
                state.session_snapshot_head = None;
                drop(state);
                assert_eq!(handoff.recover_source().await.unwrap(), recovery);
                assert_eq!(std::fs::read(&movements_path).unwrap(), settled_journal);
            }
        }
    }

#[test]
#[ignore = "requires explicitly selected read-only ledger performance evidence"]
fn selected_retained_delivery_cost() {
    let home = PathBuf::from(std::env::var("OCTOPUS_BENCH_HOME").unwrap());
    let interface = InterfaceId::new(std::env::var("OCTOPUS_BENCH_INTERFACE").unwrap());
    let path = home.join("ledger/movements.jsonl");
    let started = std::time::Instant::now();
    let count = if std::env::var_os("OCTOPUS_BENCH_FULL_LEDGER").is_some() {
        let ledger = ledger_rows::LedgerRows::detached(&path);
        let mut index = octopus_ledger_index::ArrivalDedupIndex::new();
        let mut reader = std::io::BufReader::new(std::fs::File::open(&path).unwrap());
        loop {
            match read_bounded_ledger_row(&mut reader, 16_777_216).unwrap() {
                BoundedLedgerRow::Complete(row) => {
                    let movement = ledger.decode_with_grant_if(&row, index.watermark(), |movement| {
                        movement.kind == MovementKind::InterfaceArrival && movement.invocation_metadata
                            .as_ref().and_then(|metadata| metadata.invocation())
                            .is_some_and(|metadata| !metadata.required_verified_artifacts.is_empty())
                    }).unwrap();
                    index.catch_up_movements([movement]).unwrap();
                }
                BoundedLedgerRow::Eof => break,
                BoundedLedgerRow::Partial => panic!("incomplete ledger"),
            }
        }
        index.watermark()
    } else {
        pending_delivery::Ledger::new(interface, path, 16_777_216).watermark().unwrap()
    };
    println!("retained delivery: {count} rows in {} ms", started.elapsed().as_millis());
}
