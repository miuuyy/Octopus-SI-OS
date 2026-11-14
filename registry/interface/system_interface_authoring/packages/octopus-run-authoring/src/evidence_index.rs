use super::*;

impl ProviderHandoffSession {
    pub fn new(ctx: &InterfaceLiftContext) -> Result<Self, String> {
        let limits = ctx.required_host_io_limits()?;
        let movements_path = ctx.movements_path.canonicalize().map_err(|error| {
            format!(
                "provider ledger '{}' cannot be canonicalized: {error}",
                ctx.movements_path.display()
            )
        })?;
        Ok(Self {
            interface: ctx.interface.clone(),
            movements_path: movements_path.clone(),
            writer_history: Arc::new(Mutex::new(BTreeMap::new())),
            writer_call_generations: Arc::new(Mutex::new(writer_run::WriterCallIndex::default())),
            evidence: ProviderLedgerEvidenceIndex::shared(
                ctx.interface.clone(),
                movements_path,
                limits.max_control_frame_bytes.max(1024),
                &ctx.ledger_index_path,
            ),
        })
    }

    pub fn from_invocation(
        &self,
        ctx: &InterfaceLiftContext,
        descriptor: &InterfaceDescriptor,
        run: &RunId,
        input: &InterfaceInvocationEnvelope,
        source_movement_seq: u64,
        pinned_run_record: &[u8],
    ) -> Result<ProviderOutputHandoff, String> {
        self.from_invocation_state(ctx, descriptor, run, input, source_movement_seq, pinned_run_record).map_err(Into::into)
    }

    pub async fn from_invocation_when_ready(
        &self, ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, run: &RunId,
        input: &InterfaceInvocationEnvelope, source_movement_seq: u64, pinned_run_record: &[u8],
    ) -> Result<ProviderOutputHandoff, CheckedRunError> {
        run_authoring_profile(descriptor)?;
        await_checked_evidence(ctx, || {
            self.from_invocation_state(ctx, descriptor, run, input, source_movement_seq, pinned_run_record)
        }).await
    }

    pub(crate) fn from_invocation_state(
        &self, ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, run: &RunId,
        input: &InterfaceInvocationEnvelope, source_movement_seq: u64, pinned_run_record: &[u8],
    ) -> Result<ProviderOutputHandoff, CheckedRunError> {
        self.validate_context(ctx)?;
        ProviderOutputHandoff::from_invocation_with_session(
            self.clone(),
            ctx,
            descriptor,
            run,
            input,
            source_movement_seq,
            pinned_run_record,
        )
    }

    pub(crate) fn validate_context(&self, ctx: &InterfaceLiftContext) -> Result<(), String> {
        let movements_path = ctx.movements_path.canonicalize().map_err(|error| {
            format!(
                "provider ledger '{}' cannot be canonicalized: {error}",
                ctx.movements_path.display()
            )
        })?;
        if self.interface != ctx.interface || self.movements_path != movements_path {
            return Err(format!(
                "provider handoff session belongs to interface '{}' ledger '{}', invocation uses interface '{}' ledger '{}'",
                self.interface.as_str(),
                self.movements_path.display(),
                ctx.interface.as_str(),
                movements_path.display()
            ));
        }
        Ok(())
    }

    pub(crate) fn index(&self) -> Result<std::sync::MutexGuard<'_, ProviderLedgerEvidenceIndex>, String> {
        self.evidence
            .lock()
            .map_err(|_| "provider handoff ledger index lock is poisoned".to_string())
    }

    /// POLICY.md §5: a run's place changes only through a write by another run whose
    /// grant covers its record. An input whose pinned record names any other place fails
    /// closed while that edit stands; restoring the place heals the run.
    pub fn validate_run_placement(
        &self, ctx: &InterfaceLiftContext, run: &RunId, pinned_run_record: &[u8], source_movement_seq: u64,
    ) -> Result<(), String> {
        let record = RunRecord::from_bytes(pinned_run_record).map_err(|error| error.to_string())?;
        if record.id != *run {
            return Err(format!("provider input for run '{}' pins run '{}'", run.as_str(), record.id.as_str()));
        }
        let receipts = ctx.interface_var_dir().join("placements");
        let proven = match placement::read_receipt(&receipts, run)? {
            Some(proven) => Some(proven),
            None => self.index()?.first_run_placement(run)?,
        };
        let Some((proven_seq, proven_ee)) = proven else { return Ok(()); };
        if proven_ee == record.ee.as_str() {
            return Ok(());
        }
        let limits = ctx.required_host_io_limits()?;
        let evidence = CheckedRunEvidence {
            movements_path: &ctx.movements_path, ledger_index_path: &ctx.ledger_index_path, blobs_dir: &ctx.blobs_dir,
            max_control_frame_bytes: limits.max_control_frame_bytes, max_manifest_bytes: limits.max_manifest_bytes,
            max_payload_bytes: limits.max_payload_bytes,
        };
        let path = ctx.root_slot("run_records_root")?.join(format!("{}.json", run.as_str()));
        placement::prove_move(&evidence, &path, &record, &proven_ee, proven_seq, source_movement_seq)?;
        placement::write_receipt(&receipts, run, &record.ee, source_movement_seq)
    }
}

