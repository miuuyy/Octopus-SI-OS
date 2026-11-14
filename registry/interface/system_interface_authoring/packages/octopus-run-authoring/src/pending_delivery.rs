use super::*;
use octopus_ledger::ArtifactDelivery;
use octopus_ledger_index::{InterfaceArrivalProjection, RetainedArrivalState};

pub(super) struct Ledger {
    interface: InterfaceId,
    path: PathBuf,
    ledger: Arc<ledger_rows::LedgerRows>,
    max_row_bytes: u64,
    offset: u64,
    observed_length: u64,
    next_sequence: u64,
    index: InterfaceArrivalProjection,
    submitted: HashSet<(String, RuntimeCoordinateKey)>,
    scoped: HashSet<(String, RuntimeCoordinateKey)>,
    unattributed: BTreeSet<String>,
    arrival_scopes: HashMap<u64, (u64, String, RuntimeCoordinateKey)>,
}

impl Ledger {
    pub(super) fn new(interface: InterfaceId, path: PathBuf, max_row_bytes: u64) -> Self {
        Self {
            index: InterfaceArrivalProjection::new(interface.clone()),
            interface,
            ledger: if path.as_os_str().is_empty() { ledger_rows::LedgerRows::detached(&path) } else { ledger_rows::LedgerRows::shared(&path) },
            path,
            max_row_bytes,
            offset: 0,
            observed_length: 0,
            next_sequence: 0,
            submitted: HashSet::new(),
            scoped: HashSet::new(),
            unattributed: BTreeSet::new(),
            arrival_scopes: HashMap::new(),
        }
    }

    pub(super) fn watermark(&mut self) -> Result<u64, String> {
        self.refresh()?;
        Ok(self.next_sequence)
    }

    fn refresh(&mut self) -> Result<(), String> {
        let mut file = std::fs::File::open(&self.path).map_err(|error| error.to_string())?;
        let length = file.metadata().map_err(|error| error.to_string())?.len();
        if length < self.observed_length.max(self.offset) {
            return Err("retained provider output ledger shrank behind its observed prefix".into());
        }
        self.observed_length = length;
        file.seek(std::io::SeekFrom::Start(self.offset))
            .map_err(|error| error.to_string())?;
        let mut reader = std::io::BufReader::new(file);
        loop {
            let row = match read_bounded_ledger_row(&mut reader, self.max_row_bytes)? {
                BoundedLedgerRow::Complete(row) => row,
                BoundedLedgerRow::Partial => {
                    return Err("retained provider output requires a complete ledger prefix".into());
                }
                BoundedLedgerRow::Eof => return Ok(()),
            };
            // Reading this interface's delivery evidence does not hydrate every
            // other interface's grants. The global cursor still checks every
            // row's JSON and sequence, without manufacturing skipped movements.
            #[derive(serde::Deserialize)]
            struct Route { seq: u64, from: Endpoint }
            let route: Route = serde_json::from_slice(&row).map_err(|error| error.to_string())?;
            if route.seq != self.next_sequence {
                return Err("retained output ledger is not contiguous".into());
            }
            let next_offset = self.offset.checked_add(row.len() as u64)
                .ok_or("retained output ledger offset overflow")?;
            if route.from != Endpoint::Interface(self.interface.clone()) {
                self.next_sequence += 1;
                self.offset = next_offset;
                continue;
            }
            let movement = self
                .ledger
                .decode_with_grant_if(&row, self.next_sequence, |movement| {
                    movement.kind == MovementKind::InterfaceArrival
                        && movement.invocation_metadata.as_ref().and_then(|metadata| metadata.invocation())
                            .is_some_and(|metadata| !metadata.required_verified_artifacts.is_empty())
                })?;
            if let Some(artifact) = movement.artifact.as_ref().filter(|_| {
                movement.from == Endpoint::Interface(self.interface.clone())
                    && matches!(
                        movement.kind,
                        MovementKind::ArtifactMoved
                            | MovementKind::InterfaceArrival
                            | MovementKind::DeliveryRefused
                    )
            }) {
                let artifact = artifact_ref_key(artifact);
                match movement.invocation_metadata.as_ref() {
                    Some(octopus_ledger::MovementMetadata::ArrivalFanout(metadata)) => {
                        for target in &metadata.arrival_fanout.targets {
                            let key =
                                (artifact.clone(), RuntimeCoordinateKey::from_runtime(target));
                            self.submitted.insert(key.clone());
                            self.scoped.insert(key);
                        }
                    }
                    Some(metadata) => {
                        if let Some((scope, runtime)) = metadata.invocation()
                            .filter(|_| movement.kind == MovementKind::InterfaceArrival)
                            .and_then(|metadata| metadata.delivery_scope_seq.zip(metadata.runtime.as_ref()))
                        {
                            self.arrival_scopes.insert(movement.seq.0, (scope, artifact.clone(), RuntimeCoordinateKey::from_runtime(runtime)));
                        }
                        let runtime = match metadata {
                            octopus_ledger::MovementMetadata::Invocation(metadata) => {
                                metadata.runtime.as_ref()
                            }
                            octopus_ledger::MovementMetadata::Delivery(metadata) => {
                                metadata.runtime.as_ref()
                            }
                            _ => None,
                        };
                        if let Some(runtime) = runtime {
                            self.submitted
                                .insert((artifact, RuntimeCoordinateKey::from_runtime(runtime)));
                        } else {
                            self.unattributed.insert(artifact);
                        }
                    }
                    None => {
                        self.unattributed.insert(artifact);
                    }
                }
            }
            self.index
                .observe(&movement)
                .map_err(|error| error.to_string())?;
            self.next_sequence += 1;
            self.offset = next_offset;
        }
    }

