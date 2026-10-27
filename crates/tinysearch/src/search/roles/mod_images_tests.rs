//! Images role dispatch: argument translation, fallback, and alignment of
//! results with the images each provider found.
use super::*;
use crate::{BackendConfig, ProviderConfig, ProviderFuture, SearchProvider};
use std::sync::{Arc, Mutex};
use tinysearch_bus::{SearchConfig, names::tools};

type Calls = Arc<Mutex<Vec<ExecuteToolRequest>>>;

/// What a stand-in provider does with every call.
#[derive(Clone, Copy)]
enum Outcome {
    Answer(fn() -> ExecuteToolResponse),
    Fail(fn() -> Error),
}

/// Records every request and answers with a fixed outcome.
struct Images {
    calls: Calls,
    outcome: Outcome,
}

impl SearchProvider for Images {
    fn execute<'a>(
        &'a self,
        _config: &'a ProviderConfig,
        _backend: &'a BackendConfig,
        request: &'a ExecuteToolRequest,
    ) -> ProviderFuture<'a> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(request.clone());
        }
        let outcome = match self.outcome {
            Outcome::Answer(response) => Ok(response()),
            Outcome::Fail(error) => Err(error()),
        };
        Box::pin(async move { outcome })
    }
}

fn page(title: &str, url: &str) -> SearchResult {
    SearchResult {
        title: title.into(),
        url: url.into(),
        snippet: Some("snippet".into()),
        published: None,
    }
}

/// Two images from one page, plus a page result with no image.
fn found() -> ExecuteToolResponse {
    ExecuteToolResponse {
        provider: "ignored".into(),
        results: vec![
            page("Panda", "https://zoo.example/panda"),
            page("Text only", "https://zoo.example/text"),
        ],
        citations: vec![],
        answer: None,
        status: SearchStatus::Ok,
        provider_data: Some(json!({"sources":["x"], IMAGE_ASSETS:[
            {"source_url":"https://zoo.example/panda","image_url":"https://img.example/1.jpg","title":"Panda"},
            {"source_url":"https://zoo.example/panda","image_url":"https://img.example/2.jpg"},
            {"image_url":"https://img.example/orphan.jpg"}
        ]})),
        role: None,
        fallback_from: vec![],
    }
}

/// Page results only, no images.
fn pages_only() -> ExecuteToolResponse {
    let mut response = found();
    response.provider_data = None;
    response
}

fn throttled() -> Error {
    Error::RateLimited
}

fn rejected() -> Error {
    Error::RejectedArguments("provider returned HTTP 400".into())
}

struct Fixture {
    service: SearchService,
    brave: Calls,
    searxng: Calls,
}

fn fixture(brave: Outcome, searxng: Outcome) -> Fixture {
    let mut config = SearchConfig::default();
    config.providers.insert(
        "brave".into(),
        ProviderConfig {
            credential: Some("key".into()),
            ..ProviderConfig::default()
        },
    );
    config.providers.insert(
        "searxng".into(),
        ProviderConfig {
            base_url: Some("https://searx.example".into()),
            ..ProviderConfig::default()
        },
    );
    let (brave_calls, searxng_calls) = (Calls::default(), Calls::default());
    let providers: BTreeMap<String, Arc<dyn SearchProvider>> = [
        (
            "brave".to_owned(),
            Arc::new(Images {
                calls: brave_calls.clone(),
                outcome: brave,
            }) as Arc<dyn SearchProvider>,
        ),
        (
            "searxng".to_owned(),
            Arc::new(Images {
                calls: searxng_calls.clone(),
                outcome: searxng,
            }) as Arc<dyn SearchProvider>,
        ),
    ]
    .into();
    Fixture {
        service: SearchService::with_providers(config, providers),
        brave: brave_calls,
        searxng: searxng_calls,
    }
}

fn sent(calls: &Calls) -> Vec<ExecuteToolRequest> {
    calls.lock().map(|calls| calls.clone()).unwrap_or_default()
}

fn images(arguments: Value) -> ExecuteToolRequest {
    ExecuteToolRequest {
        name: tools::WEB_IMAGE_SEARCH.into(),
        arguments,
    }
}

