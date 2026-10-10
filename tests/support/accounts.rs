//! A fake Silicon Accounts for tests: a JWKS with a test Ed25519 key, signed access tokens,
//! account lookups, introspection, proofs and signed webhook deliveries.
#![allow(
    dead_code,
    reason = "each integration test binary uses a different subset"
)]

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{SigningKey, pkcs8::EncodePrivateKey as _};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde_json::{Value, json};
use silicon_dm::domain::{ActorId, ActorRef, ActorType};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_partial_json, method, path},
};

pub const APP_ID: &str = "dm";
pub const WEBHOOK_SECRET: &str = "whsec_test-webhook-secret";
pub const APP_SECRET: &str = "sa_app_test-secret";

/// An account the fake knows.
#[derive(Clone, Debug)]
pub struct TestAccount {
    pub uuid: String,
    pub kind: &'static str,
    pub id: String,
    pub custodian: Option<(String, String)>,
    pub display_name: String,
}

impl TestAccount {
    pub fn carbon(uuid: &str, id: &str) -> Self {
        Self {
            uuid: uuid.to_owned(),
            kind: "carbon",
            id: id.to_owned(),
            custodian: None,
            display_name: id.trim_start_matches("c:").to_owned(),
        }
    }

    pub fn silicon(uuid: &str, id: &str, custodian: &TestAccount) -> Self {
        Self {
            uuid: uuid.to_owned(),
            kind: "silicon",
            id: id.to_owned(),
            custodian: Some((custodian.uuid.clone(), custodian.id.clone())),
            display_name: id.trim_start_matches("si:").to_owned(),
        }
    }

    pub fn actor(&self) -> ActorRef {
        ActorRef {
            actor_type: if self.kind == "silicon" {
                ActorType::Silicon
            } else {
                ActorType::Carbon
            },
            id: self
                .uuid
                .parse::<ActorId>()
                .unwrap_or_else(|error| panic!("test uuid: {error}")),
        }
    }

    /// What a lookup answers an app: the public identity only, never a name or photo.
    pub fn summary(&self) -> Value {
        json!({
            "uuid": self.uuid, "kind": self.kind, "id": self.id, "status": "active",
            "custodian": self.custodian.as_ref().map(|(uuid, id)| json!({"uuid": uuid, "id": id})),
        })
    }

    /// DM's user base entry: what the account shared with DM by signing in.
    pub fn member(&self) -> Value {
        json!({"user": {
            "membership_id": format!("{APP_ID}:{}", self.uuid), "uuid": self.uuid,
            "kind": self.kind, "id": self.id, "display_name": self.display_name,
            "status": "active", "source": "signin", "granted_scopes": ["profile"],
        }})
    }
}

/// An Ed25519 key with its JWK.
pub struct TestKey {
    pub kid: String,
    pub encoding: EncodingKey,
    pub jwk: Value,
}

impl TestKey {
    pub fn new(seed: u8, kid: &str) -> Self {
        let signing = SigningKey::from_bytes(&[seed; 32]);
        let der = signing
            .to_pkcs8_der()
            .unwrap_or_else(|error| panic!("test key: {error}"));
        Self {
            kid: kid.to_owned(),
            encoding: EncodingKey::from_ed_der(der.as_bytes()),
            jwk: json!({
                "kty": "OKP", "crv": "Ed25519", "alg": "EdDSA", "use": "sig", "kid": kid,
                "x": URL_SAFE_NO_PAD.encode(signing.verifying_key().as_bytes()),
            }),
        }
    }

    pub fn sign(&self, claims: &Value) -> String {
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = Some(self.kid.clone());
        encode(&header, claims, &self.encoding).unwrap_or_else(|error| panic!("signing: {error}"))
    }
}

pub fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// The fake.
pub struct FakeAccounts {
    pub server: MockServer,
    pub key: TestKey,
}

impl FakeAccounts {
    pub async fn start() -> Self {
        let server = MockServer::start().await;
        let key = TestKey::new(7, "test-key-1");
        Mock::given(method("GET"))
            .and(path("/.well-known/jwks.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "keys": [key.jwk] })))
            .mount(&server)
            .await;
        Self { server, key }
    }

    pub fn issuer(&self) -> String {
        self.server.uri()
    }

    /// Claims of a valid DM access token for the account.
    pub fn claims(&self, account: &TestAccount) -> Value {
        let now = now();
        json!({
            "iss": self.issuer(), "sub": account.uuid, "aud": APP_ID,
            "exp": now + 900, "iat": now - 1, "nbf": now - 1, "jti": uuid::Uuid::new_v4().to_string(),
            "kind": account.kind, "id": account.id, "mid": format!("dm:{}", account.uuid),
            "fid": "family-1", "scope": "profile",
        })
    }

    /// A valid DM access token for the account.
    pub fn token(&self, account: &TestAccount) -> String {
        self.key.sign(&self.claims(account))
    }

    /// A token with some claims changed.
    pub fn token_with(&self, account: &TestAccount, changes: &Value) -> String {
        let mut claims = self.claims(account);
        if let (Some(claims), Some(changes)) = (claims.as_object_mut(), changes.as_object()) {
            for (key, value) in changes {
                claims.insert(key.clone(), value.clone());
            }
        }
        self.key.sign(&claims)
    }

    /// Makes the account resolvable by uuid and by id.
    pub async fn add_account(&self, account: &TestAccount) {
        Mock::given(method("GET"))
            .and(path(format!("/v1/accounts/{}", account.uuid)))
            .respond_with(ResponseTemplate::new(200).set_body_json(account.summary()))
            .mount(&self.server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/v1/accounts/by-id/{}", account.id)))
            .respond_with(ResponseTemplate::new(200).set_body_json(account.summary()))
            .mount(&self.server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/v1/apps/{APP_ID}/users/{}", account.uuid)))
            .respond_with(ResponseTemplate::new(200).set_body_json(account.member()))
            .mount(&self.server)
            .await;
    }

    pub async fn add_accounts(&self, accounts: &[&TestAccount]) {
        for account in accounts {
            self.add_account(account).await;
        }
    }

    /// Forgets every answer and serves only these accounts (plus the JWKS, live
    /// introspection and 404 for any other id): what Accounts says after a change.
    pub async fn serve_only(&self, accounts: &[&TestAccount]) {
        self.server.reset().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/jwks.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "keys": [self.key.jwk] })),
            )
            .mount(&self.server)
            .await;
        self.add_accounts(accounts).await;
        self.unknown_ids().await;
        self.live_introspection().await;
    }

