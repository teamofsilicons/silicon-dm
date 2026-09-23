//! Direct setup of Ting's system daemon. No incoming socket, queue, or forwarding
//! runs in DM. A destination receives raw `{"tings":[...]}` for every authorized
//! app; route by `type` and fetch DM references through authenticated DM APIs.

use super::{LocalRuntime, store, validate_callback_endpoint};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::future::Future;
use ting_client::{Client, Profile, Session, TestHeaders};
use url::Url;
use uuid::Uuid;

/// Ting's test audience credential and the shared environment root key.
/// They are verified through IAM before the SLT is sent to Ting, never logged or
/// persisted here. Ting remembers validated test context with its opaque session.
pub struct DeliveryTestCredentials<'a> {
    pub app_secret: &'a str,
    pub environment_key: &'a str,
}

/// An explicit, fresh Ting login for the same member and organization as DM.
/// DM access/refresh credentials are never accepted as Ting credentials.
pub struct DeliveryLoginOptions<'a> {
    pub profile: &'a str,
    pub testing_environment_id: Option<Uuid>,
    pub ting_api_url: &'a str,
    pub short_lived_token: &'a str,
    /// Reuse this key and the exact SLT after an uncertain login response.
    pub idempotency_key: &'a str,
    pub testing: Option<DeliveryTestCredentials<'a>>,
}

/// One explicit local destination on the installed Ting system daemon.
pub struct DeliveryAttachOptions<'a> {
    pub profile: &'a str,
    pub testing_environment_id: Option<Uuid>,
    pub webhook_url: &'a Url,
    /// Reattach this exact retained hook. Omit only for initial creation or to
    /// reuse the hook already bound to this DM profile.
    pub webhook_id: Option<&'a str>,
    pub secret: Option<&'a str>,
    pub health_url: Option<&'a Url>,
    /// Explicitly take this same hook from another live receiver.
    pub takeover: bool,
    /// Ting hooks receive every eligible application, not only DM. The caller
    /// must accept and route that generic batch before enabling delivery.
    pub accept_all_apps: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct Binding {
    version: u8,
    dm_api_url: String,
    actor: crate::Actor,
    organization_id: String,
    environment_id: Option<Uuid>,
    generation: Option<i64>,
    ting_api_url: String,
    session_digest: String,
    #[serde(default)]
    webhook_id: Option<String>,
    #[serde(default)]
    detached: bool,
    #[serde(default)]
    pending: Option<AttachmentIntent>,
}

#[derive(Clone, Serialize, Deserialize)]
struct AttachmentIntent {
    // Kept inside the official private Ting profile, never the DM backend/config.
    url: String,
    fingerprint: String,
}

struct Selection {
    directory: Profile,
    dm_api_url: String,
    identity: crate::Identity,
    info: crate::IamInfo,
}

impl Selection {
    fn matches(&self, binding: &Binding) -> bool {
        binding.version == 1
            && binding.dm_api_url == self.dm_api_url
            && binding.actor == self.identity.actor
            && binding.organization_id == self.identity.organization_id
            && binding.environment_id == self.info.testing_environment_id
            && binding.generation == self.info.testing_generation
    }
}

fn digest(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex().to_string()
}

fn clean_string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    let value = value[field]
        .as_str()
        .context("Ting omitted a required response field")?;
    ting_client::nonempty(value, field)?;
    Ok(value)
}

