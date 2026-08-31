//! Silicon IAM HTTP adapter.

use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr as _,
    time::Duration,
};

use async_trait::async_trait;
use futures::StreamExt as _;
use reqwest::{Client, RequestBuilder, Response, StatusCode, redirect::Policy};
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    application::{
        auth::{AuthContext, DelegatedCredential, PresentedCredential, ServiceContext},
        ports::{AuthenticationRequest, DelegationRequest, IdentityProvider},
    },
    config::IamSettings,
    domain::{ActorId, ActorRef, ActorType, OrganizationId},
};

const DEPENDENCY: &str = "iam";
const MAX_IAM_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_OBO_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_OBO_LIFETIME_SECONDS: u64 = 60;
const DIRECTORY_PAGE_SIZE: &str = "100";
const MAX_DIRECTORY_PAGES: usize = 100;
const HOOK_DELIVERY_CAPABILITY: &str = "dm.hook_events.deliver";

/// Reqwest-backed Silicon IAM client.
#[derive(Clone)]
pub struct IamClient {
    client: Client,
    introspection_url: Url,
    userinfo_url: Url,
    obo_exchange_url: Url,
    obo_verify_url: Url,
    api_base_url: Url,
    app_id: String,
    app_secret: SecretString,
    hook_service_id: String,
}

impl IamClient {
    /// Builds an IAM adapter from validated settings.
    ///
    /// # Errors
    ///
    /// Returns an internal configuration error when the HTTP client or an IAM
    /// endpoint URL cannot be built safely.
    pub fn new(settings: &IamSettings) -> AppResult<Self> {
        let client = build_client(settings.request_timeout)?;
        let api_base_url = iam_api_base(&settings.base_url)?;
        let introspection_url = append_segments(&api_base_url, &["auth", "tokens", "introspect"])?;
        let userinfo_url = append_segments(&api_base_url, &["oauth", "userinfo"])?;
        let obo_exchange_url = append_segments(&api_base_url, &["obo-access", "exchanges"])?;
        let obo_verify_url = append_segments(&api_base_url, &["obo-access", "verify"])?;

        Ok(Self {
            client,
            introspection_url,
            userinfo_url,
            obo_exchange_url,
            obo_verify_url,
            api_base_url,
            app_id: settings.app_id.clone(),
            app_secret: settings.app_secret.clone(),
            hook_service_id: settings.hook_service_id.clone(),
        })
    }

    async fn authenticate_bearer(
        &self,
        token: &SecretString,
        organization_id: &OrganizationId,
    ) -> AppResult<AuthContext> {
        let introspection = self.introspect(token, Some(organization_id)).await?;
        validate_active_introspection(&introspection, Some(organization_id), Some(&self.app_id))?;

        let actor = self
            .bearer_actor(token, &introspection, organization_id)
            .await?;
        Ok(AuthContext {
            represented_actor_ids: BTreeSet::new(),
            capabilities: introspection.capabilities(),
            actor,
            organization_id: organization_id.clone(),
            credential: PresentedCredential::Bearer(token.clone()),
        })
    }

    async fn authenticate_obo(
        &self,
        proof: &SecretString,
        originating_app_id: &str,
        organization_id: &OrganizationId,
        action: &str,
        resource: Option<&str>,
    ) -> AppResult<AuthContext> {
        if action.trim().is_empty() || originating_app_id.trim().is_empty() {
            return Err(AppError::Unauthorized);
        }

        let request = OboVerificationRequest {
            access_proof: proof.expose_secret(),
            audience: &self.app_id,
            action,
            resource,
        };
        let response = send_redacted(
            self.application_request(self.client.post(self.obo_verify_url.clone()))
                .header("X-Org-ID", organization_id.as_str())
                .header("Idempotency-Key", obo_verification_key())
                .json(&request),
            DEPENDENCY,
        )
        .await?;
        let status = response.status();
        if status != StatusCode::OK {
            return Err(map_obo_status(status));
        }

        let verification: OboVerification = read_json(response, MAX_IAM_RESPONSE_BYTES).await?;
        if !verification.valid
            || verification.audience != self.app_id
            || verification.org_id != organization_id.as_str()
            || verification.expires_at <= OffsetDateTime::now_utc()
            || verification.action != action
            || verification.resource.as_deref() != resource
            || verification.issuer_app_id != originating_app_id
        {
            return Err(AppError::Unauthorized);
        }

        let actor = verification.actor.to_domain()?;
        Ok(AuthContext {
            represented_actor_ids: BTreeSet::new(),
            capabilities: BTreeSet::from([verification.action]),
            actor,
            organization_id: organization_id.clone(),
            credential: PresentedCredential::Obo {
                proof: proof.clone(),
                app_id: originating_app_id.to_owned(),
            },
        })
    }

