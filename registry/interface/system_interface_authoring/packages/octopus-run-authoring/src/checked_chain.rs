use super::*;

pub struct CheckedRunEvidence<'a> {
    pub movements_path: &'a Path,
    pub ledger_index_path: &'a Path,
    pub blobs_dir: &'a Path,
    pub max_control_frame_bytes: u64,
    pub max_manifest_bytes: u64,
    pub max_payload_bytes: u64,
}

pub trait CheckedRunEvidenceReader {
    fn max_payload_bytes(&self) -> u64;
    fn source_context_snapshot(&mut self, _record: &RunRecord, _config: &RunAuthoringConfig, _source: u64) -> Result<(serde_json::Value, Vec<(ArtifactRef, u64)>), String> {
        Err("checked reader does not provide exact source context evidence".into())
    }
    fn pending_output_payload(&mut self, provider: &InterfaceId, reference: &ArtifactRef, relative: &Path) -> Result<Vec<u8>, String> {
        let _ = (provider, relative);
        self.artifact(reference).map(|(_, payload)| payload)
    }
    fn admitted_movement(&mut self, seq: u64) -> Result<Option<Movement>, String> { self.movement(seq) }
    fn visit_admitted_prefix(&mut self, before_seq: u64, visitor: &mut dyn FnMut(&Movement) -> Result<(), String>) -> Result<(), String> {
        for sequence in 0..before_seq {
            let row = self.admitted_movement(sequence)?.ok_or("checked evidence prefix is incomplete")?;
            if row.seq.0 != sequence { return Err("checked evidence prefix has a sequence gap".into()); }
            visitor(&row)?;
        }
        Ok(())
    }
    fn movement(&mut self, seq: u64) -> Result<Option<Movement>, String>;
    fn artifact(&mut self, reference: &ArtifactRef) -> Result<(InterfaceArtifactManifest, Vec<u8>), String>;
    fn snapshot(&mut self, hash: &ContentHash) -> Result<Vec<u8>, String>;
    fn concurrent_writer_start(&mut self, path: &Path, record: &RunRecord, before: &[u8], after_seq: u64, writers: &[CheckedRunWriter]) -> Result<u64, String> {
        let hash = ContentHash::of_bytes(before).to_hex();
        let mut first = None;
        let mut last_segment = None;
        validate_checked_run_writers(writers)?;
        for sequence in 0..=after_seq {
            let row = self.admitted_movement(sequence)?.ok_or("checked writer evidence interval is incomplete")?;
            if row.kind != MovementKind::InterfaceOutbound { continue; }
            let Some(runtime) = row.runtime().filter(|runtime| runtime.run == record.id) else { continue; };
            if runtime.run_record_hash.as_deref() == Some(hash.as_str()) {
                first.get_or_insert(sequence); last_segment = first;
            } else { first = None; }
        }
        let observed = last_segment.map_or(after_seq, |sequence| sequence.saturating_sub(1));
        for sequence in 0..=observed {
            let row = self.admitted_movement(sequence)?.ok_or("checked writer evidence interval is incomplete")?;
            if detached_writer_starts_at(self, ledger_rows::OutboundRow::of(&row).as_ref(), path, record, before, &hash, writers)? {
                return sequence.checked_sub(1).ok_or("observed writer call has no earlier evidence boundary".into());
            }
        }
        Ok(observed)
    }
    fn input_rearm_ready(&mut self, record: &RunRecord, provider: &InterfaceId, before_seq: u64) -> Result<bool, String> {
        input_revision::reader_readiness(self, record, provider, before_seq)
    }
    fn input_payloads_ready(&mut self, path: &Path, current: &[u8], writer: &CheckedRunWriter, source: &InterfaceId, before_seq: u64) -> Result<bool, String> {
        input_revision::reader_payload_readiness(self, path, current, writer, source, before_seq)
    }
}

pub struct FileCheckedRunReader<'a> {
    pub(crate) evidence: &'a CheckedRunEvidence<'a>,
    pub(crate) rows: Arc<ledger_rows::LedgerRows>,
    pub(crate) ledger: ledger_rows::LedgerFile,
    pub(crate) reader: std::io::BufReader<std::fs::File>,
    pub(crate) next_seq: u64,
    pub(crate) snapshot_bytes: u64,
    pub(crate) provider_context: Option<InterfaceLiftContext>,
}

impl<'a> FileCheckedRunReader<'a> {
    pub fn open(evidence: &'a CheckedRunEvidence<'a>, after_seq: Option<u64>) -> Result<Self, String> {
        if [evidence.max_control_frame_bytes,evidence.max_manifest_bytes,evidence.max_payload_bytes].contains(&0)
            || [&evidence.movements_path,&evidence.ledger_index_path,&evidence.blobs_dir].iter().any(|path| !path.is_absolute()) {
            return Err("checked authoring requires explicit bounded owner evidence paths".into());
        }
        let rows = ledger_rows::LedgerRows::shared(evidence.movements_path);
        let file = std::fs::File::open(&evidence.movements_path).map_err(|e| e.to_string())?;
        let ledger = ledger_rows::LedgerFile::of(&file)?;
        let mut reader = Self { evidence, rows, ledger, reader: std::io::BufReader::new(file), next_seq: 0, snapshot_bytes: 0, provider_context: None };
        if let Some(after_seq) = after_seq {
            if !reader.resume_indexed(after_seq)? && !reader.resume_spotlight(after_seq)? {
                reader.project_through(after_seq)?;
            }
        }
        Ok(reader)
    }

    fn resume_spotlight(&mut self, after_seq: u64) -> Result<bool, String> {
        let Ok(index) = LedgerSqliteReader::open(&self.evidence.ledger_index_path) else { return Ok(false); };
        let Ok(Some(range)) = index.movement_row_range(after_seq) else { return Ok(false); };
        let rows = self.rows.clone();
        self.resume(after_seq, range.start_offset, range.end_offset,
            |row| rows.decoder.decode(row, after_seq as usize).is_ok_and(|movement| movement.seq.0 == after_seq))
    }

    pub(crate) fn project_through(&mut self, last: u64) -> Result<(), String> {
        let known = self.rows.known(self.ledger)?.min(last.checked_add(1).ok_or("checked authoring sequence overflow")?);
        if let Some(resume) = known.checked_sub(1).filter(|resume| self.next_seq <= *resume) {
            self.resume_indexed(resume)?;
        }
        while self.next_seq <= last {
            let seq = self.next_seq;
            let start = self.reader.stream_position().map_err(|e| e.to_string())?;
            let row = match read_bounded_ledger_row(&mut self.reader, self.evidence.max_control_frame_bytes)? {
                BoundedLedgerRow::Complete(row) => row,
                BoundedLedgerRow::Partial | BoundedLedgerRow::Eof => {
                    self.reader.seek(std::io::SeekFrom::Start(start)).map_err(|e| e.to_string())?;
                    return Ok(());
                }
            };
            let (movement, _) = self.rows.decoder.decode_projection(&row, seq as usize).map_err(|e| e.to_string())?;
            if movement.seq.0 != seq { return Err("checked authoring evidence ledger has a sequence gap".into()); }
            self.note(seq, start, &row, &movement)?;
            self.next_seq = seq.checked_add(1).ok_or("checked authoring sequence overflow")?;
        }
        Ok(())
    }

    fn note(&self, seq: u64, start: u64, row: &[u8], movement: &Movement) -> Result<(), String> {
        self.rows.note(self.ledger, seq, ledger_rows::LedgerRow {
            start, end: start.checked_add(row.len() as u64).ok_or("checked authoring offset overflow")?,
            digest: ContentHash::of_bytes(row), outbound: ledger_rows::OutboundRow::of(movement),
        })
    }

    pub(crate) fn resume_indexed(&mut self, seq: u64) -> Result<bool, String> {
        let Some(row) = self.rows.row(self.ledger, seq)? else { return Ok(false); };
        if self.resume(seq, row.start, row.end, |bytes| ContentHash::of_bytes(bytes) == row.digest)? { return Ok(true); }
        self.rows.forget()?;
        Ok(false)
    }

