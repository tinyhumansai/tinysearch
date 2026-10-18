//! Firecrawl search and page scrape, with the caller's own API key.
//!
//! `firecrawl_search` posts to `/v2/search` and `firecrawl_scrape` posts one
//! `/v2/scrape` per URL for markdown. Every request names the calling
//! integration in the `integration` body field; it carries no user or host
//! identifier. The key is sent as a bearer token.
use super::{configured_timeout, direct_url, normalize, required_string, urls};
use crate::{Error, ExecuteToolRequest, ExecuteToolResponse, ProviderConfig, Result};
use reqwest::Client;
use serde_json::{Value, json};
use std::time::Duration;

const BASE: &str = "https://api.firecrawl.dev";
/// Attribution tag Firecrawl records for requests made through this module.
const INTEGRATION: &str = "_tinysearch";
/// Most URLs one `firecrawl_scrape` call reads.
const MAX_SCRAPE_URLS: usize = 10;

pub(super) async fn run(
    client: &Client,
    config: &ProviderConfig,
    key: &str,
    request: &ExecuteToolRequest,
) -> Result<ExecuteToolResponse> {
    let key = key.trim();
    let value = match request.name.as_str() {
        "firecrawl_search" => search(client, config, key, &request.arguments).await?,
        "firecrawl_scrape" => scrape(client, config, key, &request.arguments).await?,
        _ => return Err(Error::UnavailableTool(request.name.clone())),
    };
    Ok(normalize("firecrawl", &request.name, &value))
}

async fn post(
    client: &Client,
    config: &ProviderConfig,
    key: &str,
    path: &str,
    body: &Value,
    timeout: Duration,
) -> Result<Value> {
    let response = client
        .post(direct_url(config.base_url.as_deref(), BASE, path)?)
        .timeout(configured_timeout(config, timeout))
        .header(reqwest::header::ACCEPT, "application/json")
        .bearer_auth(key)
        .json(body)
        .send()
        .await
        .map_err(super::super::http::transport_error)?;
    super::super::http::read_json(response).await
}

/// Domain filters: 1-20 non-blank strings, or absent.
fn domains(args: &Value, field: &str) -> Result<Option<Value>> {
    let Some(value) = args.get(field) else {
        return Ok(None);
    };
    let items = value.as_array().ok_or(Error::InvalidArguments)?;
    if items.is_empty()
        || items.len() > 20
        || items
            .iter()
            .any(|v| v.as_str().is_none_or(|s| s.trim().is_empty()))
    {
        return Err(Error::InvalidArguments);
    }
    Ok(Some(value.clone()))
}

async fn search(
    client: &Client,
    config: &ProviderConfig,
    key: &str,
    args: &Value,
) -> Result<Value> {
    let mut body = json!({
        "query": required_string(args, "query")?,
        "limit": args
            .get("max_results")
            .and_then(Value::as_u64)
            .or(config.max_results)
            .unwrap_or(5)
            .clamp(1, 20),
        "integration": INTEGRATION,
    });
    if let Some(range) = args.get("time_range") {
        body["tbs"] = json!(match range.as_str() {
            Some("day") => "qdr:d",
            Some("week") => "qdr:w",
            Some("month") => "qdr:m",
            Some("year") => "qdr:y",
            _ => return Err(Error::InvalidArguments),
        });
    }
    if let Some(country) = args.get("country") {
        body["country"] = json!(
            country
                .as_str()
                .filter(|c| !c.trim().is_empty())
                .ok_or(Error::InvalidArguments)?
        );
    }
    for (field, target) in [
        ("include_domains", "includeDomains"),
        ("exclude_domains", "excludeDomains"),
    ] {
        if let Some(list) = domains(args, field)? {
            body[target] = list;
        }
    }
    let value = post(
        client,
        config,
        key,
        "/v2/search",
        &body,
        Duration::from_secs(35),
    )
    .await?;
    Ok(search_results(&value))
}

/// Maps Firecrawl's `data.web` hits onto the fields normalization reads.
fn search_results(value: &Value) -> Value {
    let results: Vec<Value> = value
        .pointer("/data/web")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|item| {
            let text = |field: &str| {
                item.get(field)
                    .and_then(Value::as_str)
                    .filter(|text| !text.trim().is_empty())
            };
            json!({
                "url": text("url"),
                "title": text("title"),
                "snippet": text("description"),
            })
        })
        .collect();
    json!({"results": results})
}

async fn scrape(
    client: &Client,
    config: &ProviderConfig,
    key: &str,
    args: &Value,
) -> Result<Value> {
    let urls = urls(args)?;
    let urls: Vec<&str> = urls
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if urls.len() > MAX_SCRAPE_URLS {
        return Err(Error::InvalidArguments);
    }
    let mut results = Vec::new();
    let mut first_error = None;
    for url in &urls {
        let body = json!({"url": url, "formats": ["markdown"], "integration": INTEGRATION});
        let page = post(
            client,
            config,
            key,
            "/v2/scrape",
            &body,
            Duration::from_secs(65),
        )
        .await
        .and_then(|value| page_result(url, &value));
        match page {
            Ok(page) => results.push(page),
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    // A partial failure still returns the pages that were read; a total one
    // keeps the first error's classification so the role can fall back.
    if results.is_empty()
        && let Some(error) = first_error
    {
        return Err(error);
    }
    let failed = urls.len() - results.len();
    Ok(json!({"results": results, "failed_count": failed}))
}

/// One scraped page. A response without markdown counts as a failed page.
fn page_result(url: &str, value: &Value) -> Result<Value> {
    let data = value.get("data").filter(|_| value["success"] != false);
    let markdown = data
        .and_then(|data| data.get("markdown"))
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| Error::Provider("Firecrawl returned no page content".into()))?;
    let metadata = |field: &str| {
        data.and_then(|data| data.pointer(&format!("/metadata/{field}")))
            .and_then(Value::as_str)
    };
    Ok(json!({
        "url": metadata("sourceURL").unwrap_or(url),
        "title": metadata("title").map_or("", str::trim),
        "snippet": markdown,
    }))
}

#[cfg(test)]
#[path = "firecrawl/firecrawl_tests.rs"]
mod test;
