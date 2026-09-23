use super::store;
use crate::relay::{RelayRequest, RelayRequestStatus, RelayResult};
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, MutexGuard};
use uuid::Uuid;

/// Decode directly from SQLite's borrowed text instead of first copying the
/// complete encoded payload into a Rust String.
fn json_column<T: serde::de::DeserializeOwned>(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<T> {
    let text = row.get_ref(index)?.as_str()?;
    serde_json::from_str(text).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

pub(crate) struct RequestCandidate {
    pub request_id: Uuid,
    pub session: String,
    pub payload_bytes: u32,
}

#[derive(Clone)]
pub struct Queue {
    connection: Arc<Mutex<Connection>>,
}
impl Queue {
    pub fn open(state: &store::Store) -> Result<Self> {
        Ok(Self {
            connection: Arc::new(Mutex::new(open_database(state)?)),
        })
    }
    fn database(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| anyhow::anyhow!("database operation lock poisoned"))
    }
}
fn open_database(state: &store::Store) -> Result<Connection> {
    let path = state.directory().join("relay.sqlite3");
    store::secure_open(&path)?;
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
      CREATE TABLE IF NOT EXISTS generations(session TEXT PRIMARY KEY,generation INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS outbox(request_id TEXT PRIMARY KEY,session TEXT NOT NULL,request TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'pending',result TEXT,error TEXT,attempts INTEGER NOT NULL DEFAULT 0,next_attempt INTEGER NOT NULL DEFAULT 0,created INTEGER NOT NULL,expected_generation INTEGER);
      CREATE INDEX IF NOT EXISTS outbox_pending ON outbox(state,next_attempt);")?;
    let generation_column: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('outbox') WHERE name='expected_generation'",
        [],
        |r| r.get(0),
    )?;
    if generation_column == 0 {
        conn.execute(
            "ALTER TABLE outbox ADD COLUMN expected_generation INTEGER",
            [],
        )?;
    }
    Ok(conn)
}
impl Queue {
    pub fn generation(&self, session: &str) -> Result<Option<i64>> {
        Ok(self
            .database()?
            .query_row(
                "SELECT generation FROM generations WHERE session=?1",
                params![session],
                |r| r.get(0),
            )
            .optional()?)
    }
}
impl Queue {
    pub fn adopt_generation(&self, session: &str, next: Option<i64>) -> Result<()> {
        let Some(next) = next else { return Ok(()) };
        let mut conn = self.database()?;
        let tx = conn.transaction()?;
        let previous: Option<i64> = tx
            .query_row(
                "SELECT generation FROM generations WHERE session=?1",
                params![session],
                |r| r.get(0),
            )
            .optional()?;
        if previous != Some(next) {
            tx.execute("UPDATE outbox SET state='failed',error=?2 WHERE session=?1 AND state='pending' AND (expected_generation IS NULL OR expected_generation<>?3)",params![session,json!({"code":"testing_generation_changed","message":"test environment changed; inspect then explicitly resubmit this request"}).to_string(),next])?;
        }
        tx.execute("INSERT INTO generations(session,generation) VALUES(?1,?2) ON CONFLICT(session) DO UPDATE SET generation=excluded.generation",params![session,next])?;
        tx.commit()?;
        Ok(())
    }
}
impl Queue {
    pub fn enqueue(&self, request: &RelayRequest, original: &Value) -> Result<()> {
        let conn = self.database()?;
        let raw = serde_json::to_string(original)?;
        let previous: Option<String> = conn
            .query_row(
                "SELECT request FROM outbox WHERE request_id=?1",
                params![request.request_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(previous) = previous {
            if serde_json::from_str::<Value>(&previous)? != *original {
                bail!("request_id was already used for a different request")
            }
            return Ok(());
        }
        let session = store::session_key(&request.profile, request.testing_environment_id);
        let known: Option<i64> = conn
            .query_row(
                "SELECT generation FROM generations WHERE session=?1",
                params![session],
                |r| r.get(0),
            )
            .optional()?;
        if request.testing_environment_id.is_some()
            && request.request.is_mutation()
            && request.testing_generation.or(known).is_none()
        {
            bail!(
                "discover the test environment over HTTP before queuing mutations; its generation is unknown"
            );
        }
        conn.execute(
        "INSERT INTO outbox(request_id,session,request,created,expected_generation) VALUES(?1,?2,?3,?4,?5)",
        params![
            request.request_id.to_string(),
            store::session_key(&request.profile, request.testing_environment_id),
            raw,
            (store::now() as i64),
            request.testing_generation.or(known)
        ],
    )?;
        Ok(())
    }
}
impl Queue {
    pub fn request_status(&self, id: Uuid) -> Result<Option<RelayRequestStatus>> {
        Ok(self
            .database()?
            .query_row(
                "SELECT state FROM outbox WHERE request_id=?1",
                params![id.to_string()],
                |row| {
                    Ok(RelayRequestStatus {
                        request_id: id,
                        state: row.get(0)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn result(&self, id: Uuid) -> Result<Option<RelayResult>> {
        Ok(self
            .database()?
            .query_row(
                "SELECT state,result,error,request FROM outbox WHERE request_id=?1",
                params![id.to_string()],
                |row| {
                    Ok(RelayResult {
                        request_id: id,
                        state: row.get(0)?,
                        result: if matches!(row.get_ref(1)?, rusqlite::types::ValueRef::Null) {
                            None
                        } else {
                            Some(json_column(row, 1)?)
                        },
                        error: if matches!(row.get_ref(2)?, rusqlite::types::ValueRef::Null) {
                            None
                        } else {
                            Some(json_column(row, 2)?)
                        },
                        request: json_column(row, 3)?,
                    })
                },
            )
            .optional()?)
    }
}
impl Queue {
    /// Select only small metadata. The scheduler filters running sessions and
    /// reserves its byte budget before loading any payload.
    pub(crate) fn request_candidates(&self) -> Result<Vec<RequestCandidate>> {
        let conn = self.database()?;
        let mut stmt=conn.prepare("SELECT request_id,session,octet_length(request) FROM outbox o WHERE state='pending' AND next_attempt<=?1 AND NOT EXISTS (SELECT 1 FROM outbox previous WHERE previous.session=o.session AND previous.state='pending' AND previous.rowid<o.rowid) ORDER BY rowid LIMIT 50")?;
        let rows = stmt
            .query_map(params![(store::now() as i64)], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, u32>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(request_id, session, payload_bytes)| {
                Ok(RequestCandidate {
                    request_id: request_id.parse()?,
                    session,
                    payload_bytes,
                })
            })
            .collect()
    }

    pub(crate) fn load_request(&self, id: Uuid) -> Result<Option<RelayRequest>> {
        Ok(self.database()?.query_row(
            "SELECT request,expected_generation FROM outbox WHERE request_id=?1 AND state='pending' AND next_attempt<=?2",
            params![id.to_string(), store::now() as i64],
            |row| {
                let mut request: RelayRequest = json_column(row, 0)?;
                let generation: Option<i64> = row.get(1)?;
                request.testing_generation = generation.or(request.testing_generation);
                Ok(request)
            },
        ).optional()?)
    }
}
impl Queue {
    pub fn finish(&self, id: Uuid, result: Option<&Value>, error: Option<&Value>) -> Result<()> {
        self.database()?.execute(
            "UPDATE outbox SET state=?2,result=?3,error=?4 WHERE request_id=?1",
            params![
                id.to_string(),
                if error.is_some() {
                    "failed"
                } else {
                    "completed"
                },
                result.map(serde_json::to_string).transpose()?,
                error.map(serde_json::to_string).transpose()?
            ],
        )?;
        Ok(())
    }
}
impl Queue {
    /// A missing local prerequisite is not a failed backend attempt. Keep the
    /// existing attempt count and retry shortly without polling continuously.
    pub(crate) fn defer_until_ready(&self, id: Uuid, reason: &Value) -> Result<()> {
        self.database()?.execute(
            "UPDATE outbox SET next_attempt=?2+1,error=?3 WHERE request_id=?1 AND state='pending'",
            params![
                id.to_string(),
                store::now() as i64,
                serde_json::to_string(reason)?
            ],
        )?;
        Ok(())
    }

    pub fn retry(&self, id: Uuid, error: &Value) -> Result<()> {
        self.database()?.execute("UPDATE outbox SET attempts=attempts+1,next_attempt=?2+MIN(60,1 << MIN(attempts,6)),error=?3 WHERE request_id=?1",params![id.to_string(),(store::now() as i64),serde_json::to_string(error)?])?;
        Ok(())
    }
}
impl Queue {
    pub fn stats(&self) -> Result<Value> {
        let conn = self.database()?;
        let pending: i64 = conn.query_row(
            "SELECT count(*) FROM outbox WHERE state='pending'",
            [],
            |r| r.get(0),
        )?;
        // Preserve the old inbox verbatim for explicit inspection or migration.
        // Nothing in this runtime consumes or acknowledges these records.
        // New installations do not create an incoming queue at all.
        let legacy_inbox: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='inbox')",
            [],
            |r| r.get(0),
        )?;
        let (retained, retained_pending): (i64, i64) = if legacy_inbox {
            conn.query_row(
                "SELECT count(*),count(CASE WHEN delivered=0 THEN 1 END) FROM inbox",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?
        } else {
            (0, 0)
        };
        let failed: i64 = conn.query_row(
            "SELECT count(*) FROM outbox WHERE state='failed'",
            [],
            |r| r.get(0),
        )?;
        Ok(
            json!({"pending_requests":pending,"pending_webhooks":0,"failed_requests":failed,
                "retained_legacy_deliveries":retained,"retained_legacy_pending_deliveries":retained_pending,
                "incoming_delivery":"delivery_moved_to_ting"}),
        )
    }
}
impl Queue {
    pub fn expire_transient_commands(&self) -> Result<()> {
        self.database()?.execute("UPDATE outbox SET state='failed',error=?1 WHERE state='pending' AND json_extract(request,'$.request.operation')='set_presence'",params![json!({"code":"transient_expired","message":"presence change expired when the daemon stopped; send a current activity"}).to_string()])?;
        Ok(())
    }
}

#[cfg(test)]
mod outgoing_tests {
    use super::*;

    #[test]
    fn outgoing_retry_survives_restart_without_changing_the_original_request() -> Result<()> {
        let root = std::env::temp_dir().join(format!("dm-outgoing-{}", Uuid::new_v4()));
        let state = store::Store::new(&root)?;
        let request = RelayRequest {
            request_id: Uuid::new_v4(),
            profile: "default".into(),
            testing_environment_id: None,
            testing_generation: None,
            request: crate::relay::Operation::CreateConversation {
                participant_ids: vec!["c:alice".into(), "c:bob".into()],
                idempotency_key: "original-retry-key".into(),
            },
        };
        let mut original = serde_json::to_value(&request)?;
        original["caller_metadata"] = json!({"retained":"verbatim"});
        let queue = Queue::open(&state)?;
        queue.enqueue(&request, &original)?;
        queue.retry(request.request_id, &json!({"code":"transport_error"}))?;
        drop(queue);
        let queue = Queue::open(&state)?;
        let stored = queue.result(request.request_id)?.unwrap();
        assert_eq!(stored.state, "pending");
        assert_eq!(stored.request, original);
        queue.enqueue(&request, &original)?;
        let mut conflicting = original.clone();
        conflicting["request"]["idempotency_key"] = json!("different");
        assert!(queue.enqueue(&request, &conflicting).is_err());
        queue
            .database()?
            .execute("UPDATE outbox SET next_attempt=0", [])?;
        let candidates = queue.request_candidates()?;
        assert_eq!(candidates.len(), 1);
        let replay = queue.load_request(candidates[0].request_id)?.unwrap();
        assert_eq!(
            serde_json::to_value(replay)?["request"],
            original["request"]
        );
        queue.finish(request.request_id, Some(&json!({"saved":true})), None)?;
        drop(queue);
        let queue = Queue::open(&state)?;
        assert!(queue.request_candidates()?.is_empty());
        assert_eq!(queue.result(request.request_id)?.unwrap().request, original);
        drop(queue);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn legacy_incoming_records_are_retained_without_an_active_delivery_queue() -> Result<()> {
        let root = std::env::temp_dir().join(format!("dm-retired-inbox-{}", Uuid::new_v4()));
        let state = store::Store::new(&root)?;
        let queue = Queue::open(&state)?;
        assert_eq!(queue.stats()?["retained_legacy_deliveries"], 0);
        assert!(!queue.database()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='inbox')",
            [],
            |r| r.get::<_, bool>(0)
        )?);
        queue.database()?.execute_batch("CREATE TABLE inbox(session TEXT,delivery_id TEXT,actor TEXT,sequence INTEGER,frame TEXT,delivered INTEGER NOT NULL DEFAULT 0,attempts INTEGER NOT NULL DEFAULT 0); CREATE TABLE cursors(session TEXT,actor TEXT,sequence INTEGER);")?;
        queue.database()?.execute(
            "INSERT INTO inbox(session,delivery_id,actor,sequence,frame,attempts) VALUES('default:test#1','old','c:alice',1,'{\"preserved\":true}',7)", [],
        )?;
        queue.database()?.execute(
            "INSERT INTO cursors VALUES('default:test#1','c:alice',1)",
            [],
        )?;
        queue.adopt_generation("default:test", Some(2))?;
        drop(queue);
        let queue = Queue::open(&state)?;
        let stats = queue.stats()?;
        assert_eq!(stats["pending_webhooks"], 0);
        assert_eq!(stats["retained_legacy_pending_deliveries"], 1);
        assert_eq!(stats["incoming_delivery"], "delivery_moved_to_ting");
        let legacy: (String, i64, i64) = queue.database()?.query_row(
            "SELECT frame,attempts,delivered FROM inbox WHERE delivery_id='old'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(legacy, ("{\"preserved\":true}".into(), 7, 0));
        assert_eq!(
            queue
                .database()?
                .query_row("SELECT sequence FROM cursors", [], |r| r.get::<_, i64>(0))?,
            1
        );
        assert!(queue.request_candidates()?.is_empty());
        drop(queue);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}

#[cfg(test)]
mod generation_tests {
    use super::*;
    #[test]
    fn unknown_and_old_sends_cannot_cross_a_clean_or_an_environment() -> Result<()> {
        let root = std::env::temp_dir().join(format!("dm-generation-{}", Uuid::new_v4()));
        let state = store::Store::new(&root)?;
        let queue = Queue::open(&state)?;
        let id = Uuid::new_v4();
        let session = store::session_key("default", Some(id));
        let mut request = RelayRequest {
            request_id: Uuid::new_v4(),
            profile: "default".into(),
            testing_environment_id: Some(id),
            testing_generation: None,
            request: crate::relay::Operation::CreateConversation {
                participant_ids: vec!["c:alice".into(), "c:bob".into()],
                idempotency_key: "fixture".into(),
            },
        };
        assert!(
            queue
                .enqueue(&request, &serde_json::to_value(&request)?)
                .is_err()
        );
        // A pre-upgrade, unknown-generation row must not inherit the first new generation.
        queue.database()?.execute(
            "INSERT INTO outbox(request_id,session,request,created) VALUES('legacy',?1,'{}',0)",
            params![session],
        )?;
        queue.adopt_generation(&session, Some(1))?;
        queue.enqueue(&request, &serde_json::to_value(&request)?)?;
        let old = request.request_id;
        request.request_id = Uuid::new_v4();
        request.testing_environment_id = None;
        queue.enqueue(&request, &serde_json::to_value(&request)?)?;
        let production = request.request_id;
        request.request_id = Uuid::new_v4();
        request.testing_environment_id = Some(Uuid::new_v4());
        request.testing_generation = Some(9);
        queue.enqueue(&request, &serde_json::to_value(&request)?)?;
        let other = request.request_id;
        queue.adopt_generation(&session, Some(2))?;
        for (id, expected) in [
            ("legacy".into(), "failed"),
            (old.to_string(), "failed"),
            (production.to_string(), "pending"),
            (other.to_string(), "pending"),
        ] {
            let status: String = queue.database()?.query_row(
                "SELECT state FROM outbox WHERE request_id=?1",
                params![id],
                |r| r.get(0),
            )?;
            assert_eq!(status, expected);
        }
        drop(queue);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