    fn resume(&mut self, seq: u64, start: u64, end: u64, proves: impl FnOnce(&[u8]) -> bool) -> Result<bool, String> {
        let length = self.reader.get_ref().metadata().map_err(|e| e.to_string())?.len();
        if end > length || start >= end || end - start > self.evidence.max_control_frame_bytes { return Ok(false); }
        self.reader.seek(std::io::SeekFrom::Start(start)).map_err(|e| e.to_string())?;
        let valid = match read_bounded_ledger_row(&mut self.reader, self.evidence.max_control_frame_bytes)? {
            BoundedLedgerRow::Complete(row) => proves(&row),
            _ => false,
        };
        if valid && self.reader.stream_position().map_err(|e| e.to_string())? == end {
            self.next_seq = seq.checked_add(1).ok_or("checked authoring sequence overflow")?;
            return Ok(true);
        }
        self.reader.seek(std::io::SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        self.next_seq = 0;
        Ok(false)
    }

    pub(crate) fn with_provider_context(mut self, ctx: &InterfaceLiftContext) -> Self { self.provider_context = Some(ctx.clone()); self }

    pub fn next_sequence(&self) -> u64 { self.next_seq }

    pub(crate) fn indexed_outbound_edges(
        &mut self, edges: &[(&InterfaceId, &InterfaceId)],
    ) -> Result<Option<std::collections::VecDeque<octopus_ledger_index::IndexedMovementRowRange>>, String> {
        let start = self.reader.stream_position().map_err(|error| error.to_string())?;
        let selected = (|| {
            let index = LedgerSqliteReader::open(self.evidence.ledger_index_path).ok()?;
            let progress = index.progress().ok()?;
            if progress.watermark <= self.next_seq || progress.durable_end_offset < start
                || progress.durable_end_offset > self.reader.get_ref().metadata().ok()?.len() { return None; }
            let tail = index.movement_row_range(progress.watermark - 1).ok()??;
            if tail.end_offset != progress.durable_end_offset { return None; }
            let mut selected = Vec::new();
            for (from, to) in edges {
                selected.extend(index.outbound_rows_on_edge(from.as_str(), to.as_str(), self.next_seq, progress.watermark).ok()?);
            }
            selected.push(tail);
            selected.sort_by_key(|row| row.seq);
            selected.dedup();
            let mut previous_end = start;
            let mut previous_seq = None;
            for row in &selected {
                if row.seq < self.next_seq || row.seq >= progress.watermark
                    || previous_seq.is_some_and(|seq| row.seq <= seq)
                    || row.start_offset < previous_end || row.end_offset > progress.durable_end_offset
                    || row.end_offset <= row.start_offset
                    || row.end_offset - row.start_offset > self.evidence.max_control_frame_bytes { return None; }
                self.reader.seek(std::io::SeekFrom::Start(row.start_offset)).ok()?;
                let BoundedLedgerRow::Complete(bytes) = read_bounded_ledger_row(&mut self.reader, self.evidence.max_control_frame_bytes).ok()? else { return None; };
                if self.reader.stream_position().ok()? != row.end_offset { return None; }
                let (movement, _) = self.rows.decoder.decode_projection(&bytes, usize::try_from(row.seq).ok()?).ok()?;
                if movement.seq.0 != row.seq { return None; }
                if row.seq != progress.watermark - 1 && (movement.kind != MovementKind::InterfaceOutbound
                    || !edges.iter().any(|(from, to)| movement.from == Endpoint::Interface((*from).clone()) && movement.to == Endpoint::Interface((*to).clone()))) { return None; }
                previous_end = row.end_offset;
                previous_seq = Some(row.seq);
            }
            Some(selected.into())
        })();
        self.reader.seek(std::io::SeekFrom::Start(start)).map_err(|error| error.to_string())?;
        Ok(selected)
    }

    pub(crate) fn movement_with_grant_if(
        &mut self, seq: u64, needs_grant: impl FnOnce(&Movement) -> bool,
    ) -> Result<Option<Movement>, String> {
        if seq != self.next_seq { return Err("checked authoring reader lost its exact cursor".into()); }
        let start = self.reader.stream_position().map_err(|e| e.to_string())?;
        let row = match read_bounded_ledger_row(&mut self.reader, self.evidence.max_control_frame_bytes)? {
            BoundedLedgerRow::Complete(row) => row,
            BoundedLedgerRow::Partial | BoundedLedgerRow::Eof => return Ok(None),
        };
        let movement = self.rows.decode_with_grant_if(&row, seq, needs_grant)?;
        if movement.seq.0 != seq { return Err("checked authoring evidence ledger has a sequence gap".into()); }
        self.note(seq, start, &row, &movement)?;
        self.next_seq = seq.checked_add(1).ok_or("checked authoring sequence overflow")?;
        Ok(Some(movement))
    }
}

impl CheckedRunEvidenceReader for FileCheckedRunReader<'_> {
    fn max_payload_bytes(&self) -> u64 { self.evidence.max_payload_bytes }
    fn source_context_snapshot(&mut self, record: &RunRecord, config: &RunAuthoringConfig, source: u64) -> Result<(serde_json::Value, Vec<(ArtifactRef, u64)>), String> {
        source_withdrawal::source_snapshot_from_evidence(self.evidence, self.provider_context.as_ref(), record, config, source)
    }
    fn pending_output_payload(&mut self, provider: &InterfaceId, reference: &ArtifactRef, relative: &Path) -> Result<Vec<u8>, String> {
        if self.evidence.blobs_dir.join(reference.content.to_hex()).try_exists().map_err(|error| error.to_string())? {
            return self.artifact(reference).map(|(_, payload)| payload);
        }
        if let Some(ctx) = self.provider_context.as_ref().filter(|ctx| ctx.interface == *provider) {
            if relative.as_os_str().is_empty() || relative.is_absolute() || relative.components().any(|component| !matches!(component, std::path::Component::Normal(_))) {
                return Err("withdrawal closure has an unsafe private artifact path".into());
            }
            let file = InterfaceArtifactFile::new(ctx.interface_var_dir().join(relative)).map_err(|error| error.to_string())?;
            if artifact_identity(&file, &ctx.interface_var_dir())?.artifact != *reference { return Err("withdrawal closure differs from its exact private artifact".into()); }
            return file.read_payload_bytes().map_err(|error| error.to_string());
        }
        self.artifact(reference).map(|(_, payload)| payload)
    }
    fn concurrent_writer_start(&mut self, path: &Path, record: &RunRecord, before: &[u8], after_seq: u64, writers: &[CheckedRunWriter]) -> Result<u64, String> {
        concurrent_writer_evidence_start(self.evidence, path, record, before, after_seq, writers)
    }
    fn input_rearm_ready(&mut self, record: &RunRecord, provider: &InterfaceId, before_seq: u64) -> Result<bool, String> {
        input_revision::readiness(self.evidence, record, provider, before_seq)
    }
    fn input_payloads_ready(&mut self, path: &Path, current: &[u8], writer: &CheckedRunWriter, source: &InterfaceId, before_seq: u64) -> Result<bool, String> {
        input_revision::payload_readiness(self.evidence, path, current, writer, source, before_seq)
    }
    fn admitted_movement(&mut self, seq: u64) -> Result<Option<Movement>, String> {
        generated_artifacts::read_movement(self.evidence,seq).map(Some)
    }
    fn visit_admitted_prefix(&mut self, before_seq: u64, visitor: &mut dyn FnMut(&Movement) -> Result<(), String>) -> Result<(), String> {
        let mut prefix = FileCheckedRunReader::open(self.evidence, None)?;
        for sequence in 0..before_seq {
            let row = prefix.movement(sequence)?.ok_or("checked evidence prefix is incomplete")?;
            visitor(&row)?;
        }
        Ok(())
    }
    fn movement(&mut self, seq: u64) -> Result<Option<Movement>, String> {
        self.movement_with_grant_if(seq, |_| true)
    }
    fn artifact(&mut self, reference: &ArtifactRef) -> Result<(InterfaceArtifactManifest, Vec<u8>), String> {
        read_pinned_artifact_with_bounds(self.evidence.blobs_dir, self.evidence.max_manifest_bytes,
            self.evidence.max_payload_bytes, reference)
    }
    fn snapshot(&mut self, hash: &ContentHash) -> Result<Vec<u8>, String> {
        let snapshot = octopus_interfaces::read_artifact_carrier_file_bounded(
            &self.evidence.blobs_dir.join(hash.to_hex()), self.evidence.max_payload_bytes).map_err(|e| e.to_string())?;
        self.snapshot_bytes = self.snapshot_bytes.checked_add(snapshot.len() as u64).ok_or("checked authoring evidence byte overflow")?;
        let limit = self.evidence.max_payload_bytes.checked_mul(4).ok_or("checked authoring byte budget overflow")?;
        if self.snapshot_bytes > limit { return Err("checked authoring chain exceeds its bounded snapshot budget".into()); }
        Ok(snapshot)
    }
}

pub fn checked_run_file_chain(
    evidence: &CheckedRunEvidence<'_>, path: &Path, before: &[u8], after: &[u8],
    after_seq: u64, writers: &[CheckedRunWriter],
) -> Result<Vec<CheckedRunTransition>, String> {
    checked_run_file_chain_state(evidence, path, before, after, after_seq, writers).map_err(Into::into)
}

pub(crate) fn checked_run_file_chain_state(
    evidence: &CheckedRunEvidence<'_>, path: &Path, before: &[u8], after: &[u8],
    after_seq: u64, writers: &[CheckedRunWriter],
) -> Result<Vec<CheckedRunTransition>, CheckedRunError> {
    if ContentHash::of_bytes(before) == ContentHash::of_bytes(after) { return Ok(Vec::new()); }
    let mut reader = FileCheckedRunReader::open(evidence, Some(after_seq))?;
    let first = reader.next_seq;
    let transitions = checked_run_file_chain_with_reader(&mut reader, path, before, CheckedRunTarget::Snapshot(after), after_seq, first, None, writers, &[], None, None)?;
    Ok(transitions)
}

pub fn checked_run_file_chain_before(
    reader: &mut impl CheckedRunEvidenceReader, path: &Path, before: &[u8], after: &[u8],
    after_seq: u64, before_seq: u64, writers: &[CheckedRunWriter],
) -> Result<Vec<CheckedRunTransition>, String> {
    let first = after_seq.checked_add(1).ok_or("checked historical sequence overflow")?;
    if first > before_seq { return Err("checked historical interval is reversed".into()); }
    checked_run_file_chain_with_reader(reader, path, before, CheckedRunTarget::Snapshot(after), after_seq, first, Some(before_seq), writers, &[], None, None).map_err(Into::into)
}

pub(crate) fn validate_checked_run_writers(writers: &[CheckedRunWriter]) -> Result<(), String> {
    let mut unique = BTreeSet::new();
    let mut returns = BTreeSet::new();
    for writer in writers {
        if writer.owner == writer.door || !unique.insert((&writer.source, &writer.door)) || !returns.insert((&writer.door,&writer.owner)) {
            return Err("checked authoring chain has ambiguous writer identities".into());
        }
        for value in [&writer.tool, &writer.call_label, &writer.result_label] { non_empty_string(value, "checked run writer")?; }
    }
    Ok(())
}

pub(crate) enum CheckedRunTarget<'a> {
    Snapshot(&'a [u8]),
    Observed { hash: ContentHash, sequence: u64 },
}