    fn scope(&mut self, identity: &Identity) -> Result<Option<Scope>, String> {
        self.refresh()?;
        let Some(scope) = self
            .index
            .retained_arrival_scope(&identity.id()?)
            .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        if scope.artifact != Some(identity.artifact()?)
            || scope.seq.0 <= identity.source_movement_seq
        {
            return Err(
                "retained provider output scope changed its original source or artifact".into(),
            );
        }
        let edges = self
            .index
            .retained_arrival_edges(scope.seq.0)
            .map_err(|error| error.to_string())?;
        let has_output = edges.iter().any(|(coordinate, _)| {
            coordinate.run == identity.run
                && coordinate.interface == identity.provider
                && coordinate.checkpoint == identity.item.checkpoint.id
                && coordinate.epoch == identity.item.checkpoint.epoch
        });
        Ok(Some(Scope {
            seq: scope.seq.0,
            complete: edges
                .iter()
                .all(|(_, state)| matches!(state, RetainedArrivalState::Finished(_))),
            has_output,
        }))
    }

    pub(super) fn same_output_delivery(
        &mut self, run: &RunId, artifact: &ArtifactRef, left: u64, right: u64,
    ) -> Result<bool, String> {
        self.refresh()?;
        let (Some(a), Some(b)) = (self.arrival_scopes.get(&left), self.arrival_scopes.get(&right)) else {
            return Ok(false);
        };
        if a.0 != b.0 || a.1 != b.1 || a.1 != artifact_ref_key(artifact) { return Ok(false); }
        let edges = self.index.retained_arrival_edges(a.0).map_err(|error| error.to_string())?;
        let mut admitted = false;
        for (sequence, arrival) in [(left, a), (right, b)] {
            let Some((coordinate, RetainedArrivalState::Finished(verdict))) = edges.iter()
                .find(|(coordinate, _)| RuntimeCoordinateKey::from_runtime(coordinate) == arrival.2) else { return Ok(false); };
            if coordinate.run != *run || coordinate.interface != self.interface { return Ok(false); }
            if let DeliveryVerdict::Admitted { seq } = verdict {
                if *seq != sequence { return Ok(false); }
                admitted = true;
            }
        }
        Ok(admitted)
    }
}

struct Scope {
    seq: u64,
    complete: bool,
    has_output: bool,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    schema_version: u32,
    provider: InterfaceId,
    run: RunId,
    worker: InterfaceId,
    source_movement_seq: u64,
    item: PendingHandoffItem,
}

impl Identity {
    fn key(&self) -> Result<(String, RuntimeCoordinateKey), String> {
        Ok((
            artifact_ref_key(&self.artifact()?),
            RuntimeCoordinateKey::new(
                &self.run,
                &self.worker,
                &self.provider,
                &self.item.checkpoint.id,
                self.item.checkpoint.epoch,
            ),
        ))
    }

    fn id(&self) -> Result<String, String> {
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        Ok(format!(
            "provider-output:{}",
            ContentHash::of_bytes(&bytes).to_hex()
        ))
    }

