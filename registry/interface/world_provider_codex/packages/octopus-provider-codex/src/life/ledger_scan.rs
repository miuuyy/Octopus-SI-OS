use super::*;

pub(crate) fn find_artifact_admission(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    source_movement_seq: u64,
    expected: &ArtifactRef,
) -> Result<ArtifactAdmissionScan, String> {
    let (mut file, snapshot_len, max_row_bytes) = open_arbitration_snapshot(ctx)?;
    let decoder = DurableMovementDecoder::for_log(&ctx.movements_path);
    let (start_offset, expected_seq) = authoritative_source_scan_start(
        ctx,
        &mut file,
        snapshot_len,
        max_row_bytes,
        &decoder,
        source_movement_seq,
    );
    let mut source_verified = false;
    let mut admitted = None;
    let scan = scan_authoritative_snapshot(
        ctx,
        &mut file,
        snapshot_len,
        max_row_bytes,
        &decoder,
        start_offset,
        expected_seq,
        None,
        |movement| {
            if movement.seq.0 < source_movement_seq {
                return Ok(None::<()>);
            }
            if movement.seq.0 == source_movement_seq {
                if !movement_is_provider_source(ctx, run, &movement) {
                    return Err(format!(
                        "provider arbitration source movement {} does not match interface '{}' run '{}'",
                        source_movement_seq,
                        ctx.interface.as_str(),
                        run.as_str()
                    ));
                }
                source_verified = true;
                return Ok(None::<()>);
            }
            if !source_verified {
                return Err(format!(
                    "provider arbitration scan advanced past causal source {source_movement_seq}"
                ));
            }
            if movement_is_artifact_admission(ctx, run, expected, &movement)
                && admitted.replace(movement.seq.0).is_some()
            {
                return Err(format!(
                    "artifact '{}' has duplicate provider admissions",
                    expected.id
                ));
            }
            Ok(None::<()>)
        },
    )?;
    ensure_arbitration_snapshot_length(ctx, &file, snapshot_len)?;
    Ok(ArtifactAdmissionScan {
        movement_seq: admitted,
        complete_boundary: scan.complete_boundary,
    })
}

pub(crate) fn authoritative_source_scan_start(
    ctx: &InterfaceLiftContext,
    file: &mut std::fs::File,
    snapshot_len: u64,
    max_row_bytes: u64,
    decoder: &DurableMovementDecoder,
    source_movement_seq: u64,
) -> (u64, u64) {
    indexed_movement_row_hint(ctx, source_movement_seq, snapshot_len)
        .and_then(|range| {
            read_indexed_movement_candidate(ctx, file, snapshot_len, max_row_bytes, decoder, range)
                .ok()
                .filter(|movement| movement.seq.0 == source_movement_seq)
                .map(|_| (range.start_offset, source_movement_seq))
        })
        .or_else(|| row_start_before(file, snapshot_len, max_row_bytes, source_movement_seq))
        .unwrap_or((0, 0))
}

fn row_start_before(
    file: &mut std::fs::File,
    snapshot_len: u64,
    max_row_bytes: u64,
    source_movement_seq: u64,
) -> Option<(u64, u64)> {
    std::io::Seek::seek(file, std::io::SeekFrom::Start(0)).ok()?;
    let mut reader = std::io::BufReader::new(std::io::Read::take(&mut *file, snapshot_len));
    let mut offset = 0u64;
    for _ in 0..source_movement_seq {
        match read_bounded_arbitration_row(&mut reader, max_row_bytes).ok()? {
            ArbitrationLedgerRow::Complete(row) => offset = offset.checked_add(row.len() as u64)?,
            _ => return None,
        }
    }
    Some((offset, source_movement_seq))
}

pub(crate) struct AuthoritativeSnapshotScan<T> {
    pub(crate) found: Option<T>,
    pub(crate) complete_boundary: bool,
}

