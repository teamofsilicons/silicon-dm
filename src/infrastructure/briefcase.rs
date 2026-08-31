//! Briefcase HTTP adapter.

use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt as _;
use reqwest::{
    Client, RequestBuilder, Response, StatusCode, header::HeaderValue, redirect::Policy,
};
use secrecy::ExposeSecret as _;
use serde::{Deserialize, de::DeserializeOwned};
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    application::{
        auth::DelegatedCredential,
        ports::{AttachmentProvider, TemporaryAttachmentUrl},
    },
    config::ProviderSettings,
    domain::OrganizationId,
};

const DEPENDENCY: &str = "briefcase";
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// Reqwest-backed Silicon Briefcase client.
#[derive(Clone)]
pub struct BriefcaseClient {
    client: Client,
    base_url: Url,
}

impl BriefcaseClient {
    /// Builds a Briefcase adapter from validated provider settings.
    ///
    /// # Errors
    ///
    /// Returns an internal configuration error when the client or base URL
    /// cannot be constructed safely.
    pub fn new(settings: &ProviderSettings) -> AppResult<Self> {
        validate_base_url(&settings.briefcase_base_url)?;
        Ok(Self {
            client: build_client(settings.request_timeout)?,
            base_url: settings.briefcase_base_url.clone(),
        })
    }

    fn download_url_endpoint(&self, entry_id: Uuid) -> AppResult<Url> {
        append_segments(
            &self.base_url,
            &["entries", &entry_id.to_string(), "download-url"],
        )
    }
}

#[async_trait]
impl AttachmentProvider for BriefcaseClient {
    async fn temporary_url(
        &self,
        permanent_url: &Url,
        organization_id: &OrganizationId,
        credential: &DelegatedCredential,
    ) -> AppResult<TemporaryAttachmentUrl> {
        let entry_id = validate_permanent_url(permanent_url, &self.base_url)?;
        let request = self
            .client
            .post(self.download_url_endpoint(entry_id)?)
            .header("X-Org-ID", organization_id.as_str());
        let request = apply_delegated_credential(request, credential)?;

        let response = send_redacted(request).await?;
        match response.status() {
            StatusCode::CREATED => {}
            StatusCode::UNAUTHORIZED => return Err(AppError::Unauthorized),
            StatusCode::FORBIDDEN => return Err(AppError::Forbidden),
            StatusCode::NOT_FOUND => return Err(AppError::NotFound),
            StatusCode::CONFLICT => {
                return Err(AppError::conflict(
                    "Briefcase rejected a conflicting request",
                ));
            }
            StatusCode::TOO_MANY_REQUESTS => return Err(AppError::RateLimited),
            StatusCode::BAD_REQUEST
            | StatusCode::PAYLOAD_TOO_LARGE
            | StatusCode::UNSUPPORTED_MEDIA_TYPE
            | StatusCode::UNPROCESSABLE_ENTITY => {
                return Err(AppError::validation(
                    "the attachment could not be resolved by Briefcase",
                ));
            }
            status => return Err(status_dependency_error(status)),
        }

        let document: TemporaryUrlDocument = read_json(response).await?;
        validate_temporary_url(&document.url, document.expires_at)?;
        Ok(TemporaryAttachmentUrl {
            url: document.url,
            expires_at: document.expires_at,
        })
    }
}

#[derive(Debug, Deserialize)]
struct TemporaryUrlDocument {
    url: Url,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
}

/// Validates and extracts a Briefcase entry UUID from a permanent URL.
///
/// This is shared with the Waveform adapter so both integration paths enforce
/// the same SSRF boundary before forwarding a user-supplied URL.
pub(super) fn validate_permanent_url(
    permanent_url: &Url,
    briefcase_base_url: &Url,
) -> AppResult<Uuid> {
    validate_base_url(briefcase_base_url)?;
    if permanent_url.scheme() != "https"
        || permanent_url.host_str() != briefcase_base_url.host_str()
        || permanent_url.port_or_known_default() != briefcase_base_url.port_or_known_default()
        || !permanent_url.username().is_empty()
        || permanent_url.password().is_some()
        || permanent_url.query().is_some()
        || permanent_url.fragment().is_some()
        || permanent_url.path().ends_with('/')
    {
        return Err(AppError::validation(
            "attachment URL must be a canonical Briefcase permanent URL",
        ));
    }

    let base_segments = strict_path_segments(briefcase_base_url)?;
    let permanent_segments = strict_path_segments(permanent_url)?;
    if permanent_segments.len() != base_segments.len() + 2
        || permanent_segments[..base_segments.len()] != base_segments
        || permanent_segments[base_segments.len()] != "entries"
    {
        return Err(AppError::validation(
            "attachment URL must identify one Briefcase entry",
        ));
    }
    Uuid::parse_str(permanent_segments[base_segments.len() + 1])
        .map_err(|_| AppError::validation("attachment URL contains an invalid Briefcase entry ID"))
}