    fn artifact(&self) -> Result<ArtifactRef, String> {
        Ok(ArtifactRef {
            id: self.item.artifact_id.clone(),
            content: ContentHash::from_hex(&self.item.artifact_content_hash)
                .ok_or("retained output has an invalid manifest hash")?,
        })
    }

    fn path(&self, ctx: &InterfaceLiftContext) -> Result<PathBuf, String> {
        Ok(ctx
            .interface_var_dir()
            .join("handoff-delivery")
            .join(format!(
                "{}.json",
                self.id()?.strip_prefix("provider-output:").unwrap()
            )))
    }

    fn exists(&self, ctx: &InterfaceLiftContext) -> Result<bool, String> {
        let path = self.path(ctx)?;
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.to_string()),
        };
        if bytes.len() as u64 > ctx.required_host_io_limits()?.max_control_frame_bytes {
            return Err("retained provider output identity exceeds the control-frame limit".into());
        }
        let saved: Self = serde_json::from_slice(&bytes)
            .map_err(|error| format!("retained provider output identity is corrupt: {error}"))?;
        if saved != *self {
            return Err(
                "retained provider output identity changed its exact pending contract".into(),
            );
        }
        Ok(true)
    }

    fn save(&self, ctx: &InterfaceLiftContext) -> Result<(), String> {
        use std::io::Write;
        let path = self.path(ctx)?;
        let parent = path
            .parent()
            .ok_or("retained output identity has no parent")?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| error.to_string())?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|error| error.to_string())?;
        std::fs::File::open(parent)
            .and_then(|file| file.sync_all())
            .map_err(|error| error.to_string())
    }
}

pub(super) fn identity_for(record: &RunRecord, provider: &InterfaceId, pending: &PendingHandoff) -> Result<String, String> {
    let [item] = pending.items.as_slice() else { return Err("retained identity requires one output".into()); };
    Identity { schema_version: 1, provider: provider.clone(), run: record.id.clone(), worker: record.worker.clone(),
        source_movement_seq: pending.source_movement_seq, item: item.clone() }.id()
}

impl ProviderOutputHandoff {
    pub(super) fn retained_output_identity(&self, pending: &PendingHandoff) -> Result<Option<(String, u64)>, String> {
        let [item] = pending.items.as_slice() else { return Err("rejected final requires exactly one output".into()); };
        let identity = self.pending_delivery_identity(pending, item)?;
        if !identity.exists(&self.ctx)? { return Ok(None); }
        let mut index = self.session.index()?;
        index.retained_delivery.refresh()?;
        Ok(Some((identity.id()?, index.retained_delivery.next_sequence)))
    }

    fn pending_delivery_identity(
        &self,
        pending: &PendingHandoff,
        item: &PendingHandoffItem,
    ) -> Result<Identity, String> {
        let worker = if pending.source_movement_seq == self.source_movement_seq {
            self.invocation_metadata.runtime.as_ref()
                .ok_or("retained provider output has no admitted source coordinate")?.worker.clone()
        } else {
            self.session.index()?.exact_provider_source_outbound(
                &self.config.run, &self.config.provider_interface, pending.source_movement_seq,
            )?.1.worker
        };
        Ok(Identity {
            schema_version: 1,
            provider: self.config.provider_interface.clone(),
            run: self.config.run.clone(),
            worker,
            source_movement_seq: pending.source_movement_seq,
            item: item.clone(),
        })
    }

    pub(super) fn pending_delivery_complete(
        &self,
        pending: &PendingHandoff,
        item: &PendingHandoffItem,
    ) -> Result<bool, String> {
        let identity = self.pending_delivery_identity(pending, item)?;
        if !identity.exists(&self.ctx)? {
            let mut index = self.session.index()?;
            index.retained_delivery.refresh()?;
            if index.retained_delivery.scoped.contains(&identity.key()?) {
                return Err(
                    "retained provider output lost its exact saved dispatch identity".into(),
                );
            }
            return Ok(true);
        }
        let scope = self
            .session
            .index()?
            .retained_delivery
            .scope(&identity)?
            .ok_or("provider output was admitted without its saved retained scope")?;
        if !scope.has_output {
            return Err("retained provider output scope omits its original checkpoint".into());
        }
        Ok(scope.complete)
    }