    async fn exchange_bearer_credential(
        &self,
        subject_token: &SecretString,
        organization_id: &OrganizationId,
        request: &DelegationRequest,
    ) -> AppResult<DelegatedCredential> {
        let body = OboExchangeRequest {
            subject_token: subject_token.expose_secret(),
            audience: request.audience(),
            action: request.action(),
            resource: request.resource(),
            org_id: organization_id.as_str(),
        };
        let response = send_redacted(
            self.application_request(self.client.post(self.obo_exchange_url.clone()))
                .header("X-Org-ID", organization_id.as_str())
                .header(
                    "Idempotency-Key",
                    format!("dm-obo-exchange-{}", Uuid::now_v7()),
                )
                .json(&body),
            DEPENDENCY,
        )
        .await?;
        let status = response.status();
        if status != StatusCode::CREATED {
            return Err(map_obo_exchange_status(status));
        }

        let proof: OboProofResponse = read_json(response, MAX_OBO_RESPONSE_BYTES).await?;
        let now = OffsetDateTime::now_utc();
        if !(1..=MAX_OBO_LIFETIME_SECONDS).contains(&proof.expires_in)
            || proof.expires_at <= now
            || proof.expires_at > now + time::Duration::seconds(60)
            || proof.proof_id.is_nil()
            || !valid_obo_access_proof(proof.access_proof.expose_secret())
        {
            tracing::warn!(
                dependency = DEPENDENCY,
                failure = "invalid_obo_exchange_response",
                "dependency response rejected"
            );
            return Err(dependency_unavailable());
        }
        Ok(DelegatedCredential::new(
            proof.access_proof,
            self.app_id.clone(),
        ))
    }

    async fn introspect(
        &self,
        token: &SecretString,
        organization_id: Option<&OrganizationId>,
    ) -> AppResult<TokenIntrospection> {
        let mut request = self
            .application_request(self.client.post(self.introspection_url.clone()))
            .form(&TokenIntrospectionRequest {
                token: token.expose_secret(),
            });
        if let Some(organization_id) = organization_id {
            request = request.header("X-Org-ID", organization_id.as_str());
        }
        let response = send_redacted(request, DEPENDENCY).await?;
        let status = response.status();
        if status != StatusCode::OK {
            return Err(map_introspection_status(status));
        }
        read_json(response, MAX_IAM_RESPONSE_BYTES).await
    }

    async fn bearer_actor(
        &self,
        token: &SecretString,
        introspection: &TokenIntrospection,
        organization_id: &OrganizationId,
    ) -> AppResult<ActorRef> {
        let response = send_redacted(
            self.client
                .get(self.userinfo_url.clone())
                .bearer_auth(token.expose_secret())
                .header("X-Org-ID", organization_id.as_str()),
            DEPENDENCY,
        )
        .await?;
        match response.status() {
            StatusCode::OK => {}
            StatusCode::UNAUTHORIZED => return Err(AppError::Unauthorized),
            StatusCode::FORBIDDEN => return Err(AppError::Forbidden),
            status => return Err(status_dependency_error(DEPENDENCY, status)),
        }

        let userinfo: UserInfo = read_json(response, MAX_IAM_RESPONSE_BYTES).await?;
        if userinfo.org_id.as_deref() != Some(organization_id.as_str())
            || introspection.principal_id.as_deref() != Some(userinfo.sub.as_str())
            || introspection.actor_type.as_deref() != Some(userinfo.actor_type.as_str())
        {
            return Err(AppError::Unauthorized);
        }
        IamActor {
            actor_type: userinfo.actor_type,
            public_id: userinfo.public_id,
        }
        .to_domain()
    }

    fn application_request(&self, request: RequestBuilder) -> RequestBuilder {
        request.basic_auth(&self.app_id, Some(self.app_secret.expose_secret()))
    }

