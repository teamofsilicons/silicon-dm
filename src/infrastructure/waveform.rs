//! Silicon Waveform HTTP adapter.

use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt as _;
use reqwest::{
    Client, RequestBuilder, Response, StatusCode, header::HeaderValue, redirect::Policy,
};
use secrecy::ExposeSecret as _;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use url::Url;
use uuid::Uuid;

use super::briefcase::validate_permanent_url;
use crate::{
    AppError, AppResult,
    application::{
        auth::DelegatedCredential,
        ports::{Transcription, TranscriptionProvider},
    },
    config::ProviderSettings,
    domain::OrganizationId,
};

const DEPENDENCY: &str = "waveform";
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

/// Reqwest-backed Silicon Waveform client.
#[derive(Clone)]
pub struct WaveformClient {
    client: Client,
    stt_url: Url,
    briefcase_base_url: Url,
}

impl WaveformClient {
    /// Builds a Waveform adapter from validated provider settings.
    ///
    /// # Errors
    ///
    /// Returns an internal configuration error when the client or endpoint
    /// cannot be constructed safely.
    pub fn new(settings: &ProviderSettings) -> AppResult<Self> {
        validate_base_url(&settings.waveform_base_url)?;
        Ok(Self {
            client: build_client(settings.request_timeout)?,
            stt_url: append_segments(&settings.waveform_base_url, &["stt"])?,
            briefcase_base_url: settings.briefcase_base_url.clone(),
        })
    }
}

#[async_trait]
impl TranscriptionProvider for WaveformClient {
    async fn transcribe(
        &self,
        permanent_url: &Url,
        organization_id: &OrganizationId,
        credential: &DelegatedCredential,
        idempotency_key: &str,
    ) -> AppResult<Transcription> {
        validate_permanent_url(permanent_url, &self.briefcase_base_url)?;
        validate_idempotency_key(idempotency_key)?;

        let request = self
            .client
            .post(self.stt_url.clone())
            .header("X-Org-ID", organization_id.as_str())
            .header("Idempotency-Key", idempotency_key)
            .json(&SttRequest { permanent_url });
        // Credential and request-shape failures are authorization/input errors,
        // not terminal STT failures, so they must remain visible to the caller.
        let request = apply_delegated_credential(request, credential)?;
        let response = match send_redacted(request).await {
            Ok(response) => response,
            Err(AppError::DependencyUnavailable { .. }) => {
                return Ok(terminal_failure());
            }
            Err(error) => return Err(error),
        };
        match response.status() {
            StatusCode::OK => {}
            StatusCode::UNAUTHORIZED => return Err(AppError::Unauthorized),
            StatusCode::FORBIDDEN => return Err(AppError::Forbidden),
            StatusCode::NOT_FOUND => return Err(AppError::NotFound),
            StatusCode::CONFLICT => {
                return Err(AppError::conflict(
                    "Waveform rejected a conflicting request",
                ));
            }
            StatusCode::BAD_REQUEST
            | StatusCode::PAYLOAD_TOO_LARGE
            | StatusCode::UNSUPPORTED_MEDIA_TYPE
            | StatusCode::UNPROCESSABLE_ENTITY => {
                return Err(AppError::validation(
                    "the voice attachment could not be transcribed",
                ));
            }
            status => {
                log_terminal_status(status);
                return Ok(terminal_failure());
            }
        }

        let document: SttResponse = match read_json(response).await {
            Ok(document) => document,
            Err(AppError::DependencyUnavailable { .. }) => {
                return Ok(terminal_failure());
            }
            Err(error) => return Err(error),
        };
        if document.provider.trim().is_empty() {
            reject_response("missing_provider");
            return Ok(terminal_failure());
        }
        let _request_id = document.request_id;
        Ok(Transcription {
            transcript: Some(document.transcript),
            duration_milliseconds: document.duration_ms.0,
        })
    }
}

fn terminal_failure() -> Transcription {
    Transcription {
        transcript: None,
        duration_milliseconds: None,
    }
}

#[derive(Serialize)]
struct SttRequest<'a> {
    #[serde(rename = "file_url")]
    permanent_url: &'a Url,
}