pub(crate) fn checked_run_file_chain_with_reader(
    reader: &mut impl CheckedRunEvidenceReader, path: &Path, before: &[u8], target: CheckedRunTarget<'_>,
    after_seq: u64, first_seq: u64, before_seq: Option<u64>, writers: &[CheckedRunWriter],
    selected_reads: &[ArtifactRef],
    connection_config: Option<&RunAuthoringConfig>,
    selected_install: Option<&CheckedRunInstall>,
) -> Result<Vec<CheckedRunTransition>, CheckedRunError> {
    if connection_config.is_some() && (before_seq.is_some() || !selected_reads.is_empty() || matches!(target, CheckedRunTarget::Observed { .. })) {
        return Err("observed connections cannot bridge historical or selected-read evidence".into());
    }
    let start = ContentHash::of_bytes(before).to_hex();
    let (end, after, observed_sequence) = match target {
        CheckedRunTarget::Snapshot(after) => (ContentHash::of_bytes(after).to_hex(), Some(after), None),
        CheckedRunTarget::Observed { hash, sequence } => (hash.to_hex(), None, Some(sequence)),
    };
    let original = RunRecord::from_bytes(before).map_err(|e| e.to_string())?;
    if let Some(after) = after {
        let target = RunRecord::from_bytes(after).map_err(|e| e.to_string())?;
        if original.id != target.id || original.worker != target.worker {
            return Err("checked authoring chain changed run or worker".into());
        }
    }
    validate_checked_run_writers(writers)?;
    struct Candidate {
        writer: CheckedRunWriter,
        call: Movement,
        arguments: FileWriteCallArguments,
        ordered: Option<writer_run::OrderedArguments>,
        before: Vec<u8>,
    }
    let mut calls: BTreeMap<(InterfaceId, String), Candidate> = BTreeMap::new();
    let mut transitions = Vec::new();
    let mut uncertain = Vec::new();
    let mut observations = Vec::new();
    let mut placement_candidates = Vec::new();
    let mut sequence = first_seq;
    loop {
        if before_seq.is_some_and(|end| sequence >= end) { break; }
        let Some(movement) = reader.movement(sequence)? else {
            if before_seq.is_some() { return Err("checked historical interval has no complete durable prefix".into()); }
            break;
        };
        if movement.seq.0 != sequence { return Err("checked authoring evidence ledger has a sequence gap".into()); }
        sequence = sequence.checked_add(1).ok_or("checked authoring evidence sequence overflow")?;
        if movement.seq.0 <= after_seq || movement.kind != MovementKind::InterfaceOutbound { continue; }
        if selected_install.is_some_and(|install| movement.seq.0 != install.call.movement_seq && movement.seq.0 != install.result.movement_seq) { continue; }
        if before_seq.is_some() && selected_install.is_none() {
            let metadata = movement.invocation_metadata.as_ref().and_then(|m| m.invocation())
                .ok_or("checked historical outbound has no actual admission metadata")?;
            let runtime = metadata.runtime.as_ref().ok_or("checked historical outbound has no run coordinate")?;
            if runtime.run == original.id {
                if runtime.worker != original.worker || metadata.verification_failure.is_some() {
                    return Err("checked historical interval changed run worker or admitted failed evidence".into());
                }
                let hash = runtime.run_record_hash.as_deref().and_then(ContentHash::from_hex)
                    .ok_or("checked historical outbound has no pinned snapshot")?;
                observations.push((movement.seq.0, hash, (!selected_reads.is_empty()).then(|| metadata.clone())));
            }
        }
        let source_writer = writers.iter().find(|w| movement.from == Endpoint::Interface(w.source.clone()) && movement.to == Endpoint::Interface(w.door.clone()));
        let result_writer = writers.iter().find(|w| movement.from == Endpoint::Interface(w.door.clone()) && movement.to == Endpoint::Interface(w.owner.clone()));
        if source_writer.is_none() && result_writer.is_none() {
            if selected_install.is_none() && movement.artifact.is_some() {
                placement_candidates.push(movement);
            }
            continue;
        }
        if source_writer.is_some() && observed_sequence.is_some_and(|sequence| movement.seq.0 >= sequence) { continue; }
        let metadata = movement.invocation_metadata.as_ref().and_then(|m| m.invocation()).ok_or("checked authoring evidence has no invocation metadata")?;
        let runtime = metadata.runtime.as_ref().ok_or("checked authoring evidence has no runtime coordinate")?;
        if runtime.worker != original.worker || metadata.verification_failure.is_some() {
            return Err("checked authoring evidence has another worker or a verification failure".into());
        }
        let artifact = movement.artifact.as_ref().ok_or("checked authoring movement has no artifact")?;
        let (manifest, payload) = reader.artifact(artifact)?;
        let snapshot_hash = runtime.run_record_hash.as_deref().and_then(ContentHash::from_hex).ok_or("checked authoring evidence has no pinned snapshot")?;
        let snapshot = reader.snapshot(&snapshot_hash)?;
        if ContentHash::of_bytes(&snapshot) != snapshot_hash { return Err("checked authoring snapshot failed integrity".into()); }
        let snapshot_record = RunRecord::from_bytes(&snapshot).map_err(|e| e.to_string())?;
        if snapshot_record.id != runtime.run || snapshot_record.ee != runtime.ee || snapshot_record.worker != original.worker {
            return Err("checked authoring snapshot differs from its movement scope".into());
        }
        if let Some(writer) = source_writer {
            if manifest.label != writer.call_label { continue; }
            let call: octopus_tool_handlers::ToolCallArtifact = serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
            if call.tool != writer.tool { continue; }
            if call.label.as_deref().is_some_and(|label| label != writer.call_label) { return Err("checked run call label differs from its admitted manifest".into()); }
            let (arguments, ordered, before) = if call.arguments.get("ordered").is_some() {
                let ordered: writer_run::OrderedArguments = serde_json::from_value(call.arguments).map_err(|e|e.to_string())?;
                if !ordered.targets(path) { continue; }
                let (before, arguments) = ordered.checked_call(writer,&snapshot,path,reader.max_payload_bytes())?;
                (arguments,Some(ordered),before)
            } else {
                if runtime.run != original.id { continue; }
                let arguments: FileWriteCallArguments = serde_json::from_value(call.arguments).map_err(|e|e.to_string())?;
                if arguments.path != path { continue; }
                (arguments,None,snapshot)
            };
            if runtime.interface != writer.door
                || !metadata.grant.tools.contains(&writer.tool) || metadata.grant.hard_blocked_tools.contains(&writer.tool)
                || !metadata.grant.interfaces.contains(&writer.door.to_string()) {
                return Err("checked run write was not admitted under its exact writer and before snapshot".into());
            }
            let correlation = call.correlation_id.ok_or("checked run write has no immutable correlation")?;
            if calls.len() >= 1024 || calls.insert((writer.owner.clone(), correlation), Candidate { writer: writer.clone(), call: movement, arguments, ordered, before }).is_some() {
                return Err("checked authoring calls exceed their bound or repeat a correlation".into());
            }
        } else if let Some(writer) = result_writer {
            if manifest.label != writer.result_label { continue; }
            let result: ToolResultArtifact = serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
            if result.tool != writer.tool || result.label != writer.result_label { continue; }
            let correlation = result.correlation_id.clone().ok_or("checked run result has no immutable correlation")?;
            let Some(candidate) = calls.remove(&(writer.owner.clone(), correlation)) else { continue; };
            if candidate.writer != *writer { return Err("checked run result came from a different declared writer".into()); }
            if let Some(ordered) = &candidate.ordered {
                let effect = ordered.checked_result(writer,metadata,&snapshot,&result,reader.max_payload_bytes(),candidate.call.seq.0)?;
                if effect == writer_run::OrderedTargetEffect::Refused { continue; }
                let prospective = apply_run_json_patch(&candidate.before,&candidate.arguments.patch,&candidate.arguments.expected_after_hash, candidate.arguments.json_format)?.into_bytes();
                let transition = CheckedRunTransition {writer:candidate.writer,before:candidate.before,after:prospective,call:candidate.call,result:movement,placement:false};
                if matches!(effect, writer_run::OrderedTargetEffect::Uncertain | writer_run::OrderedTargetEffect::UncertainWriter) {
                    if uncertain.len() >= 1024 { return Err("checked uncertain transitions exceed their evidence bound".into()); }
                    uncertain.push(transition);
                    continue;
                }
                generated_artifacts::validate_generated_transition(reader,&transition)?;
                input_artifacts::validate_input_transition(reader, &transition)?;
                validate_verified_input_transition(reader,&transition)?;
                input_revision::validate_rearm_evidence(reader, &transition, path)?;
                source_withdrawal::validate_evidence(reader, &transition)?;
                rejected_final::validate_evidence(reader, &transition)?;
                if transitions.len() >= 1024 { return Err("checked authoring transitions exceed their evidence bound".into()); }
                transitions.push(transition);
                continue;
            }
            if runtime.run != original.id { return Err("same-run authoring result changed its execution scope".into()); }
            if !result.ok || result.error.is_some() { continue; }
            if candidate.arguments.expected_before_hash != ContentHash::of_bytes(&candidate.before).to_hex() {
                return Err("successful checked run write was not admitted under its exact before snapshot".into());
            }
            let body: FileWriteResultBody = serde_json::from_value(result.body.ok_or("checked run result has no body")?).map_err(|e| e.to_string())?;
            let [evidence] = body.writes.as_slice() else { return Err("checked run result must prove one exact file".into()); };
            let prospective = apply_run_json_patch(&candidate.before, &candidate.arguments.patch,
                &candidate.arguments.expected_after_hash, candidate.arguments.json_format)?.into_bytes();
            validate_unchanged_admitted_checkpoint(&prospective, &snapshot, &runtime.checkpoint)?;
            let checkpoint = snapshot_record.checkpoint(&runtime.checkpoint).ok_or("checked run result lost its admitted checkpoint")?;
            let binding = CheckpointBindingPolicySnapshot { inbound: checkpoint.interface_binding.clone().ok_or("checked run result has no inbound binding")?, outbound: checkpoint.outbound_interface_bindings.clone() };
            if body.result != "files_written" || evidence.path != path || evidence.before_hash.as_deref() != Some(candidate.arguments.expected_before_hash.as_str())
                || evidence.after_hash != candidate.arguments.expected_after_hash || evidence.bytes != prospective.len() as u64
                || runtime.interface != writer.owner || runtime.epoch != checkpoint.epoch.0
                || binding.inbound.interface != writer.door || binding.outbound.len() != 1 || binding.outbound[0].interface != writer.owner
                || metadata.checkpoint_binding_policy.as_ref() != Some(&binding) || metadata.outbound_interface_bindings != [writer.owner.clone()]
                || !metadata.required_verified_artifacts.is_empty() || !checkpoint.required_verified_artifacts.is_empty()
                || !metadata.checkpoint_admission.as_ref().is_some_and(|a| a.complete && a.artifact_label == manifest.label && a.required_artifact_labels.is_empty())
                || !snapshot_record.active_checkpoints.contains(&checkpoint.id) || candidate.call.seq >= movement.seq {
                return Err("checked run result differs from its exact admitted write and prospective snapshot".into());
            }
            let transition = CheckedRunTransition { writer: candidate.writer, before: candidate.before, after: prospective, call: candidate.call, result: movement, placement: false };
            if transitions.len() >= 1024 { return Err("checked authoring transitions exceed their evidence bound".into()); }
            generated_artifacts::validate_generated_transition(reader,&transition)?;
            input_artifacts::validate_input_transition(reader, &transition)?;
            validate_verified_input_transition(reader, &transition)?;
            input_revision::validate_rearm_evidence(reader, &transition, path)?;
            source_withdrawal::validate_evidence(reader, &transition)?;
            rejected_final::validate_evidence(reader, &transition)?;
            transitions.push(transition);
        }
    }

    if before_seq.is_some() && !calls.is_empty() {
        return Err("checked historical interval ends during an unjoined authoring call".into());
    }
    let mut by_before = BTreeMap::new();
    for transition in transitions {
        if by_before.insert(ContentHash::of_bytes(&transition.before).to_hex(), transition).is_some() {
            return Err("checked authoring transitions branch or exceed their evidence bound".into());
        }
    }
    let mut unproven = BTreeSet::new();
    for transition in uncertain {
        let before = ContentHash::of_bytes(&transition.before).to_hex();
        if by_before.contains_key(&before) { continue; }
        if !unproven.insert(before.clone()) {
            return Err("checked uncertain transitions branch from one snapshot".into());
        }
        by_before.insert(before, transition);
    }
    let mut hash = start.clone();
    let mut current = before.to_vec();
    let mut chain = Vec::new();
    let mut previous_call = after_seq;
    let mut connection_versions = BTreeMap::new();
    let mut placement_edits: Option<Vec<placement::ProvenWrite>> = None;
    while hash != end {
        if !by_before.contains_key(&hash) && !placement_candidates.is_empty() {
            let edits = match &mut placement_edits {
                Some(edits) => edits,
                None => placement_edits.insert(placement::proven_writes(reader, path, &original.id, &placement_candidates)?),
            };
            if let Some(transition) = placement::transition_from(edits, &current, previous_call)? {
                by_before.insert(hash.clone(), transition);
            }
        }
        let Some(transition) = by_before.remove(&hash) else {
            if let Some(config) = connection_config {
                // These are validated endpoint bytes, never synthetic writer
                // receipts. Only routing may differ between adjacent segments.
                let mut successors = BTreeMap::new();
                for bytes in by_before.values().filter(|next| next.call.seq.0 > previous_call).map(|next| next.before.as_slice())
                    .chain(calls.values().filter(|next| next.call.seq.0 > previous_call).map(|next| next.before.as_slice()))
                    .chain(after.into_iter()) {
                    if output_connections::protected_record(&current, bytes, config)?
                        .is_some_and(|protected| protected == current) {
                        successors.insert(ContentHash::of_bytes(bytes).to_hex(), bytes);
                    }
                }
                if successors.len() > 1 {
                    return Err("observed outgoing connections have ambiguous checked successors".into());
                }
                if let Some((next, bytes)) = successors.into_iter().next() {
                    if connection_versions.insert(next.clone(), chain.len()).is_some() {
                        return Err("observed outgoing connections cycle through a snapshot".into());
                    }
                    hash = next;
                    current = bytes.to_vec();
                    continue;
                }
            }
            let mut reachable = BTreeSet::from([hash.clone()]);
            let mut pending_sequences = BTreeSet::new();
            loop {
                let count = reachable.len();
                for candidate in calls.values() {
                    if reachable.contains(&candidate.arguments.expected_before_hash)
                        && candidate.arguments.expected_before_hash == ContentHash::of_bytes(&candidate.before).to_hex()
                        && candidate.call.seq.0 > previous_call
                    {
                        apply_run_json_patch(&candidate.before, &candidate.arguments.patch, &candidate.arguments.expected_after_hash, candidate.arguments.json_format)?;
                        reachable.insert(candidate.arguments.expected_after_hash.clone());
                        pending_sequences.insert(candidate.call.seq.0);
                    }
                }
                for (before, known) in &by_before {
                    if reachable.contains(before) && known.call.seq.0 > previous_call {
                        reachable.insert(ContentHash::of_bytes(&known.after).to_hex());
                    }
                }
                if reachable.contains(&end) && !pending_sequences.is_empty() {
                    return Err(CheckedRunError::AwaitingResults { call_sequences: pending_sequences.into_iter().collect() });
                }
                if reachable.len() == count { break; }
            }
            return Err("run changed without a complete admitted checked file.write chain".into());
        };
        if transition.call.seq.0 <= previous_call {
            return Err("checked authoring chain has reversed effect causality".into());
        }
        if unproven.remove(&hash) {
            generated_artifacts::validate_generated_transition(reader, &transition)?;
            input_artifacts::validate_input_transition(reader, &transition)?;
            validate_verified_input_transition(reader, &transition)?;
            input_revision::validate_rearm_evidence(reader, &transition, path)?;
            source_withdrawal::validate_evidence(reader, &transition)?;
            rejected_final::validate_evidence(reader, &transition)?;
        }
        hash = ContentHash::of_bytes(&transition.after).to_hex();
        current = transition.after.clone();
        previous_call = transition.call.seq.0;
        chain.push(transition);
    }
    if observed_sequence.is_some_and(|sequence| !unproven.contains(&end) && by_before.get(&end).is_some_and(|next| next.result.seq.0 <= sequence)) {
        return Err("observed run snapshot was already withdrawn by a checked effect".into());
    }
    // One receipt proves its exact file effect. Later coauthor state requires
    // the separate full-chain proof, not this installation's result-time snapshot.
    if selected_install.is_some() { return Ok(chain); }
    let mut versions = BTreeMap::from([(start, 0usize)]);
    for (index, transition) in chain.iter().enumerate() {
        if versions.insert(ContentHash::of_bytes(&transition.after).to_hex(), index + 1).is_some() {
            return Err("checked authoring chain cycles through a withdrawn snapshot".into());
        }
    }
    for (hash, position) in connection_versions {
        if versions.insert(hash, position).is_some() {
            return Err("observed connection reuses a checked snapshot".into());
        }
    }
    for (index, transition) in chain.iter().enumerate() {
        if transition.result.invocation_metadata.as_ref().and_then(|m|m.invocation()).and_then(|m|m.runtime.as_ref()).is_some_and(|r|r.run != original.id) { continue; }
        let hash = transition.result.invocation_metadata.as_ref().and_then(|m| m.invocation())
            .and_then(|m| m.runtime.as_ref()).and_then(|r| r.run_record_hash.as_ref())
            .ok_or("checked result has no snapshot")?;
        let position = *versions.get(hash).ok_or("checked result used an unproven snapshot")?;
        if position <= index || chain[position - 1].call.seq >= transition.result.seq {
            return Err("checked result predates its proven file effect".into());
        }
    }
    let mut observed_position = 0;
    for (seq, hash, metadata) in observations {
        let position = *versions.get(&hash.to_hex()).ok_or("checked historical movement used an unproven or withdrawn snapshot")?;
        if position < observed_position
            || (position > 0 && chain[position - 1].call.seq.0 >= seq)
            || chain.iter().skip(position).any(|transition| transition.result.seq.0 <= seq) {
            return Err("checked historical movement used an unproven or withdrawn snapshot".into());
        }
        observed_position = position;
        if let Some(metadata) = metadata {
            let record = if position == 0 { original.clone() } else {
                RunRecord::from_bytes(&chain[position - 1].after).map_err(|e| e.to_string())?
            };
            validate_observed_selected_reads(&record, &metadata, selected_reads)?;
        }
    }
    Ok(chain)
}