    async fn visible_participants(
        &self,
        context: &AuthContext,
        requested: &[ActorId],
    ) -> AppResult<Vec<ActorRef>> {
        let mut unresolved: BTreeSet<ActorId> = requested.iter().cloned().collect();
        let mut resolved = Vec::with_capacity(unresolved.len());
        if unresolved.remove(&context.actor.id) {
            resolved.push(context.actor.clone());
        }
        if unresolved.is_empty() {
            return Ok(resolved);
        }

        let PresentedCredential::Bearer(token) = &context.credential else {
            // IAM has no application-authenticated contactability endpoint and
            // the DM-audience OBO proof has already been consumed.
            return Err(AppError::Forbidden);
        };

        resolved.extend(
            self.lookup_directory_actors(token, &context.organization_id, &unresolved)
                .await?,
        );
        Ok(resolved)
    }

    async fn lookup_directory_actors(
        &self,
        token: &SecretString,
        organization_id: &OrganizationId,
        requested: &BTreeSet<ActorId>,
    ) -> AppResult<Vec<ActorRef>> {
        let mut resolved = BTreeMap::new();
        let mut next_cursor = None;
        let mut seen_cursors = BTreeSet::new();
        for _ in 0..MAX_DIRECTORY_PAGES {
            let mut url = append_segments(
                &self.api_base_url,
                &["organizations", organization_id.as_str(), "members"],
            )?;
            {
                let mut query = url.query_pairs_mut();
                query.append_pair("limit", DIRECTORY_PAGE_SIZE);
                query.append_pair("status", "active");
                if let Some(cursor) = next_cursor.as_deref() {
                    query.append_pair("cursor", cursor);
                }
            }

            let response = send_redacted(
                self.client.get(url).bearer_auth(token.expose_secret()),
                DEPENDENCY,
            )
            .await?;
            match response.status() {
                StatusCode::OK => {}
                StatusCode::UNAUTHORIZED => return Err(AppError::Unauthorized),
                StatusCode::FORBIDDEN | StatusCode::NOT_FOUND => {
                    return Err(AppError::Forbidden);
                }
                status => return Err(status_dependency_error(DEPENDENCY, status)),
            }

            let page: MembershipPage = read_json(response, MAX_IAM_RESPONSE_BYTES).await?;
            next_cursor = page.next_cursor()?;
            for membership in page.items {
                if membership.org_id != organization_id.as_str() || membership.status != "active" {
                    return Err(dependency_unavailable());
                }
                let actor = membership.principal.to_domain()?;
                if requested.contains(&actor.id)
                    && resolved.insert(actor.id.clone(), actor).is_some()
                {
                    // DM's current wire contract names actors by public ID
                    // alone. IAM explicitly permits labels that collide across
                    // actor types, so an ambiguous result must never be chosen
                    // according to directory page order.
                    return Err(dependency_unavailable());
                }
            }

            let Some(cursor) = next_cursor.as_ref() else {
                return if resolved.len() == requested.len() {
                    Ok(resolved.into_values().collect())
                } else {
                    Err(AppError::Forbidden)
                };
            };
            if !seen_cursors.insert(cursor.clone()) {
                return Err(dependency_unavailable());
            }
        }

        tracing::warn!(
            dependency = DEPENDENCY,
            failure = "directory_page_limit",
            "dependency authorization failed closed"
        );
        Err(AppError::Forbidden)
    }
}

#[async_trait]
impl IdentityProvider for IamClient {
    async fn authenticate(&self, request: AuthenticationRequest<'_>) -> AppResult<AuthContext> {
        match request {
            AuthenticationRequest::Bearer {
                token,
                organization_id,
            } => self.authenticate_bearer(token, organization_id).await,
            AuthenticationRequest::Obo {
                proof,
                app_id,
                organization_id,
                action,
                resource,
            } => {
                self.authenticate_obo(proof, app_id, organization_id, action, resource)
                    .await
            }
        }
    }

