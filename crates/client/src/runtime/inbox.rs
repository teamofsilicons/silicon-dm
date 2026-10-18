//! Reading DM's sync feed (`GET /api/v1/sync`) with a profile's sign-in: one page
//! ([`LocalRuntime::sync_page`], `dm sync`) or a stream of new message references with their
//! current messages ([`LocalRuntime::inbox`], `dm inbox --watch`).
//!
//! The feed lists references, not content: each event names a conversation and a message
//! code; the inbox fetches the message under the account's current permissions. The inbox
//! keeps its cursor per profile (`inbox-<profile>.json`, saved after every page) so a
//! restarted watcher resumes where it stopped. Delivery is at least once: a consumer that
//! must not miss anything persists the `cursor` lines it has processed and restarts with
//! `--cursor`.
use super::{LocalRuntime, store};
use crate::{Client, ReceiptStatus, SyncEventKind, SyncPage, SyncRequest};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{future::Future, io::Write, time::Duration};
use uuid::Uuid;

/// Where an inbox starts reading.
#[derive(Clone, Debug)]
pub enum InboxStart {
    /// The profile's saved cursor, else [`InboxStart::Now`].
    Saved,
    /// Only what arrives from now on.
    Now,
    /// Everything the service still keeps for the account.
    Beginning,
    /// A cursor from an earlier `cursor` line or `dm sync`.
    Cursor(String),
}

#[derive(Clone, Debug)]
pub struct InboxOptions {
    pub profile: String,
    /// Keep reading (long-poll) until stopped; otherwise read what is pending and return.
    pub watch: bool,
    /// Seconds each long-poll waits for something new (1 to 30).
    pub wait: u16,
    pub start: InboxStart,
    /// Fetch each referenced message (default); otherwise emit references only.
    pub hydrate: bool,
    /// Record a delivered receipt for each new message from someone else.
    pub mark_delivered: bool,
    pub limit: Option<u16>,
    /// Also emit conversation events (`{"type":"conversation","change":…}`): conversations
    /// that appeared for, changed for or left the account (DM 0.17, `include=conversations`).
    pub conversations: bool,
}

#[derive(Serialize, Deserialize)]
struct SavedCursor {
    cursor: String,
    account_uuid: String,
    api_url: String,
    saved_at: u64,
}

fn cursor_path(state: &store::Store, key: &str) -> std::path::PathBuf {
    let name = key.bytes().map(|b| format!("{b:02x}")).collect::<String>();
    state.directory().join(format!("inbox-{name}.json"))
}

fn load_cursor(state: &store::Store, key: &str, profile: &store::Profile) -> Option<String> {
    let bytes = std::fs::read(cursor_path(state, key)).ok()?;
    let saved: SavedCursor = serde_json::from_slice(&bytes).ok()?;
    let account = profile.account.as_ref()?;
    (saved.account_uuid == account.uuid && saved.api_url == profile.base_url)
        .then_some(saved.cursor)
}

fn save_cursor(
    state: &store::Store,
    key: &str,
    profile: &store::Profile,
    cursor: &str,
) -> Result<()> {
    let Some(account) = profile.account.as_ref() else {
        return Ok(());
    };
    let path = cursor_path(state, key);
    let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let mut file = store::secure_open(&temporary)?;
    file.write_all(&serde_json::to_vec(&SavedCursor {
        cursor: cursor.to_owned(),
        account_uuid: account.uuid.clone(),
        api_url: profile.base_url.clone(),
        saved_at: store::now(),
    })?)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temporary, &path)?;
    Ok(())
}