pub fn checked_run_install(
    reader: &mut impl CheckedRunEvidenceReader,
    path: &Path,
    install: &octopus_app_server_interface::application_consent_contract::CheckedRunInstall,
    writers: &[CheckedRunWriter],
) -> Result<CheckedRunTransition, String> {
    validate_checked_run_writers(writers)?;
    if install.call.movement_seq == 0 || install.call.movement_seq >= install.result.movement_seq {
        return Err("checked installation needs its actual ordered non-genesis call and result".into());
    }
    let call = reader.admitted_movement(install.call.movement_seq)?.ok_or("checked installation call is absent")?;
    let result = reader.admitted_movement(install.result.movement_seq)?.ok_or("checked installation result is absent")?;
    if call.artifact.as_ref() != Some(&install.call.artifact) || result.artifact.as_ref() != Some(&install.result.artifact) {
        return Err("checked installation changed its exact admitted evidence refs".into());
    }
    let snapshot_hash = |movement: &Movement| -> Result<ContentHash, String> {
        movement.invocation_metadata.as_ref().and_then(|m| m.invocation()).and_then(|m| m.runtime.as_ref())
            .and_then(|runtime| runtime.run_record_hash.as_deref()).and_then(ContentHash::from_hex)
            .ok_or("checked installation has no admitted snapshot".into())
    };
    let admitted = reader.snapshot(&snapshot_hash(&call)?)?;
    let (manifest, payload) = reader.artifact(&install.call.artifact)?;
    let writer = writers.iter().find(|writer| call.from == Endpoint::Interface(writer.source.clone())
        && call.to == Endpoint::Interface(writer.door.clone()))
        .ok_or("checked installation call has no exact declared writer")?;
    let artifact: octopus_tool_handlers::ToolCallArtifact = serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
    if manifest.label != writer.call_label || artifact.tool != writer.tool {
        return Err("checked installation call differs from its declared writer".into());
    }
    let (before, after) = if artifact.arguments.get("ordered").is_some() {
        let ordered: writer_run::OrderedArguments = serde_json::from_value(artifact.arguments).map_err(|e| e.to_string())?;
        let (before, arguments) = ordered.checked_call(writer, &admitted, path,reader.max_payload_bytes())?;
        let after = apply_run_json_patch(&before, &arguments.patch, &arguments.expected_after_hash, arguments.json_format)?.into_bytes();
        (before, after)
    } else {
        let arguments: FileWriteCallArguments = serde_json::from_value(artifact.arguments).map_err(|e| e.to_string())?;
        if arguments.path != path { return Err("checked installation targets another path".into()); }
        let after = apply_run_json_patch(&admitted, &arguments.patch, &arguments.expected_after_hash, arguments.json_format)?.into_bytes();
        (admitted, after)
    };
    let chain = checked_run_file_chain_with_reader(reader, path, &before, CheckedRunTarget::Snapshot(&after), install.call.movement_seq - 1,
        install.call.movement_seq, Some(install.result.movement_seq.checked_add(1).ok_or("checked installation sequence overflow")?),
        writers, &[], None, Some(install)).map_err(String::from)?;
    let [transition] = chain.as_slice() else { return Err("checked installation does not prove one exact write".into()); };
    if transition.call != call || transition.result != result {
        return Err("checked installation differs from its exact admitted write".into());
    }
    Ok(transition.clone())
}

pub fn checked_declared_run_install(
    evidence: &CheckedRunEvidence<'_>, path: &Path, declaration: &[u8],
    install: &CheckedRunInstall, seeds: Vec<CheckedRunWriter>,
) -> Result<CheckedRunTransition, String> {
    let record = RunRecord::from_bytes(declaration).map_err(|e| e.to_string())?;
    let writers = declared_writer_closure(&record, seeds)?;
    let prior = install.call.movement_seq.checked_sub(1).ok_or("checked installation call is genesis")?;
    let mut reader = FileCheckedRunReader::open(evidence, Some(prior))?;
    while reader.next_seq < install.call.movement_seq {
        reader.movement(reader.next_seq)?.ok_or("checked installation call is absent")?;
    }
    let transition = checked_run_install(&mut reader, path, install, &writers)?;
    let before = RunRecord::from_bytes(&transition.before).map_err(|e| e.to_string())?;
    if before.id != record.id || before.worker != record.worker {
        return Err("checked installation changed its declared target scope".into());
    }
    validate_checked_owner_transition(&transition)?;
    Ok(transition)
}

pub fn validate_selected_return_writers(
    record: &RunRecord,
    provider: &CheckedRunWriter,
    owner: Option<&CheckedRunWriter>,
) -> Result<(), String> {
    let state = record.interface_state.get(&provider.owner).ok_or("selected return lost its admitted provider state")?;
    let topology: BoundRunAuthoringTopology = serde_json::from_value(state.get(RUN_AUTHORING_BINDING)
        .cloned().ok_or("selected return lost its admitted provider topology")?).map_err(|e| e.to_string())?;
    topology.validate_identity(record, &provider.owner)?;
    let call = topology.checkpoints.iter().find(|cp| cp.role == RunAuthoringCheckpointRole::AuthoringCall)
        .ok_or("selected return has no declared provider write edge")?;
    let result = topology.checkpoints.iter().find(|cp| cp.role == RunAuthoringCheckpointRole::AuthoringResult)
        .ok_or("selected return has no declared provider write result")?;
    if provider.source != topology.authoring_source_interface || call.interface() != &provider.source
        || call.outbound_interfaces().collect::<Vec<_>>() != [&provider.door]
        || result.interface() != &provider.door || result.outbound_interfaces().collect::<Vec<_>>() != [&provider.owner]
        || !call.interface_binding.admits(&provider.call_label) || !result.interface_binding.admits(&provider.result_label)
        || !call.required_verified_artifacts.iter().any(|requirement| requirement.artifact_label == provider.call_label
            && requirement.verifier.as_str() == "tool_call_allowed"
            && requirement.params == serde_json::json!({"tools":[provider.tool]})) {
        return Err("selected return writer is not the exact admitted provider authoring contract".into());
    }
    let declared = topology.checkpoints.iter().filter_map(|cp| cp.owner_authoring.as_ref()).collect::<Vec<_>>();
    match owner {
        Some(owner) if owner.owner != provider.owner && declared.contains(&owner) => Ok(()),
        None => Ok(()),
        _ => Err("selected return owner is not an independently declared writer".into()),
    }
}

pub fn checked_return_interval(
    reader: &mut impl CheckedRunEvidenceReader,
    selected: &octopus_app_server_interface::application_consent_contract::SelectedConsentReturn,
    before: &[u8], after: &[u8], after_seq: u64, before_seq: u64,
    protected: &[CheckpointId],
) -> Result<Vec<CheckedRunTransition>, String> {
    let record = RunRecord::from_bytes(before).map_err(|e| e.to_string())?;
    if !octopus_app_server_interface::application_consent_contract::normal_path(&selected.run_path)
        || selected.run_path.file_name().and_then(|name| name.to_str()) != Some(&format!("{}.json", record.id)) {
        return Err("selected return changed its original exact task write path".into());
    }
    validate_selected_return_writers(&record, &selected.provider_writer, selected.owner_writer.as_ref())?;
    validate_payload_artifact_selection(&selected.selected_artifacts)?;
    let mut writers = vec![selected.provider_writer.clone()];
    writers.extend(selected.owner_writer.clone());
    let first = after_seq.checked_add(1).ok_or("selected return interval overflow")?;
    if first > before_seq { return Err("selected return interval is reversed".into()); }
    let chain = checked_run_file_chain_with_reader(reader, &selected.run_path, before, CheckedRunTarget::Snapshot(after), after_seq, first, Some(before_seq), &writers, &selected.selected_artifacts, None, None)?;
    for transition in &chain {
        let prior = RunRecord::from_bytes(&transition.before).map_err(|e| e.to_string())?;
        let next = RunRecord::from_bytes(&transition.after).map_err(|e| e.to_string())?;
        for record in [&prior, &next] {
            validate_selected_return_writers(record, &selected.provider_writer, selected.owner_writer.as_ref())?;
        }
        if transition.writer == selected.provider_writer { validate_checked_provider_transition(transition)?; }
        else if Some(&transition.writer) == selected.owner_writer.as_ref() { validate_checked_owner_transition(transition)?; }
        else { return Err("selected return changed the actual checked writer".into()); }
        for checkpoint in protected {
            validate_unchanged_admitted_checkpoint(&transition.before, &transition.after, checkpoint)?;
        }
        for (movement, record) in [(&transition.call, &prior), (&transition.result, &next)] {
            let metadata = movement.invocation_metadata.as_ref().and_then(|m| m.invocation())
                .ok_or("selected return write lost its admitted read bindings")?;
            validate_observed_selected_reads(record, metadata, &selected.selected_artifacts)?;
        }
    }
    for checkpoint in protected { validate_unchanged_admitted_checkpoint(before, after, checkpoint)?; }
    Ok(chain)
}