    async fn authenticate_service(&self, token: &SecretString) -> AppResult<ServiceContext> {
        let introspection = self.introspect(token, None).await?;
        validate_active_introspection(&introspection, None, Some(&self.app_id))?;
        if introspection.actor_type.as_deref() != Some("service")
            || introspection.principal_id.is_none()
        {
            return Err(AppError::Unauthorized);
        }
        let service_id = introspection
            .client_id
            .as_deref()
            .ok_or(AppError::Unauthorized)?;
        if service_id != self.hook_service_id {
            return Err(AppError::Forbidden);
        }
        Ok(ServiceContext {
            service_id: service_id.to_owned(),
            capabilities: introspection.capabilities(),
            credential: token.clone(),
        })
    }

    async fn exchange_actor_credential(
        &self,
        context: &AuthContext,
        request: &DelegationRequest,
    ) -> AppResult<DelegatedCredential> {
        let PresentedCredential::Bearer(subject_token) = &context.credential else {
            // IAM exchanges only actor-bound access tokens issued to this
            // application. A proof already consumed by DM cannot be chained.
            return Err(AppError::Forbidden);
        };
        self.exchange_bearer_credential(subject_token, &context.organization_id, request)
            .await
    }

    async fn authorize_participants(
        &self,
        context: &AuthContext,
        actor_ids: &[ActorId],
    ) -> AppResult<Vec<ActorRef>> {
        self.visible_participants(context, actor_ids).await
    }

    async fn authorize_presence(
        &self,
        context: &AuthContext,
        actor_id: &ActorId,
    ) -> AppResult<ActorRef> {
        let mut actors = self
            .visible_participants(context, std::slice::from_ref(actor_id))
            .await?;
        let actor = actors.pop().ok_or(AppError::Forbidden)?;
        if actor.id != *actor_id || !actors.is_empty() {
            return Err(AppError::Forbidden);
        }
        Ok(actor)
    }

    async fn authorize_hook_target(
        &self,
        service: &ServiceContext,
        organization_id: &OrganizationId,
        silicon_id: &ActorId,
    ) -> AppResult<ActorRef> {
        if service.service_id != self.hook_service_id
            || !service.capabilities.contains(HOOK_DELIVERY_CAPABILITY)
        {
            return Err(AppError::Forbidden);
        }

        let requested = BTreeSet::from([silicon_id.clone()]);
        let mut actors = self
            .lookup_directory_actors(&service.credential, organization_id, &requested)
            .await?;
        let actor = actors.pop().ok_or(AppError::Forbidden)?;
        if actor.actor_type != ActorType::Silicon || actor.id != *silicon_id || !actors.is_empty() {
            return Err(AppError::Forbidden);
        }
        Ok(actor)
    }
}

#[derive(Serialize)]
struct TokenIntrospectionRequest<'a> {
    token: &'a str,
}

#[derive(Debug, Deserialize)]
struct TokenIntrospection {
    active: bool,
    #[serde(default)]
    principal_id: Option<String>,
    #[serde(default)]
    actor_type: Option<String>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    org_id: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    audience: Option<Audience>,
    #[serde(default)]
    expires_at: Option<i64>,
}

