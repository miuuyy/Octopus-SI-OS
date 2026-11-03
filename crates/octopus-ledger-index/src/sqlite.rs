use std::ops::ControlFlow;
use std::path::Path;

use octopus_ledger::{Endpoint, Movement, MovementKind, MovementLog, Seq};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

const SCHEMA_VERSION: i64 = 4;
pub const AUTHORITATIVE_LEDGER_SCAN_MOVEMENT_BUDGET: usize = 256;

const RUNTIME_INTERFACE_OUTBOUND_CANDIDATE_SQL: &str = "SELECT seq, start_offset, end_offset
     FROM movements INDEXED BY idx_mv_runtime_interface_outbound
     WHERE runtime_interface = ?1
       AND runtime_interface IS NOT NULL
       AND kind = 'InterfaceOutbound'
       AND from_kind = 'interface'
       AND seq >= ?2
       AND seq < ?3
     ORDER BY seq ASC
     LIMIT 1";

const ENDPOINT_INTERFACE_OUTBOUND_CANDIDATE_SQL: &str = "SELECT seq, start_offset, end_offset
     FROM movements INDEXED BY idx_mv_interface_outbound_to
     WHERE kind = 'InterfaceOutbound'
       AND from_kind = 'interface'
       AND to_kind = 'interface'
       AND to_id = ?1
       AND seq >= ?2
       AND seq < ?3
     ORDER BY seq ASC
     LIMIT 1";