pub(crate) fn open_arbitration_snapshot(
    ctx: &InterfaceLiftContext,
) -> Result<(std::fs::File, u64, u64), String> {
    let file = std::fs::File::open(&ctx.movements_path).map_err(|error| {
        format!(
            "movement ledger '{}' cannot be read during provider arbitration: {error}",
            ctx.movements_path.display()
        )
    })?;
    let snapshot_len = file
        .metadata()
        .map_err(|error| {
            format!(
                "movement ledger '{}' cannot be inspected during provider arbitration: {error}",
                ctx.movements_path.display()
            )
        })?
        .len();
    let max_row_bytes = ctx.required_host_io_limits()?.max_control_frame_bytes;
    Ok((file, snapshot_len, max_row_bytes))
}

pub(crate) fn ensure_arbitration_snapshot_length(
    ctx: &InterfaceLiftContext,
    file: &std::fs::File,
    snapshot_len: u64,
) -> Result<(), String> {
    let current_len = file
        .metadata()
        .map_err(|error| {
            format!(
                "movement ledger '{}' cannot be reinspected during provider arbitration: {error}",
                ctx.movements_path.display()
            )
        })?
        .len();
    if current_len < snapshot_len {
        return Err(format!(
            "movement ledger '{}' shrank from captured snapshot byte {snapshot_len} to {current_len}",
            ctx.movements_path.display()
        ));
    }
    Ok(())
}

pub(crate) fn indexed_movement_row_hint(
    ctx: &InterfaceLiftContext,
    seq: u64,
    snapshot_len: u64,
) -> Option<octopus_ledger_index::IndexedMovementRowRange> {
    let index = octopus_ledger_index::LedgerSqliteReader::open(&ctx.ledger_index_path).ok()?;
    let progress = index.progress().ok()?;
    if seq >= progress.watermark || progress.durable_end_offset > snapshot_len {
        return None;
    }
    let range = index.movement_row_range(seq).ok()??;
    (range.seq == seq
        && range.start_offset < range.end_offset
        && range.end_offset <= progress.durable_end_offset
        && range.end_offset <= snapshot_len)
        .then_some(range)
}

pub(crate) fn read_indexed_movement_candidate(
    ctx: &InterfaceLiftContext,
    file: &mut std::fs::File,
    snapshot_len: u64,
    max_row_bytes: u64,
    decoder: &DurableMovementDecoder,
    range: octopus_ledger_index::IndexedMovementRowRange,
) -> Result<octopus_ledger::Movement, String> {
    let row_len = range
        .end_offset
        .checked_sub(range.start_offset)
        .ok_or_else(|| "indexed provider arbitration row range underflowed".to_string())?;
    if range.end_offset > snapshot_len || row_len == 0 || row_len > max_row_bytes {
        return Err(format!(
            "indexed provider arbitration movement {} has unusable byte range [{}, {})",
            range.seq, range.start_offset, range.end_offset
        ));
    }
    let row_len = usize::try_from(row_len).map_err(|_| {
        format!(
            "indexed provider arbitration movement {} row length exceeds addressable memory",
            range.seq
        )
    })?;
    std::io::Seek::seek(file, std::io::SeekFrom::Start(range.start_offset)).map_err(|error| {
        format!(
            "movement ledger '{}' cannot seek to indexed movement {}: {error}",
            ctx.movements_path.display(),
            range.seq
        )
    })?;
    let mut row = vec![0; row_len];
    std::io::Read::read_exact(file, &mut row).map_err(|error| {
        format!(
            "movement ledger '{}' ended before indexed movement {}: {error}",
            ctx.movements_path.display(),
            range.seq
        )
    })?;
    let row = complete_arbitration_row(&row)?;
    if row.iter().all(|byte| byte.is_ascii_whitespace()) {
        return Err(format!(
            "indexed provider arbitration movement {} points to an empty row",
            range.seq
        ));
    }
    let line = usize::try_from(range.seq).map_err(|_| {
        format!(
            "provider arbitration movement sequence {} exceeds the decoder line range",
            range.seq
        )
    })?;
    let movement = decoder.decode(row, line).map_err(|error| {
        format!(
            "movement ledger '{}' has malformed indexed row {} during provider arbitration: {error}",
            ctx.movements_path.display(),
            range.seq
        )
    })?;
    if movement.seq.0 != range.seq {
        return Err(format!(
            "indexed provider arbitration movement {} resolves to movement {}",
            range.seq, movement.seq.0
        ));
    }
    Ok(movement)
}

