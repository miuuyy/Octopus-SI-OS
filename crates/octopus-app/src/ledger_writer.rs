use std::ops::ControlFlow;
use std::sync::{Arc, Mutex};

use octopus_ledger::{Movement, MovementDraft, MovementLog, MovementMetadata, Seq};
use octopus_ledger_index::{LedgerSqliteIndex, SqliteIndexError};

#[derive(Clone)]
pub struct LedgerWriter {
    log: Arc<Mutex<Box<dyn MovementLog + Send>>>,
    spotlight: Arc<Mutex<Option<LedgerSqliteIndex>>>,
    movement_wakes: Option<octopus_interface_transport::InterfaceMovementWakes>,
}

impl LedgerWriter {
    pub fn new<L: MovementLog + Send + 'static>(
        log: L,
        spotlight: Option<LedgerSqliteIndex>,
    ) -> Self {
        Self {
            log: Arc::new(Mutex::new(Box::new(log))),
            spotlight: Arc::new(Mutex::new(spotlight)),
            movement_wakes: None,
        }
    }

    pub fn with_movement_wakes(
        mut self,
        movement_wakes: Option<octopus_interface_transport::InterfaceMovementWakes>,
    ) -> Self {
        self.movement_wakes = movement_wakes;
        self
    }

    fn index_appended_movements(
        &self,
        index: &mut LedgerSqliteIndex,
        through: Seq,
    ) -> Result<(), SqliteIndexError> {
        loop {
            let from = index.watermark()?;
            let snapshot = {
                let log = self
                    .log
                    .lock()
                    .expect("ledger writer mutex is not poisoned");
                let len = log.len();
                if from > len as u64 {
                    return Err(SqliteIndexError::LogBehindIndex {
                        watermark: from,
                        log_len: len as u64,
                    });
                }
                if from == len as u64 || from > through.0 {
                    return Ok(());
                }
                let mut rows = Vec::new();
                log.scan_from(Seq(from), &mut |mut movement| {
                    if let Some(MovementMetadata::Invocation(metadata)) =
                        movement.invocation_metadata.as_mut()
                    {
                        metadata.grant = Default::default();
                    }
                    let range = log.durable_row_byte_range(movement.seq);
                    rows.push((movement, range));
                    if rows.len() == 256 {
                        ControlFlow::Break(())
                    } else {
                        ControlFlow::Continue(())
                    }
                });
                let end = (from as usize + 256).min(len);
                if rows.len() != end - from as usize {
                    return Err(SqliteIndexError::InconsistentSnapshot {
                        message: "ledger tail ended before its recorded length".into(),
                    });
                }
                let durable_end = if end == len {
                    log.durable_end_offset()
                } else {
                    rows.last().and_then(|(_, range)| range.map(|(_, end)| end))
                };
                LedgerIndexSnapshot {
                    rows,
                    len: end,
                    durable_end,
                }
            };
            index.ingest_from(&snapshot)?;
        }
    }
}

struct LedgerIndexSnapshot {
    rows: Vec<(Movement, Option<(u64, u64)>)>,
    len: usize,
    durable_end: Option<u64>,
}

impl MovementLog for LedgerIndexSnapshot {
    fn append(&mut self, _: MovementDraft) -> Movement {
        panic!("ledger index snapshots are read-only")
    }
    fn read_from(&self, from: Seq) -> Vec<Movement> {
        self.rows
            .iter()
            .filter(|(row, _)| row.seq >= from)
            .map(|(row, _)| row.clone())
            .collect()
    }
    fn len(&self) -> usize {
        self.len
    }
    fn durable_end_offset(&self) -> Option<u64> {
        self.durable_end
    }
    fn durable_row_byte_range(&self, seq: Seq) -> Option<(u64, u64)> {
        let first = self.rows.first()?.0.seq.0;
        self.rows.get(seq.0.checked_sub(first)? as usize)?.1
    }
}

impl MovementLog for LedgerWriter {
    fn append(&mut self, draft: MovementDraft) -> Movement {
        let movement = self
            .log
            .lock()
            .expect("ledger writer mutex is not poisoned")
            .append(draft);
        let mut spotlight = self
            .spotlight
            .lock()
            .expect("ledger index mutex is not poisoned");
        let ingest_error = match spotlight.as_mut() {
            Some(index) => self.index_appended_movements(index, movement.seq).err(),
            None => None,
        };
        if let Some(error) = ingest_error {
            eprintln!(
                "run_daemon: ledger spotlight ingest failed (cache goes stale; ledger untouched; \
                 next boot rebuilds): {error}"
            );
            *spotlight = None;
        }
        drop(spotlight);
        if let Some(wakes) = &self.movement_wakes {
            for reader in octopus_interface_transport::movement_readers(&movement) {
                let _ = wakes.send((reader, movement.seq.0));
            }
        }
        movement
    }