async fn verify_identity(
    client: &Client,
    session: &Session,
    identity: &crate::Identity,
    info: &crate::IamInfo,
) -> Result<()> {
    let me = client
        .json(
            "GET",
            "/v1/me",
            None,
            Some(&session.token),
            &TestHeaders::default(),
        )
        .await?;
    let kind = match identity.actor.actor_type {
        crate::ActorType::Carbon => "carbon",
        crate::ActorType::Silicon => "silicon",
    };
    ensure!(
        me["authenticated"] == true && me["id"] == identity.actor.id && me["kind"] == kind,
        "Ting session belongs to another member or member type"
    );
    // The session's live attestation is authoritative. A locally selected test
    // profile or a successful login alone cannot prove Ting's current plane.
    // In particular, older servers that omit this field are never production.
    let environment = &me["environment"];
    let matches = match (info.testing_environment_id, info.testing_generation) {
        (None, None) => {
            environment["kind"] == "production"
                && environment.get("id").is_none()
                && environment.get("generation").is_none()
        }
        (Some(id), Some(generation)) if !id.is_nil() && generation > 0 => {
            environment["kind"] == "testing"
                && environment["id"]
                    .as_str()
                    .and_then(|value| Uuid::parse_str(value).ok())
                    == Some(id)
                && environment["generation"].as_i64() == Some(generation)
        }
        _ => false,
    };
    ensure!(
        matches,
        "Ting did not attest the selected DM environment and generation; use a current Ting server and log in again for the matching context"
    );
    let orgs = client
        .json(
            "GET",
            "/v1/orgs",
            None,
            Some(&session.token),
            &TestHeaders::default(),
        )
        .await?;
    crate::ting::resolve_ting_organization(&identity.organization_id, &orgs)?;
    Ok(())
}

impl LocalRuntime {
    async fn delivery_selection(&self, name: &str, test: Option<Uuid>) -> Result<Selection> {
        let key = store::session_key(name, test);
        let (config, profile) = self.store.fresh_profile(&key).await?;
        ensure!(
            profile.testing_environment_id == test,
            "DM profile testing context does not match"
        );
        let client = store::client(&config, &profile)?;
        let info = client.iam().await?;
        ensure!(
            info.testing_environment_id == test,
            "DM did not confirm the selected testing environment"
        );
        ensure!(
            match test {
                Some(_) => info
                    .testing_generation
                    .is_some_and(|generation| generation > 0),
                None => info.testing_generation.is_none(),
            },
            "DM returned an invalid testing generation"
        );
        let identity = client.me().await?;
        ensure!(
            identity.actor == profile.tokens.actor
                && identity.organization_id == profile.tokens.organization_id,
            "DM profile identity or organization changed; log in explicitly again"
        );
        Ok(Selection {
            directory: self.store.ting_profile(&key, info.testing_generation)?,
            dm_api_url: profile.base_url.trim_end_matches('/').to_owned(),
            identity,
            info,
        })
    }