fn strict_path_segments(url: &Url) -> AppResult<Vec<&str>> {
    let segments: Vec<_> = url
        .path_segments()
        .ok_or_else(|| AppError::validation("attachment URL cannot identify a Briefcase entry"))?
        .collect();
    let last_nonempty = segments.last().is_some_and(|segment| segment.is_empty());
    let normalized = if last_nonempty {
        &segments[..segments.len().saturating_sub(1)]
    } else {
        &segments
    };
    if normalized.iter().any(|segment| segment.is_empty()) {
        return Err(AppError::validation(
            "attachment URL contains an invalid path",
        ));
    }
    Ok(normalized.to_vec())
}

fn validate_temporary_url(url: &Url, expires_at: OffsetDateTime) -> AppResult<()> {
    let now = OffsetDateTime::now_utc();
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || expires_at <= now
        || expires_at > now + time::Duration::hours(13)
    {
        tracing::warn!(
            dependency = DEPENDENCY,
            failure = "invalid_temporary_url",
            "dependency response rejected"
        );
        return Err(dependency_unavailable());
    }
    Ok(())
}

fn apply_delegated_credential(
    request: RequestBuilder,
    credential: &DelegatedCredential,
) -> AppResult<RequestBuilder> {
    let mut proof = HeaderValue::from_bytes(credential.proof().expose_secret().as_bytes())
        .map_err(|_| dependency_unavailable())?;
    proof.set_sensitive(true);
    let app_id = HeaderValue::from_bytes(credential.issuer_app_id().as_bytes())
        .map_err(|_| dependency_unavailable())?;
    Ok(request
        .header("X-IAM-OBO-Access-Proof", proof)
        .header("X-App-ID", app_id))
}

fn build_client(timeout: Duration) -> AppResult<Client> {
    Client::builder()
        .connect_timeout(timeout.min(Duration::from_secs(5)))
        .timeout(timeout)
        .redirect(Policy::none())
        .referer(false)
        .user_agent(concat!("silicon-dm/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| AppError::internal(anyhow::anyhow!("failed to build Briefcase HTTP client")))
}

fn append_segments(base_url: &Url, segments: &[&str]) -> AppResult<Url> {
    validate_base_url(base_url)?;
    let mut endpoint = base_url.clone();
    {
        let mut path = endpoint.path_segments_mut().map_err(|()| {
            AppError::internal(anyhow::anyhow!("invalid Briefcase endpoint base URL"))
        })?;
        path.pop_if_empty();
        for segment in segments {
            path.push(segment);
        }
    }
    Ok(endpoint)
}

fn validate_base_url(url: &Url) -> AppResult<()> {
    if !matches!(url.scheme(), "http" | "https")
        || url.cannot_be_a_base()
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        tracing::error!(dependency = DEPENDENCY, "invalid dependency base URL");
        return Err(AppError::internal(anyhow::anyhow!(
            "invalid Briefcase base URL"
        )));
    }
    Ok(())
}

async fn send_redacted(request: RequestBuilder) -> AppResult<Response> {
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
        tracing::warn!(
            dependency = DEPENDENCY,
            failure,
            "dependency request failed"
        );
        dependency_unavailable()
    })
}

async fn read_json<T>(response: Response) -> AppResult<T>
where
    T: DeserializeOwned,
{
    if !has_json_content_type(&response) {
        return Err(reject_response("invalid_content_type"));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(reject_response("response_too_large"));
    }

    let mut body = Vec::with_capacity(
        response
            .content_length()
            .and_then(|length| usize::try_from(length).ok())
            .unwrap_or_default()
            .min(MAX_RESPONSE_BYTES),
    );
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| reject_response("body_read"))?;
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(reject_response("response_too_large"));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| reject_response("invalid_json"))
}

fn has_json_content_type(response: &Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
}