    /// Ids nobody has.
    pub async fn unknown_ids(&self) {
        Mock::given(method("GET"))
            .and(wiremock::matchers::path_regex(r"^/v1/accounts/.*$"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error": {
                "code": "account_not_found", "message": "No account has this id.", "hint": "Ids can change."}})))
            .with_priority(10)
            .mount(&self.server)
            .await;
    }

    /// Introspection answers `active` for every token.
    pub async fn introspection(&self, active: bool, sub: &str) {
        Mock::given(method("POST"))
            .and(path("/v1/oauth/introspect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "active": active, "sub": sub, "token_type": "Bearer"})))
            .mount(&self.server)
            .await;
    }

    /// Introspection answers active for every token, with the token's own subject (the
    /// fake reads the unverified payload; verification is DM's job and already happened).
    pub async fn live_introspection(&self) {
        Mock::given(method("POST"))
            .and(path("/v1/oauth/introspect"))
            .respond_with(LiveIntrospection)
            .mount(&self.server)
            .await;
    }

    /// `proof_token` verifies as a User verification proof for `user` from `issuing_app`.
    pub async fn proof(
        &self,
        proof_token: &str,
        user: &TestAccount,
        issuing_app: &str,
        scopes: &[&str],
    ) {
        let expires = time::OffsetDateTime::now_utc() + time::Duration::minutes(10);
        Mock::given(method("POST"))
            .and(path("/v1/proofs/verify"))
            .and(body_partial_json(json!({ "proof_token": proof_token })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "valid": true, "proof_id": "prf_1", "kind": "user_verification",
                "expires_at": expires.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
                "issuing_app": {"app_id": issuing_app, "name": issuing_app},
                "receiving_app": {"app_id": APP_ID, "name": "DM"},
                "user": {"uuid": user.uuid, "id": user.id, "kind": user.kind, "membership_id": format!("{issuing_app}:{}", user.uuid)},
                "scopes": scopes,
            })))
            .mount(&self.server)
            .await;
    }

    /// Any other proof is invalid.
    pub async fn invalid_proofs(&self) {
        Mock::given(method("POST"))
            .and(path("/v1/proofs/verify"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"valid": false, "expires_at": null})),
            )
            .with_priority(10)
            .mount(&self.server)
            .await;
    }

    /// Issues App and User verification proofs for Ting.
    pub async fn issue_proofs(&self) {
        let expires = time::OffsetDateTime::now_utc() + time::Duration::minutes(10);
        let at = expires
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        for (route, token) in [
            ("/v1/proofs/app-verification", "sap_app"),
            ("/v1/proofs/user-verification", "sap_user"),
        ] {
            Mock::given(method("POST"))
                .and(path(route))
                .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                    "proof_id": format!("prf_{token}"), "kind": if token == "sap_app" { "app_verification" } else { "user_verification" },
                    "proof_token": token, "expires_at": at, "proof_refresh_token": "sapr_refresh",
                    "refresh_expires_at": at, "issuing_app": "dm", "receiving_app": "ting", "scopes": ["tings.send"],
                })))
                .mount(&self.server)
                .await;
        }
    }
}

/// A signed webhook delivery: (headers, raw body).
pub fn webhook(
    event_type: &str,
    data: &Value,
    occurred_at: &str,
) -> (Vec<(&'static str, String)>, String) {
    let event_id = uuid::Uuid::new_v4().to_string();
    let body = json!({
        "app_id": APP_ID, "data": data, "event_id": event_id, "occurred_at": occurred_at,
        "silicon": null, "type": event_type,
    })
    .to_string();
    signed(&event_id, event_type, body)
}

/// Signs an exact body (for replays and tampering tests).
pub fn signed(
    event_id: &str,
    event_type: &str,
    body: String,
) -> (Vec<(&'static str, String)>, String) {
    let timestamp = now();
    let signature =
        silicon_accounts_client::sign_webhook(WEBHOOK_SECRET, timestamp, body.as_bytes());
    (
        vec![
            ("x-accounts-event-id", event_id.to_owned()),
            ("x-accounts-event-type", event_type.to_owned()),
            ("x-accounts-timestamp", timestamp.to_string()),
            ("x-accounts-signature", signature),
            ("content-type", "application/json".to_owned()),
        ],
        body,
    )
}

/// Introspection that echoes the presented token's subject as active.
struct LiveIntrospection;

impl wiremock::Respond for LiveIntrospection {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let form = String::from_utf8_lossy(&request.body).to_string();
        let token = form
            .split('&')
            .find_map(|pair| pair.strip_prefix("token="))
            .unwrap_or_default();
        let sub = token
            .split('.')
            .nth(1)
            .and_then(|payload| URL_SAFE_NO_PAD.decode(payload).ok())
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|claims| claims["sub"].as_str().map(str::to_owned))
            .unwrap_or_default();
        ResponseTemplate::new(200).set_body_json(json!({"active": !sub.is_empty(), "sub": sub}))
    }
}