    /// Log in to Ting separately using a Ting-bound SLT. This does not enroll DM
    /// delivery consent, attach a webhook, or start another daemon.
    pub async fn delivery_login(&self, options: &DeliveryLoginOptions<'_>) -> Result<Value> {
        ting_client::nonempty(options.short_lived_token, "Ting SLT")?;
        ensure!(
            (16..=200).contains(&options.idempotency_key.len())
                && options
                    .idempotency_key
                    .bytes()
                    .all(|b| b.is_ascii_graphic()),
            "Ting login idempotency key must contain 16-200 visible ASCII bytes"
        );
        ensure!(
            options.testing.is_some() == options.testing_environment_id.is_some(),
            "Ting test credentials are required together only for the selected test environment"
        );
        let selection = self
            .delivery_selection(options.profile, options.testing_environment_id)
            .await?;
        let _lock = selection.directory.lock()?;
        let client = Client::new(options.ting_api_url)?;
        if let Some(previous) = selection.directory.read::<Binding>("dm-binding.json")? {
            ensure!(
                selection.matches(&previous) && previous.ting_api_url == client.origin,
                "Ting profile belongs to another member, organization, backend or environment"
            );
        }
        ensure!(
            selection
                .directory
                .read::<Session>("session.json")?
                .is_none(),
            "Ting delivery is already logged in; use delivery_status or delivery_logout before a new login"
        );
        let mut testing = TestHeaders::default();
        if let Some(credentials) = &options.testing {
            let iam = silicon_iam_client::Client::builder(&selection.info.iam_base_url)?
                .credential(silicon_iam_client::Credential::application(
                    "tos>ting",
                    credentials.app_secret,
                ))
                .environment(silicon_iam_client::EnvironmentKey::new(
                    credentials.environment_key,
                )?)
                .telemetry(false)
                .build()?;
            let context =
                iam.applications().testing_context().await.map_err(|_| {
                    anyhow::anyhow!("IAM could not verify Ting's testing credentials")
                })?;
            ensure!(
                Some(context.environment_id) == options.testing_environment_id
                    && context.application.app_id == "tos>ting"
                    && ting_client::api_origin(&context.application.base_url)? == client.origin,
                "IAM Ting audience, environment or backend does not match DM's selected context"
            );
            testing.app_secret = Some(credentials.app_secret.to_owned());
            testing.key = Some(credentials.environment_key.to_owned());
        }
        let discovery = client
            .json("GET", "/v1/iam", None, None, &TestHeaders::default())
            .await?;
        ensure!(
            discovery["app_id"] == "tos>ting",
            "Ting backend disclosed an unexpected IAM audience"
        );
        let fingerprint = digest(&json!({"slt":options.short_lived_token,"api":client.origin,
            "environment":options.testing_environment_id,"generation":selection.info.testing_generation,
            "testing":testing}).to_string());
        let mut created = store::now();
        if let Some(attempt) = selection.directory.read::<Value>("dm-login-attempt.json")? {
            let age = store::now().saturating_sub(attempt["created"].as_u64().unwrap_or(0));
            if age <= 120 {
                ensure!(
                    attempt["fingerprint"] == fingerprint
                        && attempt["key"] == options.idempotency_key,
                    "Ting login outcome is uncertain; retry the exact SLT, context and key within two minutes"
                );
                created = attempt["created"]
                    .as_u64()
                    .context("invalid saved Ting login attempt")?;
            } else {
                ensure!(
                    attempt["slt_digest"] != digest(options.short_lived_token)
                        && attempt["key"] != options.idempotency_key,
                    "Ting login replay expired; obtain a fresh SLT and operation key"
                );
            }
        }
        selection.directory.save("dm-login-attempt.json", &json!({"fingerprint":fingerprint,
            "slt_digest":digest(options.short_lived_token),"key":options.idempotency_key,"created":created}))?;
        let response = client
            .request(
                "POST",
                "/v1/session",
                Some(serde_json::to_vec(
                    &json!({"slt":options.short_lived_token}),
                )?),
                None,
                &testing,
                Some(options.idempotency_key),
            )
            .await?;
        let session = Session {
            api_url: client.origin.clone(),
            id: clean_string(&response, "id")?.into(),
            token: clean_string(&response, "session_token")?.into(),
            context: response.get("context").cloned(),
        };
        ensure!(
            session.id == selection.identity.actor.id,
            "Ting login returned another member"
        );
        verify_identity(&client, &session, &selection.identity, &selection.info).await?;
        let mut binding = selection
            .directory
            .read::<Binding>("dm-binding.json")?
            .unwrap_or(Binding {
                version: 1,
                dm_api_url: selection.dm_api_url.clone(),
                actor: selection.identity.actor.clone(),
                organization_id: selection.identity.organization_id.clone(),
                environment_id: selection.info.testing_environment_id,
                generation: selection.info.testing_generation,
                ting_api_url: client.origin,
                session_digest: String::new(),
                webhook_id: None,
                detached: false,
                pending: None,
            });
        binding.session_digest = digest(&session.token);
        // The daemon ties its local destination to the previous opaque token.
        // A new login must explicitly rebind the retained ID and destination.
        if binding.webhook_id.is_some() {
            binding.detached = true;
        }
        // Save binding first: an interrupted write can be recovered by exact SLT
        // replay, and an unbound session is never trusted by attachment.
        selection.directory.save("dm-binding.json", &binding)?;
        selection.directory.save("session.json", &session)?;
        selection.directory.remove("dm-login-attempt.json")?;
        Ok(
            json!({"authenticated":true,"profile":options.profile,"member":selection.identity.actor,
            "organization_id":selection.identity.organization_id,"testing_environment_id":binding.environment_id,
            "testing_generation":binding.generation,"delivery_provider":"ting"}),
        )
    }

    async fn delivery_bound(
        &self,
        name: &str,
        test: Option<Uuid>,
    ) -> Result<(Selection, std::fs::File, Binding, Session)> {
        let selected = self.delivery_selection(name, test).await?;
        let lock = selected.directory.lock()?;
        let binding = selected
            .directory
            .read::<Binding>("dm-binding.json")?
            .context("call delivery_login with a fresh Ting-bound SLT first")?;
        ensure!(
            selected.matches(&binding),
            "saved Ting delivery context does not match the current DM session"
        );
        let session = selected.directory.session(&binding.ting_api_url)?;
        ensure!(
            digest(&session.token) == binding.session_digest && session.id == binding.actor.id,
            "Ting session is not bound to this verified DM delivery profile; log in explicitly again"
        );
        verify_identity(
            &Client::new(&binding.ting_api_url)?,
            &session,
            &selected.identity,
            &selected.info,
        )
        .await?;
        Ok((selected, lock, binding, session))
    }