fn reject_response(failure: &'static str) -> AppError {
    tracing::warn!(
        dependency = DEPENDENCY,
        failure,
        "dependency response rejected"
    );
    dependency_unavailable()
}

fn status_dependency_error(status: StatusCode) -> AppError {
    tracing::warn!(
        dependency = DEPENDENCY,
        status = status.as_u16(),
        "dependency returned an unexpected status"
    );
    dependency_unavailable()
}

fn dependency_unavailable() -> AppError {
    AppError::DependencyUnavailable {
        dependency: DEPENDENCY,
    }
}

#[cfg(test)]
mod tests {
    use std::{str::FromStr as _, time::Duration};

    use secrecy::SecretString;
    use time::{OffsetDateTime, format_description::well_known::Rfc3339};
    use url::Url;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    use super::{BriefcaseClient, validate_permanent_url};
    use crate::{
        application::{auth::DelegatedCredential, ports::AttachmentProvider as _},
        config::ProviderSettings,
        domain::OrganizationId,
    };

    #[test]
    fn permanent_url_requires_exact_briefcase_resource_path() {
        let base = Url::parse("https://briefcase.example/api/v1");
        let valid = Url::parse(
            "https://briefcase.example/api/v1/entries/018f0d52-7b2a-7e29-a41d-7c02b93f6f42",
        );
        assert!(
            base.as_ref()
                .ok()
                .zip(valid.as_ref().ok())
                .is_some_and(|(base, valid)| validate_permanent_url(valid, base).is_ok())
        );
    }

    #[test]
    fn permanent_url_rejects_ssrf_variants() {
        let base = Url::parse("https://briefcase.example/api/v1");
        let candidates = [
            "http://briefcase.example/api/v1/entries/018f0d52-7b2a-7e29-a41d-7c02b93f6f42",
            "https://evil.example/api/v1/entries/018f0d52-7b2a-7e29-a41d-7c02b93f6f42",
            "https://briefcase.example/api/v1/entries/018f0d52-7b2a-7e29-a41d-7c02b93f6f42?x=1",
            "https://briefcase.example/api/v1/entries/018f0d52-7b2a-7e29-a41d-7c02b93f6f42/",
            "https://briefcase.example/api/v1/entries/not-a-uuid",
        ];
        assert!(
            base.as_ref()
                .is_ok_and(|base| candidates.iter().all(|candidate| {
                    Url::parse(candidate)
                        .is_ok_and(|candidate| validate_permanent_url(&candidate, base).is_err())
                }))
        );
    }

    #[tokio::test]
    async fn requests_a_temporary_url_with_a_scoped_obo_proof()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        let entry_id = "018f0d52-7b2a-7e29-a41d-7c02b93f6f42";
        let expires_at = (OffsetDateTime::now_utc() + time::Duration::hours(1)).format(&Rfc3339)?;
        Mock::given(method("POST"))
            .and(path(format!("/api/v1/entries/{entry_id}/download-url")))
            .and(header("x-org-id", "org-1"))
            .and(header(
                "x-iam-obo-access-proof",
                "obo_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG",
            ))
            .and(header("x-app-id", "silicon-dm"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "url": "https://cdn.example/download?signature=redacted",
                "expires_at": expires_at
            })))
            .expect(1)
            .mount(&server)
            .await;

        let settings = ProviderSettings {
            briefcase_base_url: format!("{}/api/v1", server.uri()).parse()?,
            briefcase_iam_audience: "silicon-briefcase".to_owned(),
            waveform_base_url: "https://waveform.example/api/v1".parse()?,
            waveform_iam_audience: "waveform".to_owned(),
            giphy_api_base_url: "https://api.giphy.com/v1/gifs".parse()?,
            giphy_api_key: None,
            request_timeout: Duration::from_secs(2),
            trending_cache_ttl: Duration::from_secs(60),
        };
        let permanent_url = format!(
            "{}/api/v1/entries/{entry_id}",
            server.uri().replacen("http://", "https://", 1)
        )
        .parse()?;
        let organization_id = OrganizationId::from_str("org-1")?;
        let result = BriefcaseClient::new(&settings)?
            .temporary_url(
                &permanent_url,
                &organization_id,
                &DelegatedCredential::new(
                    SecretString::from(
                        "obo_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG".to_owned(),
                    ),
                    "silicon-dm".to_owned(),
                ),
            )
            .await?;

        assert_eq!(result.url.host_str(), Some("cdn.example"));
        Ok(())
    }
}