    fn read_from(&self, from: Seq) -> Vec<Movement> {
        self.log
            .lock()
            .expect("ledger writer mutex is not poisoned")
            .read_from(from)
    }

    fn read_at(&self, seq: Seq) -> Option<Movement> {
        self.log
            .lock()
            .expect("ledger writer mutex is not poisoned")
            .read_at(seq)
    }

    fn len(&self) -> usize {
        self.log
            .lock()
            .expect("ledger writer mutex is not poisoned")
            .len()
    }

    fn durable_end_offset(&self) -> Option<u64> {
        self.log
            .lock()
            .expect("ledger writer mutex is not poisoned")
            .durable_end_offset()
    }

    fn durable_row_byte_range(&self, seq: Seq) -> Option<(u64, u64)> {
        self.log
            .lock()
            .expect("ledger writer mutex is not poisoned")
            .durable_row_byte_range(seq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_descriptors::InterfaceId;
    use octopus_ledger::{Endpoint, InMemoryLog, MovementKind, TraceId};

    fn moved(run: &str) -> MovementDraft {
        MovementDraft {
            trace: TraceId("t".into()),
            kind: MovementKind::ArtifactMoved,
            from: Endpoint::Interface(InterfaceId::new(run)),
            to: Endpoint::Interface(InterfaceId::new(run)),
            artifact: None,
            invocation_metadata: None,
            causal_floor: None,
        }
    }

    #[test]
    fn clones_share_one_serialized_append_order() {
        let writer = LedgerWriter::new(InMemoryLog::new(), None);
        let mut a = writer.clone();
        let mut b = writer.clone();
        let m0 = a.append(moved("a"));
        let m1 = b.append(moved("b"));
        let m2 = a.append(moved("a"));
        assert_eq!(m0.seq, Seq(0));
        assert_eq!(m1.seq, Seq(1));
        assert_eq!(m2.seq, Seq(2));
        assert_eq!(
            writer.len(),
            3,
            "every clone appends into the one shared log"
        );
        let rows = writer.read_from(Seq(0));
        assert_eq!(rows.len(), 3);
        assert!(
            rows.windows(2).all(|w| w[0].seq.0 + 1 == w[1].seq.0),
            "the single writer keeps a strict append-only seq order (movement is holy)"
        );
    }

    #[test]
    fn sparse_replay_reads_only_the_requested_movement() {
        use std::ops::ControlFlow;
        struct StreamingLog(InMemoryLog, Arc<std::sync::atomic::AtomicUsize>);
        impl MovementLog for StreamingLog {
            fn append(&mut self, draft: MovementDraft) -> Movement {
                self.0.append(draft)
            }
            fn read_from(&self, _: Seq) -> Vec<Movement> {
                panic!("sparse replay must stream")
            }
            fn len(&self) -> usize {
                self.0.len()
            }
            fn scan_from(&self, from: Seq, visit: &mut dyn FnMut(Movement) -> ControlFlow<()>) {
                self.0.scan_from(from, &mut |movement| {
                    self.1.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    visit(movement)
                });
            }
        }
        let visited = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut writer = LedgerWriter::new(StreamingLog(InMemoryLog::new(), visited.clone()), None);
        for _ in 0..1000 {
            writer.append(moved("a"));
        }
        for expected in [Seq(0), Seq(400), Seq(999)] {
            assert_eq!(writer.read_at(expected).unwrap().seq, expected);
        }
        assert_eq!(visited.load(std::sync::atomic::Ordering::Relaxed), 3);
    }

    #[test]
    fn index_contention_does_not_hold_the_ledger_and_catchup_preserves_offsets() {
        use octopus_ledger_store::DurableLog;
        use std::sync::mpsc;
        use std::time::Duration;
        let dir = tempfile::tempdir().unwrap();
        let mut log = DurableLog::open(dir.path().join("movements.jsonl")).unwrap();
        for _ in 0..260 {
            log.append(moved("seed"));
        }
        let index = LedgerSqliteIndex::open(&dir.path().join("ledger-index.sqlite")).unwrap();
        let writer = LedgerWriter::new(log, Some(index));
        let held = writer.spotlight.lock().unwrap();
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let mut writer = writer.clone();
                std::thread::spawn(move || writer.append(moved("parallel")))
            })
            .collect();
        let reader = writer.clone();
        let (sent, received) = mpsc::channel();
        let read = std::thread::spawn(move || {
            while reader.len() < 262 {
                std::thread::yield_now();
            }
            sent.send(reader.read_at(Seq(261))).unwrap();
        });
        let readable = received.recv_timeout(Duration::from_secs(5));
        drop(held);
        for handle in handles {
            handle.join().unwrap();
        }
        read.join().unwrap();
        assert_eq!(readable.unwrap().unwrap().seq, Seq(261));
        let mut held = writer.spotlight.lock().unwrap();
        let index = held.as_mut().unwrap();
        assert_eq!(index.watermark().unwrap(), 262);
        assert!(index.verify_against(&writer).unwrap());
    }
}