impl LocalRuntime {
    /// Runs `call` with the profile's DM client, refreshing the sign-in first when needed.
    /// A credential the service refuses is refreshed once and the call repeated (a 401 means
    /// nothing ran).
    pub async fn with_client<T, F, Fut>(&self, profile: &str, call: F) -> Result<T>
    where
        F: Fn(Client) -> Fut,
        Fut: Future<Output = crate::Result<T>>,
    {
        let key = store::session_key(profile);
        for attempt in 0..2 {
            let (config, stored) = self.store.fresh_profile(&key).await?;
            let client = store::client(&config, &stored)?;
            match call(client).await {
                Err(error) if error.unauthorized() && attempt == 0 => {
                    let refused = stored.access_token().map(str::to_owned);
                    self.store.update(|config| {
                        if let Some(current) = config.profiles.get_mut(&key)
                            && current.access_token().map(str::to_owned) == refused
                        {
                            current.expires_at = 0;
                        }
                        Ok(())
                    })?;
                }
                result => return result.map_err(Into::into),
            }
        }
        unreachable!("the second attempt always returns")
    }

    /// One page of the account's sync feed.
    pub async fn sync_page(&self, profile: &str, request: &SyncRequest) -> Result<SyncPage> {
        self.with_client(profile, |client| async move { client.sync(request).await })
            .await
    }
}

