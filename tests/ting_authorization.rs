//! Separate approval, encrypted durable tokens and safe refresh retry on both planes.
mod support;
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};
use silicon_dm::{
    AppError,
    application::auth::{AuthContext, PresentedCredential},
    config::{DatabaseSettings, IamSettings},
    domain::{ActorRef, ActorType},
    infrastructure::{
        iam::IamClient,
        postgres::{PostgresStore, TingDeliveryContext},
        ting_authorization::TingAuthorization,
        ting_credentials::TingCredentialCache,
    },
};
use std::{collections::BTreeSet, error::Error, time::Duration};
use time::OffsetDateTime;
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_partial_json, method, path},
};
type Result<T = ()> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

fn auth() -> Result<AuthContext> {
    Ok(AuthContext {
        actor: ActorRef {
            actor_type: ActorType::Carbon,
            id: "c:alice".parse()?,
        },
        organization_id: "tos".parse()?,
        session_id: None,
        org_role: None,
        tag_ids: None,
        represented_actor_ids: BTreeSet::new(),
        capabilities: BTreeSet::from(["self.identity.read".into()]),
        credential: PresentedCredential::Bearer(SecretString::from("oat_login_not_obo")),
        credential_expires_at: OffsetDateTime::now_utc() + time::Duration::hours(1),
    })
}
fn pair(endpoint: &str, grant: Uuid, seconds: i64, testing: Option<Uuid>) -> Result<Value> {
    Ok(
        json!({"actor":{"type":"carbon","public_id":"c:alice"},"grant_id":grant,"access_token":format!("oba_{endpoint}"),"refresh_token":format!("obr_{endpoint}"),"token_type":"Bearer","expires_in":seconds,"expires_at":(OffsetDateTime::now_utc()+time::Duration::seconds(seconds)).format(&time::format_description::well_known::Rfc3339)?,"audience":"ting","endpoint_id":endpoint,"org_id":"tos","scope":format!("obo:ting:{endpoint}"),"testing_context":testing.map(|_|json!({"app_id":"ting","app_secret":"ask_test_ting","iam_test_key":"k".repeat(32)}))}),
    )
}
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one isolated database follows the complete grant lifecycle in both planes"
)]
async fn separate_grants_are_durable_encrypted_scoped_and_refresh_retries_are_stable() -> Result {
    let db = support::TestDatabase::start().await?;
    let settings = DatabaseSettings {
        url: SecretString::from(db.url),
        max_connections: 8.try_into()?,
        min_connections: 1,
        acquire_timeout: Duration::from_secs(10),
        statement_timeout: Duration::from_secs(30),
    };
    let production = PostgresStore::connect(&settings).await?;
    production.migrate().await?;
    for testing in [None, Some(Uuid::new_v4())] {
        let server = MockServer::start().await;
        let iam_settings = IamSettings {
            base_url: server.uri().parse()?,
            app_id: "dm".into(),
            app_secret: SecretString::from("fixture-dm-secret"),
            webhook_secret: SecretString::from("fixture-webhook-secret-fixture-secret"),
            webhook_key_version: 1,
            request_timeout: Duration::from_secs(3),
        };
        let identity = if let Some(id) = testing {
            IamClient::for_environment(
                &iam_settings,
                "dm",
                SecretString::from("fixture-dm-test-secret"),
                SecretString::from("k".repeat(32)),
                id,
            )?
        } else {
            IamClient::new(&iam_settings)?
        };
        let store = if let Some(id) = testing {
            let schema = format!("dm_test_{}", id.simple());
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}; CREATE TYPE {schema}.actor_kind AS ENUM('carbon','silicon'); CREATE TABLE {schema}.ting_authorizations(LIKE dm.ting_authorizations INCLUDING ALL); CREATE TABLE {schema}.ting_obo_credentials(LIKE dm.ting_obo_credentials INCLUDING ALL); CREATE TABLE {schema}.ting_handoffs(LIKE dm.ting_handoffs INCLUDING ALL); ALTER TABLE {schema}.ting_authorizations ALTER COLUMN actor_kind TYPE {schema}.actor_kind USING actor_kind::text::{schema}.actor_kind; ALTER TABLE {schema}.ting_obo_credentials ALTER COLUMN actor_kind TYPE {schema}.actor_kind USING actor_kind::text::{schema}.actor_kind; ALTER TABLE {schema}.ting_handoffs ALTER COLUMN originator_kind TYPE {schema}.actor_kind USING originator_kind::text::{schema}.actor_kind;"))).execute(production.pool()).await?;
            PostgresStore::connect_schema(&settings, &schema).await?
        } else {
            production.clone()
        };
        let context = TingDeliveryContext {
            app_id: "dm".into(),
            testing_environment_id: testing,
            testing_generation: testing.map(|_| 1),
        };
        let cache = || {
            TingCredentialCache::new(
                store.clone(),
                &SecretString::from("local-encryption-fixture"),
                context.clone(),
            )
        };
        let broker = TingAuthorization::new(cache()?);
        let auth = auth()?;
        assert_eq!(
            broker.status(&auth).await?["status"],
            "authorization_required"
        );
        assert!(matches!(
            broker
                .authority(&identity, &auth.organization_id, &auth.actor, "tings.send")
                .await,
            Err(AppError::TingAuthorizationRequired)
        ));
        let request = Uuid::new_v4();
        let register = Uuid::new_v4();
        let send = Uuid::new_v4();
        let detail = json!({"id":request,"app_id":"dm","app_name":"DM","actor":{"type":"carbon","public_id":"c:alice"},"org_id":"tos","status":"approved","version":1,"expires_at":(OffsetDateTime::now_utc()+time::Duration::hours(1)).format(&time::format_description::well_known::Rfc3339)?,"endpoints":[],"providers":[{"app_id":"ting","app_name":"Ting","actor":{"type":"carbon","public_id":"c:alice"},"org_id":"tos"}],"authorization_url":format!("https://iam.example.test/obo/consent?request={request}")});
        let start_attempt = std::sync::atomic::AtomicUsize::new(0);
        let start_reply = detail.clone();
        Mock::given(method("POST")).and(path("/api/v1/obo-access/authorizations")).and(body_partial_json(json!({"subject_token":"oat_login_not_obo","org_id":"tos","endpoints":[{"audience":"ting","endpoint_id":"subscriptions.register"},{"audience":"ting","endpoint_id":"tings.send"}]}))).respond_with(move |_: &wiremock::Request| {
            if start_attempt.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                ResponseTemplate::new(503)
            } else { ResponseTemplate::new(200).set_body_json(&start_reply) }
        }).mount(&server).await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/obo-access/authorizations/{request}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(&detail))
            .mount(&server)
            .await;
        if let Some(id) = testing {
            Mock::given(method("GET")).and(path("/api/v1/application/testing-context")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"environment_id":id,"application":{"app_id":"ting","base_url":"https://ting.example.test","app_scope":{"iam":[],"external":[]},"webhook_scope":[],"testing_idle_days":30}}))).mount(&server).await;
        }
        let registration_pair = pair("subscriptions.register", register, 3, testing)?;
        let send_pair = pair("tings.send", send, 3, testing)?;
        Mock::given(method("POST"))
            .and(path("/api/v1/obo-access/tokens"))
            .and(body_partial_json(
                json!({"authorization_id":request,"authorization_code":"approved-code"}),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"items":[registration_pair,send_pair]})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let start_key = Uuid::new_v4().to_string();

        assert!(broker.begin(&identity, &auth, &start_key).await.is_err());
        let retained: Vec<u8> = sqlx::query_scalar(
            "SELECT start_ciphertext FROM ting_authorizations WHERE start_key=$1",
        )
        .bind(&start_key)
        .fetch_one(store.pool())
        .await?;
        assert!(!retained.windows(4).any(|bytes| bytes == b"oat_"));
        assert_eq!(
            broker.status(&auth).await?["status"],
            "authorization_required"
        );
        let mut rotated = auth.clone();
        rotated.credential = PresentedCredential::Bearer(SecretString::from("oat_rotated_login"));
        // A new process and a refreshed current login must replay the original
        // encrypted start body under the same IAM idempotency key.
        TingAuthorization::new(cache()?)
            .begin(&identity, &rotated, &start_key)
            .await?;
        let starts: Vec<_> = server
            .received_requests()
            .await
            .ok_or("missing starts")?
            .into_iter()
            .filter(|r| r.url.path() == "/api/v1/obo-access/authorizations")
            .collect();
        assert_eq!(starts.len(), 2);
        assert_eq!(starts[0].body, starts[1].body);
        assert_eq!(
            starts[0].headers.get("idempotency-key"),
            starts[1].headers.get("idempotency-key")
        );
        assert_eq!(
            starts[1].body_json::<Value>()?["subject_token"],
            "oat_login_not_obo"
        );
        let mut wrong = auth.clone();
        wrong.actor.id = "c:bob".parse()?;
        assert!(matches!(
            broker
                .complete(&identity, &wrong, request, "approved-code")
                .await,
            Err(AppError::NotFound)
        ));
        broker
            .complete(&identity, &auth, request, "approved-code")
            .await
            .map_err(|e| format!("complete {testing:?}: {e}"))?;
        broker
            .complete(&identity, &auth, request, "approved-code")
            .await
            .map_err(|e| format!("complete {testing:?}: {e}"))?;
        assert!(matches!(
            broker
                .complete(&identity, &auth, request, "different-code")
                .await,
            Err(AppError::Conflict(_))
        ));
        let ciphertext: Vec<u8> = sqlx::query_scalar(
            "SELECT ciphertext FROM ting_obo_credentials WHERE endpoint_id='tings.send'",
        )
        .fetch_one(store.pool())
        .await?;
        assert!(
            !ciphertext
                .windows(4)
                .any(|window| window == b"oba_" || window == b"obr_")
        );
        // An uncertain refresh must retry the exact old token and mutation identity.
        let failure = Mock::given(method("POST"))
            .and(path("/api/v1/obo-access/tokens"))
            .and(body_partial_json(json!({"refresh_token":"obr_tings.send"})))
            .respond_with(ResponseTemplate::new(503))
            .mount_as_scoped(&server)
            .await;
        assert!(
            broker
                .authority(&identity, &auth.organization_id, &auth.actor, "tings.send")
                .await
                .is_err()
        );
        drop(failure);
        let mut renewed = pair("tings.send", send, 300, testing)?;
        renewed["refresh_token"] = "obr_rotated".into();
        renewed["access_token"] = "oba_rotated".into();
        Mock::given(method("POST"))
            .and(path("/api/v1/obo-access/tokens"))
            .and(body_partial_json(json!({"refresh_token":"obr_tings.send"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items":[renewed]})))
            .expect(1)
            .mount(&server)
            .await;
        let restarted = TingAuthorization::new(cache()?);
        let (a, b) = tokio::join!(
            restarted.authority(&identity, &auth.organization_id, &auth.actor, "tings.send"),
            broker.authority(&identity, &auth.organization_id, &auth.actor, "tings.send")
        );
        assert_eq!(a?.proof_token.expose_secret(), "oba_rotated");
        assert_eq!(b?.proof_token.expose_secret(), "oba_rotated");
        let calls = server.received_requests().await.ok_or("requests missing")?;
        let keys: Vec<_> = calls
            .iter()
            .filter(|r| {
                r.body_json::<Value>()
                    .is_ok_and(|b| b["refresh_token"] == "obr_tings.send")
            })
            .map(|r| r.headers.get("idempotency-key").cloned())
            .collect();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0], keys[1]);
        assert!(matches!(
            broker
                .authority(&identity, &auth.organization_id, &wrong.actor, "tings.send")
                .await,
            Err(AppError::TingAuthorizationRequired)
        ));
        let mut next = context.clone();
        if testing.is_some() {
            next.testing_generation = Some(2);
            let next = TingAuthorization::new(TingCredentialCache::new(
                store.clone(),
                &SecretString::from("local-encryption-fixture"),
                next,
            )?);
            assert_eq!(
                next.status(&auth).await?["status"],
                "authorization_required"
            );
        }
        Mock::given(method("POST"))
            .and(path("/api/v1/obo-access/tokens"))
            .and(body_partial_json(
                json!({"refresh_token":"obr_subscriptions.register"}),
            ))
            .respond_with(
                ResponseTemplate::new(403).set_body_json(
                    json!({"error":{"code":"obo_grant_revoked","message":"Revoked"}}),
                ),
            )
            .expect(1)
            .mount(&server)
            .await;
        assert!(matches!(
            broker
                .authority(
                    &identity,
                    &auth.organization_id,
                    &auth.actor,
                    "subscriptions.register"
                )
                .await,
            Err(AppError::TingAuthorizationRequired)
        ));
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM ting_obo_credentials WHERE endpoint_id='subscriptions.register'").fetch_one(store.pool()).await?,0);
        assert_eq!(
            broker
                .authority(&identity, &auth.organization_id, &auth.actor, "tings.send")
                .await?
                .proof_token
                .expose_secret(),
            "oba_rotated"
        );
        broker.disconnect(&auth).await?;
        assert_eq!(
            broker.status(&auth).await?["status"],
            "authorization_required"
        );
    }
    Ok(())
}