impl TokenIntrospection {
    fn capabilities(&self) -> BTreeSet<String> {
        self.scope
            .iter()
            .flat_map(|scope| scope.split_ascii_whitespace())
            .filter(|capability| !capability.is_empty())
            .map(str::to_owned)
            .collect()
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

impl Audience {
    fn contains(&self, expected: &str) -> bool {
        match self {
            Self::One(audience) => audience == expected,
            Self::Many(audiences) => audiences.iter().any(|audience| audience == expected),
        }
    }
}

#[derive(Debug, Deserialize)]
struct UserInfo {
    sub: String,
    actor_type: String,
    public_id: String,
    #[serde(default)]
    org_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IamActor {
    #[serde(rename = "type", alias = "actor_type")]
    actor_type: String,
    #[serde(alias = "id")]
    public_id: String,
}

impl IamActor {
    fn to_domain(&self) -> AppResult<ActorRef> {
        let actor_type =
            ActorType::from_str(&self.actor_type).map_err(|_| AppError::Unauthorized)?;
        let id = ActorId::from_str(&self.public_id).map_err(|_| AppError::Unauthorized)?;
        Ok(ActorRef { actor_type, id })
    }
}

#[derive(Serialize)]
struct OboVerificationRequest<'a> {
    access_proof: &'a str,
    audience: &'a str,
    action: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    resource: Option<&'a str>,
}

#[derive(Serialize)]
struct OboExchangeRequest<'a> {
    subject_token: &'a str,
    audience: &'a str,
    action: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    resource: Option<&'a str>,
    org_id: &'a str,
}

#[derive(Debug, Deserialize)]
struct OboProofResponse {
    access_proof: SecretString,
    proof_id: Uuid,
    expires_in: u64,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
}

#[derive(Debug, Deserialize)]
struct OboVerification {
    valid: bool,
    #[serde(rename = "proof_id")]
    _proof_id: Uuid,
    issuer_app_id: String,
    actor: IamActor,
    org_id: String,
    audience: String,
    action: String,
    #[serde(default)]
    resource: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
    #[serde(rename = "consumed_at", with = "time::serde::rfc3339")]
    _consumed_at: OffsetDateTime,
}

#[derive(Debug, Deserialize)]
struct MembershipPage {
    items: Vec<Membership>,
    page: PageInfo,
}

impl MembershipPage {
    fn next_cursor(&self) -> AppResult<Option<String>> {
        let cursor = self
            .page
            .next_cursor
            .as_ref()
            .filter(|cursor| !cursor.is_empty());
        match (self.page.has_more, cursor) {
            (true, Some(cursor)) => Ok(Some(cursor.clone())),
            (false, None) => Ok(None),
            _ => Err(dependency_unavailable()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct PageInfo {
    #[serde(default)]
    next_cursor: Option<String>,
    has_more: bool,
}

#[derive(Debug, Deserialize)]
struct Membership {
    org_id: String,
    status: String,
    principal: IamActor,
}

fn validate_active_introspection(
    introspection: &TokenIntrospection,
    organization_id: Option<&OrganizationId>,
    expected_audience: Option<&str>,
) -> AppResult<()> {
    if !introspection.active
        || introspection
            .expires_at
            .is_some_and(|expires_at| expires_at <= OffsetDateTime::now_utc().unix_timestamp())
        || organization_id.is_some_and(|organization_id| {
            introspection.org_id.as_deref() != Some(organization_id.as_str())
        })
        || expected_audience.is_some_and(|expected| {
            !introspection
                .audience
                .as_ref()
                .is_some_and(|audience| audience.contains(expected))
        })
    {
        return Err(AppError::Unauthorized);
    }
    Ok(())
}

fn obo_verification_key() -> String {
    format!("dm-obo-verify-{}", Uuid::now_v7())
}

fn build_client(timeout: Duration) -> AppResult<Client> {
    Client::builder()
        .connect_timeout(timeout.min(Duration::from_secs(5)))
        .timeout(timeout)
        .redirect(Policy::none())
        .referer(false)
        .user_agent(concat!("silicon-dm/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| AppError::internal(anyhow::anyhow!("failed to build IAM HTTP client")))
}

fn iam_api_base(base_url: &Url) -> AppResult<Url> {
    validate_base_url(base_url, DEPENDENCY)?;
    let segments: Vec<_> = base_url
        .path_segments()
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("invalid IAM base URL")))?
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.ends_with(&["api", "v1"]) {
        Ok(base_url.clone())
    } else {
        append_segments(base_url, &["api", "v1"])
    }
}

fn append_segments(base_url: &Url, segments: &[&str]) -> AppResult<Url> {
    validate_base_url(base_url, DEPENDENCY)?;
    let mut endpoint = base_url.clone();
    endpoint.set_query(None);
    {
        let mut path = endpoint
            .path_segments_mut()
            .map_err(|()| AppError::internal(anyhow::anyhow!("invalid IAM endpoint base URL")))?;
        path.pop_if_empty();
        for segment in segments {
            path.push(segment);
        }
    }
    Ok(endpoint)
}

fn validate_base_url(url: &Url, dependency: &'static str) -> AppResult<()> {
    if !matches!(url.scheme(), "http" | "https")
        || url.cannot_be_a_base()
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        tracing::error!(dependency, "invalid dependency base URL");
        return Err(AppError::internal(anyhow::anyhow!(
            "invalid dependency base URL"
        )));
    }
    Ok(())
}

async fn send_redacted(request: RequestBuilder, dependency: &'static str) -> AppResult<Response> {
    request.send().await.map_err(|error| {
        let failure = if error.is_timeout() {
            "timeout"
        } else if error.is_connect() {
            "connect"
        } else if error.is_redirect() {
            "redirect"
        } else {
            "transport"
        };
        tracing::warn!(dependency, failure, "dependency request failed");
        AppError::DependencyUnavailable { dependency }
    })
}

async fn read_json<T>(response: Response, maximum_bytes: usize) -> AppResult<T>
where
    T: DeserializeOwned,
{
    if !has_json_content_type(&response) {
        tracing::warn!(
            dependency = DEPENDENCY,
            failure = "invalid_content_type",
            "dependency response rejected"
        );
        return Err(dependency_unavailable());
    }
    if response
        .content_length()
        .is_some_and(|length| length > maximum_bytes as u64)
    {
        tracing::warn!(
            dependency = DEPENDENCY,
            failure = "response_too_large",
            "dependency response rejected"
        );
        return Err(dependency_unavailable());
    }

    let mut body = Vec::with_capacity(
        response
            .content_length()
            .and_then(|length| usize::try_from(length).ok())
            .unwrap_or_default()
            .min(maximum_bytes),
    );
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| dependency_unavailable())?;
        if body.len().saturating_add(chunk.len()) > maximum_bytes {
            tracing::warn!(
                dependency = DEPENDENCY,
                failure = "response_too_large",
                "dependency response rejected"
            );
            return Err(dependency_unavailable());
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| {
        tracing::warn!(
            dependency = DEPENDENCY,
            failure = "invalid_json",
            "dependency response rejected"
        );
        dependency_unavailable()
    })
}

fn has_json_content_type(response: &Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
}

fn map_introspection_status(status: StatusCode) -> AppError {
    match status {
        StatusCode::BAD_REQUEST
        | StatusCode::UNAUTHORIZED
        | StatusCode::FORBIDDEN
        | StatusCode::UNPROCESSABLE_ENTITY => AppError::Unauthorized,
        _ => status_dependency_error(DEPENDENCY, status),
    }
}

fn map_obo_status(status: StatusCode) -> AppError {
    match status {
        StatusCode::FORBIDDEN => AppError::Forbidden,
        StatusCode::BAD_REQUEST
        | StatusCode::UNAUTHORIZED
        | StatusCode::CONFLICT
        | StatusCode::GONE
        | StatusCode::UNPROCESSABLE_ENTITY => AppError::Unauthorized,
        _ => status_dependency_error(DEPENDENCY, status),
    }
}

fn map_obo_exchange_status(status: StatusCode) -> AppError {
    match status {
        StatusCode::BAD_REQUEST => AppError::Unauthorized,
        StatusCode::FORBIDDEN => AppError::Forbidden,
        StatusCode::TOO_MANY_REQUESTS => AppError::RateLimited,
        _ => status_dependency_error(DEPENDENCY, status),
    }
}

fn valid_obo_access_proof(value: &str) -> bool {
    value.len() == 47
        && value.starts_with("obo_")
        && value[4..]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn status_dependency_error(dependency: &'static str, status: StatusCode) -> AppError {
    if status == StatusCode::TOO_MANY_REQUESTS {
        return AppError::RateLimited;
    }
    tracing::warn!(
        dependency,
        status = status.as_u16(),
        "dependency returned an unexpected status"
    );
    AppError::DependencyUnavailable { dependency }
}

fn dependency_unavailable() -> AppError {
    AppError::DependencyUnavailable {
        dependency: DEPENDENCY,
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, str::FromStr as _, time::Duration};

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use secrecy::SecretString;
    use time::{OffsetDateTime, format_description::well_known::Rfc3339};
    use url::Url;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_json, body_string_contains, header, method, path, query_param},
    };

    use super::{
        Audience, IamClient, MembershipPage, OboVerification, TokenIntrospection, iam_api_base,
        obo_verification_key, validate_active_introspection,
    };
    use crate::{
        application::{
            auth::{AuthContext, PresentedCredential},
            ports::{AuthenticationRequest, DelegationRequest, IdentityProvider as _},
        },
        config::IamSettings,
        domain::{ActorId, ActorRef, ActorType, OrganizationId},
    };

    fn settings(server: &MockServer) -> Result<IamSettings, Box<dyn std::error::Error>> {
        Ok(IamSettings {
            base_url: server.uri().parse()?,
            app_id: "silicon-dm".to_owned(),
            app_secret: SecretString::from("iam-secret".to_owned()),
            hook_service_id: "silicon-hook".to_owned(),
            request_timeout: Duration::from_secs(2),
        })
    }

    #[test]
    fn iam_api_base_is_not_duplicated() {
        let root = Url::parse("https://iam.example");
        let versioned = Url::parse("https://iam.example/api/v1");
        assert_eq!(
            root.ok().and_then(|url| iam_api_base(&url).ok()),
            versioned.ok()
        );
    }

    #[test]
    fn audience_matching_supports_single_and_multiple_claims() {
        assert!(Audience::One("silicon-dm".to_owned()).contains("silicon-dm"));
        assert!(
            Audience::Many(vec!["another-app".to_owned(), "silicon-dm".to_owned()])
                .contains("silicon-dm")
        );
    }

    #[test]
    fn obo_verification_attempts_use_unique_non_secret_keys() {
        let first = obo_verification_key();
        let second = obo_verification_key();
        assert_ne!(first, second);
        assert!(first.starts_with("dm-obo-verify-"));
    }

    #[test]
    fn obo_verification_uses_the_documented_singular_action_contract() {
        let response = serde_json::json!({
            "valid": true,
            "proof_id": "018f0d52-7b2a-7e29-a41d-7c02b93f6f42",
            "issuer_app_id": "origin-app",
            "audience": "silicon-dm",
            "actor": {
                "principal_id": "018f0d52-7b2a-7e29-a41d-7c02b93f6f43",
                "type": "carbon",
                "public_id": "carbon-1"
            },
            "org_id": "org-1",
            "action": "dm.messages.create",
            "resource": "/api/v1/conversations/conversation-1/messages",
            "expires_at": "2030-01-01T00:00:00Z",
            "consumed_at": "2029-12-31T23:59:30Z"
        });
        assert!(serde_json::from_value::<OboVerification>(response).is_ok());
        assert!(
            serde_json::from_value::<OboVerification>(serde_json::json!({
                "valid": true,
                "actions": ["dm.messages.create"]
            }))
            .is_err()
        );
    }

    #[test]
    fn active_introspection_requires_the_dm_audience() {
        let introspection = serde_json::from_value::<TokenIntrospection>(serde_json::json!({
            "active": true,
            "principal_id": "018f0d52-7b2a-7e29-a41d-7c02b93f6f43",
            "actor_type": "carbon",
            "org_id": "org-1"
        }));
        let organization_id = OrganizationId::from_str("org-1");
        assert!(
            introspection
                .as_ref()
                .ok()
                .zip(organization_id.as_ref().ok())
                .is_some_and(|(introspection, organization_id)| {
                    validate_active_introspection(
                        introspection,
                        Some(organization_id),
                        Some("silicon-dm"),
                    )
                    .is_err()
                })
        );
    }

    #[test]
    fn membership_page_requires_consistent_pagination_metadata() {
        let terminal = serde_json::from_value::<MembershipPage>(serde_json::json!({
            "items": [],
            "page": { "next_cursor": null, "has_more": false }
        }));
        let malformed = serde_json::from_value::<MembershipPage>(serde_json::json!({
            "items": [],
            "page": { "next_cursor": null, "has_more": true }
        }));
        assert!(terminal.is_ok_and(|page| page.next_cursor().is_ok_and(|cursor| cursor.is_none())));
        assert!(malformed.is_ok_and(|page| page.next_cursor().is_err()));
    }

    #[tokio::test]
    async fn bearer_authentication_binds_introspection_to_userinfo()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        let application_authorization = format!(
            "Basic {}",
            STANDARD.encode("silicon-dm:iam-secret".as_bytes())
        );
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/tokens/introspect"))
            .and(header("authorization", application_authorization))
            .and(header("x-org-id", "org-1"))
            .and(body_string_contains("token=actor-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": true,
                "principal_id": "018f0d52-7b2a-7e29-a41d-7c02b93f6f42",
                "actor_type": "carbon",
                "client_id": "silicon-dm",
                "org_id": "org-1",
                "scope": "dm.messages.create",
                "audience": "silicon-dm",
                "expires_at": 2_000_000_000
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/oauth/userinfo"))
            .and(header("authorization", "Bearer actor-token"))
            .and(header("x-org-id", "org-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "sub": "018f0d52-7b2a-7e29-a41d-7c02b93f6f42",
                "actor_type": "carbon",
                "public_id": "carbon-1",
                "org_id": "org-1"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let organization_id = OrganizationId::from_str("org-1")?;
        let token = SecretString::from("actor-token".to_owned());
        let context = IamClient::new(&settings(&server)?)?
            .authenticate(AuthenticationRequest::Bearer {
                token: &token,
                organization_id: &organization_id,
            })
            .await?;

        assert_eq!(context.actor.actor_type, ActorType::Carbon);
        assert_eq!(context.actor.id.as_str(), "carbon-1");
        assert!(context.has_capability("dm.messages.create"));
        Ok(())
    }

    #[tokio::test]
    async fn exchanges_a_bearer_for_a_provider_scoped_proof()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        let application_authorization = format!(
            "Basic {}",
            STANDARD.encode("silicon-dm:iam-secret".as_bytes())
        );
        let expires_at =
            (OffsetDateTime::now_utc() + time::Duration::seconds(30)).format(&Rfc3339)?;
        Mock::given(method("POST"))
            .and(path("/api/v1/obo-access/exchanges"))
            .and(header("authorization", application_authorization))
            .and(header("x-org-id", "org-1"))
            .and(body_json(serde_json::json!({
                "subject_token": "actor-token-for-silicon-dm-application",
                "audience": "waveform",
                "action": "waveform.stt",
                "org_id": "org-1"
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "access_proof": "obo_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG",
                "proof_id": "018f0d52-7b2a-7e29-a41d-7c02b93f6f44",
                "expires_in": 30,
                "expires_at": expires_at
            })))
            .expect(1)
            .mount(&server)
            .await;

        let organization_id = OrganizationId::from_str("org-1")?;
        let context = AuthContext {
            actor: ActorRef {
                actor_type: ActorType::Carbon,
                id: ActorId::from_str("carbon-1")?,
            },
            organization_id,
            represented_actor_ids: BTreeSet::new(),
            capabilities: BTreeSet::new(),
            credential: PresentedCredential::Bearer(SecretString::from(
                "actor-token-for-silicon-dm-application".to_owned(),
            )),
        };
        let credential = IamClient::new(&settings(&server)?)?
            .exchange_actor_credential(&context, &DelegationRequest::waveform_stt("waveform"))
            .await?;

        assert_eq!(credential.issuer_app_id(), "silicon-dm");
        assert!(!format!("{credential:?}").contains("obo_"));
        Ok(())
    }

    #[tokio::test]
    async fn hook_target_lookup_uses_request_scoped_service_authority()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/tokens/introspect"))
            .and(body_string_contains("token=hook-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": true,
                "principal_id": "018f0d52-7b2a-7e29-a41d-7c02b93f6f42",
                "actor_type": "service",
                "client_id": "silicon-hook",
                "scope": "dm.hook_events.deliver",
                "audience": "silicon-dm",
                "expires_at": 2_000_000_000
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/organizations/org-1/members"))
            .and(query_param("limit", "100"))
            .and(query_param("status", "active"))
            .and(header("authorization", "Bearer hook-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [{
                    "id": "018f0d52-7b2a-7e29-a41d-7c02b93f6f44",
                    "org_id": "org-1",
                    "principal": {
                        "principal_id": "018f0d52-7b2a-7e29-a41d-7c02b93f6f45",
                        "type": "silicon",
                        "public_id": "silicon-1"
                    },
                    "status": "active"
                }],
                "page": { "next_cursor": null, "has_more": false }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = IamClient::new(&settings(&server)?)?;
        let service = client
            .authenticate_service(&SecretString::from("hook-token".to_owned()))
            .await?;
        let organization_id = OrganizationId::from_str("org-1")?;
        let silicon_id = ActorId::from_str("silicon-1")?;
        let actor = client
            .authorize_hook_target(&service, &organization_id, &silicon_id)
            .await?;

        assert_eq!(actor.actor_type, ActorType::Silicon);
        assert_eq!(actor.id, silicon_id);
        Ok(())
    }
}