    /// Configure the requested endpoint directly on Ting's installed daemon.
    /// It receives raw batches for all apps. Durably accept or route every item
    /// before returning HTTP 204 for the whole batch; do not acknowledge an
    /// unhandled app. Ting then owns the delivery/read ACK. Use DM's fetch/sync
    /// APIs for content and explicitly send DM delivered/read receipts as needed.
    pub async fn delivery_attach(&self, options: &DeliveryAttachOptions<'_>) -> Result<Value> {
        self.delivery_attach_with(options, ting_client::ipc).await
    }

    async fn delivery_attach_with<F, Fut>(
        &self,
        options: &DeliveryAttachOptions<'_>,
        ipc: F,
    ) -> Result<Value>
    where
        F: Fn(Value) -> Fut,
        Fut: Future<Output = ting_client::Result<Value>>,
    {
        ensure!(
            options.accept_all_apps,
            "Ting webhooks receive all authorized applications; explicitly accept generic Ting batches and route by type"
        );
        validate_callback_endpoint(options.webhook_url)?;
        if let Some(url) = options.health_url {
            validate_callback_endpoint(url)?;
        }
        if let Some(secret) = options.secret {
            ting_client::nonempty(secret, "webhook secret")?;
            ensure!(secret.len() <= 8192, "webhook secret is too long");
        }
        if let Some(id) = options.webhook_id {
            ting_client::nonempty(id, "webhook ID")?;
        }
        let (selected, _lock, mut binding, session) = self
            .delivery_bound(options.profile, options.testing_environment_id)
            .await?;
        if let (Some(requested), Some(saved)) = (options.webhook_id, binding.webhook_id.as_deref())
        {
            ensure!(
                requested == saved,
                "this profile retains another Ting hook; reconnect or unhook that exact ID instead of replacing it"
            );
        }
        let fingerprint = digest(
            &json!({"url":options.webhook_url,"secret":options.secret,
            "health_url":options.health_url,"takeover":options.takeover})
            .to_string(),
        );
        let mut id = options
            .webhook_id
            .map(str::to_owned)
            .or_else(|| binding.webhook_id.clone());
        if id.is_none() {
            // Recover a committed hook after losing the daemon's IPC response.
            // If it cannot be identified, never issue a second blind creation.
            let destinations = ipc(ipc_request(
                &selected.directory,
                &binding,
                &session,
                "destinations",
                json!({}),
            ))
            .await?;
            let matches = destinations
                .as_object()
                .context("Ting daemon returned invalid destinations")?
                .iter()
                .filter(|(_, url)| url.as_str() == Some(options.webhook_url.as_str()))
                .map(|(id, _)| id.to_owned())
                .collect::<Vec<_>>();
            ensure!(
                matches.len() <= 1,
                "several Ting hooks use this URL; supply the exact retained webhook_id"
            );
            id = matches.into_iter().next();
            if let Some(pending) = &binding.pending {
                ensure!(
                    pending.fingerprint == fingerprint,
                    "a prior Ting attachment is unresolved; retry its original settings or supply its exact webhook_id"
                );
                ensure!(
                    id.is_some(),
                    "Ting attachment outcome is uncertain; inspect Ting's retained hooks and supply its exact webhook_id; no replacement was created"
                );
            }
        }
        binding.pending = Some(AttachmentIntent {
            url: options.webhook_url.to_string(),
            fingerprint,
        });
        if let Some(id) = &id {
            binding.webhook_id = Some(id.clone());
        }
        selected.directory.save("dm-binding.json", &binding)?;
        let result = ipc(ipc_request(&selected.directory,&binding,&session,"webhook",json!({
            "url":options.webhook_url,"id":id,"secret":options.secret,"health_url":options.health_url,
            "takeover":options.takeover,"clear_secret":false,"clear_health_url":false,"headers":TestHeaders::default()
        }))).await;
        if let Err(error) = &result
            && let Some(recovered) = error
                .details
                .as_ref()
                .and_then(|v| v["webhook_id"].as_str())
        {
            ting_client::nonempty(recovered, "webhook ID")?;
            ensure!(
                binding
                    .webhook_id
                    .as_deref()
                    .is_none_or(|id| id == recovered),
                "Ting returned another hook ID"
            );
            binding.webhook_id = Some(recovered.to_owned());
            selected.directory.save("dm-binding.json", &binding)?;
        }
        let response = result?;
        let accepted_id = clean_string(&response, "id")?;
        ensure!(
            binding
                .webhook_id
                .as_deref()
                .is_none_or(|id| id == accepted_id),
            "Ting returned another hook ID"
        );
        ensure!(
            response["for"] == binding.actor.id && response["state"] == "connected",
            "Ting did not confirm this member's connected webhook"
        );
        binding.webhook_id = Some(accepted_id.to_owned());
        binding.pending = None;
        binding.detached = false;
        selected.directory.save("dm-binding.json", &binding)?;
        Ok(
            json!({"delivery_provider":"ting","hooked":true,"webhook_id":accepted_id,
            "webhook_url":options.webhook_url,"all_apps":true,"payload":"tings"}),
        )
    }