pub fn validate_selected_return_reads(metadata: &InvocationMetadata, selected: &[ArtifactRef]) -> Result<(), String> {
    for reference in selected {
        if !metadata.artifact_binding_refs.iter().filter(|bound| bound.id == reference.id).eq([reference])
            || metadata.grant.artifact_access.get(reference.id.as_str()).is_none_or(|access| !access.read) {
            return Err("selected return lost or replaced an original exact readable artifact".into());
        }
    }
    Ok(())
}

pub(crate) fn validate_observed_selected_reads(record: &RunRecord, metadata: &InvocationMetadata,
    selected: &[ArtifactRef]) -> Result<(), String> {
    let runtime = metadata.runtime.as_ref().ok_or("selected read witness has no admitted runtime scope")?;
    let checkpoint = record.checkpoint(&runtime.checkpoint).ok_or("selected read witness lost its actual authored checkpoint")?;
    if checkpoint.epoch.0 != runtime.epoch || !record.active_checkpoints.contains(&checkpoint.id) {
        return Err("selected read witness changed its actual authored checkpoint generation".into());
    }
    for reference in selected {
        let authored = checkpoint.artifact_bindings.iter().any(|binding| binding.id == reference.id && binding.access.read);
        let carried = metadata.artifact_binding_refs.iter().any(|bound| bound.id == reference.id);
        if authored || carried {
            validate_selected_return_reads(metadata, std::slice::from_ref(reference))?;
        }
    }
    Ok(())
}

pub(crate) fn observed_run_evidence_start(
    evidence: &CheckedRunEvidence<'_>, record: &RunRecord, before: &[u8], after_seq: u64,
) -> Result<u64, String> {
    let hash = ContentHash::of_bytes(before).to_hex();
    let upper = after_seq.checked_add(1).ok_or("checked writer evidence interval overflow")?;
    let snapshot = |row: &Movement| -> Option<String> {
        if row.kind != MovementKind::InterfaceOutbound { return None; }
        let runtime = row.invocation_metadata.as_ref()?.invocation()?.runtime.as_ref()?;
        (runtime.run == record.id).then(|| runtime.run_record_hash.clone()).flatten()
    };
    if let Ok(index) = LedgerSqliteReader::open(evidence.ledger_index_path) {
        if index.progress().is_ok_and(|progress| progress.watermark >= upper) {
            if let Ok(Some(rows)) = index.movement_rows_touching_run(record.id.as_str(), 0, upper) {
                let mut earliest = None;
                for indexed in rows.iter().rev() {
                    let row = generated_artifacts::read_movement(evidence, indexed.seq)?;
                    match snapshot(&row) {
                        Some(current) if current == hash => earliest = Some(row.seq.0),
                        Some(_) if earliest.is_some() => break,
                        _ => {},
                    }
                }
                return Ok(earliest.map_or(after_seq, |seq| seq.saturating_sub(1)));
            }
        }
    }
    let mut prefix = ledger_rows::outbound_prefix(evidence, upper)?;
    let candidates = prefix.matching(|row| row.run_snapshot(&record.id).is_some())?;
    let mut earliest = None;
    for seq in candidates.into_iter().rev() {
        match prefix.verified(seq)?.run_snapshot(&record.id) {
            Some(current) if current == hash => earliest = Some(seq),
            Some(_) if earliest.is_some() => break,
            _ => {},
        }
    }
    Ok(earliest.map_or(after_seq, |seq| seq.saturating_sub(1)))
}

pub(crate) fn detached_writer_starts_at(
    reader: &mut (impl CheckedRunEvidenceReader + ?Sized), call: Option<&ledger_rows::OutboundRow>, path: &Path,
    record: &RunRecord, before: &[u8], before_hash: &str, writers: &[CheckedRunWriter],
) -> Result<bool, String> {
    let Some(call) = call else { return Ok(false); };
    let Some(writer) = writers.iter().find(|writer| call.from == Endpoint::Interface(writer.source.clone())
        && call.to == Endpoint::Interface(writer.door.clone())) else { return Ok(false); };
    let (manifest, payload) = reader.artifact(call.artifact.as_ref().ok_or("observed writer call has no artifact")?)?;
    if manifest.label != writer.call_label { return Ok(false); }
    let artifact: octopus_tool_handlers::ToolCallArtifact = serde_json::from_slice(&payload).map_err(|error| error.to_string())?;
    if artifact.tool != writer.tool || artifact.arguments.get("ordered").is_none()
        || artifact.arguments.pointer("/writes/1/expected_before_hash").and_then(|value| value.as_str()) != Some(before_hash)
    { return Ok(false); }
    let ordered: writer_run::OrderedArguments = serde_json::from_value(artifact.arguments).map_err(|error| error.to_string())?;
    if !ordered.targets(path) { return Ok(false); }
    let runtime = call.runtime.as_ref().ok_or("observed writer call has no admitted coordinate")?;
    let hash = runtime.run_record_hash.as_deref().and_then(ContentHash::from_hex)
        .ok_or("observed writer call has no admitted writer snapshot")?;
    let snapshot = reader.snapshot(&hash)?;
    if ContentHash::of_bytes(&snapshot) != hash { return Err("observed writer snapshot failed integrity".into()); }
    let declaration = RunRecord::from_bytes(&snapshot).map_err(|error| error.to_string())?;
    if declaration.id != runtime.run || declaration.ee != runtime.ee || declaration.worker != record.worker
        || runtime.worker != record.worker || runtime.interface != writer.door {
        return Err("observed writer snapshot changed its exact admitted scope".into());
    }
    let (preimage, _) = ordered.checked_call(writer, &snapshot, path, reader.max_payload_bytes())?;
    Ok(preimage == before)
}

pub(crate) fn concurrent_writer_evidence_start(
    evidence: &CheckedRunEvidence<'_>, path: &Path, record: &RunRecord, before: &[u8],
    after_seq: u64, writers: &[CheckedRunWriter],
) -> Result<u64, String> {
    validate_checked_run_writers(writers)?;
    let first = observed_run_evidence_start(evidence, record, before, after_seq)?;
    let before_hash = ContentHash::of_bytes(before).to_hex();
    let upper = first.checked_add(1).ok_or("checked writer evidence interval overflow")?;
    let mut reader = FileCheckedRunReader::open(evidence, None)?;
    // Detached calls pin their writer Run, while their ordered target write
    // carries the selected Run's preimage. Both precede the same full checker.
    let indexed = LedgerSqliteReader::open(evidence.ledger_index_path).ok().and_then(|index| {
        if !index.progress().is_ok_and(|progress| progress.watermark >= upper) { return None; }
        let mut sequences = BTreeSet::new();
        for writer in writers {
            for row in index.movement_rows_for_runtime_interface(writer.door.as_str(), 0, upper).ok()?? {
                sequences.insert(row.seq);
            }
        }
        Some(sequences)
    });
    if let Some(sequences) = indexed {
        for seq in sequences {
            let row = reader.admitted_movement(seq)?.ok_or("checked writer evidence interval is incomplete")?;
            if detached_writer_starts_at(&mut reader, ledger_rows::OutboundRow::of(&row).as_ref(), path, record, before, &before_hash, writers)? {
                return seq.checked_sub(1).ok_or("observed writer call has no earlier evidence boundary".into());
            }
        }
        return Ok(first);
    }
    let mut prefix = ledger_rows::outbound_prefix(evidence, upper)?;
    let candidates = prefix.matching(|row| writers.iter().any(|writer| row.writes_through(writer)))?;
    for seq in candidates {
        let row = prefix.verified(seq)?;
        let writer = writers.iter().find(|writer| row.writes_through(writer)).ok_or("indexed writer call lost its declared writer")?;
        if let Some(artifact) = row.artifact.as_ref() {
            if ledger_rows::writer_call_excluded(evidence, artifact, writer, &before_hash)? { continue; }
        }
        if detached_writer_starts_at(&mut reader, Some(&row), path, record, before, &before_hash, writers)? {
            return seq.checked_sub(1).ok_or("observed writer call has no earlier evidence boundary".into());
        }
    }
    if prefix.available < upper { return Err("checked writer evidence interval is incomplete".into()); }
    Ok(first)
}

pub(crate) fn checked_selected_run_advance(
    ctx: &InterfaceLiftContext,
    path: &Path,
    before: &[u8],
    after: &[u8],
    after_seq: u64,
    config: &RunAuthoringConfig,
) -> Result<Vec<CheckedRunTransition>,String> {
    checked_selected_run_advance_state(ctx, path, before, after, after_seq, config).map_err(Into::into)
}

pub(crate) fn checked_selected_run_advance_state(
    ctx: &InterfaceLiftContext, path: &Path, before: &[u8], after: &[u8], after_seq: u64,
    config: &RunAuthoringConfig,
) -> Result<Vec<CheckedRunTransition>, CheckedRunError> {
    let mut writers = config.checkpoints.iter().filter_map(|cp| cp.owner_authoring.clone()).collect::<Vec<_>>();
    let original = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
    let current = RunRecord::from_bytes(after).map_err(|error| error.to_string())?;
    let payload_revision = input_revision::is_payload_revision(&original, &current, &config.provider_interface)?;
    if writers.is_empty() && !payload_revision && !result_inputs::enabled(config)
        && payload_artifact_bindings(&original, &config.provider_interface)?.is_empty()
        && payload_artifact_bindings(&current, &config.provider_interface)?.is_empty() {
        if let Some(protected) = output_connections::protected_record(before, after, config)? {
            if ContentHash::of_bytes(before) != ContentHash::of_bytes(&protected) {
                return Err("outgoing connection changed protected provider state without a checked writer chain".into());
            }
        }
        return Ok(Vec::new());
    }
    let provider_writer = CheckedRunWriter {
        owner:config.provider_interface.clone(),source:config.authoring_source_interface.clone(),
        door:config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?.interface().clone(),
        tool:config.file_write_tool.clone(),call_label:config.file_write_label.clone(),result_label:config.file_write_result_label.clone(),
    };
    writers.push(provider_writer.clone());
    let limits = ctx.required_host_io_limits()?;
    let evidence = CheckedRunEvidence { movements_path:&ctx.movements_path,ledger_index_path:&ctx.ledger_index_path,blobs_dir:&ctx.blobs_dir,max_control_frame_bytes:limits.max_control_frame_bytes,max_manifest_bytes:limits.max_manifest_bytes,max_payload_bytes:limits.max_payload_bytes };
    if ContentHash::of_bytes(before) == ContentHash::of_bytes(after) { return Ok(Vec::new()); }
    let record = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
    let writers = declared_writer_closure(&record, writers)?;
    let first = concurrent_writer_evidence_start(&evidence, path, &record, before, after_seq, &writers)?;
    let mut reader = FileCheckedRunReader::open(&evidence, Some(first))?.with_provider_context(ctx);
    let scan_start = reader.next_seq;
    let chain = checked_run_file_chain_with_reader(&mut reader, path, before, CheckedRunTarget::Snapshot(after), first, scan_start,
        None, &writers, &[], Some(config), None)?;
    for transition in &chain {
        let original = RunRecord::from_bytes(&transition.before).map_err(|error| error.to_string())?;
        let next = RunRecord::from_bytes(&transition.after).map_err(|error| error.to_string())?;
        if payload_artifact_bindings(&original, &config.provider_interface)? != payload_artifact_bindings(&next, &config.provider_interface)?
            && !(transition.writer == provider_writer && input_revision::is_payload_revision(&original, &next, &config.provider_interface)?) {
            return Err("checked owner advance changed the exact payload selection".into());
        }
        if transition.writer == provider_writer { validate_checked_provider_transition(transition)?; }
        else { validate_checked_owner_transition(transition)?; }
    }
    Ok(chain)
}

