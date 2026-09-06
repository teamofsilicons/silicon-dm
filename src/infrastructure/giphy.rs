//! Giphy HTTP adapter.

use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use futures::StreamExt as _;
use reqwest::{Client, RequestBuilder, Response, StatusCode, redirect::Policy};
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, de::DeserializeOwned};
use tokio::sync::Mutex;
use url::Url;

use crate::{
    AppError, AppResult, application::ports::GifProvider, config::ProviderSettings, domain::Gif,
};

const DEPENDENCY: &str = "giphy";
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const RESULT_LIMIT: &str = "25";
const MAX_RESULTS: usize = 25;
const SAFE_RATING: &str = "g";
const RENDITION_BUNDLE: &str = "messaging_non_clips";
const MAX_SEARCH_CHARACTERS: usize = 50;

/// Reqwest-backed Giphy client with a single-flight process-local trending
/// cache.
#[derive(Clone)]
pub struct GiphyClient {
    client: Client,
    trending_url: Url,
    search_url: Url,
    api_key: SecretString,
    cache_ttl: Duration,
    trending_cache: Arc<Mutex<Option<CachedTrending>>>,
}

impl GiphyClient {
    /// Builds a Giphy adapter from validated provider settings.
    /// # Errors
    ///
    /// Returns an internal configuration error when the HTTP client or endpoint
    /// URLs cannot be constructed safely.
    pub fn new(settings: &ProviderSettings) -> AppResult<Self> {
        validate_base_url(&settings.giphy_api_base_url)?;
        Ok(Self {
            client: build_client(settings.request_timeout)?,
            trending_url: append_segments(&settings.giphy_api_base_url, &["trending"])?,
            search_url: append_segments(&settings.giphy_api_base_url, &["search"])?,
            api_key: settings.giphy_api_key.clone(),
            cache_ttl: settings.trending_cache_ttl,
            trending_cache: Arc::new(Mutex::new(None)),
        })
    }

    async fn fetch(&self, endpoint: &Url, query: Option<&str>) -> AppResult<Vec<Gif>> {
        let mut url = endpoint.clone();
        {
            let mut parameters = url.query_pairs_mut();
            parameters.append_pair("api_key", self.api_key.expose_secret());
            parameters.append_pair("limit", RESULT_LIMIT);
            parameters.append_pair("rating", SAFE_RATING);
            parameters.append_pair("bundle", RENDITION_BUNDLE);
            parameters.append_pair("remove_low_contrast", "true");
            if let Some(query) = query {
                parameters.append_pair("q", query);
            }
        }

        let response = send_redacted(self.client.get(url)).await?;
        match response.status() {
            StatusCode::OK => {}
            StatusCode::TOO_MANY_REQUESTS => return Err(AppError::RateLimited),
            status => return Err(status_dependency_error(status)),
        }
        let document: GiphyResponse = read_json(response).await?;
        if document.meta.status != StatusCode::OK.as_u16() {
            return Err(reject_response("provider_meta_status"));
        }
        map_gifs(&document.data)
    }
}

#[async_trait]
impl GifProvider for GiphyClient {
    async fn trending(&self) -> AppResult<Vec<Gif>> {
        let now = Instant::now();
        let mut cache = self.trending_cache.lock().await;
        if let Some(cached) = cache.as_ref().filter(|cached| cached.expires_at > now) {
            return Ok(cached.items.clone());
        }

        let items = self.fetch(&self.trending_url, None).await?;
        if let Some(expires_at) = Instant::now().checked_add(self.cache_ttl) {
            *cache = Some(CachedTrending {
                expires_at,
                items: items.clone(),
            });
        } else {
            *cache = None;
        }
        Ok(items)
    }

    async fn search(&self, query: &str) -> AppResult<Vec<Gif>> {
        validate_search_query(query)?;
        self.fetch(&self.search_url, Some(query.trim())).await
    }
}

struct CachedTrending {
    expires_at: Instant,
    items: Vec<Gif>,
}

#[derive(Debug, Deserialize)]
struct GiphyResponse {
    data: Vec<GiphyItem>,
    meta: GiphyMeta,
}

#[derive(Debug, Deserialize)]
struct GiphyMeta {
    status: u16,
}

#[derive(Debug, Deserialize)]
struct GiphyItem {
    id: String,
    #[serde(default)]
    title: String,
    images: BTreeMap<String, GiphyRendition>,
}

#[derive(Debug, Deserialize)]
struct GiphyRendition {
    #[serde(default)]
    url: String,
}

fn map_gifs(items: &[GiphyItem]) -> AppResult<Vec<Gif>> {
    if items.len() > MAX_RESULTS {
        return Err(reject_response("too_many_results"));
    }
    let provider_item_count = items.len();
    let mapped: Vec<_> = items.iter().filter_map(map_gif).collect();
    if provider_item_count > 0 && mapped.is_empty() {
        return Err(reject_response("no_valid_renditions"));
    }
    Ok(mapped)
}

