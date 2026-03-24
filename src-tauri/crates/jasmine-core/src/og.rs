use reqwest::header::CONTENT_TYPE;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::{CoreError, Result};

const OG_FETCH_TIMEOUT_SECONDS: u64 = 5;
const OG_MAX_REDIRECTS: usize = 2;
const OG_MAX_BODY_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OgMetadata {
    pub url: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub image_url: Option<String>,
    pub site_name: Option<String>,
}

impl OgMetadata {
    pub fn empty(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            title: None,
            description: None,
            image_url: None,
            site_name: None,
        }
    }
}

pub async fn fetch_og_metadata(url: &str) -> Result<OgMetadata> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(OG_FETCH_TIMEOUT_SECONDS))
        .redirect(reqwest::redirect::Policy::limited(OG_MAX_REDIRECTS))
        .build()
        .map_err(|error| {
            CoreError::Persistence(format!("failed to build OG fetch client: {error}"))
        })?;

    let response = match client.get(url).send().await {
        Ok(response) => response,
        Err(error) if error.is_timeout() || error.is_builder() => {
            warn!(url = url, error = %error, "OG metadata fetch degraded gracefully");
            return Ok(OgMetadata::empty(url));
        }
        Err(error) => {
            return Err(CoreError::Persistence(format!(
                "OG fetch request failed for {url}: {error}"
            )));
        }
    };

    if !response.status().is_success() {
        return Err(CoreError::Persistence(format!(
            "OG fetch returned HTTP {} for {url}",
            response.status()
        )));
    }

    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    if let Some(content_type) = content_type.as_deref() {
        if !is_html_content_type(content_type) {
            return Ok(OgMetadata::empty(url));
        }
    }

    let body_bytes = match read_response_body(response, url).await {
        Ok(body) => body,
        Err(error) if error.is_timeout() => {
            warn!(url = url, error = %error, "OG metadata body read timed out");
            return Ok(OgMetadata::empty(url));
        }
        Err(error) => {
            return Err(CoreError::Persistence(format!(
                "OG fetch body read failed for {url}: {error}"
            )));
        }
    };

    Ok(parse_og_metadata(
        url,
        String::from_utf8_lossy(&body_bytes).as_ref(),
    ))
}

async fn read_response_body(
    mut response: reqwest::Response,
    url: &str,
) -> reqwest::Result<Vec<u8>> {
    let mut body = Vec::new();
    let mut truncated = false;

    while let Some(chunk) = response.chunk().await? {
        let remaining = OG_MAX_BODY_BYTES.saturating_sub(body.len());

        if remaining == 0 {
            truncated = true;
            break;
        }

        if chunk.len() > remaining {
            body.extend_from_slice(&chunk[..remaining]);
            truncated = true;
            break;
        }

        body.extend_from_slice(&chunk);
    }

    if truncated {
        warn!(
            url = url,
            max_bytes = OG_MAX_BODY_BYTES,
            "OG metadata body truncated"
        );
    }

    Ok(body)
}

fn parse_og_metadata(url: &str, html: &str) -> OgMetadata {
    let selector = Selector::parse("meta").expect("meta selector must be valid");
    let document = Html::parse_document(html);
    let mut metadata = OgMetadata::empty(url);

    for element in document.select(&selector) {
        let Some(property) = element
            .value()
            .attr("property")
            .or_else(|| element.value().attr("name"))
        else {
            continue;
        };

        let Some(content) = element
            .value()
            .attr("content")
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };

        match property.trim().to_ascii_lowercase().as_str() {
            "og:title" => set_first_value(&mut metadata.title, content),
            "og:description" => set_first_value(&mut metadata.description, content),
            "og:image" => set_first_value(&mut metadata.image_url, content),
            "og:site_name" => set_first_value(&mut metadata.site_name, content),
            _ => {}
        }
    }

    metadata
}

fn set_first_value(field: &mut Option<String>, value: &str) {
    if field.is_none() {
        *field = Some(value.to_string());
    }
}

fn is_html_content_type(content_type: &str) -> bool {
    let normalized = content_type.trim().to_ascii_lowercase();

    normalized.starts_with("text/html") || normalized.starts_with("application/xhtml+xml")
}

#[cfg(test)]
mod tests {
    use super::parse_og_metadata;

    #[test]
    fn og_parser_is_case_insensitive_and_keeps_first_value() {
        let metadata = parse_og_metadata(
            "https://example.com/post",
            r#"
                <html>
                    <head>
                        <meta property="OG:TITLE" content="First title" />
                        <meta property="og:title" content="Second title" />
                        <meta name="og:description" content="Description" />
                        <meta property="og:image" content="https://cdn.example.com/cover.png" />
                    </head>
                </html>
            "#,
        );

        assert_eq!(metadata.url, "https://example.com/post");
        assert_eq!(metadata.title.as_deref(), Some("First title"));
        assert_eq!(metadata.description.as_deref(), Some("Description"));
        assert_eq!(
            metadata.image_url.as_deref(),
            Some("https://cdn.example.com/cover.png")
        );
        assert_eq!(metadata.site_name, None);
    }
}