#[derive(Debug, Deserialize)]
struct SttResponse {
    request_id: Uuid,
    transcript: String,
    #[serde(rename = "detected_language")]
    _detected_language: RequiredNullable<String>,
    provider: String,
    duration_ms: RequiredNullable<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
struct RequiredNullable<T>(Option<T>);

fn validate_idempotency_key(key: &str) -> AppResult<()> {
    if !(8..=255).contains(&key.len()) || !key.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(AppError::validation(
            "Idempotency-Key must contain 8 to 255 visible ASCII characters",
        ));
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
        .map_err(|_| AppError::internal(anyhow::anyhow!("failed to build Waveform HTTP client")))
}

fn append_segments(base_url: &Url, segments: &[&str]) -> AppResult<Url> {
    validate_base_url(base_url)?;
    let mut endpoint = base_url.clone();
    {
        let mut path = endpoint.path_segments_mut().map_err(|()| {
            AppError::internal(anyhow::anyhow!("invalid Waveform endpoint base URL"))
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
            "invalid Waveform base URL"
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

fn log_terminal_status(status: StatusCode) {
    tracing::warn!(
        dependency = DEPENDENCY,
        status = status.as_u16(),
        "speech-to-text reached a terminal provider failure"
    );
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
    use url::Url;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_json, header, method, path},
    };

    use super::{SttResponse, WaveformClient, append_segments, validate_idempotency_key};
    use crate::{
        application::{auth::DelegatedCredential, ports::TranscriptionProvider as _},
        config::ProviderSettings,
        domain::OrganizationId,
    };

    #[test]
    fn stt_endpoint_preserves_version_prefix() {
        let endpoint = Url::parse("https://waveform.example/api/v1")
            .ok()
            .and_then(|base| append_segments(&base, &["stt"]).ok());
        assert_eq!(
            endpoint.as_ref().map(Url::as_str),
            Some("https://waveform.example/api/v1/stt")
        );
    }

    #[test]
    fn idempotency_key_rejects_control_characters_and_bad_lengths() {
        assert!(validate_idempotency_key("voice-123").is_ok());
        assert!(validate_idempotency_key("short").is_err());
        assert!(validate_idempotency_key("voice\n123").is_err());
    }

    #[test]
    fn successful_stt_response_requires_transcript_and_nullable_duration() {
        let response = serde_json::json!({
            "request_id": "018f0d52-7b2a-7e29-a41d-7c02b93f6f42",
            "transcript": "hello",
            "detected_language": null,
            "provider": "provider-1",
            "duration_ms": null
        });
        assert!(serde_json::from_value::<SttResponse>(response).is_ok());
        assert!(
            serde_json::from_value::<SttResponse>(serde_json::json!({
                "request_id": "018f0d52-7b2a-7e29-a41d-7c02b93f6f42",
                "detected_language": null,
                "provider": "provider-1",
                "duration_ms": null
            }))
            .is_err()
        );
    }

    #[tokio::test]
    async fn posts_the_permanent_url_with_actor_and_idempotency_context()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        let file_url =
            "https://briefcase.example/api/v1/entries/018f0d52-7b2a-7e29-a41d-7c02b93f6f42";
        Mock::given(method("POST"))
            .and(path("/api/v1/stt"))
            .and(header("x-org-id", "org-1"))
            .and(header("idempotency-key", "voice-request-1"))
            .and(header(
                "x-iam-obo-access-proof",
                "obo_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG",
            ))
            .and(header("x-app-id", "silicon-dm"))
            .and(body_json(serde_json::json!({ "file_url": file_url })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "request_id": "018f0d52-7b2a-7e29-a41d-7c02b93f6f43",
                "transcript": "hello",
                "detected_language": "en",
                "provider": "provider-1",
                "duration_ms": 1_250
            })))
            .expect(1)
            .mount(&server)
            .await;

        let settings = ProviderSettings {
            briefcase_base_url: "https://briefcase.example/api/v1".parse()?,
            briefcase_iam_audience: "silicon-briefcase".to_owned(),
            waveform_base_url: format!("{}/api/v1", server.uri()).parse()?,
            waveform_iam_audience: "waveform".to_owned(),
            giphy_api_base_url: "https://api.giphy.com/v1/gifs".parse()?,
            giphy_api_key: None,
            request_timeout: Duration::from_secs(2),
            trending_cache_ttl: Duration::from_secs(60),
        };
        let organization_id = OrganizationId::from_str("org-1")?;
        let result = WaveformClient::new(&settings)?
            .transcribe(
                &Url::parse(file_url)?,
                &organization_id,
                &DelegatedCredential::new(
                    SecretString::from(
                        "obo_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG".to_owned(),
                    ),
                    "silicon-dm".to_owned(),
                ),
                "voice-request-1",
            )
            .await?;

        assert_eq!(result.transcript.as_deref(), Some("hello"));
        assert_eq!(result.duration_milliseconds, Some(1_250));
        Ok(())
    }

    #[tokio::test]
    async fn terminal_provider_failure_allows_voice_delivery_without_transcript()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        let file_url =
            "https://briefcase.example/api/v1/entries/018f0d52-7b2a-7e29-a41d-7c02b93f6f42";
        Mock::given(method("POST"))
            .and(path("/api/v1/stt"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&server)
            .await;

        let settings = ProviderSettings {
            briefcase_base_url: "https://briefcase.example/api/v1".parse()?,
            briefcase_iam_audience: "silicon-briefcase".to_owned(),
            waveform_base_url: format!("{}/api/v1", server.uri()).parse()?,
            waveform_iam_audience: "waveform".to_owned(),
            giphy_api_base_url: "https://api.giphy.com/v1/gifs".parse()?,
            giphy_api_key: None,
            request_timeout: Duration::from_secs(2),
            trending_cache_ttl: Duration::from_secs(60),
        };
        let result = WaveformClient::new(&settings)?
            .transcribe(
                &Url::parse(file_url)?,
                &OrganizationId::from_str("org-1")?,
                &DelegatedCredential::new(
                    SecretString::from(
                        "obo_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG".to_owned(),
                    ),
                    "silicon-dm".to_owned(),
                ),
                "voice-request-2",
            )
            .await?;

        assert_eq!(result.transcript, None);
        assert_eq!(result.duration_milliseconds, None);
        Ok(())
    }
}