#[derive(Debug, thiserror::Error)]
pub enum SqliteIndexError {
    #[error("ledger index sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("ledger index schema generation {actual} does not match expected {expected}")]
    SchemaGeneration { expected: u64, actual: u64 },
    #[error("ledger index metadata '{key}' is not an unsigned integer: '{value}'")]
    InvalidMetadata { key: String, value: String },
    #[error(
        "movement log ({log_len} rows) is behind the sqlite index watermark ({watermark}); \
         the index cache must be rebuilt from the ledger"
    )]
    LogBehindIndex { watermark: u64, log_len: u64 },
    #[error("movement log returned seq {got} where {expected} was expected (non-contiguous tail)")]
    NonContiguousTail { expected: u64, got: u64 },
    #[error(
        "lightweight movement projection has {projected} rows while the movement log has {log_len}"
    )]
    ProjectionLength { projected: u64, log_len: u64 },
    #[error("ledger index row {seq} has invalid durable byte range [{start_offset}, {end_offset})")]
    InvalidRowRange {
        seq: u64,
        start_offset: i64,
        end_offset: i64,
    },
    #[error("ledger index snapshot is inconsistent: {message}")]
    InconsistentSnapshot { message: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct IndexedMovement {
    pub seq: u64,
    pub lamport: u64,
    pub recorded_at_unix_ms: u64,
    pub trace: String,
    pub kind: String,
    pub mark: Option<String>,
    pub from_kind: String,
    pub from_id: String,
    pub to_kind: String,
    pub to_id: String,
    pub artifact_id: Option<String>,
    pub artifact_label: Option<String>,
    pub artifact_content: Option<String>,
    pub runtime_run: Option<String>,
    pub runtime_checkpoint: Option<String>,
    pub runtime_interface: Option<String>,
}

pub struct LedgerSqliteIndex {
    conn: Connection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LedgerIndexProgress {
    pub watermark: u64,
    pub durable_end_offset: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexedMovementRowRange {
    pub seq: u64,
    pub start_offset: u64,
    pub end_offset: u64,
}

pub struct LedgerSqliteReader {
    conn: Connection,
}

impl LedgerSqliteIndex {
    pub fn open(path: &Path) -> Result<Self, SqliteIndexError> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let mut index = Self { conn };
        index.ensure_schema()?;
        Ok(index)
    }

    pub fn open_healthy(
        path: &Path,
        log: &(dyn MovementLog + Send),
    ) -> Result<Self, SqliteIndexError> {
        let mut index = Self::open(path)?;
        if !index.verify_against(log)? {
            index.rebuild_from(log)?;
        } else {
            index.ingest_from(log)?;
        }
        Ok(index)
    }

    pub fn open_healthy_from_movements(
        path: &Path,
        log: &(dyn MovementLog + Send),
        movements: &[Movement],
    ) -> Result<Self, SqliteIndexError> {
        validate_complete_projection(log, movements)?;
        let mut index = Self::open(path)?;
        if !index.verify_against_movements(log, movements)? {
            index.rebuild_from_movements(log, movements)?;
        } else {
            index.ingest_from_movements(log, movements)?;
        }
        Ok(index)
    }

    fn ensure_schema(&mut self) -> Result<(), SqliteIndexError> {
        let version: Option<i64> = self
            .conn
            .query_row(
                "SELECT value FROM index_meta WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .unwrap_or(None)
            .and_then(|v| v.parse().ok());
        if version == Some(SCHEMA_VERSION) {
            self.ensure_query_indexes()?;
            return Ok(());
        }
        self.conn.execute_batch(
            "
            DROP TABLE IF EXISTS movements;
            DROP TABLE IF EXISTS index_meta;
            CREATE TABLE index_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE movements(
                seq INTEGER PRIMARY KEY,
                lamport INTEGER NOT NULL,
                recorded_at_unix_ms INTEGER NOT NULL,
                trace TEXT NOT NULL,
                kind TEXT NOT NULL,
                mark TEXT,
                from_kind TEXT NOT NULL,
                from_id TEXT NOT NULL,
                to_kind TEXT NOT NULL,
                to_id TEXT NOT NULL,
                artifact_id TEXT,
                artifact_label TEXT,
                artifact_content TEXT,
                runtime_run TEXT,
                runtime_checkpoint TEXT,
                runtime_interface TEXT,
                start_offset INTEGER,
                end_offset INTEGER
            );
            CREATE INDEX idx_mv_to ON movements(to_kind, to_id, seq);
            CREATE INDEX idx_mv_from ON movements(from_kind, from_id, seq);
            CREATE INDEX idx_mv_artifact ON movements(artifact_id) WHERE artifact_id IS NOT NULL;
            CREATE INDEX idx_mv_kind ON movements(kind, seq);
            CREATE INDEX idx_mv_mark ON movements(mark) WHERE mark IS NOT NULL;
            CREATE INDEX idx_mv_runtime_interface
                ON movements(runtime_interface, seq) WHERE runtime_interface IS NOT NULL;
            CREATE INDEX idx_mv_runtime_interface_outbound
                ON movements(runtime_interface, seq)
                WHERE runtime_interface IS NOT NULL
                  AND kind = 'InterfaceOutbound'
                  AND from_kind = 'interface';
            CREATE INDEX idx_mv_interface_outbound_to
                ON movements(to_id, seq)
                WHERE kind = 'InterfaceOutbound'
                  AND from_kind = 'interface'
                  AND to_kind = 'interface';
            CREATE INDEX idx_mv_runtime_ckpt
                ON movements(runtime_run, runtime_checkpoint)
                WHERE runtime_checkpoint IS NOT NULL;
            ",
        )?;
        self.ensure_query_indexes()?;
        self.set_meta("schema_version", &SCHEMA_VERSION.to_string())?;
        self.set_meta("watermark", "0")?;
        Ok(())
    }

    fn ensure_query_indexes(&self) -> Result<(), SqliteIndexError> {
        self.conn.execute_batch(
            "
            CREATE INDEX IF NOT EXISTS idx_mv_artifact_label
                ON movements(artifact_label, seq) WHERE artifact_label IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_mv_runtime_run
                ON movements(runtime_run, seq) WHERE runtime_run IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_mv_runtime_interface_outbound
                ON movements(runtime_interface, seq)
                WHERE runtime_interface IS NOT NULL
                  AND kind = 'InterfaceOutbound'
                  AND from_kind = 'interface';
            CREATE INDEX IF NOT EXISTS idx_mv_interface_outbound_to
                ON movements(to_id, seq)
                WHERE kind = 'InterfaceOutbound'
                  AND from_kind = 'interface'
                  AND to_kind = 'interface';
            ",
        )?;
        Ok(())
    }

    fn set_meta(&self, key: &str, value: &str) -> Result<(), SqliteIndexError> {
        self.conn.execute(
            "INSERT INTO index_meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    fn meta_u64(&self, key: &str) -> Result<u64, SqliteIndexError> {
        let value: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM index_meta WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value.and_then(|v| v.parse().ok()).unwrap_or(0))
    }

    pub fn watermark(&self) -> Result<u64, SqliteIndexError> {
        self.meta_u64("watermark")
    }

    pub fn ingest_from(&mut self, log: &(dyn MovementLog + Send)) -> Result<u64, SqliteIndexError> {
        let watermark = self.watermark()?;
        let log_len = log.len() as u64;
        if log_len < watermark {
            return Err(SqliteIndexError::LogBehindIndex { watermark, log_len });
        }
        if log_len == watermark {
            match log.durable_end_offset() {
                Some(offset) => self.set_meta("durable_end_offset", &offset.to_string())?,
                None => {
                    self.conn.execute(
                        "DELETE FROM index_meta WHERE key = 'durable_end_offset'",
                        [],
                    )?;
                }
            }
            return Ok(0);
        }
        let tx = self.conn.transaction()?;
        let mut expected = watermark;
        let mut failure: Option<SqliteIndexError> = None;
        {
            let mut insert = tx.prepare_cached(
                "INSERT OR REPLACE INTO movements(
                    seq, lamport, recorded_at_unix_ms, trace, kind, mark,
                    from_kind, from_id, to_kind, to_id,
                    artifact_id, artifact_label, artifact_content,
                    runtime_run, runtime_checkpoint, runtime_interface,
                    start_offset, end_offset
                ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
            )?;
            log.scan_from(Seq(watermark), &mut |movement| {
                if movement.seq.0 != expected {
                    failure = Some(SqliteIndexError::NonContiguousTail {
                        expected,
                        got: movement.seq.0,
                    });
                    return ControlFlow::Break(());
                }
                let byte_range = log.durable_row_byte_range(movement.seq);
                let row = project(&movement);
                if let Err(error) = insert.execute(params![
                    row.seq as i64,
                    row.lamport as i64,
                    row.recorded_at_unix_ms as i64,
                    row.trace,
                    row.kind,
                    row.mark,
                    row.from_kind,
                    row.from_id,
                    row.to_kind,
                    row.to_id,
                    row.artifact_id,
                    row.artifact_label,
                    row.artifact_content,
                    row.runtime_run,
                    row.runtime_checkpoint,
                    row.runtime_interface,
                    byte_range.map(|range| range.0 as i64),
                    byte_range.map(|range| range.1 as i64),
                ]) {
                    failure = Some(error.into());
                    return ControlFlow::Break(());
                }
                expected += 1;
                ControlFlow::Continue(())
            });
        }
        if let Some(error) = failure {
            return Err(error);
        }
        tx.execute(
            "INSERT INTO index_meta(key, value) VALUES ('watermark', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![expected.to_string()],
        )?;
        match log.durable_end_offset() {
            Some(offset) => {
                tx.execute(
                    "INSERT INTO index_meta(key, value) VALUES ('durable_end_offset', ?1)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![offset.to_string()],
                )?;
            }
            None => {
                tx.execute(
                    "DELETE FROM index_meta WHERE key = 'durable_end_offset'",
                    [],
                )?;
            }
        }
        tx.commit()?;
        Ok(expected - watermark)
    }

    pub fn ingest_from_movements(
        &mut self,
        log: &(dyn MovementLog + Send),
        movements: &[Movement],
    ) -> Result<u64, SqliteIndexError> {
        validate_complete_projection(log, movements)?;
        let watermark = self.watermark()?;
        let log_len = log.len() as u64;
        if log_len < watermark {
            return Err(SqliteIndexError::LogBehindIndex { watermark, log_len });
        }
        if log_len == watermark {
            match log.durable_end_offset() {
                Some(offset) => self.set_meta("durable_end_offset", &offset.to_string())?,
                None => {
                    self.conn.execute(
                        "DELETE FROM index_meta WHERE key = 'durable_end_offset'",
                        [],
                    )?;
                }
            }
            return Ok(0);
        }
        let tx = self.conn.transaction()?;
        let mut expected = watermark;
        {
            let mut insert = tx.prepare_cached(
                "INSERT OR REPLACE INTO movements(
                    seq, lamport, recorded_at_unix_ms, trace, kind, mark,
                    from_kind, from_id, to_kind, to_id,
                    artifact_id, artifact_label, artifact_content,
                    runtime_run, runtime_checkpoint, runtime_interface,
                    start_offset, end_offset
                ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
            )?;
            for movement in movements.iter().skip(watermark as usize) {
                if movement.seq.0 != expected {
                    return Err(SqliteIndexError::NonContiguousTail {
                        expected,
                        got: movement.seq.0,
                    });
                }
                let byte_range = log.durable_row_byte_range(movement.seq);
                let row = project(movement);
                insert.execute(params![
                    row.seq as i64,
                    row.lamport as i64,
                    row.recorded_at_unix_ms as i64,
                    row.trace,
                    row.kind,
                    row.mark,
                    row.from_kind,
                    row.from_id,
                    row.to_kind,
                    row.to_id,
                    row.artifact_id,
                    row.artifact_label,
                    row.artifact_content,
                    row.runtime_run,
                    row.runtime_checkpoint,
                    row.runtime_interface,
                    byte_range.map(|range| range.0 as i64),
                    byte_range.map(|range| range.1 as i64),
                ])?;
                expected += 1;
            }
        }
        tx.execute(
            "INSERT INTO index_meta(key, value) VALUES ('watermark', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![expected.to_string()],
        )?;
        match log.durable_end_offset() {
            Some(offset) => {
                tx.execute(
                    "INSERT INTO index_meta(key, value) VALUES ('durable_end_offset', ?1)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![offset.to_string()],
                )?;
            }
            None => {
                tx.execute(
                    "DELETE FROM index_meta WHERE key = 'durable_end_offset'",
                    [],
                )?;
            }
        }
        tx.commit()?;
        Ok(expected - watermark)
    }

    pub fn verify_against(&self, log: &(dyn MovementLog + Send)) -> Result<bool, SqliteIndexError> {
        let watermark = self.watermark()?;
        let log_len = log.len() as u64;
        if watermark > log_len {
            return Ok(false);
        }
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM movements", [], |row| row.get(0))?;
        if count as u64 != watermark {
            return Ok(false);
        }
        if !self.durable_prefix_matches(log, watermark, log_len)? {
            return Ok(false);
        }
        if watermark == 0 {
            return Ok(true);
        }
        let last = log.read_from(Seq(watermark - 1));
        let Some(truth) = last.first() else {
            return Ok(false);
        };
        let spot: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT lamport, kind FROM movements WHERE seq = ?1",
                params![(watermark - 1) as i64],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((lamport, kind)) = spot else {
            return Ok(false);
        };
        Ok(lamport as u64 == truth.lamport.0 && kind == kind_label(&truth.kind).0)
    }

    pub fn verify_against_movements(
        &self,
        log: &(dyn MovementLog + Send),
        movements: &[Movement],
    ) -> Result<bool, SqliteIndexError> {
        validate_complete_projection(log, movements)?;
        let watermark = self.watermark()?;
        let log_len = log.len() as u64;
        if watermark > log_len {
            return Ok(false);
        }
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM movements", [], |row| row.get(0))?;
        if count as u64 != watermark {
            return Ok(false);
        }
        if !self.durable_prefix_matches(log, watermark, log_len)? {
            return Ok(false);
        }
        if watermark == 0 {
            return Ok(true);
        }
        let Some(truth) = movements.get((watermark - 1) as usize) else {
            return Ok(false);
        };
        let spot: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT lamport, kind FROM movements WHERE seq = ?1",
                params![(watermark - 1) as i64],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((lamport, kind)) = spot else {
            return Ok(false);
        };
        Ok(lamport as u64 == truth.lamport.0 && kind == kind_label(&truth.kind).0)
    }

    fn durable_prefix_matches(
        &self,
        log: &(dyn MovementLog + Send),
        watermark: u64,
        log_len: u64,
    ) -> Result<bool, SqliteIndexError> {
        let Some(log_end_offset) = log.durable_end_offset() else {
            return Ok(true);
        };
        let expected_offset = if watermark == log_len {
            log_end_offset
        } else {
            let Some((start_offset, _)) = log.durable_row_byte_range(Seq(watermark)) else {
                return Ok(false);
            };
            start_offset
        };
        Ok(self.meta_u64("durable_end_offset")? == expected_offset)
    }

    pub fn rebuild_from(
        &mut self,
        log: &(dyn MovementLog + Send),
    ) -> Result<u64, SqliteIndexError> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM movements", [])?;
        tx.execute(
            "INSERT INTO index_meta(key, value) VALUES ('watermark', '0')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [],
        )?;
        tx.commit()?;
        self.ingest_from(log)
    }

    pub fn rebuild_from_movements(
        &mut self,
        log: &(dyn MovementLog + Send),
        movements: &[Movement],
    ) -> Result<u64, SqliteIndexError> {
        validate_complete_projection(log, movements)?;
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM movements", [])?;
        tx.execute(
            "INSERT INTO index_meta(key, value) VALUES ('watermark', '0')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [],
        )?;
        tx.commit()?;
        self.ingest_from_movements(log, movements)
    }

    pub fn count(&self) -> Result<u64, SqliteIndexError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM movements", [], |row| row.get(0))?;
        Ok(count as u64)
    }

    pub fn movements_for_runtime_interface(
        &self,
        interface: &str,
        after_seq: u64,
        limit: u64,
    ) -> Result<Vec<IndexedMovement>, SqliteIndexError> {
        self.query_rows(
            "SELECT * FROM movements
             WHERE runtime_interface = ?1 AND seq >= ?2
             ORDER BY seq ASC LIMIT ?3",
            params![interface, after_seq as i64, limit as i64],
        )
    }

    pub fn movements_touching_run(
        &self,
        run: &str,
        after_seq: u64,
        limit: u64,
    ) -> Result<Vec<IndexedMovement>, SqliteIndexError> {
        self.query_rows(
            "SELECT * FROM movements
             WHERE seq >= ?2
               AND runtime_run = ?1
             ORDER BY seq ASC LIMIT ?3",
            params![run, after_seq as i64, limit as i64],
        )
    }

    pub fn movements_by_artifact(
        &self,
        artifact_id: &str,
    ) -> Result<Vec<IndexedMovement>, SqliteIndexError> {
        self.query_rows(
            "SELECT * FROM movements WHERE artifact_id = ?1 ORDER BY seq ASC",
            params![artifact_id],
        )
    }

    pub fn admitted_marks(
        &self,
        mark: Option<&str>,
        after_seq: u64,
        limit: u64,
    ) -> Result<Vec<IndexedMovement>, SqliteIndexError> {
        match mark {
            Some(mark) => self.query_rows(
                "SELECT * FROM movements
                 WHERE mark = ?1 AND seq >= ?2 ORDER BY seq ASC LIMIT ?3",
                params![mark, after_seq as i64, limit as i64],
            ),
            None => self.query_rows(
                "SELECT * FROM movements
                 WHERE mark IS NOT NULL AND seq >= ?1 ORDER BY seq ASC LIMIT ?2",
                params![after_seq as i64, limit as i64],
            ),
        }
    }

    fn query_rows(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<IndexedMovement>, SqliteIndexError> {
        let mut stmt = self.conn.prepare_cached(sql)?;
        let rows = stmt.query_map(params, |row| {
            Ok(IndexedMovement {
                seq: row.get::<_, i64>("seq")? as u64,
                lamport: row.get::<_, i64>("lamport")? as u64,
                recorded_at_unix_ms: row.get::<_, i64>("recorded_at_unix_ms")? as u64,
                trace: row.get("trace")?,
                kind: row.get("kind")?,
                mark: row.get("mark")?,
                from_kind: row.get("from_kind")?,
                from_id: row.get("from_id")?,
                to_kind: row.get("to_kind")?,
                to_id: row.get("to_id")?,
                artifact_id: row.get("artifact_id")?,
                artifact_label: row.get("artifact_label")?,
                artifact_content: row.get("artifact_content")?,
                runtime_run: row.get("runtime_run")?,
                runtime_checkpoint: row.get("runtime_checkpoint")?,
                runtime_interface: row.get("runtime_interface")?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

fn validate_complete_projection(
    log: &(dyn MovementLog + Send),
    movements: &[Movement],
) -> Result<(), SqliteIndexError> {
    let log_len = log.len() as u64;
    if movements.len() as u64 != log_len {
        return Err(SqliteIndexError::ProjectionLength {
            projected: movements.len() as u64,
            log_len,
        });
    }
    for (expected, movement) in movements.iter().enumerate() {
        if movement.seq.0 != expected as u64 {
            return Err(SqliteIndexError::NonContiguousTail {
                expected: expected as u64,
                got: movement.seq.0,
            });
        }
    }
    Ok(())
}

impl LedgerSqliteReader {
    pub fn open(path: &Path) -> Result<Self, SqliteIndexError> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let schema_version = required_meta_u64(&conn, "schema_version")?;
        if schema_version != SCHEMA_VERSION as u64 {
            return Err(SqliteIndexError::SchemaGeneration {
                expected: SCHEMA_VERSION as u64,
                actual: schema_version,
            });
        }
        Ok(Self { conn })
    }

    pub fn progress(&self) -> Result<LedgerIndexProgress, SqliteIndexError> {
        let (watermark, durable_end_offset, first_seq, last_seq, last_end_offset): (
            String,
            String,
            Option<i64>,
            Option<i64>,
            Option<i64>,
        ) = self.conn.query_row(
            "SELECT
                (SELECT value FROM index_meta WHERE key = 'watermark'),
                (SELECT value FROM index_meta WHERE key = 'durable_end_offset'),
                (SELECT seq FROM movements ORDER BY seq ASC LIMIT 1),
                (SELECT seq FROM movements ORDER BY seq DESC LIMIT 1),
                (SELECT end_offset FROM movements ORDER BY seq DESC LIMIT 1)
            ",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
        let progress = LedgerIndexProgress {
            watermark: parse_meta_u64("watermark", watermark)?,
            durable_end_offset: parse_meta_u64("durable_end_offset", durable_end_offset)?,
        };
        let expected_first = (progress.watermark > 0).then_some(0_i64);
        let expected_last = progress
            .watermark
            .checked_sub(1)
            .map(i64::try_from)
            .transpose()
            .map_err(|_| SqliteIndexError::InconsistentSnapshot {
                message: format!("watermark {} exceeds SQLite range", progress.watermark),
            })?;
        let expected_end = if progress.watermark == 0 {
            if progress.durable_end_offset != 0 {
                return Err(SqliteIndexError::InconsistentSnapshot {
                    message: format!(
                        "empty index has durable end offset {}",
                        progress.durable_end_offset
                    ),
                });
            }
            None
        } else {
            Some(i64::try_from(progress.durable_end_offset).map_err(|_| {
                SqliteIndexError::InconsistentSnapshot {
                    message: format!(
                        "durable end offset {} exceeds SQLite range",
                        progress.durable_end_offset
                    ),
                }
            })?)
        };
        if first_seq != expected_first
            || last_seq != expected_last
            || last_end_offset != expected_end
        {
            return Err(SqliteIndexError::InconsistentSnapshot {
                message: format!(
                    "watermark {} and durable end offset {} do not name one contiguous indexed boundary",
                    progress.watermark, progress.durable_end_offset
                ),
            });
        }
        Ok(progress)
    }

    pub fn bounded_interface_delivery_candidate(
        &self,
        interface: &str,
        after_seq: u64,
        before_seq: u64,
    ) -> Result<Option<IndexedMovementRowRange>, SqliteIndexError> {
        if after_seq >= before_seq {
            return Ok(None);
        }
        let after_seq = sqlite_u64("after sequence", after_seq)?;
        let before_seq = sqlite_u64("before sequence", before_seq)?;
        let runtime = query_indexed_candidate(
            &self.conn,
            RUNTIME_INTERFACE_OUTBOUND_CANDIDATE_SQL,
            interface,
            after_seq,
            before_seq,
        )?;
        let endpoint = query_indexed_candidate(
            &self.conn,
            ENDPOINT_INTERFACE_OUTBOUND_CANDIDATE_SQL,
            interface,
            after_seq,
            before_seq,
        )?;
        Ok(match (runtime, endpoint) {
            (Some(left), Some(right)) if left.seq == right.seq => {
                if left != right {
                    return Err(SqliteIndexError::InconsistentSnapshot {
                        message: format!(
                            "movement {} has conflicting indexed byte ranges",
                            left.seq
                        ),
                    });
                }
                Some(left)
            }
            (Some(left), Some(right)) => Some(if left.seq < right.seq { left } else { right }),
            (Some(candidate), None) | (None, Some(candidate)) => Some(candidate),
            (None, None) => None,
        })
    }

    pub fn outbound_rows_on_edge(
        &self,
        from: &str,
        to: &str,
        after_seq: u64,
        before_seq: u64,
    ) -> Result<Vec<IndexedMovementRowRange>, SqliteIndexError> {
        let mut statement = self.conn.prepare_cached(
            "SELECT seq, start_offset, end_offset FROM movements
             WHERE from_kind = 'interface' AND from_id = ?1
               AND to_kind = 'interface' AND to_id = ?2
               AND kind = 'InterfaceOutbound' AND seq >= ?3 AND seq < ?4
             ORDER BY seq ASC",
        )?;
        let rows = statement.query_map(
            params![from, to, sqlite_u64("after sequence", after_seq)?, sqlite_u64("before sequence", before_seq)?],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)),
        )?;
        rows.map(|row| {
            let (seq, start, end) = row?;
            if seq < 0 {
                return Err(SqliteIndexError::InconsistentSnapshot { message: "negative outbound sequence".into() });
            }
            checked_indexed_row_range(seq as u64, start, end)
        }).collect()
    }

    pub fn movement_rows_touching_run(
        &self,
        run: &str,
        after_seq: u64,
        before_seq: u64,
    ) -> Result<Option<Vec<IndexedMovementRowRange>>, SqliteIndexError> {
        if after_seq >= before_seq {
            return Ok(Some(Vec::new()));
        }
        let mut stmt = self.conn.prepare_cached(
            "SELECT seq, start_offset, end_offset FROM movements
             WHERE seq >= ?2
               AND seq < ?3
               AND runtime_run = ?1
             ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(params![run, after_seq as i64, before_seq as i64], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        })?;
        let mut movements = Vec::new();
        for row in rows {
            let (seq, start_offset, end_offset) = row?;
            let (Some(start_offset), Some(end_offset)) = (start_offset, end_offset) else {
                return Ok(None);
            };
            if seq < 0 || start_offset < 0 || end_offset <= start_offset {
                return Err(SqliteIndexError::InvalidRowRange {
                    seq: seq.max(0) as u64,
                    start_offset,
                    end_offset,
                });
            }
            movements.push(IndexedMovementRowRange {
                seq: seq as u64,
                start_offset: start_offset as u64,
                end_offset: end_offset as u64,
            });
        }
        Ok(Some(movements))
    }

    pub fn movement_row_range(
        &self,
        seq: u64,
    ) -> Result<Option<IndexedMovementRowRange>, SqliteIndexError> {
        let seq = sqlite_u64("movement sequence", seq)?;
        let row = self
            .conn
            .query_row(
                "SELECT start_offset, end_offset FROM movements WHERE seq = ?1",
                params![seq],
                |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .optional()?;
        let Some((start_offset, end_offset)) = row else {
            return Ok(None);
        };
        let (Some(start_offset), Some(end_offset)) = (start_offset, end_offset) else {
            return Err(SqliteIndexError::InconsistentSnapshot {
                message: format!("indexed movement {seq} has no durable byte range"),
            });
        };
        checked_indexed_row_range(seq as u64, start_offset, end_offset).map(Some)
    }

    pub fn movement_rows_for_runtime_interface(
        &self,
        interface: &str,
        after_seq: u64,
        before_seq: u64,
    ) -> Result<Option<Vec<IndexedMovementRowRange>>, SqliteIndexError> {
        if after_seq >= before_seq {
            return Ok(Some(Vec::new()));
        }
        let join = if after_seq == 0 {
            "FROM movements AS arrival INDEXED BY idx_mv_runtime_interface
             CROSS JOIN movements AS movement INDEXED BY idx_mv_artifact"
        } else {
            "FROM movements AS movement INDEXED BY idx_mv_kind
             CROSS JOIN movements AS arrival INDEXED BY idx_mv_artifact"
        };
        let sql = format!(
            "SELECT seq, start_offset, end_offset
             FROM movements
             WHERE runtime_interface = ?1 AND seq >= ?2 AND seq < ?3
             UNION
             SELECT movement.seq, movement.start_offset, movement.end_offset
             {join}
             WHERE arrival.runtime_interface = ?1
               AND arrival.kind = 'InterfaceArrival'
               AND arrival.artifact_id IS NOT NULL
               AND arrival.to_kind = 'interface'
               AND arrival.seq < ?3
               AND movement.artifact_id = arrival.artifact_id
               AND movement.artifact_id IS NOT NULL
               AND movement.kind = 'MarkOutcomeAdmitted'
               AND movement.from_kind = 'interface'
               AND movement.to_kind = 'interface'
               AND movement.from_id = arrival.from_id
               AND movement.to_id = arrival.to_id
               AND movement.runtime_run = arrival.runtime_run
               AND movement.runtime_checkpoint = arrival.runtime_checkpoint
               AND movement.seq > arrival.seq
               AND movement.seq >= ?2
               AND movement.seq < ?3
             ORDER BY seq ASC"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(
            params![interface, after_seq as i64, before_seq as i64],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            },
        )?;
        let mut movements = Vec::new();
        for row in rows {
            let (seq, start_offset, end_offset) = row?;
            let (Some(start_offset), Some(end_offset)) = (start_offset, end_offset) else {
                return Ok(None);
            };
            if seq < 0 || start_offset < 0 || end_offset <= start_offset {
                return Err(SqliteIndexError::InvalidRowRange {
                    seq: seq.max(0) as u64,
                    start_offset,
                    end_offset,
                });
            }
            movements.push(IndexedMovementRowRange {
                seq: seq as u64,
                start_offset: start_offset as u64,
                end_offset: end_offset as u64,
            });
        }
        Ok(Some(movements))
    }
}

fn checked_indexed_row_range(
    seq: u64,
    start_offset: i64,
    end_offset: i64,
) -> Result<IndexedMovementRowRange, SqliteIndexError> {
    if start_offset < 0 || end_offset <= start_offset {
        return Err(SqliteIndexError::InvalidRowRange {
            seq,
            start_offset,
            end_offset,
        });
    }
    Ok(IndexedMovementRowRange {
        seq,
        start_offset: start_offset as u64,
        end_offset: end_offset as u64,
    })
}

fn query_indexed_candidate(
    connection: &Connection,
    sql: &str,
    interface: &str,
    after_seq: i64,
    before_seq: i64,
) -> Result<Option<IndexedMovementRowRange>, SqliteIndexError> {
    let row = connection
        .query_row(sql, params![interface, after_seq, before_seq], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        })
        .optional()?;
    let Some((seq, start_offset, end_offset)) = row else {
        return Ok(None);
    };
    let (Some(start_offset), Some(end_offset)) = (start_offset, end_offset) else {
        return Err(SqliteIndexError::InconsistentSnapshot {
            message: "indexed interface candidate has no durable byte range".to_string(),
        });
    };
    if seq < 0 {
        return Err(SqliteIndexError::InconsistentSnapshot {
            message: format!("movement seq is negative: {seq}"),
        });
    }
    checked_indexed_row_range(seq as u64, start_offset, end_offset).map(Some)
}

fn sqlite_u64(label: &str, value: u64) -> Result<i64, SqliteIndexError> {
    i64::try_from(value).map_err(|_| SqliteIndexError::InconsistentSnapshot {
        message: format!("{label} {value} exceeds SQLite range"),
    })
}

fn required_meta_u64(conn: &Connection, key: &str) -> Result<u64, SqliteIndexError> {
    let value: String = conn.query_row(
        "SELECT value FROM index_meta WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )?;
    parse_meta_u64(key, value)
}

fn parse_meta_u64(key: &str, value: String) -> Result<u64, SqliteIndexError> {
    value
        .parse()
        .map_err(|_| SqliteIndexError::InvalidMetadata {
            key: key.to_string(),
            value,
        })
}

fn project(movement: &Movement) -> IndexedMovement {
    let (kind, mark) = kind_label(&movement.kind);
    let (from_kind, from_id) = endpoint_parts(&movement.from);
    let (to_kind, to_id) = endpoint_parts(&movement.to);
    let runtime = movement.runtime().filter(|runtime| match movement.kind {
        MovementKind::InterfaceArrival => {
            matches!(
                (&movement.from, &movement.to),
                (Endpoint::Interface(interface), Endpoint::Interface(worker))
                    if interface == &runtime.interface && worker == &runtime.worker
            )
        }
        MovementKind::InterfaceOutbound => {
            matches!(
                (&movement.from, &movement.to),
                (Endpoint::Interface(_), Endpoint::Interface(interface))
                    if interface == &runtime.interface
            )
        }
        _ => true,
    });
    let mark_outcome = movement
        .invocation_metadata
        .as_ref()
        .and_then(|metadata| metadata.mark_outcome());
    IndexedMovement {
        seq: movement.seq.0,
        lamport: movement.lamport.0,
        recorded_at_unix_ms: movement.recorded_at_unix_ms,
        trace: movement.trace.0.clone(),
        kind,
        mark,
        from_kind: from_kind.to_string(),
        from_id,
        to_kind: to_kind.to_string(),
        to_id,
        artifact_id: movement
            .artifact
            .as_ref()
            .map(|a| a.id.as_str().to_string()),
        artifact_label: movement.artifact.as_ref().and_then(|a| {
            a.id.as_str()
                .rsplit_once(':')
                .map(|(label, _)| label)
                .filter(|label| !label.trim().is_empty())
                .map(str::to_string)
        }),
        artifact_content: movement.artifact.as_ref().map(|a| a.content.to_hex()),
        runtime_run: runtime
            .map(|runtime| runtime.run.as_str().to_string())
            .or_else(|| mark_outcome.map(|mark| mark.run.as_str().to_string())),
        runtime_checkpoint: runtime
            .map(|runtime| runtime.checkpoint.as_str().to_string())
            .or_else(|| mark_outcome.map(|mark| mark.subject.as_str().to_string())),
        runtime_interface: runtime.map(|runtime| runtime.interface.as_str().to_string()),
    }
}

fn kind_label(kind: &MovementKind) -> (String, Option<String>) {
    match serde_json::to_value(kind) {
        Ok(serde_json::Value::String(label)) => (label, None),
        Ok(serde_json::Value::Object(map)) => {
            if let Some((label, value)) = map.into_iter().next() {
                let mark = value.as_str().map(str::to_string);
                (label, mark)
            } else {
                ("Unknown".to_string(), None)
            }
        }
        _ => ("Unknown".to_string(), None),
    }
}

fn endpoint_parts(endpoint: &Endpoint) -> (&'static str, String) {
    match endpoint {
        Endpoint::Interface(id) => ("interface", id.as_str().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_descriptors::{ArtifactId, CheckpointId, EeId, InterfaceId, MarkId, RunId};
    use octopus_interfaces::{
        GrantProjection, InvocationMetadata, MovementMetadata, RuntimeInvocationMetadata,
    };
    use octopus_ledger::{ArtifactRef, ContentHash, InMemoryLog, MovementDraft, TraceId};

    fn metadata(run: &str, checkpoint: &str, interface: &str) -> MovementMetadata {
        InvocationMetadata::new(GrantProjection {
            tools: vec!["tool.expensive_grant".to_string()],
            ..GrantProjection::default()
        })
        .with_runtime(RuntimeInvocationMetadata {
            run: RunId::new(run),
            ee: EeId::new("ee_test"),
            worker: InterfaceId::new("system_worker_test"),
            checkpoint: CheckpointId::new(checkpoint),
            interface: InterfaceId::new(interface),
            epoch: 0,
            run_record_hash: None,
        })
        .into()
    }

    fn seed_log() -> InMemoryLog {
        let mut log = InMemoryLog::new();
        log.append(MovementDraft {
            trace: TraceId("t1".into()),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(InterfaceId::new("world_provider_codex")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_test")),
            artifact: Some(ArtifactRef {
                id: ArtifactId::new(format!(
                    "interface.input:{}",
                    ContentHash::of_bytes(b"one").to_hex()
                )),
                content: ContentHash::of_bytes(b"one"),
            }),
            invocation_metadata: Some(metadata("run_alpha", "c1", "world_provider_codex")),
            causal_floor: None,
        });
        log.append(MovementDraft {
            trace: TraceId("t1".into()),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(InterfaceId::new("world_provider_codex")),
            to: Endpoint::Interface(InterfaceId::new("world_operator_chat")),
            artifact: Some(ArtifactRef {
                id: ArtifactId::new(format!(
                    "interface.output:{}",
                    ContentHash::of_bytes(b"two").to_hex()
                )),
                content: ContentHash::of_bytes(b"two"),
            }),
            invocation_metadata: Some(metadata("run_alpha", "c2", "world_operator_chat")),
            causal_floor: None,
        });
        log.append(MovementDraft {
            trace: TraceId("t1".into()),
            kind: MovementKind::MarkOutcomeAdmitted(MarkId::new("coder.build.success")),
            from: Endpoint::Interface(InterfaceId::new("system_worker_test")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_test")),
            artifact: None,
            invocation_metadata: Some(
                octopus_ledger::Mark {
                    name: MarkId::new("coder.build.success"),
                    subject: CheckpointId::new("c1"),
                    run: RunId::new("run_alpha"),
                    run_epoch: 0,
                    class: octopus_ledger::MarkClass::Success,
                    evidence: None,
                    arrival_seq: None,
                    verifier: octopus_descriptors::VerifierId::new("test"),
                    verifier_package: None,
                    details: serde_json::json!({}),
                }
                .into(),
            ),
            causal_floor: None,
        });
        log
    }

    struct OffsetLog {
        inner: InMemoryLog,
        end_offset: u64,
    }

    impl MovementLog for OffsetLog {
        fn append(&mut self, draft: MovementDraft) -> Movement {
            self.inner.append(draft)
        }

        fn read_from(&self, from: Seq) -> Vec<Movement> {
            self.inner.read_from(from)
        }

        fn len(&self) -> usize {
            self.inner.len()
        }

        fn durable_end_offset(&self) -> Option<u64> {
            Some(self.end_offset)
        }

        fn durable_row_byte_range(&self, seq: Seq) -> Option<(u64, u64)> {
            let len = self.inner.len() as u64;
            if seq.0 >= len || len == 0 {
                return None;
            }
            Some((
                self.end_offset * seq.0 / len,
                self.end_offset * (seq.0 + 1) / len,
            ))
        }
    }

    fn temp_db() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("ledger-index.sqlite");
        (dir, path)
    }

    fn lightweight_projection(log: &InMemoryLog) -> Vec<Movement> {
        let mut movements = log.read_from(Seq(0));
        for movement in &mut movements {
            if let Some(MovementMetadata::Invocation(metadata)) =
                movement.invocation_metadata.as_mut()
            {
                metadata.grant = GrantProjection::default();
            }
        }
        movements
    }

    #[test]
    fn ingest_projects_rows_and_queries_answer_from_btree_indexes() {
        let log = seed_log();
        let (_dir, path) = temp_db();
        let mut index = LedgerSqliteIndex::open(&path).unwrap();
        assert_eq!(index.ingest_from(&log).unwrap(), 3);
        assert_eq!(index.count().unwrap(), 3);
        assert_eq!(index.watermark().unwrap(), 3);

        let mine = index
            .movements_for_runtime_interface("world_provider_codex", 0, 100)
            .unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].seq, 0);
        assert_eq!(mine[0].kind, "InterfaceArrival");
        assert_eq!(mine[0].runtime_checkpoint.as_deref(), Some("c1"));
        assert_eq!(mine[0].artifact_label.as_deref(), Some("interface.input"));

        assert_eq!(
            index
                .movements_touching_run("run_alpha", 0, 100)
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            index
                .movements_touching_run("run_alpha", 2, 100)
                .unwrap()
                .len(),
            1
        );

        let marks = index.admitted_marks(None, 0, 10).unwrap();
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].mark.as_deref(), Some("coder.build.success"));
        assert!(
            index
                .admitted_marks(Some("coder.build.success"), 0, 10)
                .unwrap()
                .len()
                == 1
        );

        let by_artifact = index
            .movements_by_artifact(&format!(
                "interface.output:{}",
                ContentHash::of_bytes(b"two").to_hex()
            ))
            .unwrap();
        assert_eq!(by_artifact.len(), 1);
        assert_eq!(by_artifact[0].to_id, "world_operator_chat");
    }

    #[test]
    fn read_only_progress_pins_the_indexed_durable_prefix() {
        let log = OffsetLog {
            inner: seed_log(),
            end_offset: 98_765,
        };
        let (_dir, path) = temp_db();
        let mut index = LedgerSqliteIndex::open(&path).unwrap();
        index.ingest_from(&log).unwrap();
        drop(index);

        let reader = LedgerSqliteReader::open(&path).unwrap();
        assert_eq!(
            reader.progress().unwrap(),
            LedgerIndexProgress {
                watermark: 3,
                durable_end_offset: 98_765,
            }
        );
        assert_eq!(
            reader
                .bounded_interface_delivery_candidate("world_operator_chat", 0, 3)
                .unwrap()
                .unwrap()
                .seq,
            1
        );
        assert!(
            reader
                .bounded_interface_delivery_candidate("world_unrelated", 0, 3)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            reader
                .movement_rows_touching_run("run_alpha", 0, 3)
                .unwrap()
                .unwrap()
                .into_iter()
                .map(|movement| movement.seq)
                .collect::<Vec<_>>(),
            vec![0, 1, 2],
        );
        assert!(
            reader
                .movement_rows_touching_run("run_unrelated", 0, 3)
                .unwrap()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn incremental_ingest_equals_full_rebuild_and_reopen_keeps_watermark() {
        let mut log = seed_log();
        let (_dir, path) = temp_db();
        {
            let mut index = LedgerSqliteIndex::open(&path).unwrap();
            index.ingest_from(&log).unwrap();
        }
        log.append(MovementDraft {
            trace: TraceId("t2".into()),
            kind: MovementKind::VerificationFailed,
            from: Endpoint::Interface(InterfaceId::new("system_coder_env")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_test")),
            artifact: None,
            invocation_metadata: None,
            causal_floor: None,
        });
        let mut reopened = LedgerSqliteIndex::open(&path).unwrap();
        assert_eq!(
            reopened.watermark().unwrap(),
            3,
            "watermark survives reopen"
        );
        assert_eq!(
            reopened.ingest_from(&log).unwrap(),
            1,
            "only the delta ingests"
        );

        let (_dir2, path2) = temp_db();
        let mut rebuilt = LedgerSqliteIndex::open(&path2).unwrap();
        rebuilt.rebuild_from(&log).unwrap();
        assert_eq!(
            reopened
                .movements_touching_run("run_alpha", 0, 100)
                .unwrap(),
            rebuilt.movements_touching_run("run_alpha", 0, 100).unwrap(),
            "incremental ingest and full rebuild project identical rows"
        );
    }

    #[test]
    fn a_foreign_or_stale_cache_fails_verify_and_open_healthy_rebuilds_it() {
        let log = seed_log();
        let (_dir, path) = temp_db();
        {
            let mut index = LedgerSqliteIndex::open(&path).unwrap();
            index.ingest_from(&log).unwrap();
        }
        let short = InMemoryLog::new();
        {
            let index = LedgerSqliteIndex::open(&path).unwrap();
            assert!(!index.verify_against(&short).unwrap());
        }
        let healthy = LedgerSqliteIndex::open_healthy(&path, &short).unwrap();
        assert_eq!(
            healthy.count().unwrap(),
            0,
            "rebuilt against the real ledger"
        );

        let healthy = LedgerSqliteIndex::open_healthy(&path, &log).unwrap();
        assert_eq!(healthy.count().unwrap(), 3);
        assert!(healthy.verify_against(&log).unwrap());
    }

    #[test]
    fn lightweight_projection_rebuilds_ingests_tail_and_rejects_incomplete_input() {
        let mut log = OffsetLog {
            inner: seed_log(),
            end_offset: 300,
        };
        let mut movements = lightweight_projection(&log.inner);
        assert!(matches!(
            log.inner.read_from(Seq(0))[0].invocation_metadata.as_ref(),
            Some(MovementMetadata::Invocation(metadata)) if metadata.grant.tools == ["tool.expensive_grant"]
        ));
        assert!(matches!(
            movements[0].invocation_metadata.as_ref(),
            Some(MovementMetadata::Invocation(metadata)) if metadata.grant.tools.is_empty()
        ));

        let (_dir, path) = temp_db();
        let index =
            LedgerSqliteIndex::open_healthy_from_movements(&path, &log, &movements).unwrap();
        assert_eq!(index.count().unwrap(), 3);
        assert_eq!(index.watermark().unwrap(), 3);
        let runtime_rows = index
            .movements_for_runtime_interface("world_provider_codex", 0, 10)
            .unwrap();
        assert_eq!(runtime_rows.len(), 1);
        assert_eq!(runtime_rows[0].runtime_run.as_deref(), Some("run_alpha"));
        assert_eq!(runtime_rows[0].runtime_checkpoint.as_deref(), Some("c1"));
        drop(index);
        let reader = LedgerSqliteReader::open(&path).unwrap();
        assert_eq!(
            reader
                .bounded_interface_delivery_candidate("world_operator_chat", 0, 3)
                .unwrap(),
            Some(IndexedMovementRowRange {
                seq: 1,
                start_offset: 100,
                end_offset: 200,
            })
        );
        drop(reader);

        log.append(MovementDraft {
            trace: TraceId("t2".into()),
            kind: MovementKind::VerificationFailed,
            from: Endpoint::Interface(InterfaceId::new("system_coder_env")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_test")),
            artifact: None,
            invocation_metadata: None,
            causal_floor: None,
        });
        log.end_offset = 400;
        movements = lightweight_projection(&log.inner);
        let stale = LedgerSqliteIndex::open(&path).unwrap();
        assert!(stale.verify_against_movements(&log, &movements).unwrap());
        drop(stale);
        let index =
            LedgerSqliteIndex::open_healthy_from_movements(&path, &log, &movements).unwrap();
        assert_eq!(index.count().unwrap(), 4);
        assert_eq!(index.watermark().unwrap(), 4);

        assert!(matches!(
            LedgerSqliteIndex::open_healthy_from_movements(
                &path,
                &log,
                &movements[..movements.len() - 1],
            ),
            Err(SqliteIndexError::ProjectionLength {
                projected: 3,
                log_len: 4
            })
        ));
        let mut non_contiguous = movements;
        non_contiguous[2].seq = Seq(9);
        assert!(matches!(
            LedgerSqliteIndex::open_healthy_from_movements(&path, &log, &non_contiguous),
            Err(SqliteIndexError::NonContiguousTail {
                expected: 2,
                got: 9
            })
        ));
    }

    #[test]
    fn interface_history_keeps_prior_arrivals_exact_runs_order_and_unique_marks() {
        let (_dir, path) = temp_db();
        let index = LedgerSqliteIndex::open(&path).unwrap();
        for (seq, kind, from, to, artifact, interface) in [
            (0, "MarkOutcomeAdmitted", "in", "worker1", Some("a"), None),
            (1, "InterfaceArrival", "in", "worker1", Some("a"), Some("i")),
            (2, "InterfaceArrival", "in", "worker1", Some("a"), Some("i")),
            (
                3,
                "InterfaceArrival",
                "in",
                "worker2",
                Some("a"),
                Some("other"),
            ),
            (4, "MarkOutcomeAdmitted", "in", "worker1", Some("a"), None),
            (5, "MarkOutcomeAdmitted", "in", "worker2", Some("a"), None),
            (6, "MarkOutcomeAdmitted", "in", "worker1", None, None),
            (
                7,
                "MarkOutcomeAdmitted",
                "in",
                "worker1",
                Some("other"),
                None,
            ),
            (
                8,
                "MarkOutcomeAdmitted",
                "wrong_source",
                "worker2",
                Some("a"),
                None,
            ),
            (
                9,
                "MarkOutcomeAdmitted",
                "in",
                "worker1",
                Some("a"),
                Some("i"),
            ),
        ] {
            index.conn.execute(
                "INSERT INTO movements(seq,lamport,recorded_at_unix_ms,trace,kind,from_kind,from_id,to_kind,to_id,artifact_id,runtime_interface,runtime_run,runtime_checkpoint,start_offset,end_offset)
                 VALUES (?1,?1,1,'trace',?2,'interface',?3,'interface',?4,?5,?6,?4,'c1',?1*100,(?1+1)*100)",
                params![seq, kind, from, to, artifact, interface],
            ).unwrap();
        }
        let reader = LedgerSqliteReader::open(&path).unwrap();
        for (after, before, expected) in [
            (0, 10, vec![1, 2, 4, 9]),
            (3, 10, vec![4, 9]),
            (3, 9, vec![4]),
            (10, 10, vec![]),
        ] {
            let rows = reader
                .movement_rows_for_runtime_interface("i", after, before)
                .unwrap()
                .unwrap();
            assert_eq!(rows.iter().map(|row| row.seq).collect::<Vec<_>>(), expected);
        }
        index
            .conn
            .execute("UPDATE movements SET start_offset = NULL WHERE seq = 4", [])
            .unwrap();
        assert!(
            reader
                .movement_rows_for_runtime_interface("i", 3, 9)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn mismatched_outbound_interface_does_not_project_runtime_coordinates() {
        let movement = Movement {
            seq: Seq(0),
            lamport: octopus_ledger::Lamport(1),
            recorded_at_unix_ms: 1,
            trace: TraceId("legacy".into()),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(InterfaceId::new("system_worker_test")),
            to: Endpoint::Interface(InterfaceId::new("world_operator_chat")),
            artifact: None,
            invocation_metadata: Some(metadata("run_alpha", "c1", "world_other")),
        };

        let row = project(&movement);
        assert_eq!(row.runtime_run, None);
        assert_eq!(row.runtime_checkpoint, None);
        assert_eq!(row.runtime_interface, None);
    }
}
