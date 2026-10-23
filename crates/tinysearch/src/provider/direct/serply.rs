//! The `serply` provider, reached directly with the user's own key.
//!
//! Search is `GET https://api.serply.io/v1/search?q=...&num=...` with an
//! `X-Api-Key` header (<https://serply.io/docs>), returning
//! `{results: [{title, link, description, position, ...}], ...}`.
use super::{Prepared, count, required_string};
use crate::{Error, ExecuteToolRequest, Result};
use reqwest::Method;
use serde_json::{Value, json};
use std::time::Duration;

/// Serply returns at most ten organic results per request.
const MAX_PAGE: u64 = 10;

pub(super) fn prepare(request: &ExecuteToolRequest, key: &str) -> Result<Prepared> {
    if request.name != "serply_search" {
        return Err(Error::UnavailableTool(request.name.clone()));
    }
    let args = &request.arguments;
    let mut params = vec![
        ("q", required_string(args, "query")?.to_owned()),
        ("num", count(args, "max_results").min(MAX_PAGE).to_string()),
    ];
    if let Some(country) = args.get("country").and_then(Value::as_str) {
        params.push(("gl", country.into()));
    }
    Ok((
        Method::GET,
        "https://api.serply.io/v1",
        "/search".into(),
        None,
        params,
        ("X-Api-Key", key.into()),
        Duration::from_secs(35),
    ))
}

/// Maps Serply's organic `results` (`link`, `description`) onto the shared
/// result fields. `num` is an upper bound that a full page can exceed, so the
/// list is trimmed to the requested count.
pub(super) fn unwrap(request: &ExecuteToolRequest, value: &Value) -> Value {
    let limit = usize::try_from(count(&request.arguments, "max_results")).unwrap_or(20);
    let results: Vec<Value> = value
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let link = item.get("link")?.as_str()?;
            Some(json!({
                "title":item.get("title"),
                "url":link,
                "description":item.get("description"),
            }))
        })
        .take(limit)
        .collect();
    json!({"results":results})
}

#[cfg(test)]
#[path = "serply/serply_tests.rs"]
mod test;