impl ProviderLedgerEvidenceIndex {
    pub(crate) fn new(interface: InterfaceId, movements_path: PathBuf, max_row_bytes: u64) -> Self {
        let retained_delivery = pending_delivery::Ledger::new(interface.clone(), movements_path.clone(), max_row_bytes);
        Self {
            interface,
            ledger: if movements_path.as_os_str().is_empty() { ledger_rows::LedgerRows::detached(&movements_path) } else { ledger_rows::LedgerRows::shared(&movements_path) },
            movements_path,
            max_row_bytes,
            spotlight: None,
            byte_offset: 0,
            observed_len: 0,
            next_seq: 0,
            through_seq: None,
            poison: None,
            handoff_metadata_poison: HashMap::new(),
            session_metadata_poison: HashMap::new(),
            context_metadata_poison: HashMap::new(),
            latest_run_movement: HashMap::new(),
            first_run_placements: HashMap::new(),
            provider_run_arrivals: HashMap::new(),
            session_run_movements: HashMap::new(),
            session_provider_arrivals: HashMap::new(),
            session_provider_inputs: HashMap::new(),
            coordinate_arrivals: HashMap::new(),
            pending_arrival_proofs: HashMap::new(),
            mark_arrival_coordinates: HashMap::new(),
            coordinate_artifacts: HashMap::new(),
            coordinate_outbounds: HashMap::new(),
            coordinate_marks: HashMap::new(),
            retained_delivery,
            ledger_file: None,
            tail: None,
        }
    }

    pub(crate) fn shared(
        interface: InterfaceId,
        movements_path: PathBuf,
        max_row_bytes: u64,
        ledger_index_path: &Path,
    ) -> Arc<Mutex<Self>> {
        type Shared = Vec<((InterfaceId, PathBuf, PathBuf, u64), Arc<Mutex<ProviderLedgerEvidenceIndex>>)>;
        static SHARED: std::sync::OnceLock<Mutex<Shared>> = std::sync::OnceLock::new();
        let key = (interface.clone(), movements_path.clone(), ledger_index_path.to_path_buf(), max_row_bytes);
        let mut shared = SHARED.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(position) = shared.iter().position(|(known, _)| *known == key) {
            let (known, index) = shared.remove(position);
            if index.try_lock().is_ok_and(|index| index.reusable()) {
                shared.push((known, index.clone()));
                return index;
            }
        }
        let index = Arc::new(Mutex::new(Self::with_spotlight(interface, movements_path, max_row_bytes, ledger_index_path)));
        shared.push((key, index.clone()));
        if shared.len() > 16 { shared.remove(0); }
        index
    }

    fn reusable(&self) -> bool {
        if self.poison.is_some() || self.through_seq.is_some() { return false; }
        let Ok(metadata) = std::fs::metadata(&self.movements_path) else { return false; };
        if metadata.len() < self.byte_offset.max(self.observed_len)
            || self.ledger_file.is_some_and(|file| file != ledger_rows::LedgerFile::from_metadata(&metadata)) {
            return false;
        }
        if self.byte_offset == 0 { return true; }
        let Some((start, digest)) = self.tail else { return false; };
        let Ok(length) = usize::try_from(self.byte_offset - start) else { return false; };
        let mut row = vec![0; length];
        std::fs::File::open(&self.movements_path)
            .and_then(|mut file| file.seek(std::io::SeekFrom::Start(start)).and_then(|_| file.read_exact(&mut row)))
            .is_ok_and(|_| ContentHash::of_bytes(&row) == digest)
    }

    pub(crate) fn with_spotlight(
        interface: InterfaceId,
        movements_path: PathBuf,
        max_row_bytes: u64,
        ledger_index_path: &Path,
    ) -> Self {
        let mut index = Self::new(interface, movements_path, max_row_bytes);
        index.spotlight = LedgerSqliteReader::open(ledger_index_path).ok();
        index
    }

    pub(crate) fn first_run_placement(&mut self, run: &RunId) -> Result<Option<(u64, String)>, String> {
        self.catch_up()?;
        Ok(self.first_run_placements.get(run.as_str()).cloned())
    }

