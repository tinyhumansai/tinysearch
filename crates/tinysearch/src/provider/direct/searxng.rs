//! Keyless `SearXNG` search against a host-enabled instance.
use super::{configured_timeout, direct_url, required_string};
use crate::{
    Citation, Error, ExecuteToolRequest, ExecuteToolResponse, ProviderConfig, Result, SearchResult,
    SearchStatus,
};
use reqwest::Client;
use serde_json::{Value, json};
use std::time::Duration;
use tinysearch_bus::IMAGE_ASSETS;

const MAX_RESULTS: usize = 50;

pub(super) async fn run(
    client: &Client,
    config: &ProviderConfig,
    request: &ExecuteToolRequest,
) -> Result<ExecuteToolResponse> {
    if request.name != "searxng_search" {
        return Err(Error::UnavailableTool(request.name.clone()));
    }
    let base = config
        .base_url
        .as_deref()
        .filter(|url| !url.trim().is_empty())
        .ok_or_else(|| Error::Provider("SearXNG URL unavailable".into()))?;
    let path = if base.trim_end_matches('/').ends_with("/search") {
        ""
    } else {
        "/search"
    };
    let endpoint = direct_url(Some(base), base, path)?;
    let query = required_string(&request.arguments, "query")?.trim();
    let categories = categories(request.arguments.get("categories"))?;
    let language = request
        .arguments
        .get("language")
        .and_then(Value::as_str)
        .or(config.default_language.as_deref())
        .unwrap_or("en")
        .trim();
    let max_results = request
        .arguments
        .get("max_results")
        .and_then(Value::as_u64)
        .or(config.max_results)
        .unwrap_or(10)
        .clamp(1, MAX_RESULTS as u64);
    let max_results = usize::try_from(max_results).unwrap_or(MAX_RESULTS);
    let mut params = vec![("q", query), ("format", "json")];
    let category_string = categories.join(",");
    if !category_string.is_empty() {
        params.push(("categories", &category_string));
    }
    if !language.is_empty() {
        params.push(("language", language));
    }
    let response = client
        .get(endpoint)
        .query(&params)
        .timeout(configured_timeout(config, Duration::from_secs(10)))
        .send()
        .await
        .map_err(super::super::http::transport_error)?;
    let value: Value = super::super::http::read_json(response).await?;
    Ok(normalize(&value, max_results))
}

pub(super) fn categories(value: Option<&Value>) -> Result<Vec<&'static str>> {
    let mut normalized = Vec::new();
    for item in value.and_then(Value::as_array).into_iter().flatten() {
        let category = item.as_str().ok_or(Error::InvalidArguments)?.trim();
        if category.is_empty() {
            continue;
        }
        let mapped = match category.to_ascii_lowercase().as_str() {
            "web" | "general" => "general",
            "news" => "news",
            "images" => "images",
            _ => return Err(Error::InvalidArguments),
        };
        if !normalized.contains(&mapped) {
            normalized.push(mapped);
        }
    }
    Ok(normalized)
}

fn clipped(text: &str, max: usize) -> String {
    text.trim().chars().take(max).collect()
}

/// An image result's direct image URL: `img_src`, made absolute when the
/// instance returns it scheme-relative. Anything that is not then an
/// `http(s)` URL (a relative path, a `data:` URI) is dropped.
fn image_url(item: &Value) -> Option<String> {
    let src = item.get("img_src")?.as_str()?.trim();
    let src = if src.starts_with("//") {
        format!("https:{src}")
    } else {
        src.to_owned()
    };
    (src.starts_with("https://") || src.starts_with("http://")).then_some(src)
}

fn normalize(value: &Value, max_results: usize) -> ExecuteToolResponse {
    let mut results = Vec::new();
    let mut assets = Vec::new();
    let mut citations = Vec::new();
    let mut sources = Vec::new();
    for item in value
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let url = clipped(item.get("url").and_then(Value::as_str).unwrap_or(""), 2048);
        if url.is_empty() {
            continue;
        }
        let title = clipped(item.get("title").and_then(Value::as_str).unwrap_or(""), 300);
        let title = if title.is_empty() { url.clone() } else { title };
        let snippet = ["content", "snippet"]
            .into_iter()
            .filter_map(|key| item.get(key).and_then(Value::as_str))
            .find(|text| !text.trim().is_empty())
            .map(|text| clipped(text, 1200));
        let source = item
            .get("engine")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                item.get("engines")
                    .and_then(Value::as_array)
                    .and_then(|items| {
                        items
                            .iter()
                            .filter_map(Value::as_str)
                            .find(|s| !s.trim().is_empty())
                    })
            })
            .unwrap_or("searxng");
        sources.push(clipped(source, 300));
        if citations.len() < 40
            && !citations
                .iter()
                .any(|citation: &Citation| citation.url == url)
        {
            citations.push(Citation {
                url: url.clone(),
                title: Some(title.clone()),
            });
        }
        if let Some(image_url) = image_url(item)
            && let Some(asset) = super::image_asset(&url, &image_url, Some(&title))
        {
            assets.push(asset);
        }
        results.push(SearchResult {
            title,
            url,
            snippet,
            published: None,
        });
        if results.len() >= max_results {
            break;
        }
    }
    let status = if results.is_empty() {
        SearchStatus::Empty
    } else {
        SearchStatus::Ok
    };
    ExecuteToolResponse {
        provider: "searxng".into(),
        results,
        citations,
        answer: None,
        status,
        provider_data: Some(if assets.is_empty() {
            json!({"sources":sources})
        } else {
            json!({"sources":sources, IMAGE_ASSETS:assets})
        }),
        role: None,
        fallback_from: Vec::new(),
    }
}