impl LocalRuntime {
    /// Reads the sync feed and calls `emit` with one JSON line per event, then a `cursor` line
    /// after each page that had events:
    ///
    /// - `{"type":"message"|"message_status","event_id","sequence","conversation_id",
    ///   "message_id","message"?}` (`message` is the current message; `inaccessible: true`
    ///   when the account can no longer read it),
    /// - `{"type":"cursor","cursor","upper_sequence"}`,
    /// - `{"type":"start","from":"now"|"beginning"|"saved"|"cursor","cursor"}` first, and
    ///   `{"type":"reset",…}` when the service asks for a fresh start (refresh conversations).
    ///
    /// Passing problems (network, the service restarting) are retried with a growing pause and
    /// reported through `notice`; a sign-in that ended stops the inbox with an error. Without
    /// `watch` it returns once nothing more is pending.
    pub async fn inbox(
        &self,
        options: &InboxOptions,
        mut emit: impl FnMut(Value) -> Result<()>,
        mut notice: impl FnMut(&str),
    ) -> Result<Value> {
        let key = store::session_key(&options.profile);
        let (_, profile) = self.store.fresh_profile(&key).await?;
        let me = profile
            .account
            .as_ref()
            .map(|a| a.uuid.clone())
            .unwrap_or_default();
        let wait = options.wait.clamp(1, 30);
        let (mut cursor, from) = match &options.start {
            InboxStart::Cursor(cursor) => (Some(cursor.clone()), "cursor"),
            InboxStart::Beginning => (None, "beginning"),
            InboxStart::Saved => match load_cursor(&self.store, &key, &profile) {
                Some(saved) => (Some(saved), "saved"),
                None => (None, "now"),
            },
            InboxStart::Now => (None, "now"),
        };
        if from == "now" {
            let page = self
                .sync_page(
                    &options.profile,
                    &SyncRequest {
                        reset: true,
                        ..SyncRequest::default()
                    },
                )
                .await?;
            cursor = Some(page.cursor);
        }
        emit(json!({"type":"start","from":from,"cursor":cursor,"profile":options.profile}))?;
        let mut emitted = 0_u64;
        let mut pause = Duration::from_secs(1);
        loop {
            let request = SyncRequest {
                cursor: cursor.clone(),
                limit: options.limit,
                reset: false,
                wait: options.watch.then_some(wait),
                include: options
                    .conversations
                    .then(|| crate::SYNC_INCLUDE_CONVERSATIONS.to_owned()),
            };
            let page = match self.sync_page(&options.profile, &request).await {
                Ok(page) => {
                    pause = Duration::from_secs(1);
                    page
                }
                Err(error)
                    if error
                        .downcast_ref::<crate::Error>()
                        .is_some_and(crate::Error::sync_reset_required) =>
                {
                    let page = self
                        .sync_page(
                            &options.profile,
                            &SyncRequest {
                                reset: true,
                                ..SyncRequest::default()
                            },
                        )
                        .await?;
                    cursor = Some(page.cursor.clone());
                    save_cursor(&self.store, &key, &profile, &page.cursor)?;
                    emit(
                        json!({"type":"reset","reason":"sync_reset_required","cursor":page.cursor,
                        "message":"The service could not continue from the previous cursor (it expired or DM was upgraded); events in between may be missing. Read current conversations with dm conversations list."}),
                    )?;
                    continue;
                }
                Err(error) if options.watch && passing(&error) => {
                    let said = format!("{error:#}");
                    // The service's words end a sentence already; the retry is the next one.
                    let joiner = if said.ends_with(['.', '!', '?']) {
                        " "
                    } else {
                        ". "
                    };
                    notice(&format!("{said}{joiner}Retrying in {} s.", pause.as_secs()));
                    tokio::time::sleep(pause).await;
                    pause = (pause * 2).min(Duration::from_secs(30));
                    continue;
                }
                Err(error) => return Err(error),
            };
            let had_events = !page.events.is_empty();
            for event in &page.events {
                if event.kind == SyncEventKind::Conversation {
                    // A conversation changed: there is no message to fetch, and reading the
                    // conversation is up to the reader (it may be gone: `left`, `removed`).
                    // A `read` event says what is unread there now.
                    let mut line = json!({
                        "type": "conversation",
                        "event_id": event.event_id,
                        "sequence": event.sequence,
                        "conversation_id": event.conversation_id,
                        "change": event.change,
                    });
                    if let Some(unread) = event.unread_count {
                        line["unread_count"] = json!(unread);
                        line["last_read_message_id"] = json!(event.last_read_message_id);
                    }
                    emit(line)?;
                    emitted += 1;
                    continue;
                }
                let mut line = json!({
                    "type": match event.kind { SyncEventKind::MessageStatus => "message_status", _ => "message" },
                    "event_id": event.event_id,
                    "sequence": event.sequence,
                    "conversation_id": event.conversation_id,
                    "message_id": event.message_id,
                });
                if options.hydrate || options.mark_delivered {
                    let (conversation, message_id) = (&event.conversation_id, &event.message_id);
                    match self
                        .with_client(&options.profile, |client| async move {
                            client.message(conversation, message_id).await
                        })
                        .await
                    {
                        Ok(message) => {
                            let from_someone_else =
                                message.sender.uuid.as_deref() != Some(me.as_str());
                            if options.mark_delivered
                                && event.kind == SyncEventKind::Message
                                && from_someone_else
                                && message.deleted_at.is_none()
                                && message.delivered_at.is_none()
                            {
                                let device = profile.device_id.as_str();
                                let receipt = self
                                    .with_client(&options.profile, |client| async move {
                                        client
                                            .record_receipt(
                                                conversation,
                                                message_id,
                                                ReceiptStatus::Delivered,
                                                device,
                                            )
                                            .await
                                    })
                                    .await;
                                line["delivered_receipt"] = json!(receipt.is_ok());
                            }
                            if options.hydrate {
                                line["message"] = serde_json::to_value(&message)?;
                            }
                        }
                        Err(error)
                            if error
                                .downcast_ref::<crate::Error>()
                                .and_then(crate::Error::status)
                                .is_some_and(|status| status == 403 || status == 404) =>
                        {
                            line["inaccessible"] = json!(true);
                        }
                        Err(error) => return Err(error.context("reading a referenced message")),
                    }
                }
                emit(line)?;
                emitted += 1;
            }
            cursor = Some(page.cursor.clone());
            save_cursor(&self.store, &key, &profile, &page.cursor)
                .context("saving the inbox cursor")?;
            if had_events {
                emit(
                    json!({"type":"cursor","cursor":page.cursor,"upper_sequence":page.upper_sequence}),
                )?;
            }
            if !page.has_more && !options.watch {
                return Ok(json!({"type":"done","events":emitted,"cursor":cursor}));
            }
        }
    }
}

fn passing(error: &anyhow::Error) -> bool {
    if let Some(error) = error.downcast_ref::<crate::Error>() {
        return error.retryable();
    }
    error
        .downcast_ref::<crate::accounts::SignInError>()
        .is_some_and(crate::accounts::SignInError::is_retryable)
}