    pub(crate) fn catch_up(&mut self) -> Result<(), String> {
        if let Some(error) = &self.poison {
            return Err(error.clone());
        }
        if let Err(error) = self.catch_up_unpoisoned() {
            self.poison = Some(error.clone());
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn catch_up_unpoisoned(&mut self) -> Result<(), String> {
        let metadata = std::fs::metadata(&self.movements_path)
            .map_err(|error| {
                format!(
                    "provider ledger index cannot stat '{}': {error}",
                    self.movements_path.display()
                )
            })?;
        let length = metadata.len();
        self.ledger_file.get_or_insert(ledger_rows::LedgerFile::from_metadata(&metadata));
        let watermark = self.byte_offset.max(self.observed_len);
        if length < watermark {
            return Err(format!(
                "provider ledger '{}' shrank behind observed watermark {} to {}",
                self.movements_path.display(),
                watermark,
                length
            ));
        }
        self.observed_len = length;
        if length == self.byte_offset {
            return Ok(());
        }
        if self.through_seq.is_none() && self.catch_up_spotlight_prefix(length)? && length == self.byte_offset {
            return Ok(());
        }
        self.catch_up_full_tail()
    }

    pub(crate) fn catch_up_spotlight_prefix(&mut self, length: u64) -> Result<bool, String> {
        let Some(spotlight) = &self.spotlight else {
            return Ok(false);
        };
        let progress = match spotlight.progress() {
            Ok(progress) => progress,
            Err(_) => {
                self.spotlight = None;
                return Ok(false);
            }
        };
        if progress.watermark <= self.next_seq {
            return Ok(false);
        }
        if progress.durable_end_offset < self.byte_offset || progress.durable_end_offset > length {
            self.spotlight = None;
            return Ok(false);
        }
        let indexed_rows = match spotlight.movement_rows_for_runtime_interface(
            self.interface.as_str(),
            self.next_seq,
            progress.watermark,
        ) {
            Ok(Some(rows)) => rows,
            Ok(None) => return Ok(false),
            Err(_) => {
                self.spotlight = None;
                return Ok(false);
            }
        };
        let Some(movements) = self.read_spotlight_rows(
            &indexed_rows,
            progress.watermark,
            progress.durable_end_offset,
        ) else {
            return Ok(false);
        };
        for movement in &movements {
            self.note(movement)?;
        }
        self.byte_offset = progress.durable_end_offset;
        self.observed_len = self.observed_len.max(progress.durable_end_offset);
        self.next_seq = progress.watermark;
        self.tail = None;
        Ok(true)
    }

    pub(crate) fn read_spotlight_rows(
        &self,
        rows: &[octopus_ledger_index::IndexedMovementRowRange],
        watermark: u64,
        durable_end_offset: u64,
    ) -> Option<Vec<Movement>> {
        if rows.is_empty() {
            return Some(Vec::new());
        }
        let mut file = std::fs::File::open(&self.movements_path).ok()?;
        let mut movements = Vec::with_capacity(rows.len());
        let mut previous_seq = None;
        let mut previous_end = self.byte_offset;
        for indexed in rows {
            if indexed.seq < self.next_seq
                || indexed.seq >= watermark
                || indexed.start_offset < self.byte_offset
                || indexed.end_offset > durable_end_offset
                || indexed.end_offset <= indexed.start_offset
                || previous_seq.is_some_and(|seq| indexed.seq <= seq)
                || indexed.start_offset < previous_end
            {
                return None;
            }
            let row_len = indexed.end_offset.checked_sub(indexed.start_offset)?;
            if row_len > self.max_row_bytes {
                return None;
            }
            let row_len = usize::try_from(row_len).ok()?;
            file.seek(std::io::SeekFrom::Start(indexed.start_offset))
                .ok()?;
            let mut row = vec![0_u8; row_len];
            file.read_exact(&mut row).ok()?;
            if row.last() != Some(&b'\n') {
                return None;
            }
            let movement = self.ledger.decode(&row, indexed.seq).ok()?;
            if movement.seq.0 != indexed.seq {
                return None;
            }
            previous_seq = Some(indexed.seq);
            previous_end = indexed.end_offset;
            movements.push(movement);
        }
        Some(movements)
    }

    pub(crate) fn catch_up_full_tail(&mut self) -> Result<(), String> {
        let mut file = std::fs::File::open(&self.movements_path).map_err(|error| {
            format!(
                "provider ledger index cannot open '{}': {error}",
                self.movements_path.display()
            )
        })?;
        file.seek(std::io::SeekFrom::Start(self.byte_offset))
            .map_err(|error| format!("provider ledger index seek failed: {error}"))?;
        let ledger_file = ledger_rows::LedgerFile::of(&file)?;
        let mut reader = std::io::BufReader::new(file);
        loop {
            if self.through_seq.is_some_and(|last| self.next_seq > last) { return Ok(()); }
            match read_bounded_ledger_row(&mut reader, self.max_row_bytes)? {
                BoundedLedgerRow::Complete(row) => {
                    let movement = self
                        .ledger
                        .decode(&row, self.next_seq)
                        .map_err(|error| {
                        format!(
                            "provider ledger index encountered malformed movement at byte {}: {error}",
                            self.byte_offset
                        )
                    })?;
                    if movement.seq.0 != self.next_seq {
                        return Err(format!(
                            "provider ledger index expected movement seq {}, found {}",
                            self.next_seq, movement.seq.0
                        ));
                    }
                    self.note(&movement)?;
                    let start = self.byte_offset;
                    self.byte_offset = self
                        .byte_offset
                        .checked_add(row.len() as u64)
                        .ok_or_else(|| "provider ledger byte watermark overflowed".to_string())?;
                    let digest = ContentHash::of_bytes(&row);
                    self.tail = Some((start, digest));
                    self.ledger.note(ledger_file, self.next_seq, ledger_rows::LedgerRow {
                        start, end: self.byte_offset, digest, outbound: ledger_rows::OutboundRow::of(&movement),
                    })?;
                    self.observed_len = self.observed_len.max(self.byte_offset);
                    self.next_seq = self.next_seq.checked_add(1).ok_or_else(|| {
                        "provider ledger sequence watermark overflowed".to_string()
                    })?;
                }
                BoundedLedgerRow::Partial | BoundedLedgerRow::Eof => return Ok(()),
            }
        }
    }

    pub(crate) fn note(&mut self, movement: &Movement) -> Result<(), String> {
        let Some(artifact) = movement.artifact.as_ref() else {
            if movement.kind == MovementKind::InterfaceArrival
                && movement.from == Endpoint::Interface(self.interface.clone())
                && movement.invocation_metadata.as_ref().and_then(|metadata| metadata.invocation())
                    .is_some_and(|metadata| metadata.replaces_arrival_seq.is_some())
            {
                return Err("provider replacement arrival has no exact artifact".into());
            }
            return Ok(());
        };
        let artifact_key = artifact_ref_key(artifact);
        if let MovementKind::MarkOutcomeAdmitted(mark) = &movement.kind {
            if let Some(run) = self.related_mark_run(movement, artifact) {
                self.note_admitted_mark(movement, artifact, mark, &run);
            }
            return Ok(());
        }
        let addressed_to_exact_interface = match (&movement.kind, &movement.from, &movement.to) {
            (MovementKind::InterfaceArrival, Endpoint::Interface(interface), Endpoint::Interface(_)) => {
                interface == &self.interface
            }
            (
                MovementKind::InterfaceOutbound,
                Endpoint::Interface(_),
                Endpoint::Interface(interface),
            ) => interface == &self.interface,
            _ => false,
        };
        if !addressed_to_exact_interface {
            return Ok(());
        }
        let metadata = movement
            .invocation_metadata
            .as_ref()
            .ok_or_else(|| {
                format!(
                    "provider exact-interface movement {} carries no invocation metadata",
                    movement.seq.0
                )
            })?
            .invocation()
            .cloned()
            .ok_or_else(|| {
                format!(
                    "provider exact-interface movement {} carries non-invocation metadata",
                    movement.seq.0
                )
            })?;
        let Some(runtime) = valid_interface_movement_runtime(movement, &metadata) else {
            return Err(format!(
                "provider exact-interface movement {} carries mismatched runtime coordinates",
                movement.seq.0
            ));
        };
        if runtime.interface != self.interface {
            return Err(format!(
                "provider exact-interface movement {} names interface '{}' instead of '{}'",
                movement.seq.0,
                runtime.interface.as_str(),
                self.interface.as_str()
            ));
        }
        let run_key = runtime.run.as_str().to_string();
        self.first_run_placements
            .entry(run_key.clone())
            .or_insert_with(|| (movement.seq.0, runtime.ee.as_str().to_string()));
        self.latest_run_movement
            .insert((artifact_key.clone(), run_key.clone()), movement.seq.0);
        self.session_run_movements
            .entry((artifact_key.clone(), run_key.clone()))
            .or_default()
            .insert(movement.seq.0);
        let coordinate = RuntimeCoordinateKey::from_runtime(runtime);
        if movement.kind == MovementKind::InterfaceArrival {
            let coordinate_key = (artifact_key.clone(), coordinate.clone());
            let replaced = match metadata.replaces_arrival_seq {
                Some(sequence) => {
                    let prior = self.pending_arrival_proofs.get(&coordinate_key)
                        .filter(|prior| prior.seq.0 == sequence)
                        .filter(|prior| matches!(self.coordinate_arrivals.get(&coordinate_key),
                            Some(UniqueEvidence::One(seq)) if *seq == prior.seq.0))
                        .filter(|prior| octopus_ledger::replaces_pending_interface_arrival(prior, movement))
                        .ok_or("provider replacement arrival has no unique exact unmarked predecessor")?;
                    let label = &metadata.checkpoint_admission.as_ref().unwrap().artifact_label;
                    let candidates = self.coordinate_artifacts.get(&coordinate).into_iter().flatten()
                        .filter_map(|(artifact, _)| self.pending_arrival_proofs.get(&(artifact_ref_key(artifact), coordinate.clone())))
                        .filter(|arrival| arrival.invocation_metadata.as_ref().and_then(|metadata| metadata.invocation())
                            .and_then(|metadata| metadata.checkpoint_admission.as_ref())
                            .is_some_and(|admission| &admission.artifact_label == label)).count();
                    if candidates != 1 || !matches!(self.mark_arrival_coordinates.get(&(
                        artifact_key.clone(), run_key.clone(), coordinate.checkpoint.clone(), coordinate.epoch)),
                        Some(UniqueArrivalCoordinate::One(existing)) if existing == &coordinate)
                    {
                        return Err("provider replacement arrival has ambiguous pending requirement evidence".into());
                    }
                    Some(prior.seq.0)
                }
                None => None,
            };
            let context_key = (
                artifact_key.clone(),
                run_key.clone(),
                runtime.interface.as_str().to_string(),
            );
            if let Some(sequence) = replaced {
                for sequences in [self.provider_run_arrivals.get_mut(&context_key),
                    self.session_provider_arrivals.get_mut(&context_key),
                    self.session_run_movements.get_mut(&(artifact_key.clone(), run_key.clone()))]
                    .into_iter().flatten()
                {
                    sequences.remove(&sequence);
                }
                if let Some(artifacts) = self.coordinate_artifacts.get_mut(&coordinate) {
                    artifacts.retain(|(_, seq)| *seq != sequence);
                }
            }
            self.provider_run_arrivals
                .entry(context_key.clone())
                .or_default()
                .insert(movement.seq.0);
            self.session_provider_arrivals
                .entry(context_key)
                .or_default()
                .insert(movement.seq.0);
            match self.coordinate_arrivals.entry(coordinate_key.clone()) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(UniqueEvidence::One(movement.seq.0));
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    entry.insert(if replaced.is_some() { UniqueEvidence::One(movement.seq.0) }
                        else { UniqueEvidence::Repeated });
                }
            }
            if octopus_ledger::interface_arrival_mark_proof_key(movement).is_some()
                && matches!(self.coordinate_arrivals.get(&coordinate_key),
                    Some(UniqueEvidence::One(seq)) if *seq == movement.seq.0)
            {
                self.pending_arrival_proofs.insert(coordinate_key, movement.clone());
            } else {
                self.pending_arrival_proofs.remove(&coordinate_key);
            }
            let mark_coordinate_key = (
                artifact_key.clone(),
                run_key,
                coordinate.checkpoint.clone(),
                coordinate.epoch,
            );
            match self.mark_arrival_coordinates.entry(mark_coordinate_key) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(UniqueArrivalCoordinate::One(coordinate.clone()));
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    if !matches!(
                        entry.get(),
                        UniqueArrivalCoordinate::One(existing) if existing == &coordinate
                    ) {
                        entry.insert(UniqueArrivalCoordinate::Repeated);
                    }
                }
            }
            self.coordinate_artifacts
                .entry(coordinate.clone())
                .or_default()
                .push((artifact.clone(), movement.seq.0));
        } else {
            let Endpoint::Interface(source) = &movement.from;
            if let (Some(admission), Some(binding)) = (
                metadata.checkpoint_admission.as_ref(),
                metadata.checkpoint_binding_policy.as_ref(),
            ) {
                if source != &self.interface
                    && admission.complete
                    && metadata.verification_failure.is_none()
                    && binding.inbound.interface == *source
                    && binding.inbound.admits(&admission.artifact_label)
                    && metadata.outbound_interface_bindings.contains(&self.interface)
                    && binding.outbound.iter().any(|target| {
                        target.interface == self.interface
                            && target.admits(&admission.artifact_label)
                    })
                    && admission.required_artifact_labels == metadata.required_verified_artifacts
                        .iter().map(|required| required.artifact_label.clone()).collect::<Vec<_>>()
                {
                    self.session_provider_inputs
                        .entry((artifact_key, run_key))
                        .or_default()
                        .insert(movement.seq.0, admission.artifact_label.clone());
                }
            }
            self.coordinate_outbounds
                .entry(coordinate)
                .or_default()
                .push(IndexedOutboundArtifact {
                    source: source.clone(),
                    worker: runtime.worker.clone(),
                    artifact: artifact.clone(),
                    seq: movement.seq.0,
                    run_record_hash: runtime.run_record_hash.clone(),
                    outbound_interface_bindings: metadata.outbound_interface_bindings.clone(),
                    checkpoint_binding_policy: metadata.checkpoint_binding_policy.clone(),
                    required_verified_artifacts: metadata.required_verified_artifacts.clone(),
                    artifact_binding_refs: metadata.artifact_binding_refs.clone(),
                    readable_artifacts: metadata.grant.artifact_access.iter().filter(|(_, access)| access.read).map(|(id, _)| ArtifactId::new(id)).collect(),
                    role_artifacts: metadata.grant.role_artifacts.clone(),
                });
        }
        Ok(())
    }

    pub(crate) fn related_mark_run(&self, movement: &Movement, artifact: &ArtifactRef) -> Option<String> {
        let artifact_key = artifact_ref_key(artifact);
        if let Some(mark) = movement
            .invocation_metadata
            .as_ref()
            .and_then(|metadata| metadata.mark_outcome())
        {
            let key = (
                artifact_key,
                mark.run.as_str().to_string(),
                mark.subject.as_str().to_string(),
                mark.run_epoch,
            );
            return self
                .mark_arrival_coordinates
                .contains_key(&key)
                .then(|| mark.run.as_str().to_string());
        }
        None
    }

    pub(crate) fn note_admitted_mark(
        &mut self,
        movement: &Movement,
        artifact: &ArtifactRef,
        admitted_mark: &octopus_descriptors::MarkId,
        related_run: &str,
    ) {
        let artifact_key = artifact_ref_key(artifact);
        let result = (|| -> Result<(RuntimeCoordinateKey, String), String> {
            let raw = movement.invocation_metadata.as_ref().ok_or_else(|| {
                format!(
                    "provider recovery admitted mark movement {} has no metadata",
                    movement.seq.0
                )
            })?;
            let mark = raw.mark_outcome().ok_or_else(|| {
                format!(
                    "provider recovery admitted mark movement {} metadata is not a Mark outcome",
                    movement.seq.0
                )
            })?;
            if mark.name != *admitted_mark {
                return Err(format!(
                    "provider recovery admitted mark movement {} kind differs from its metadata",
                    movement.seq.0
                ));
            }
            if mark.evidence.as_ref() != Some(artifact) {
                return Err(format!(
                    "provider recovery admitted mark movement {} evidence differs from its artifact",
                    movement.seq.0
                ));
            }
            if mark.verifier.as_str().is_empty()
                || mark.verifier.as_str().trim() != mark.verifier.as_str()
            {
                return Err(format!(
                    "provider recovery admitted mark movement {} has no canonical verifier id",
                    movement.seq.0
                ));
            }
            let mark_coordinate_key = (
                artifact_key.clone(),
                mark.run.as_str().to_string(),
                mark.subject.as_str().to_string(),
                mark.run_epoch,
            );
            let coordinate = match self.mark_arrival_coordinates.get(&mark_coordinate_key) {
                Some(UniqueArrivalCoordinate::One(coordinate)) => coordinate,
                Some(UniqueArrivalCoordinate::Repeated) => {
                    return Err(format!(
                        "provider recovery admitted mark movement {} matches multiple arrival coordinates",
                        movement.seq.0
                    ));
                }
                None => {
                    return Err(format!(
                        "provider recovery admitted mark movement {} has no exact preceding arrival",
                        movement.seq.0
                    ));
                }
            };
            if movement.from != Endpoint::Interface(InterfaceId::new(&coordinate.interface))
                || movement.to != Endpoint::Interface(InterfaceId::new(&coordinate.worker))
            {
                return Err(format!(
                    "provider recovery admitted mark movement {} endpoints differ from its proving arrival",
                    movement.seq.0
                ));
            }
            let arrival = self
                .coordinate_arrivals
                .get(&(artifact_key.clone(), coordinate.clone()))
                .ok_or_else(|| {
                    format!(
                        "provider recovery admitted mark movement {} has no exact preceding arrival",
                        movement.seq.0
                    )
                })?;
            let verifier = mark.verifier.as_str().to_string();
            match arrival {
                UniqueEvidence::One(seq) if *seq < movement.seq.0 => {
                    let pending = self.pending_arrival_proofs.get(&(artifact_key.clone(), coordinate.clone()));
                    if mark.arrival_seq.is_some() || pending.is_some() {
                        if !pending.is_some_and(|arrival| octopus_ledger::mark_outcome_proves_interface_arrival(arrival, movement)) {
                            return Err("provider recovery mark does not prove the exact current arrival prefix".into());
                        }
                    }
                    Ok((coordinate.clone(), verifier))
                }
                UniqueEvidence::One(_) => Err(format!(
                    "provider recovery admitted mark movement {} does not follow its exact arrival",
                    movement.seq.0
                )),
                UniqueEvidence::Repeated => Err(format!(
                    "provider recovery admitted mark movement {} follows repeated arrival evidence",
                    movement.seq.0
                )),
            }
        })();
        let (coordinate, verifier) = match result {
            Ok(evidence) => evidence,
            Err(error) => {
                if !related_run.is_empty() && related_run.trim() == related_run {
                    self.handoff_metadata_poison
                        .entry((artifact_key, related_run.to_string()))
                        .or_insert(error);
                }
                return;
            }
        };
        self.pending_arrival_proofs.remove(&(artifact_key.clone(), coordinate.clone()));
        let mark_key = (
            artifact_key,
            coordinate,
            admitted_mark.as_str().to_string(),
            verifier,
        );
        match self.coordinate_marks.entry(mark_key) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(UniqueEvidence::One(movement.seq.0));
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                entry.insert(UniqueEvidence::Repeated);
            }
        }
    }

    pub(crate) fn require_interface(&self, interface: &InterfaceId) -> Result<(), String> {
        if interface != &self.interface {
            return Err(format!(
                "provider ledger evidence index belongs to exact interface '{}', requested '{}'",
                self.interface.as_str(),
                interface.as_str()
            ));
        }
        Ok(())
    }

    pub(crate) fn require_coordinate_interface(
        &self,
        coordinate: &RuntimeCoordinateKey,
    ) -> Result<(), String> {
        if coordinate.interface != self.interface.as_str() {
            return Err(format!(
                "provider ledger evidence index belongs to exact interface '{}', coordinate names '{}'",
                self.interface.as_str(),
                coordinate.interface
            ));
        }
        Ok(())
    }

    pub(crate) fn bound_provenance<'a>(
        &mut self,
        run: &RunId,
        provider: &InterfaceId,
        artifacts: impl Iterator<Item = &'a ArtifactRef>,
    ) -> Result<BTreeMap<String, BoundArtifactProvenance>, String> {
        self.require_interface(provider)?;
        self.catch_up()?;
        let mut result = BTreeMap::new();
        for artifact in artifacts {
            let artifact_key = artifact_ref_key(artifact);
            let context_key = (
                artifact_key.clone(),
                run.as_str().to_string(),
                provider.as_str().to_string(),
            );
            if let Some(error) = self.context_metadata_poison.get(&context_key) {
                return Err(error.clone());
            }
            result.insert(
                artifact_key.clone(),
                BoundArtifactProvenance {
                    movement_seq: self
                        .latest_run_movement
                        .get(&(artifact_key.clone(), run.as_str().to_string()))
                        .copied(),
                    provider_arrival_seq: self
                        .provider_run_arrivals
                        .get(&context_key)
                        .and_then(|seqs| seqs.iter().next_back().copied()),
                },
            );
        }
        Ok(result)
    }

    pub(crate) fn session_evidence(
        &mut self,
        run: &RunId,
        provider: &InterfaceId,
        artifacts: impl Iterator<Item = String>,
    ) -> Result<BTreeMap<String, SessionSourceLedgerEvidence>, String> {
        self.require_interface(provider)?;
        self.catch_up()?;
        let mut result = BTreeMap::new();
        for artifact_key in artifacts {
            if let Some(error) = self
                .session_metadata_poison
                .get(&(artifact_key.clone(), run.as_str().to_string()))
            {
                return Err(error.clone());
            }
            result.insert(
                artifact_key.clone(),
                SessionSourceLedgerEvidence {
                    movements: self
                        .session_run_movements
                        .get(&(artifact_key.clone(), run.as_str().to_string()))
                        .cloned()
                        .unwrap_or_default(),
                    provider_inputs: self
                        .session_provider_inputs
                        .get(&(artifact_key.clone(), run.as_str().to_string()))
                        .cloned()
                        .unwrap_or_default(),
                    provider_arrivals: self
                        .session_provider_arrivals
                        .get(&(
                            artifact_key,
                            run.as_str().to_string(),
                            provider.as_str().to_string(),
                        ))
                        .cloned()
                        .unwrap_or_default(),
                },
            );
        }
        Ok(result)
    }

    pub(crate) fn admitted_handoff_seq(
        &mut self,
        coordinate: PendingHandoffLedgerCoordinate<'_>,
    ) -> Result<Option<u64>, String> {
        self.require_interface(coordinate.interface)?;
        self.catch_up()?;
        self.indexed_handoff_seq(coordinate, u64::MAX)
    }

    pub(crate) fn indexed_handoff_seq(&self, coordinate: PendingHandoffLedgerCoordinate<'_>, before_seq: u64) -> Result<Option<u64>, String> {
        let artifact_key = artifact_ref_key(coordinate.artifact);
        if let Some(error) = self
            .handoff_metadata_poison
            .get(&(artifact_key.clone(), coordinate.run.as_str().to_string()))
        {
            return Err(error.clone());
        }
        let runtime = RuntimeCoordinateKey::new(
            coordinate.run,
            coordinate.worker,
            coordinate.interface,
            coordinate.checkpoint,
            coordinate.epoch,
        );
        let arrival = match self
            .coordinate_arrivals
            .get(&(artifact_key.clone(), runtime.clone()))
        {
            None => return Ok(None),
            Some(UniqueEvidence::One(seq)) => *seq,
            Some(UniqueEvidence::Repeated) => {
                return Err(format!(
                    "provider recovery found repeated admitted arrival evidence for artifact '{}' at checkpoint '{}'",
                    coordinate.artifact.id.as_str(),
                    coordinate.checkpoint.as_str()
                ));
            }
        };
        if arrival >= before_seq { return Ok(None); }
        if let Some((mark, verifier)) = coordinate.required_evidence {
            let mark_seq = match self.coordinate_marks.get(&(
                artifact_key,
                runtime,
                mark.to_string(),
                verifier.to_string(),
            )) {
                None => return Ok(None),
                Some(UniqueEvidence::One(seq)) => *seq,
                Some(UniqueEvidence::Repeated) => {
                    return Err(format!(
                        "provider recovery found repeated admitted mark '{}' for artifact '{}' at checkpoint '{}'",
                        mark,
                        coordinate.artifact.id.as_str(),
                        coordinate.checkpoint.as_str()
                    ));
                }
            };
            if mark_seq <= arrival {
                return Err(format!(
                    "provider recovery mark '{}' for artifact '{}' does not follow its arrival at checkpoint '{}'",
                    mark,
                    coordinate.artifact.id.as_str(),
                    coordinate.checkpoint.as_str()
                ));
            }
            if mark_seq >= before_seq { return Ok(None); }
        }
        Ok(Some(arrival))
    }

    pub(crate) fn admitted_slot_artifacts(
        &mut self,
        coordinate: &RuntimeCoordinateKey,
        required_mark: &str,
        required_verifier: &str,
    ) -> Result<Vec<OneShotAdmittedArtifact>, String> {
        self.require_coordinate_interface(coordinate)?;
        self.catch_up()?;
        let mut admitted = Vec::new();
        for (artifact, seq) in self
            .coordinate_artifacts
            .get(coordinate)
            .into_iter()
            .flatten()
        {
            let artifact_key = artifact_ref_key(artifact);
            if let Some(error) = self
                .handoff_metadata_poison
                .get(&(artifact_key.clone(), coordinate.run.clone()))
            {
                return Err(error.clone());
            }
            match self
                .coordinate_arrivals
                .get(&(artifact_key.clone(), coordinate.clone()))
            {
                Some(UniqueEvidence::Repeated) => {
                    return Err(format!(
                        "provider one-shot checkpoint '{}' has repeated arrival evidence for artifact '{}'",
                        coordinate.checkpoint,
                        artifact.id.as_str()
                    ));
                }
                Some(UniqueEvidence::One(_)) => {}
                None => continue,
            }
            let mark_seq = match self.coordinate_marks.get(&(
                artifact_key,
                coordinate.clone(),
                required_mark.to_string(),
                required_verifier.to_string(),
            )) {
                None => continue,
                Some(UniqueEvidence::One(mark_seq)) => *mark_seq,
                Some(UniqueEvidence::Repeated) => {
                    return Err(format!(
                        "provider one-shot checkpoint '{}' has repeated mark '{}' for artifact '{}'",
                        coordinate.checkpoint,
                        required_mark,
                        artifact.id.as_str()
                    ));
                }
            };
            if mark_seq <= *seq {
                return Err(format!(
                    "provider one-shot checkpoint '{}' mark '{}' for artifact '{}' does not follow its arrival",
                    coordinate.checkpoint,
                    required_mark,
                    artifact.id.as_str()
                ));
            }
            admitted.push(OneShotAdmittedArtifact {
                artifact: artifact.clone(),
                arrival_seq: *seq,
                mark_seq,
            });
        }
        if admitted.len() > 1 {
            return Err(format!(
                "provider one-shot checkpoint '{}' has {} admitted artifacts",
                coordinate.checkpoint,
                admitted.len()
            ));
        }
        Ok(admitted)
    }

    pub(crate) fn outbound_artifacts(
        &mut self, coordinate: &RuntimeCoordinateKey, source: &InterfaceId, after_seq: Option<u64>,
    ) -> Result<Vec<IndexedOutboundArtifact>, String> {
        self.require_coordinate_interface(coordinate)?;
        self.catch_up()?;
        Ok(self.coordinate_outbounds.get(coordinate).into_iter().flatten()
            .filter(|evidence| evidence.source == *source && after_seq.is_none_or(|after| evidence.seq > after))
            .cloned().collect())
    }

    pub(crate) fn unique_outbound_artifact(
        &mut self,
        coordinate: &RuntimeCoordinateKey,
        source: &InterfaceId,
        after_seq: Option<u64>,
    ) -> Result<Option<IndexedOutboundArtifact>, String> {
        self.require_coordinate_interface(coordinate)?;
        self.catch_up()?;
        let mut matches = self
            .coordinate_outbounds
            .get(coordinate)
            .into_iter()
            .flatten()
            .filter(|evidence| {
                evidence.source == *source && after_seq.map_or(true, |after| evidence.seq > after)
            });
        let evidence = matches.next().cloned();
        if matches.next().is_some() {
            return Err(format!(
                "provider checkpoint '{}' has repeated outbound artifact evidence from '{}'",
                coordinate.checkpoint,
                source.as_str()
            ));
        }
        Ok(evidence)
    }

    pub(crate) fn exact_provider_source_outbound(
        &mut self,
        run: &RunId,
        provider: &InterfaceId,
        sequence: u64,
    ) -> Result<(RuntimeCoordinateKey, IndexedOutboundArtifact), String> {
        self.require_interface(provider)?;
        self.catch_up()?;
        let mut matches = self
            .coordinate_outbounds
            .iter()
            .filter(|(coordinate, _)| {
                coordinate.run == run.as_str()
                    && coordinate.interface == provider.as_str()
            })
            .flat_map(|(coordinate, evidence)| {
                evidence
                    .iter()
                    .filter(move |item| item.seq == sequence)
                    .map(move |item| (coordinate.clone(), item.clone()))
            });
        let found = matches.next().ok_or_else(|| {
            format!(
                "provider source movement {sequence} has no exact outbound evidence for run '{}' and interface '{}'",
                run.as_str(),
                provider.as_str()
            )
        })?;
        if matches.next().is_some() {
            return Err(format!(
                "provider source movement {sequence} has repeated exact outbound evidence for run '{}' and interface '{}'",
                run.as_str(),
                provider.as_str()
            ));
        }
        Ok(found)
    }
}