pub(crate) fn stable_run_observation<T>(
    path: &Path,
    captured: &[u8],
    max_bytes: u64,
    observe: impl FnOnce() -> Result<T, CheckedRunError>,
) -> Result<T, CheckedRunError> {
    let result = observe();
    let current = octopus_interfaces::read_artifact_carrier_file_bounded(path, max_bytes)
        .map_err(|error| error.to_string())?;
    if current != captured {
        return Err(CheckedRunError::ChangedDuringRead {
            captured: ContentHash::of_bytes(captured).to_hex(),
            current: ContentHash::of_bytes(&current).to_hex(),
        });
    }
    result
}

pub(crate) fn checked_live_run_advance(
    ctx: &InterfaceLiftContext,
    path: &Path,
    before: &[u8],
    after_seq: u64,
    config: &RunAuthoringConfig,
) -> Result<(Vec<u8>, Vec<CheckedRunTransition>), CheckedRunError> {
    let limit = ctx.required_host_io_limits()?.max_payload_bytes;
    let current = octopus_interfaces::read_artifact_carrier_file_bounded(path, limit)
        .map_err(|error| error.to_string())?;
    let chain = stable_run_observation(path, &current, limit, || {
        checked_selected_run_advance_state(ctx, path, before, &current, after_seq, config)
    })?;
    Ok((current, chain))
}

pub fn checked_provider_run_advance(
    ctx: &InterfaceLiftContext, path: &Path, before: &[u8], after: &[u8], after_seq: u64,
    provider: &InterfaceId, profile: &RunAuthoringProfile,
) -> Result<Vec<CheckedRunTransition>, String> {
    let record = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
    let config = config_from_persisted_profile(&record, provider, profile)?;
    checked_selected_run_advance(ctx, path, before, after, after_seq, &config)
}

pub(crate) fn extend_amendment_context_bindings(
    record: &RunRecord,
    provider: &InterfaceId,
    context: &mut Vec<ArtifactId>,
    static_context: &mut Vec<ArtifactId>,
) -> Result<(), String> {
    let prior_context = context.iter().cloned().collect::<BTreeSet<_>>();
    for binding in provider_context_bindings(record, provider)? {
        if binding.access.read && !prior_context.contains(&binding.id) {
            append_artifact(static_context, binding.id.clone());
            append_artifact(context, binding.id);
        }
    }
    Ok(())
}

pub fn provider_context_bindings(record: &RunRecord, provider: &InterfaceId) -> Result<Vec<ArtifactBinding>, String> {
    let topology: BoundRunAuthoringTopology = serde_json::from_value(record.interface_state.get(provider)
        .and_then(|state| state.get(RUN_AUTHORING_BINDING)).ok_or("provider context has no declared topology")?.clone())
        .map_err(|error| error.to_string())?;
    topology.validate_identity(record, provider)?;
    validate_checkpoint_topology(record, provider, &topology.checkpoints, None)?;
    current_context_bindings(record, &topology.checkpoints)
}

pub fn independent_provider_input(record: &RunRecord, provider: &InterfaceId, checkpoint: &CheckpointId) -> Result<Option<bool>, String> {
    let topology: BoundRunAuthoringTopology = serde_json::from_value(record.interface_state.get(provider)
        .and_then(|state| state.get(RUN_AUTHORING_BINDING)).ok_or("provider input has no declared topology")?.clone())
        .map_err(|error| error.to_string())?;
    let checkpoint = record.checkpoint(checkpoint).ok_or("provider input checkpoint is absent from its run")?;
    let matching = topology.checkpoints.iter().filter(|template| template.role.is_provider_input()
        && checkpoint.interface_binding.as_ref() == Some(&template.interface_binding)
        && checkpoint.outbound_interface_bindings == template.outbound_interface_bindings
        && checkpoint.required_verified_artifacts == template.required_verified_artifacts).collect::<Vec<_>>();
    let [template] = matching.as_slice() else {
        return Err("provider input checkpoint matches no unique authored template".into());
    };
    Ok(match template.role {
        RunAuthoringCheckpointRole::IndependentInput => Some(!template.include_context),
        RunAuthoringCheckpointRole::Stable => Some(false),
        _ => None,
    })
}

pub fn open_handoff_before(record: &RunRecord, provider: &InterfaceId, source_movement_seq: u64) -> Result<bool, String> {
    Ok(persisted_pending_handoff(record, provider)?
        .is_some_and(|pending| !pending.terminal && pending.source_movement_seq < source_movement_seq))
}

pub(crate) fn current_context_bindings(record: &RunRecord, templates: &[RunAuthoringCheckpoint]) -> Result<Vec<ArtifactBinding>, String> {
    let mut context = None;
    for template in templates.iter().filter(|template| template.role == RunAuthoringCheckpointRole::Stable && template.include_context) {
        let candidates = record.checkpoints.iter().filter(|cp| record.active_checkpoints.contains(&cp.id)
            && checkpoint_record_matches_template(cp, template, &record.id)).collect::<Vec<_>>();
        let [checkpoint] = candidates.as_slice() else { return Err("provider context has no unique stable input".into()); };
        if context.as_ref().is_some_and(|bound| bound != &checkpoint.artifact_bindings) {
            return Err("provider stable inputs carry different context".into());
        }
        context = Some(checkpoint.artifact_bindings.clone());
    }
    context.ok_or_else(|| "provider has no declared stable context".into())
}

pub fn validate_unchanged_run_neighbors(
    before: &[u8], after: &[u8], owner: &InterfaceId,
    before_owned: &BTreeSet<String>, after_owned: &BTreeSet<String>, permits_status_change: bool,
) -> Result<(), String> {
    let original: serde_json::Value = serde_json::from_slice(before).map_err(|e| e.to_string())?;
    let next: serde_json::Value = serde_json::from_slice(after).map_err(|e| e.to_string())?;
    let old_rows = original.get("checkpoints").and_then(serde_json::Value::as_array).ok_or("checked run has no checkpoints")?;
    for id in after_owned.difference(before_owned) {
        if old_rows.iter().any(|cp| cp.get("id").and_then(serde_json::Value::as_str) == Some(id.as_str())) {
            return Err("checked author tried to claim a preexisting neighboring checkpoint".into());
        }
    }
    let project = |mut value: serde_json::Value, owned: &BTreeSet<String>| -> Result<serde_json::Value, String> {
        let rows = value.get_mut("checkpoints").and_then(serde_json::Value::as_array_mut).ok_or("checked run has no checkpoints")?;
        for id in owned {
            if rows.iter().filter(|cp| cp.get("id").and_then(serde_json::Value::as_str) == Some(id.as_str())).count() != 1 { return Err("checked author owns a missing or duplicated checkpoint".into()); }
        }
        rows.retain(|cp| !cp.get("id").and_then(serde_json::Value::as_str).is_some_and(|id| owned.contains(id)));
        value.get_mut("active_checkpoints").and_then(serde_json::Value::as_array_mut).ok_or("checked run has no active checkpoints")?.retain(|id| !id.as_str().is_some_and(|id| owned.contains(id)));
        value.get_mut("interface_state").and_then(serde_json::Value::as_object_mut).ok_or("checked run has no interface state")?.remove(owner.as_str()).ok_or("checked author has no own interface state")?;
        if permits_status_change { value.as_object_mut().ok_or("checked run is not an object")?.remove("status"); }
        Ok(value)
    };
    if project(original, before_owned)? != project(next, after_owned)? { return Err("checked author changed a protected run field, owner or neighboring checkpoint".into()); }
    Ok(())
}

pub fn validate_unchanged_admitted_checkpoint(before: &[u8], after: &[u8], checkpoint: &CheckpointId) -> Result<(),String> {
    let before = RunRecord::from_bytes(before).map_err(|e| e.to_string())?;
    let after = RunRecord::from_bytes(after).map_err(|e| e.to_string())?;
    let original = before.checkpoint(checkpoint).ok_or("shared run input lost its original checkpoint")?;
    if before.id != after.id || before.worker != after.worker || after.checkpoint(checkpoint) != Some(original) || !after.active_checkpoints.contains(checkpoint) {
        return Err("shared run changed the exact admitted checkpoint before consumption".into());
    }
    Ok(())
}