fn map_gif(item: &GiphyItem) -> Option<Gif> {
    let provider_id = item.id.trim();
    if provider_id.is_empty()
        || provider_id.len() > 255
        || provider_id.chars().any(char::is_control)
    {
        return None;
    }
    let url = rendition_url(&item.images, &["original", "downsized", "fixed_width"])?;
    let preview_url = rendition_url(
        &item.images,
        &[
            "fixed_width_small",
            "fixed_width",
            "preview_gif",
            "downsized_small",
        ],
    );
    let title = item.title.trim();
    let title = (!title.is_empty() && title.chars().count() <= 1_000).then(|| title.to_owned());
    Some(Gif {
        provider_id: provider_id.to_owned(),
        url,
        preview_url,
        title,
    })
}

fn rendition_url(images: &BTreeMap<String, GiphyRendition>, names: &[&str]) -> Option<Url> {
    names.iter().find_map(|name| {
        let url = Url::parse(images.get(*name)?.url.trim()).ok()?;
        safe_media_url(&url).then_some(url)
    })
}

fn safe_media_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str().is_some()
        && url.as_str().len() <= 8_192
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}

fn validate_search_query(query: &str) -> AppResult<()> {
    let character_count = query.chars().count();
    if query.trim().is_empty()
        || character_count > MAX_SEARCH_CHARACTERS
        || query.chars().any(char::is_control)
    {
        return Err(AppError::validation(
            "GIF search query must contain 1 to 50 non-control characters",
        ));
    }
    Ok(())
}

fn build_client(timeout: Duration) -> AppResult<Client> {
    Client::builder()
        .connect_timeout(timeout.min(Duration::from_secs(5)))
        .timeout(timeout)
        .redirect(Policy::none())
        .referer(false)
        .user_agent(concat!("silicon-dm/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| AppError::internal(anyhow::anyhow!("failed to build Giphy HTTP client")))
}

fn append_segments(base_url: &Url, segments: &[&str]) -> AppResult<Url> {
    validate_base_url(base_url)?;
    let mut endpoint = base_url.clone();
    {
        let mut path = endpoint
            .path_segments_mut()
            .map_err(|()| AppError::internal(anyhow::anyhow!("invalid Giphy endpoint base URL")))?;
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
            "invalid Giphy base URL"
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
    use std::{collections::BTreeMap, time::Duration};

    use secrecy::SecretString;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, query_param},
    };

    use super::{GiphyClient, GiphyItem, GiphyRendition, map_gif, validate_search_query};
    use crate::{application::ports::GifProvider as _, config::ProviderSettings};

    #[test]
    fn maps_only_https_giphy_renditions() {
        let images = BTreeMap::from([
            (
                "original".to_owned(),
                GiphyRendition {
                    url: "https://media.giphy.com/full.gif".to_owned(),
                },
            ),
            (
                "fixed_width_small".to_owned(),
                GiphyRendition {
                    url: "https://media.giphy.com/preview.gif".to_owned(),
                },
            ),
        ]);
        let gif = map_gif(&GiphyItem {
            id: "gif-1".to_owned(),
            title: "A GIF".to_owned(),
            images,
        });
        assert!(gif.is_some_and(|gif| gif.provider_id == "gif-1" && gif.preview_url.is_some()));
    }

    #[test]
    fn validates_search_length_and_controls() {
        assert!(validate_search_query("celebration").is_ok());
        assert!(validate_search_query("  ").is_err());
        assert!(validate_search_query("bad\nquery").is_err());
        assert!(validate_search_query(&"x".repeat(51)).is_err());
    }

    #[tokio::test]
    async fn trending_uses_safe_query_parameters_and_process_cache()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/gifs/trending"))
            .and(query_param("api_key", "test-key"))
            .and(query_param("limit", "25"))
            .and(query_param("rating", "g"))
            .and(query_param("bundle", "messaging_non_clips"))
            .and(query_param("remove_low_contrast", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "id": "gif-1",
                    "title": "Hello",
                    "images": {
                        "original": { "url": "https://media.giphy.com/full.gif" }
                    }
                }],
                "meta": { "status": 200 }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let settings = ProviderSettings {
            giphy_api_base_url: format!("{}/v1/gifs", server.uri()).parse()?,
            giphy_api_key: SecretString::from("test-key".to_owned()),
            request_timeout: Duration::from_secs(2),
            trending_cache_ttl: Duration::from_secs(60),
        };
        let client = GiphyClient::new(&settings)?;
        let first = client.trending().await?;
        let second = client.trending().await?;

        assert_eq!(first, second);
        assert_eq!(
            first.first().map(|gif| gif.provider_id.as_str()),
            Some("gif-1")
        );
        Ok(())
    }
}