pub(crate) fn valid_interface_movement_runtime<'a>(
    movement: &Movement,
    metadata: &'a InvocationMetadata,
) -> Option<&'a octopus_interfaces::RuntimeInvocationMetadata> {
    let runtime = metadata.runtime()?;
    match (&movement.kind, &movement.from, &movement.to) {
        (MovementKind::InterfaceArrival, Endpoint::Interface(source), Endpoint::Interface(worker))
            if source == &runtime.interface && worker == &runtime.worker =>
        {
            Some(runtime)
        }
        (MovementKind::InterfaceOutbound, Endpoint::Interface(_), Endpoint::Interface(target))
            if target == &runtime.interface =>
        {
            Some(runtime)
        }
        _ => None,
    }
}

impl RuntimeCoordinateKey {
    pub(crate) fn new(
        run: &RunId,
        worker: &InterfaceId,
        interface: &InterfaceId,
        checkpoint: &CheckpointId,
        epoch: u64,
    ) -> Self {
        Self {
            run: run.as_str().to_string(),
            worker: worker.as_str().to_string(),
            interface: interface.as_str().to_string(),
            checkpoint: checkpoint.as_str().to_string(),
            epoch,
        }
    }

    pub(crate) fn from_runtime(runtime: &octopus_interfaces::RuntimeInvocationMetadata) -> Self {
        Self::new(
            &runtime.run,
            &runtime.worker,
            &runtime.interface,
            &runtime.checkpoint,
            runtime.epoch,
        )
    }
}

pub(crate) fn read_bounded_ledger_row(
    reader: &mut impl BufRead,
    max_row_bytes: u64,
) -> Result<BoundedLedgerRow, String> {
    let mut row = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|error| format!("provider ledger index read failed: {error}"))?;
        if available.is_empty() {
            return Ok(if row.is_empty() {
                BoundedLedgerRow::Eof
            } else {
                BoundedLedgerRow::Partial
            });
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |index| index + 1);
        if row.len() as u64 + take as u64 > max_row_bytes {
            return Err(format!(
                "provider ledger row exceeds the declared {} byte limit",
                max_row_bytes
            ));
        }
        row.extend_from_slice(&available[..take]);
        reader.consume(take);
        if newline.is_some() {
            return Ok(BoundedLedgerRow::Complete(row));
        }
    }
}
