use super::store;
use crate::{
    ServerFrame,
    relay::{RelayRequest, RelayRequestStatus, RelayResult},
};
use anyhow::{Context, Result, bail};
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

pub(crate) struct WebhookCandidate {
    pub session: String,
    pub delivery_id: String,
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
      CREATE TABLE IF NOT EXISTS cursors(session TEXT NOT NULL, actor TEXT NOT NULL, sequence INTEGER NOT NULL, PRIMARY KEY(session,actor));
      CREATE TABLE IF NOT EXISTS generations(session TEXT PRIMARY KEY,generation INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS inbox(session TEXT NOT NULL, delivery_id TEXT NOT NULL, actor TEXT NOT NULL, sequence INTEGER NOT NULL, frame TEXT NOT NULL, delivered INTEGER NOT NULL DEFAULT 0, attempts INTEGER NOT NULL DEFAULT 0, next_attempt INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(session,delivery_id), UNIQUE(session,actor,sequence));
      CREATE TABLE IF NOT EXISTS outbox(request_id TEXT PRIMARY KEY,session TEXT NOT NULL,request TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'pending',result TEXT,error TEXT,attempts INTEGER NOT NULL DEFAULT 0,next_attempt INTEGER NOT NULL DEFAULT 0,created INTEGER NOT NULL,expected_generation INTEGER);
      CREATE INDEX IF NOT EXISTS inbox_pending ON inbox(delivered,next_attempt);
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
    pub fn cursor(&self, session: &str, actor: &str) -> Result<i64> {
        Ok(self
            .database()?
            .query_row(
                "SELECT sequence FROM cursors WHERE session=?1 AND actor=?2",
                params![session, actor],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }
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
pub fn stream_session(session: &str, generation: Option<i64>) -> String {
    generation.map_or_else(|| session.to_owned(), |n| format!("{session}#{n}"))
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
        if previous.is_some() && previous != Some(next) {
            let stream = stream_session(session, previous);
            tx.execute(
                "UPDATE inbox SET delivered=-1 WHERE session=?1 AND delivered=0",
                params![stream],
            )?;
            tx.execute("UPDATE outbox SET state='failed',error=?2 WHERE session=?1 AND state='pending'",params![session,json!({"code":"testing_generation_changed","message":"test environment changed; inspect then explicitly resubmit this request"}).to_string()])?;
        }
        tx.execute("INSERT INTO generations(session,generation) VALUES(?1,?2) ON CONFLICT(session) DO UPDATE SET generation=excluded.generation",params![session,next])?;
        tx.commit()?;
        Ok(())
    }
}
/// Commits the exact frame and contiguous cursor in one FULL-synchronous transaction.
impl Queue {
    pub fn receive(&self, session: &str, frame: &ServerFrame, raw: &str) -> Result<i64> {
        let (id, actor, sequence) = frame
            .delivery_position()
            .context("frame has no delivery sequence")?;
        let mut conn = self.database()?;
        let tx = conn.transaction()?;
        let old: Option<(String, i64, StoredDeliveryIdentity)> = tx
            .query_row(
                "SELECT actor,sequence,frame FROM inbox WHERE session=?1 AND delivery_id=?2",
                params![session, id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, json_column(r, 2)?)),
            )
            .optional()?;
        if let Some((old_actor, old_sequence, old_frame)) = old {
            if old_actor != actor
                || old_sequence != sequence
                || !same_delivery_identity(&old_frame, frame)
            {
                bail!("server reused a delivery ID with a different immutable identity")
            }
            // Replays may hydrate newer message versions/status. Keep the
            // already committed callback payload stable for this delivery ID;
            // revisions and receipts also have their own durable delivery IDs.
        } else {
            tx.execute(
            "INSERT INTO inbox(session,delivery_id,actor,sequence,frame) VALUES(?1,?2,?3,?4,?5)",
            params![session, id.to_string(), actor, sequence, raw],
        )?;
        }
        let mut through: i64 = tx
            .query_row(
                "SELECT sequence FROM cursors WHERE session=?1 AND actor=?2",
                params![session, actor],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        loop {
            let next: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM inbox WHERE session=?1 AND actor=?2 AND sequence=?3)",
                params![session, actor, through + 1],
                |r| r.get(0),
            )?;
            if !next {
                break;
            }
            through += 1;
        }
        tx.execute("INSERT INTO cursors(session,actor,sequence) VALUES(?1,?2,?3) ON CONFLICT(session,actor) DO UPDATE SET sequence=excluded.sequence",params![session,actor,through])?;
        tx.commit()?;
        Ok(through)
    }
}

// Struct deserialization skips unknown content fields in-place. An internally
// tagged enum would buffer the entire message before choosing its variant.
#[derive(serde::Deserialize)]
struct StoredDeliveryIdentity {
    #[serde(rename = "type")]
    kind: String,
    message: Option<StoredMessageIdentity>,
    message_id: Option<Uuid>,
    data: Option<StoredDeliveryData>,
}
#[derive(serde::Deserialize)]
struct StoredDeliveryData {
    id: Option<Uuid>,
    conversation_id: Option<Uuid>,
    sender: Option<crate::Actor>,
    sequence: Option<i64>,
    created_at: Option<String>,
    message_id: Option<Uuid>,
}
#[derive(serde::Deserialize)]
struct StoredMessageIdentity {
    id: Uuid,
    conversation_id: Uuid,
    sender: crate::Actor,
    sequence: i64,
    created_at: String,
}
fn same_delivery_identity(old: &StoredDeliveryIdentity, new: &ServerFrame) -> bool {
    match new {
        ServerFrame::Message { message: new, .. }
            if matches!(old.kind.as_str(), "message" | "new_message") =>
        {
            if let Some(data) = &old.data {
                return data.id == Some(new.id)
                    && data.conversation_id == Some(new.conversation_id)
                    && data.sender.as_ref() == Some(&new.sender)
                    && data.sequence == Some(new.sequence)
                    && data.created_at.as_ref() == Some(&new.created_at);
            }
            let Some(old) = &old.message else {
                return false;
            };
            old.id == new.id
                && old.conversation_id == new.conversation_id
                && old.sender == new.sender
                && old.sequence == new.sequence
                && old.created_at == new.created_at
        }
        ServerFrame::Receipt {
            message_id: new, ..
        } if old.kind == "receipt" => {
            old.message_id
                .as_ref()
                .or_else(|| old.data.as_ref().and_then(|data| data.message_id.as_ref()))
                == Some(new)
        }
        _ => false,
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
        // Requests accepted before the first socket handshake bind once the epoch
        // is known. Never replace an already captured epoch during a later retry.
        conn.execute("UPDATE outbox SET expected_generation=(SELECT generation FROM generations WHERE generations.session=outbox.session) WHERE state='pending' AND expected_generation IS NULL AND EXISTS (SELECT 1 FROM generations WHERE generations.session=outbox.session)", [])?;
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
pub struct WebhookWork {
    pub session: String,
    pub delivery_id: String,
    pub frame: Value,
}
impl Queue {
    pub(crate) fn webhook_candidates(&self, profiles: &[String]) -> Result<Vec<WebhookCandidate>> {
        let conn = self.database()?;
        let mut stmt=conn.prepare("SELECT session,delivery_id,octet_length(frame) FROM inbox i WHERE delivered=0 AND next_attempt<=?1 AND (CASE WHEN instr(session,'#')>0 THEN substr(session,1,instr(session,'#')-1) ELSE session END) IN (SELECT value FROM json_each(?2)) AND NOT EXISTS (SELECT 1 FROM inbox prior WHERE prior.session=i.session AND prior.actor=i.actor AND prior.delivered=0 AND prior.sequence<i.sequence) ORDER BY rowid LIMIT 50")?;
        let rows = stmt
            .query_map(
                params![(store::now() as i64), serde_json::to_string(profiles)?],
                |r| {
                    Ok(WebhookCandidate {
                        session: r.get(0)?,
                        delivery_id: r.get(1)?,
                        payload_bytes: r.get(2)?,
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub(crate) fn load_webhook(&self, session: &str, id: &str) -> Result<Option<WebhookWork>> {
        Ok(self.database()?.query_row(
            "SELECT frame FROM inbox WHERE session=?1 AND delivery_id=?2 AND delivered=0 AND next_attempt<=?3",
            params![session, id, store::now() as i64],
            |row| Ok(WebhookWork {
                session: session.to_owned(), delivery_id: id.to_owned(), frame: json_column(row, 0)?,
            }),
        ).optional()?)
    }
}
impl Queue {
    pub fn webhook_is_pending(&self, session: &str, id: &str) -> Result<bool> {
        Ok(self.database()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM inbox WHERE session=?1 AND delivery_id=?2 AND delivered=0)",
            params![session, id],
            |row| row.get(0),
        )?)
    }

    pub fn webhook_done(
        &self,
        session: &str,
        id: &str,
        success: bool,
        receipt: Option<&RelayRequest>,
    ) -> Result<()> {
        let mut conn = self.database()?;
        let tx = conn.transaction()?;
        // A generation change can archive this row while the callback is in
        // flight. Completion must neither revive it nor queue an old receipt.
        let changed = tx.execute(
            "UPDATE inbox SET delivered=?3,attempts=attempts+1,next_attempt=?4+MIN(300,1 << MIN(attempts,9)) WHERE session=?1 AND delivery_id=?2 AND delivered=0",
            params![session, id, success, store::now() as i64],
        )?;
        if changed == 0 {
            tx.commit()?;
            return Ok(());
        }
        if success && let Some(request) = receipt {
            tx.execute(
            "INSERT OR IGNORE INTO outbox(request_id,session,request,created,expected_generation) VALUES(?1,?2,?3,?4,?5)",
            params![
                request.request_id.to_string(),
                store::session_key(&request.profile, request.testing_environment_id),
                serde_json::to_string(request)?,
                store::now() as i64,
                request.testing_generation
            ],
        )?;
        }
        tx.commit()?;
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
        let callbacks: i64 =
            conn.query_row("SELECT count(*) FROM inbox WHERE delivered=0", [], |r| {
                r.get(0)
            })?;
        let failed: i64 = conn.query_row(
            "SELECT count(*) FROM outbox WHERE state='failed'",
            [],
            |r| r.get(0),
        )?;
        Ok(
            json!({"pending_requests":pending,"pending_webhooks":callbacks,"failed_requests":failed}),
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
mod webhook_tests {
    use super::*;
    #[test]
    fn unhooked_profiles_do_not_starve_hooked_profiles_or_lose_events() -> Result<()> {
        let root = std::env::temp_dir().join(format!("dm-hook-queue-{}", Uuid::new_v4()));
        let store = store::Store::new(&root)?;
        let queue = Queue::open(&store)?;
        for i in 0..60 {
            queue.database()?.execute("INSERT INTO inbox(session,delivery_id,actor,sequence,frame) VALUES(?1,?2,'cos:tos',1,'{}')",
                params![format!("unhooked-{i}:production"), i.to_string()])?;
        }
        queue.database()?.execute("INSERT INTO inbox(session,delivery_id,actor,sequence,frame) VALUES('hooked:test#3','last','cos:tos',1,'{}')", [])?;
        assert!(queue.webhook_candidates(&[])?.is_empty());
        let work = queue.webhook_candidates(&["hooked:test".into()])?;
        assert_eq!(work.len(), 1);
        assert_eq!(work[0].delivery_id, "last");
        assert_eq!(
            queue
                .webhook_candidates(&["unhooked-0:production".into()])?
                .len(),
            1
        );
        assert_eq!(
            queue.database()?.query_row(
                "SELECT count(*) FROM inbox WHERE delivered=0",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            61
        );
        drop(queue);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