    /// Detach the saved stable hook through Ting. Its ID remains saved so explicit
    /// reattachment reuses the same destination and retained delivery history.
    pub async fn delivery_unhook(&self, name: &str, test: Option<Uuid>) -> Result<Value> {
        self.delivery_unhook_with(name, test, ting_client::ipc)
            .await
    }

    async fn delivery_unhook_with<F, Fut>(
        &self,
        name: &str,
        test: Option<Uuid>,
        ipc: F,
    ) -> Result<Value>
    where
        F: Fn(Value) -> Fut,
        Fut: Future<Output = ting_client::Result<Value>>,
    {
        let (selected, _lock, mut binding, session) = self.delivery_bound(name, test).await?;
        let id = binding.webhook_id.clone().context(
            "no saved Ting hook; recover any uncertain attachment by its exact ID first",
        )?;
        let response = ipc(ipc_request(
            &selected.directory,
            &binding,
            &session,
            "unhook",
            json!({"id":id,"headers":TestHeaders::default()}),
        ))
        .await?;
        ensure!(
            response["id"] == id && response["removed"] == true,
            "Ting did not confirm detaching the saved webhook"
        );
        binding.detached = true;
        binding.pending = None;
        selected.directory.save("dm-binding.json", &binding)?;
        Ok(json!({"delivery_provider":"ting","hooked":false,"webhook_id":id}))
    }

    /// Verify current DM/Ting authority and ask Ting for daemon status. This never
    /// creates or reconnects hooks and never treats a local file as delivery proof.
    pub async fn delivery_status(&self, name: &str, test: Option<Uuid>) -> Result<Value> {
        self.delivery_status_with(name, test, ting_client::ipc)
            .await
    }

    async fn delivery_status_with<F, Fut>(
        &self,
        name: &str,
        test: Option<Uuid>,
        ipc: F,
    ) -> Result<Value>
    where
        F: Fn(Value) -> Fut,
        Fut: Future<Output = ting_client::Result<Value>>,
    {
        let (selected, _lock, binding, session) = self.delivery_bound(name, test).await?;
        let status = ipc(ipc_request(
            &selected.directory,
            &binding,
            &session,
            "status",
            json!({}),
        ))
        .await?;
        Ok(
            json!({"authenticated":true,"delivery_provider":"ting","webhook_id":binding.webhook_id,
            "detached":binding.detached,"attachment_pending":binding.pending.is_some(),"daemon":status}),
        )
    }

    /// Explicitly reconnect the retained Ting destination. This asks Ting to
    /// rebind an attached destination and never creates a replacement webhook.
    /// After unhook or a new login, use `delivery_attach` with the retained ID.
    pub async fn delivery_reconnect(&self, name: &str, test: Option<Uuid>) -> Result<Value> {
        self.delivery_reconnect_with(name, test, ting_client::ipc)
            .await
    }