#[tokio::test]
async fn brave_serves_images_and_results_follow_the_image_list() -> Result<()> {
    let fixture = fixture(Outcome::Answer(found), Outcome::Answer(found));
    let response = fixture
        .service
        .execute_tool(images(
            json!({"query":"red panda","max_results":4,"country":"gb"}),
        ))
        .await?;
    assert_eq!(response.provider, "brave");
    assert_eq!(response.role, Some(Role::Images));
    assert_eq!(response.status, SearchStatus::Ok);
    let brave = sent(&fixture.brave);
    assert_eq!(brave.len(), 1);
    assert_eq!(brave[0].name, "brave_image_search");
    assert_eq!(
        brave[0].arguments,
        json!({"query":"red panda","count":4,"country":"gb"})
    );
    assert_eq!(sent(&fixture.searxng).len(), 0);
    // One result per image, in image order; the orphan asset without a
    // source page and the page without an image are both dropped.
    assert_eq!(
        response
            .results
            .iter()
            .map(|result| (result.title.as_str(), result.url.as_str()))
            .collect::<Vec<_>>(),
        [
            ("Panda", "https://zoo.example/panda"),
            ("", "https://zoo.example/panda")
        ]
    );
    assert!(
        response
            .results
            .iter()
            .all(|result| result.snippet.is_none())
    );
    let data = response.provider_data.ok_or(Error::InvalidArguments)?;
    assert_eq!(
        data[IMAGE_ASSETS][1]["image_url"],
        "https://img.example/2.jpg"
    );
    assert_eq!(data["sources"], json!(["x"]));
    Ok(())
}

#[tokio::test]
async fn images_fall_back_to_searxng_with_its_images_category() -> Result<()> {
    let fixture = fixture(Outcome::Fail(throttled), Outcome::Answer(found));
    let response = fixture
        .service
        .execute_tool(images(
            json!({"query":"red panda","count":3,"country":"gb"}),
        ))
        .await?;
    assert_eq!(response.provider, "searxng");
    assert_eq!(response.fallback_from, ["brave"]);
    let searxng = sent(&fixture.searxng);
    assert_eq!(searxng.len(), 1);
    assert_eq!(searxng[0].name, "searxng_search");
    // `count` is the alias of max_results; SearXNG has no country filter.
    assert_eq!(
        searxng[0].arguments,
        json!({"query":"red panda","categories":["images"],"max_results":3})
    );
    Ok(())
}

#[tokio::test]
async fn an_explicit_image_provider_is_not_fallen_back_from() {
    let fixture = fixture(Outcome::Fail(throttled), Outcome::Answer(found));
    let error = fixture
        .service
        .execute_tool(images(json!({"query":"red panda","provider":"brave"})))
        .await;
    assert!(matches!(error, Err(Error::RateLimited)));
    assert_eq!(sent(&fixture.searxng).len(), 0);
}

#[tokio::test]
async fn a_rejected_image_query_stops_the_call() {
    let fixture = fixture(Outcome::Fail(rejected), Outcome::Answer(found));
    let error = fixture
        .service
        .execute_tool(images(json!({"query":"red panda"})))
        .await;
    assert!(matches!(error, Err(Error::RejectedArguments(_))));
    assert_eq!(sent(&fixture.searxng).len(), 0);
}

#[tokio::test]
async fn a_response_without_images_is_empty() -> Result<()> {
    let fixture = fixture(Outcome::Answer(pages_only), Outcome::Answer(found));
    let response = fixture
        .service
        .execute_tool(images(json!({"query":"red panda"})))
        .await?;
    assert_eq!(response.provider, "brave");
    assert_eq!(response.status, SearchStatus::Empty);
    assert_eq!(response.results.len(), 0);
    // Without a count the provider's own default applies.
    assert_eq!(
        sent(&fixture.brave)[0].arguments,
        json!({"query":"red panda"})
    );
    Ok(())
}

#[tokio::test]
async fn image_arguments_are_validated_against_the_role_schema() {
    let fixture = fixture(Outcome::Answer(found), Outcome::Answer(found));
    for arguments in [
        json!({}),
        json!({"query":"red panda","max_results":21}),
        json!({"query":"red panda","country":"gbr"}),
        json!({"query":"red panda","provider":"exa"}),
    ] {
        let result = fixture.service.execute_tool(images(arguments)).await;
        assert!(matches!(result, Err(Error::InvalidArguments)), "{result:?}");
    }
    let unknown = fixture
        .service
        .execute_tool(images(json!({"query":"red panda","safesearch":"off"})))
        .await;
    assert!(matches!(unknown, Err(Error::UnsupportedArgument(_))));
    assert_eq!(sent(&fixture.brave).len(), 0);
}