    pub(super) fn pending_output_has_submission(
        &self, pending: &PendingHandoff, item: &PendingHandoffItem,
    ) -> Result<bool, String> {
        let identity = self.pending_delivery_identity(pending, item)?;
        if identity.exists(&self.ctx)? { return Ok(true); }
        let (_, reference) = load_pending_artifact(&self.var_dir, item)?;
        let key = artifact_ref_key(&reference);
        let mut index = self.session.index()?;
        index.retained_delivery.refresh()?;
        Ok(index.retained_delivery.unattributed.contains(&key)
            || index.retained_delivery.submitted.iter().any(|(artifact, _)| artifact == &key))
    }

    fn prepare_pending_delivery(
        &self,
        pending: &PendingHandoff,
        item: &PendingHandoffItem,
    ) -> Result<Identity, String> {
        let identity = self.pending_delivery_identity(pending, item)?;
        if identity.exists(&self.ctx)? {
            return Ok(identity);
        }
        if !pending.dispatch_fenced {
            return Err("legacy provider pending output has no durable unsubmitted proof; an explicit checked transition is required".into());
        }
        let record = RunRecord::from_bytes(
            &std::fs::read(&self.run_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        if source_withdrawal::dispatch_path(&self.ctx, &record, item)
            .try_exists()
            .map_err(|error| error.to_string())?
        {
            return Err("legacy provider pending output was dispatched without a retained identity; its outcome remains unresolved".into());
        }
        {
            let mut index = self.session.index()?;
            index.retained_delivery.refresh()?;
            if index.retained_delivery.submitted.contains(&identity.key()?)
                || index
                    .retained_delivery
                    .unattributed
                    .contains(&artifact_ref_key(&identity.artifact()?))
            {
                return Err("provider pending output already has delivery evidence without its retained identity".into());
            }
        }
        identity.save(&self.ctx)?;
        Ok(identity)
    }

    pub(super) async fn hand_retained_pending(
        &self,
        pending: &PendingHandoff,
        item: &PendingHandoffItem,
        artifact: InterfaceArtifactFile,
    ) -> Result<DeliveryVerdict, String> {
        if self.ctx.cancel.is_cancelled() {
            return Err("provider pending output delivery was cancelled".into());
        }
        let limits = self.ctx.required_host_io_limits()?;
        let artifact = artifact
            .capture_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
            .map_err(|error| format!("provider pending output capture failed: {error}"))?;
        let captured = artifact
            .read_manifest_bounded(limits.max_manifest_bytes)
            .map_err(|error| format!("provider pending output identity failed: {error}"))?;
        let content = ContentHash::of_bytes(&captured.manifest_bytes);
        let captured_ref = ArtifactRef {
            id: ArtifactId::new(format!("{}:{}", captured.manifest.label, content.to_hex())),
            content,
        };
        let identity = self.prepare_pending_delivery(pending, item)?;
        if captured_ref != identity.artifact()? {
            return Err("provider pending output changed its retained artifact".into());
        }
        let delivery_id = identity.id()?;
        self.record_pending_dispatch(pending, item)?;
        let mut original_scope = self
            .session
            .index()?
            .retained_delivery
            .scope(&identity)?
            .map(|scope| scope.seq);
        let delivery = async {
            loop {
                let delivery = self
                    .ctx
                    .hand_arrival_retained(artifact.clone(), Vec::new(), &delivery_id)
                    .await?;
                if matches!(&delivery, ArtifactDelivery::Complete { verdict } if verdict.awaits_current_source()) {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    continue;
                }
                let scope = self
                    .session
                    .index()?
                    .retained_delivery
                    .scope(&identity)?
                    .ok_or("retained provider output reply has no durable scope")?;
                if !scope.has_output {
                    return Err("retained provider output scope omits its original checkpoint".into());
                }
                if *original_scope.get_or_insert(scope.seq) != scope.seq {
                    return Err("retained provider output changed its original scope".into());
                }
                match delivery {
                    ArtifactDelivery::Pending { scope_seq } if scope.seq == scope_seq => {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                    ArtifactDelivery::Pending { .. } => {
                        return Err("retained provider output changed its original scope".into());
                    }
                    ArtifactDelivery::Complete { verdict }
                        if scope.complete && !verdict.awaits_current_source() =>
                    {
                        return Ok(verdict);
                    }
                    ArtifactDelivery::Complete { .. } => {
                        return Err("retained provider output completed without all original edge decisions".into());
                    }
                }
            }
        };
        tokio::select! {
            biased;
            _ = self.ctx.cancel.cancelled() => Err("provider pending output delivery was cancelled".into()),
            result = delivery => result,
        }
    }
}