    async fn delivery_reconnect_with<F, Fut>(
        &self,
        name: &str,
        test: Option<Uuid>,
        ipc: F,
    ) -> Result<Value>
    where
        F: Fn(Value) -> Fut,
        Fut: Future<Output = ting_client::Result<Value>>,
    {
        let (selected, _lock, mut binding, session) = self.delivery_bound(name, test).await?;
        let id = binding
            .webhook_id
            .clone()
            .context("no retained Ting hook; attach or recover its exact ID first")?;
        ensure!(
            !binding.detached,
            "Ting hook is detached; use delivery_attach to explicitly reattach its saved ID"
        );
        ensure!(
            binding.pending.is_none(),
            "finish the pending Ting attachment using its exact hook ID before reconnecting"
        );
        let destinations = ipc(ipc_request(
            &selected.directory,
            &binding,
            &session,
            "destinations",
            json!({}),
        ))
        .await?;
        ensure!(
            destinations.get(&id).is_some_and(Value::is_string),
            "Ting daemon has no destination bound to this session; use delivery_attach with the retained webhook ID"
        );
        let response = ipc(ipc_request(
            &selected.directory,
            &binding,
            &session,
            "reconnect",
            json!({"headers":TestHeaders::default()}),
        ))
        .await?;
        ensure!(
            response["reconnected"] == true,
            "Ting did not confirm receiver reconnection"
        );
        binding.detached = false;
        selected.directory.save("dm-binding.json", &binding)?;
        Ok(json!({"delivery_provider":"ting","reconnected":true,"webhook_id":id}))
    }

    /// End only this explicitly bound Ting session. Stops local forwarding first,
    /// retains the stable hook ID, and reports any unconfirmed remote revocation.
    pub async fn delivery_logout(&self, name: &str, test: Option<Uuid>) -> Result<Value> {
        // Logout deliberately remains possible after the DM/IAM session expires.
        let config = self.store.load()?;
        let profile = config
            .profiles
            .get(&store::session_key(name, test))
            .context("no saved DM profile")?;
        let info = store::client(&config, profile)?.iam().await?;
        ensure!(
            info.testing_environment_id == test,
            "DM testing context changed"
        );
        let directory = self
            .store
            .ting_profile(&store::session_key(name, test), info.testing_generation)?;
        let _lock = directory.lock()?;
        let mut binding = directory
            .read::<Binding>("dm-binding.json")?
            .context("no verified Ting delivery login")?;
        ensure!(
            binding.actor == profile.tokens.actor
                && binding.organization_id == profile.tokens.organization_id
                && binding.environment_id == test
                && binding.generation == info.testing_generation
                && binding.dm_api_url == profile.base_url.trim_end_matches('/'),
            "Ting profile identity or context changed"
        );
        let session = directory.session(&binding.ting_api_url)?;
        ensure!(
            digest(&session.token) == binding.session_digest,
            "Ting session binding changed"
        );
        let local = ting_client::ipc(ipc_request(
            &directory,
            &binding,
            &session,
            "logout",
            json!({}),
        ))
        .await;
        let remote = Client::new(&binding.ting_api_url)?
            .json(
                "DELETE",
                "/v1/session",
                None,
                Some(&session.token),
                &TestHeaders::default(),
            )
            .await;
        directory.remove("session.json")?;
        directory.remove("dm-login-attempt.json")?;
        binding.session_digest.clear();
        binding.detached = true;
        directory.save("dm-binding.json", &binding)?;
        if let Err(error) = local
            && error.code != "daemon_unavailable"
        {
            return Err(error.into());
        }
        remote?;
        Ok(
            json!({"authenticated":false,"delivery_provider":"ting","webhook_id":binding.webhook_id}),
        )
    }
}

fn ipc_request(
    profile: &Profile,
    binding: &Binding,
    session: &Session,
    op: &str,
    mut extra: Value,
) -> Value {
    let object = extra.as_object_mut().expect("internal IPC object");
    object.insert("op".into(), json!(op));
    object.insert("api_url".into(), json!(binding.ting_api_url));
    object.insert("org_id".into(), json!(binding.organization_id));
    object.insert("profile".into(), json!(profile.dir));
    object.insert("session_token".into(), json!(session.token));
    extra
}

#[cfg(test)]
#[path = "ting_delivery_tests.rs"]
mod tests;