pub(crate) fn scan_authoritative_snapshot<T>(
    ctx: &InterfaceLiftContext,
    file: &mut std::fs::File,
    snapshot_len: u64,
    max_row_bytes: u64,
    decoder: &DurableMovementDecoder,
    start_offset: u64,
    mut expected_seq: u64,
    stop_before_seq: Option<u64>,
    mut visit: impl FnMut(octopus_ledger::Movement) -> Result<Option<T>, String>,
) -> Result<AuthoritativeSnapshotScan<T>, String> {
    let remaining = snapshot_len.checked_sub(start_offset).ok_or_else(|| {
        format!(
            "provider arbitration scan offset {start_offset} exceeds captured ledger length {snapshot_len}"
        )
    })?;
    std::io::Seek::seek(file, std::io::SeekFrom::Start(start_offset)).map_err(|error| {
        format!(
            "movement ledger '{}' cannot seek to provider arbitration offset {start_offset}: {error}",
            ctx.movements_path.display()
        )
    })?;
    let snapshot = std::io::Read::take(file, remaining);
    let mut reader = std::io::BufReader::new(snapshot);
    let mut consumed = start_offset;
    loop {
        if let Some(stop_before_seq) = stop_before_seq {
            if expected_seq == stop_before_seq {
                return Ok(AuthoritativeSnapshotScan {
                    found: None,
                    complete_boundary: true,
                });
            }
            if expected_seq > stop_before_seq {
                return Err(format!(
                    "provider arbitration scan started after requested ledger boundary {stop_before_seq}"
                ));
            }
        }
        match read_bounded_arbitration_row(&mut reader, max_row_bytes)? {
            ArbitrationLedgerRow::Complete(row) => {
                consumed = consumed
                    .checked_add(row.len() as u64)
                    .ok_or_else(|| "provider arbitration ledger offset overflowed".to_string())?;
                let row = complete_arbitration_row(&row)?;
                if row.iter().all(|byte| byte.is_ascii_whitespace()) {
                    return Err(format!(
                        "movement ledger '{}' has an empty complete row at movement {} during provider arbitration",
                        ctx.movements_path.display(),
                        expected_seq
                    ));
                }
                let line = usize::try_from(expected_seq).map_err(|_| {
                    format!(
                        "provider arbitration movement sequence {expected_seq} exceeds the decoder line range"
                    )
                })?;
                let movement = decoder.decode(row, line).map_err(|error| {
                    format!(
                        "movement ledger '{}' has malformed row {} during provider arbitration: {error}",
                        ctx.movements_path.display(),
                        expected_seq
                    )
                })?;
                if movement.seq.0 != expected_seq {
                    return Err(format!(
                        "provider arbitration expected movement {}, found {}",
                        expected_seq, movement.seq.0
                    ));
                }
                if let Some(found) = visit(movement)? {
                    return Ok(AuthoritativeSnapshotScan {
                        found: Some(found),
                        complete_boundary: false,
                    });
                }
                expected_seq = expected_seq.checked_add(1).ok_or_else(|| {
                    "provider arbitration movement sequence overflowed".to_string()
                })?;
            }
            ArbitrationLedgerRow::Partial(row) => {
                consumed = consumed
                    .checked_add(row.len() as u64)
                    .ok_or_else(|| "provider arbitration ledger offset overflowed".to_string())?;
                if consumed != snapshot_len {
                    return Err(format!(
                        "movement ledger '{}' ended at byte {consumed} before captured snapshot byte {snapshot_len}",
                        ctx.movements_path.display()
                    ));
                }
                return Ok(AuthoritativeSnapshotScan {
                    found: None,
                    complete_boundary: false,
                });
            }
            ArbitrationLedgerRow::Eof => {
                if consumed != snapshot_len {
                    return Err(format!(
                        "movement ledger '{}' ended at byte {consumed} before captured snapshot byte {snapshot_len}",
                        ctx.movements_path.display()
                    ));
                }
                return Ok(AuthoritativeSnapshotScan {
                    found: None,
                    complete_boundary: stop_before_seq.is_none(),
                });
            }
        }
    }
}