pub fn validate_checked_provider_transition(transition: &CheckedRunTransition) -> Result<(), String> {
    if transition.placement { return placement::validate_placement_change(&transition.before, &transition.after); }
    let before = RunRecord::from_bytes(&transition.before).map_err(|e| e.to_string())?;
    let after = RunRecord::from_bytes(&transition.after).map_err(|e| e.to_string())?;
    if before.ee != after.ee { return Err("a run's own writer cannot change its placement".into()); }
    let provider = &transition.writer.owner;
    if result_inputs::definition_transition(transition)? { return Ok(()); }
    source_settlement::validate_checked_transition(&before, &after, &transition.writer)?;
    source_withdrawal::validate_checked_transition(&before, &after, &transition.writer)?;
    let replacement_config = config_from_persisted_profile(&before, provider, &RunAuthoringProfile {
        file_write_tool: transition.writer.tool.clone(), file_write_label: transition.writer.call_label.clone(),
        file_write_result_label: transition.writer.result_label.clone(), result_timeout_ms: 1,
    })?;
    rejected_final::validate_transition(&before, &after, &replacement_config)?;
    if capability_amendment::validate_transition(transition)? { return Ok(()); }
    let read = |record: &RunRecord| -> Result<BoundRunAuthoringTopology, String> {
        let state = record.interface_state.get(provider).ok_or("checked provider lost its state")?;
        serde_json::from_value(state.get(RUN_AUTHORING_BINDING).cloned().ok_or("checked provider lost its topology")?).map_err(|e| e.to_string())
    };
    let topology = read(&before)?;
    topology.validate_identity(&before,provider)?;
    let next_topology = read(&after)?;
    next_topology.validate_identity(&after,provider)?;
    refuse_selected_consent_state(&before, provider)?;
    refuse_selected_consent_state(&after, provider)?;
    if input_revision::is_payload_revision(&before, &after, provider)? {
        return input_revision::validate_payload_revision(&transition.before, &transition.after, &transition.writer);
    }
    if input_revision::is_revision(&topology, &next_topology)
        || input_revision::is_rearm(&before, &after, provider)? {
        return input_revision::validate(&transition.before, &transition.after, &transition.writer);
    }
    if !preserves_provider_topology(&topology, &next_topology)
        || topology.authoring_source_interface != transition.writer.source {
        return Err("coauthor changed the provider's exact topology or writer".into());
    }
    for (role, source, target) in [
        (RunAuthoringCheckpointRole::AuthoringCall,&transition.writer.source,&transition.writer.door),
        (RunAuthoringCheckpointRole::AuthoringResult,&transition.writer.door,provider),
    ] {
        let matches = topology.checkpoints.iter().filter(|cp| cp.role == role && cp.interface_binding.interface == *source && cp.outbound_interface_bindings.len() == 1 && cp.outbound_interface_bindings[0].interface == *target).count();
        if matches != 1 { return Err("coauthor writer is not the provider's exact declared authoring edge".into()); }
    }
    validate_checkpoint_topology(&before,provider,&topology.checkpoints,None)?;
    validate_checkpoint_topology(&after,provider,&next_topology.checkpoints,None)?;
    let roles = |record: &RunRecord| -> Result<Vec<ArtifactBinding>,String> {
        if let Some(binding) = writer_run::binding(record,provider)? {
            return binding.roles(record,provider,&topology.checkpoints);
        }
        let ids = raw_frontier_checkpoint_ids(record.interface_state.get(provider).ok_or("provider state absent")?)?;
        let rows = ids.iter().map(|id| record.checkpoint(&CheckpointId::new(id)).ok_or("provider frontier absent")).collect::<Result<Vec<_>,_>>()?;
        let call = topology.checkpoints.iter().find(|cp| cp.role == RunAuthoringCheckpointRole::AuthoringCall).ok_or("provider authoring call absent")?;
        let result = topology.checkpoints.iter().find(|cp| cp.role == RunAuthoringCheckpointRole::AuthoringResult).ok_or("provider authoring result absent")?;
        canonical_role_bindings(&rows,call,result,&record.id)
    };
    let mut expected_roles = roles(&before)?;
    if topology.writer_run.is_none() {
        for checkpoint in next_topology.checkpoints.iter().filter(|cp| !topology.checkpoints.contains(cp)
            && !matches!(cp.role, RunAuthoringCheckpointRole::VerifiedInput | RunAuthoringCheckpointRole::ResultInput)) {
            for id in &checkpoint.static_artifact_bindings {
                let binding: ArtifactBinding = serde_json::from_value(serde_json::json!({"id": id})).map_err(|error| error.to_string())?;
                if !expected_roles.contains(&binding) { expected_roles.push(binding); }
            }
        }
    }
    if expected_roles != roles(&after)? { return Err("coauthor changed the mandatory role binding closure beyond its authored additions".into()); }
    let prior = before.interface_state.get(provider).ok_or("provider state absent")?;
    let next = after.interface_state.get(provider).ok_or("provider state absent")?;
    if optional_u64(next,"authoring_generation")? != optional_u64(prior,"authoring_generation")?.checked_add(1).ok_or("provider generation overflow")? {
        return Err("checked provider transition skipped an authoring generation".into());
    }
    let old_context = optional_u64(prior,"context_generation")?;
    let new_context = optional_u64(next,"context_generation")?;
    if new_context < old_context { return Err("checked provider context generation moved backwards".into()); }
    let pending = persisted_pending_handoff(&after,provider)?.ok_or("checked provider write has no pending handoff")?;
    validate_verified_input_change(&topology, &next_topology, &pending)?;
    let next_config = next_topology.config(&RunAuthoringProfile {
        file_write_tool: transition.writer.tool.clone(), file_write_label: transition.writer.call_label.clone(),
        file_write_result_label: transition.writer.result_label.clone(), result_timeout_ms: 1,
    });
    next_config.validate(provider, &after.id)?;
    result_inputs::validate_transition(&before, &after, &next_config)?;
    if pending.source_movement_seq >= transition.call.seq.0 || after.status != serde_json::json!(if pending.terminal { "idle" } else { "running" }) {
        return Err("checked provider write has invalid source timing or status".into());
    }
    let stable = |state: &BTreeMap<String,serde_json::Value>| {
        let mut state = state.clone();
        for key in [FRONTIER_CHECKPOINT_IDS,"context_generation","authoring_generation","last_authored_artifact","pending_handoff",RUN_AUTHORING_BINDING] { state.remove(key); }
        state
    };
    if stable(prior) != stable(next) { return Err("checked provider changed immutable interface state".into()); }
    let owned = |record: &RunRecord| -> Result<BTreeSet<String>,String> {
        let topology = read(record)?;
        let mut ids = raw_frontier_checkpoint_ids(record.interface_state.get(provider).ok_or("provider state absent")?)?.into_iter().collect::<BTreeSet<_>>();
        for cp in &record.checkpoints {
            if topology.checkpoints.iter().any(|template| matches!(template.role,RunAuthoringCheckpointRole::Stable | RunAuthoringCheckpointRole::IndependentInput | RunAuthoringCheckpointRole::VerifiedInput | RunAuthoringCheckpointRole::ResultInput) && checkpoint_record_matches_template(cp,template,&record.id)) {
                ids.insert(cp.id.to_string());
            }
        }
        Ok(ids)
    };
    validate_unchanged_run_neighbors(&transition.before,&transition.after,provider,&owned(&before)?,&owned(&after)?,true)
}

pub(crate) fn preserves_provider_topology(before: &BoundRunAuthoringTopology, after: &BoundRunAuthoringTopology) -> bool {
    let mut prior = before.clone();
    let mut next = after.clone();
    prior.checkpoints.clear();
    next.checkpoints.clear();
    let old = before.checkpoints.iter().filter(|cp| !matches!(cp.role, RunAuthoringCheckpointRole::VerifiedInput | RunAuthoringCheckpointRole::ResultInput)).collect::<Vec<_>>();
    let new = after.checkpoints.iter().filter(|cp| !matches!(cp.role, RunAuthoringCheckpointRole::VerifiedInput | RunAuthoringCheckpointRole::ResultInput)).collect::<Vec<_>>();
    prior == next && new.starts_with(&old) && new[old.len()..].iter().all(|checkpoint| {
        checkpoint.role == RunAuthoringCheckpointRole::ProviderInterfaceOutput && checkpoint.owner_authoring.is_none()
    })
}

pub(crate) fn declared_writer_closure(record: &RunRecord, seeds: Vec<CheckedRunWriter>) -> Result<Vec<CheckedRunWriter>, String> {
    let mut pending = seeds;
    let mut selected = BTreeMap::new();
    while let Some(writer) = pending.pop() {
        if let Some(prior) = selected.get(&writer.owner) {
            if prior != &writer { return Err("declared coauthors disagree on an exact writer".into()); }
            continue;
        }
        if let Some(value) = record.interface_state.get(&writer.owner).and_then(|state| state.get(RUN_AUTHORING_BINDING)) {
            let topology: BoundRunAuthoringTopology = serde_json::from_value(value.clone()).map_err(|e|e.to_string())?;
            topology.validate_identity(record, &writer.owner)?;
            pending.extend(topology.checkpoints.iter().filter_map(|cp| cp.owner_authoring.clone()));
        }
        selected.insert(writer.owner.clone(), writer);
    }
    Ok(selected.into_values().collect())
}

pub fn validate_independent_provider_transition(record: &RunRecord, provider: &InterfaceId, profile: &RunAuthoringProfile, transition: &CheckedRunTransition) -> Result<(),String> {
    let config = config_from_persisted_profile(record,provider,profile)?;
    let own = CheckedRunWriter {owner:provider.clone(),source:config.authoring_source_interface.clone(),
        door:config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?.interface().clone(),tool:profile.file_write_tool.clone(),call_label:profile.file_write_label.clone(),result_label:profile.file_write_result_label.clone()};
    if !declared_writer_closure(record,vec![own])?.contains(&transition.writer) { return Err("independent transition has no exact declared writer".into()); }
    validate_checked_owner_transition(transition)
}

pub fn checked_declared_run_advance(
    evidence: &CheckedRunEvidence<'_>, path: &Path, before: &[u8], after: &[u8],
    after_seq: u64, seeds: Vec<CheckedRunWriter>,
) -> Result<Vec<CheckedRunTransition>, CheckedRunError> {
    let record = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
    let writers = declared_writer_closure(&record, seeds)?;
    let first = concurrent_writer_evidence_start(evidence, path, &record, before, after_seq, &writers)?;
    let transitions = checked_run_file_chain_state(evidence, path, before, after, first, &writers)?;
    for transition in &transitions { validate_checked_owner_transition(transition)?; }
    Ok(transitions)
}

pub fn checked_declared_run_snapshot(
    evidence: &CheckedRunEvidence<'_>, path: &Path, before: &[u8], after_seq: u64,
    hash: &str, observed_sequence: u64, seeds: Vec<CheckedRunWriter>,
) -> Result<Vec<u8>, CheckedRunError> {
    if after_seq >= observed_sequence { return Err("observed run snapshot has no earlier admitted anchor".into()); }
    let hash = ContentHash::from_hex(hash).filter(|pin| pin.to_hex() == hash)
        .ok_or("observed run snapshot has no canonical content hash")?;
    let record = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
    let writers = declared_writer_closure(&record, seeds)?;
    validate_checked_run_writers(&writers)?;
    let first = concurrent_writer_evidence_start(evidence, path, &record, before, after_seq, &writers)?;
    let mut reader = FileCheckedRunReader::open(evidence, Some(first))?;
    let scan_start = reader.next_seq;
    let transitions = checked_run_file_chain_with_reader(&mut reader, path, before,
        CheckedRunTarget::Observed { hash, sequence: observed_sequence }, first, scan_start,
        None, &writers, &[], None, None)?;
    for transition in &transitions { validate_checked_owner_transition(transition)?; }
    let after = transitions.last().map_or_else(|| before.to_vec(), |transition| transition.after.clone());
    let target = RunRecord::from_bytes(&after).map_err(|error| error.to_string())?;
    if ContentHash::of_bytes(&after) != hash || target.id != record.id || target.worker != record.worker {
        return Err("observed run snapshot differs from its exact checked target scope".into());
    }
    Ok(after)
}

pub(crate) fn validate_verified_input_change(before: &BoundRunAuthoringTopology, after: &BoundRunAuthoringTopology, pending: &PendingHandoff) -> Result<(), String> {
    let old = before.checkpoints.iter().filter(|cp| cp.role == RunAuthoringCheckpointRole::VerifiedInput).collect::<Vec<_>>();
    let new = after.checkpoints.iter().filter(|cp| cp.role == RunAuthoringCheckpointRole::VerifiedInput).collect::<Vec<_>>();
    if old == new { return Ok(()); }
    if !pending.terminal || old.iter().any(|cp| new.contains(cp)) {
        return Err("verified-input change must replace the complete set in a terminal handoff".into());
    }
    if !new.is_empty() {
        let clients = pending.items.iter().filter(|item| item.checkpoint.interface_binding.interface == before.provider_interface)
            .filter_map(|item| match item.checkpoint.outbound_interface_bindings.as_slice() { [client] => Some(client.interface.clone()), _ => None })
            .filter(|client| before.checkpoints.iter().any(|cp| cp.role == RunAuthoringCheckpointRole::ProviderArtifactOutput && cp.outbound_interfaces().eq([client])))
            .collect::<BTreeSet<_>>();
        if clients.len() != 1 || new.iter().any(|cp| !clients.contains(cp.interface())) {
            return Err("verified-input request lost its exact pending client projection".into());
        }
    }
    Ok(())
}

pub(crate) fn validate_verified_input_transition(reader: &mut impl CheckedRunEvidenceReader, transition: &CheckedRunTransition) -> Result<(), String> {
    result_inputs::validate_checked(reader, transition)?;
    let before = RunRecord::from_bytes(&transition.before).map_err(|e| e.to_string())?;
    let after = RunRecord::from_bytes(&transition.after).map_err(|e| e.to_string())?;
    let read = |record: &RunRecord| -> Result<Option<BoundRunAuthoringTopology>, String> {
        record.interface_state.get(&transition.writer.owner).and_then(|state| state.get(RUN_AUTHORING_BINDING))
            .map(|value|serde_json::from_value(value.clone()).map_err(|e|e.to_string())).transpose()
    };
    let (Some(old), Some(new)) = (read(&before)?, read(&after)?) else { return Ok(()); };
    let prior = old.checkpoints.iter().filter(|cp| cp.role == RunAuthoringCheckpointRole::VerifiedInput).collect::<Vec<_>>();
    let next = new.checkpoints.iter().filter(|cp| cp.role == RunAuthoringCheckpointRole::VerifiedInput).collect::<Vec<_>>();
    if prior == next { return Ok(()); }
    let pending = persisted_pending_handoff(&after, &transition.writer.owner)?.ok_or("verified-input change has no handoff")?;
    validate_verified_input_change(&old, &new, &pending)?;
    let metadata = transition.call.invocation_metadata.as_ref().and_then(|m|m.invocation()).ok_or("verified-input change has no admitted writer grant")?;
    if next.iter().flat_map(|cp| &cp.required_verified_artifacts).any(|r| !metadata.grant.mark_outcomes.iter().any(|id| id == r.mark.as_str())) {
        return Err("verified-input request names an ungranted mark".into());
    }
    if prior.is_empty() { return Ok(()); }
    let source = reader.admitted_movement(pending.source_movement_seq)?.ok_or("verified-input retirement has no admitted source")?;
    let metadata = source.invocation_metadata.as_ref().and_then(|m|m.invocation()).ok_or("verified-input source has no metadata")?;
    let runtime = metadata.runtime.as_ref().ok_or("verified-input source has no coordinate")?;
    let hash = runtime.run_record_hash.as_deref().and_then(ContentHash::from_hex).ok_or("verified-input source has no pinned snapshot")?;
    let bytes = reader.snapshot(&hash)?;
    if ContentHash::of_bytes(&bytes) != hash { return Err("verified-input source snapshot failed integrity".into()); }
    let pinned = RunRecord::from_bytes(&bytes).map_err(|e|e.to_string())?;
    let checkpoint = pinned.checkpoint(&runtime.checkpoint).ok_or("verified-input source checkpoint is absent")?;
    let matches = prior.iter().filter(|cp| checkpoint_record_matches_template(checkpoint, cp, &before.id)).collect::<Vec<_>>();
    if source.kind != MovementKind::InterfaceOutbound || source.seq.0 >= transition.call.seq.0
        || source.to != Endpoint::Interface(transition.writer.owner.clone()) || runtime.interface != transition.writer.owner
        || runtime.run != before.id || runtime.worker != before.worker || runtime.ee != pinned.ee
        || pinned.id != before.id || pinned.worker != before.worker
        || !pinned.active_checkpoints.contains(&runtime.checkpoint) || !before.active_checkpoints.contains(&runtime.checkpoint)
        || before.checkpoint(&runtime.checkpoint) != Some(checkpoint) || runtime.epoch != checkpoint.epoch.0 || matches.len() != 1
        || metadata.verification_failure.is_some() || metadata.checkpoint_binding_policy.as_ref() != Some(&matches[0].binding_policy_snapshot())
        || metadata.required_verified_artifacts != matches[0].required_verified_artifacts
        || metadata.outbound_interface_bindings.iter().ne(checkpoint.outbound_interfaces())
        || !artifact_refs_match_bindings(&metadata.artifact_binding_refs, &checkpoint.artifact_bindings)
        || !metadata.checkpoint_admission.as_ref().is_some_and(|a|a.complete && a.artifact_label == matches[0].required_verified_artifacts[0].artifact_label
            && a.required_artifact_labels == vec![a.artifact_label.clone()])
    { return Err("verified-input retirement did not consume its exact admitted verified source".into()); }
    let template = matches[0];
    if source.from != Endpoint::Interface(template.interface().clone()) { return Err("verified-input source owner differs".into()); }
    let (manifest, _) = reader.artifact(source.artifact.as_ref().ok_or("verified-input source has no artifact")?)?;
    if manifest.label != template.required_verified_artifacts[0].artifact_label { return Err("verified-input source label differs".into()); }
    Ok(())
}

pub fn validate_checked_owner_transition(transition: &CheckedRunTransition) -> Result<(), String> {
    if transition.placement { return placement::validate_placement_change(&transition.before, &transition.after); }
    let record = RunRecord::from_bytes(&transition.before).map_err(|e| e.to_string())?;
    if record.ee != RunRecord::from_bytes(&transition.after).map_err(|e| e.to_string())?.ee {
        return Err("a run's own writer cannot change its placement".into());
    }
    if record.interface_state.get(&transition.writer.owner).is_some_and(|state| state.contains_key(RUN_AUTHORING_BINDING)) {
        validate_declared_provider_writer(&record, &transition.writer)?;
        validate_declared_provider_writer(&RunRecord::from_bytes(&transition.after).map_err(|e| e.to_string())?, &transition.writer)?;
        return validate_checked_provider_transition(transition);
    }
    let owner = &transition.writer.owner;
    let before = RunRecord::from_bytes(&transition.before).map_err(|e| e.to_string())?;
    let after = RunRecord::from_bytes(&transition.after).map_err(|e| e.to_string())?;
    let detached = owner_writer_binding(&before, owner)?;
    if detached != owner_writer_binding(&after, owner)? { return Err("checked owner changed its exact writer Run".into()); }
    let owner_frontier = |state: &BTreeMap<String, serde_json::Value>| {
        if detached.is_some() && state.get(FRONTIER_CHECKPOINT_IDS) == Some(&serde_json::json!([])) {
            Ok(BTreeSet::new())
        } else {
            raw_frontier_checkpoint_ids(state)
        }
    };
    let metadata = transition.call.invocation_metadata.as_ref().and_then(|m| m.invocation())
        .ok_or("checked owner write has no admitted role grant")?;
    let role_bindings = generated_artifacts::mandatory_role_bindings(&metadata.grant)?;
    let raw_ids = |record: &RunRecord| -> Result<BTreeSet<String>,String> {
        record.interface_state.get(owner).and_then(|state|state.get("output_artifact_checkpoints"))
            .map(|value|serde_json::from_value(value.clone()).map_err(|e|e.to_string())).unwrap_or_else(||Ok(BTreeSet::new()))
    };
    let ownership = |record: &RunRecord| -> Result<BTreeSet<String>, String> {
        let state = record.interface_state.get(owner).ok_or("checked owner has no interface state")?;
        let values = state.get("owned_checkpoint_ids").and_then(serde_json::Value::as_array).ok_or("checked owner has no exact checkpoint set")?;
        let mut ids = BTreeSet::new();
        for value in values {
            let id = value.as_str().filter(|s| !s.is_empty()).ok_or("checked owner checkpoint id is invalid")?;
            if !ids.insert(id.into()) { return Err("checked owner repeats a checkpoint id".into()); }
            let cp = record.checkpoint(&CheckpointId::new(id)).ok_or("checked owner checkpoint is absent")?;
            let raw = raw_ids(record)?.contains(id);
            if raw && (!cp.outbound_interface_bindings.is_empty() || cp.artifact_bindings != role_bindings || !cp.required_verified_artifacts.is_empty()
                || cp.interface_binding.as_ref().is_none_or(|binding| !matches!(&binding.config.artifact_policy,BindingArtifactPolicy::Allow { names,namespaces } if namespaces.is_empty() && names.len()==1 && names[0].strip_prefix("tool.output.").is_some_and(|hash| ContentHash::from_hex(hash).is_some())))) {
                return Err("checked owner raw output is not an exact admission-only checkpoint".into());
            }
            if !record.active_checkpoints.contains(&cp.id) || (!raw && cp.inbound_interface() != Some(owner) && cp.outbound_interfaces().next() != Some(owner)) {
                return Err("checked owner claims a foreign or inactive checkpoint".into());
            }
        }
        let frontier = owner_frontier(state)?;
        if detached.is_some() {
            if !frontier.is_empty() { return Err("detached owner retains a target authoring frontier".into()); }
            return Ok(ids);
        }
        if frontier.len() != 2 || frontier.iter().any(|id| !ids.contains(id)) { return Err("checked owner frontier is not two owned edges".into()); }
        let result = frontier.iter().filter_map(|id| record.checkpoint(&CheckpointId::new(id))).filter(|cp| cp.inbound_interface() == Some(&transition.writer.door) && cp.outbound_interfaces().collect::<Vec<_>>() == [owner]).count();
        if result != 1 { return Err("checked owner lost its one private authoring return".into()); }
        Ok(ids)
    };
    let before_owned = ownership(&before)?;
    let after_owned = ownership(&after)?;
    let stable = |record: &RunRecord| -> Result<BTreeMap<String,serde_json::Value>,String> {
        let mut state = record.interface_state.get(owner).ok_or("checked owner state absent")?.clone();
        state.remove(FRONTIER_CHECKPOINT_IDS);state.remove("owned_checkpoint_ids");state.remove("output_artifact_checkpoints");state.remove("generated_artifacts");state.remove("input_artifact_bindings");Ok(state)
    };
    if stable(&before)? != stable(&after)? { return Err("checked owner changed immutable interface configuration".into()); }
    let before_outputs = generated_artifacts::recorded_generated_artifacts(&before,owner)?;
    let after_outputs = generated_artifacts::recorded_generated_artifacts(&after,owner)?;
    if before_outputs.iter().any(|old|!after_outputs.contains(old)) { return Err("checked owner removed or replaced a generated artifact".into()); }
    let mut output_ids = BTreeSet::new();
    for output in &after_outputs {
        if !output_ids.insert(output.artifact.id.clone()) || output.admission_seq >= transition.call.seq.0
            || output.source_movement_seq >= output.admission_seq { return Err("checked owner generated artifact has invalid identity or timing".into()); }
    }
    let without_outputs = |bindings: &[ArtifactBinding]| bindings.iter().filter(|binding|!output_ids.contains(&binding.id)).cloned().collect::<Vec<_>>();
    let before_frontier = owner_frontier(before.interface_state.get(owner).ok_or("checked owner state absent")?)?;
    let returns = before_frontier.iter().filter_map(|id| before.checkpoint(&CheckpointId::new(id)))
        .filter(|cp| cp.inbound_interface() == Some(&transition.writer.door)
            && cp.outbound_interfaces().collect::<Vec<_>>() == [owner]).collect::<Vec<_>>();
    let original = if let Some(binding) = &detached {
        without_outputs(&binding.role_artifact_bindings)
    } else {
        let [first] = returns.as_slice() else { return Err("checked owner has no unique original authoring return".into()); };
        without_outputs(&first.artifact_bindings)
    };
    if role_bindings.iter().any(|role| !original.iter().any(|binding| binding.id == role.id && binding.access.read)) {
        return Err("checked owner role grant differs from its original admitted bindings".into());
    }
    let mut context_inputs = Vec::new();
    for (provider, state) in &before.interface_state {
        let Some(value) = state.get(RUN_AUTHORING_BINDING) else { continue; };
        let topology: BoundRunAuthoringTopology = serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
        topology.validate_identity(&before, provider)?;
        for template in topology.checkpoints.iter().filter(|template| template.role == RunAuthoringCheckpointRole::OwnerInput
            && template.include_context && template.owner_authoring.as_ref() == Some(&transition.writer)) {
            context_inputs.push((template.clone(), provider_context_bindings(&before, provider)?));
        }
    }
    for (record, ids, publishing) in [(&before,&before_owned,false),(&after,&after_owned,true)] {
        let raw = raw_ids(record)?;
        if !raw.is_subset(ids) { return Err("checked owner does not own its raw output checkpoints".into()); }
        for id in ids {
            if raw.contains(id) { continue; }
            let checkpoint = record.checkpoint(&CheckpointId::new(id)).ok_or("checked owner row absent")?;
            let bindings = &checkpoint.artifact_bindings;
            if let Some((template, context)) = context_inputs.iter().find(|(template, _)| checkpoint_record_matches_template(checkpoint, template, &record.id)) {
                if !checkpoint_role_bindings_match(checkpoint, &role_bindings, template)
                    || bindings.iter().any(|binding| !binding.access.read)
                    || (publishing && *bindings != input_artifacts::read_bindings(record, owner, context.clone())?) {
                    return Err("checked input owner changed its declared provider context bindings".into());
                }
                continue;
            }
            if bindings.iter().any(|binding|!binding.access.read || (output_ids.contains(&binding.id) && binding.access != octopus_policy::ArtifactAccess::read_only())) || without_outputs(bindings) != original {
                return Err("checked owner changed its exact role, prelude or topology bindings".into());
            }
        }
    }
    validate_unchanged_run_neighbors(&transition.before,&transition.after,owner,&before_owned,&after_owned,false)
}