pub(crate) fn complete_arbitration_row(row: &[u8]) -> Result<&[u8], String> {
    row.strip_suffix(b"\n")
        .map(|row| row.strip_suffix(b"\r").unwrap_or(row))
        .ok_or_else(|| "provider arbitration complete row lost its newline".to_string())
}

pub(crate) fn movement_is_artifact_admission(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    expected: &ArtifactRef,
    movement: &octopus_ledger::Movement,
) -> bool {
    movement.kind == MovementKind::InterfaceArrival
        && movement.from == Endpoint::Interface(ctx.interface.clone())
        && movement.runtime().is_some_and(|runtime| {
            movement.to == Endpoint::Interface(runtime.worker.clone())
                && runtime.interface == ctx.interface
        })
        && movement
            .invocation_metadata
            .as_ref()
            .and_then(octopus_ledger::MovementMetadata::invocation)
            .filter(|metadata| metadata.verification_failure.is_none())
            .and_then(octopus_ledger::InvocationMetadata::runtime)
            .is_some_and(|runtime| &runtime.run == run)
        && movement.artifact.as_ref() == Some(expected)
}

pub(crate) fn movement_is_provider_source(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    movement: &octopus_ledger::Movement,
) -> bool {
    movement.kind == MovementKind::InterfaceOutbound
        && matches!(&movement.from, Endpoint::Interface(_))
        && movement.to == Endpoint::Interface(ctx.interface.clone())
        && movement.artifact.is_some()
        && movement
            .invocation_metadata
            .as_ref()
            .and_then(octopus_ledger::MovementMetadata::invocation)
            .and_then(octopus_ledger::InvocationMetadata::runtime)
            .is_some_and(|runtime| &runtime.run == run && &runtime.interface == &ctx.interface)
}

pub(crate) enum ArbitrationLedgerRow {
    Complete(Vec<u8>),
    Partial(Vec<u8>),
    Eof,
}

pub(crate) fn read_bounded_arbitration_row(
    reader: &mut impl std::io::BufRead,
    max_row_bytes: u64,
) -> Result<ArbitrationLedgerRow, String> {
    let mut row = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|error| format!("provider arbitration ledger read failed: {error}"))?;
        if available.is_empty() {
            return Ok(if row.is_empty() {
                ArbitrationLedgerRow::Eof
            } else {
                ArbitrationLedgerRow::Partial(row)
            });
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |index| index + 1);
        if row.len() as u64 + take as u64 > max_row_bytes {
            return Err(format!(
                "provider arbitration ledger row exceeds declared {max_row_bytes} byte limit"
            ));
        }
        row.extend_from_slice(&available[..take]);
        reader.consume(take);
        if newline.is_some() {
            return Ok(ArbitrationLedgerRow::Complete(row));
        }
    }
}

pub(crate) async fn await_artifact_admission_scan(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    source_movement_seq: u64,
    expected: &ArtifactRef,
) -> Result<ArtifactAdmissionScan, String> {
    for _ in 0..128 {
        let scan = find_artifact_admission(ctx, run, source_movement_seq, expected)?;
        if scan.movement_seq.is_some() || scan.complete_boundary {
            return Ok(scan);
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    Err(format!(
        "movement ledger '{}' retained an incomplete tail while provider admission '{}' was pending",
        ctx.movements_path.display(),
        expected.id
    ))
}

pub(crate) async fn control_source_gate(
    control_gates: &ControlSourceGates,
    run: &RunId,
) -> Arc<tokio::sync::Mutex<()>> {
    let mut gates = control_gates.lock().await;
    gates.retain(|_, gate| gate.strong_count() > 0);
    if let Some(gate) = gates.get(run.as_str()).and_then(std::sync::Weak::upgrade) {
        return gate;
    }
    let gate = Arc::new(tokio::sync::Mutex::new(()));
    gates.insert(run.as_str().to_string(), Arc::downgrade(&gate));
    gate
}
