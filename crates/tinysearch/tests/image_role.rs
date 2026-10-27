//! The Images role end to end through the built-in providers: a host that
//! offers `web_image_search_tool` gets each image's direct URL and its source
//! page, from Brave first and from `SearXNG` when Brave cannot answer.
use serde_json::{Value, json};
use tinysearch::{
    ExecuteToolRequest, ProviderConfig, Role, SearchConfig, SearchService, SearchStatus,
    role_tool_name,
};
use tinysearch_bus::{IMAGE_ASSETS, render::render_text};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// Brave with a key and `SearXNG`, both served by `server`.
fn brave_and_searxng(server: &MockServer) -> SearchConfig {
    let mut config = SearchConfig::default();
    config.providers.insert(
        "brave".into(),
        ProviderConfig {
            credential: Some("brave-key".into()),
            base_url: Some(server.uri()),
            ..ProviderConfig::default()
        },
    );
    config.providers.insert(
        "searxng".into(),
        ProviderConfig {
            base_url: Some(format!("{}/searx", server.uri())),
            ..ProviderConfig::default()
        },
    );
    config
}

fn image_search(arguments: Value) -> ExecuteToolRequest {
    ExecuteToolRequest {
        name: role_tool_name(Role::Images).into(),
        arguments,
    }
}

#[tokio::test]
async fn brave_answers_the_image_role_with_image_and_page_urls() -> TestResult<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/images/search"))
        .and(query_param("q", "red panda"))
        .and(query_param("count", "2"))
        .and(query_param("country", "gb"))
        .and(header("x-subscription-token", "brave-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results":[
            {"url":"https://zoo.test/panda","title":"Red panda","properties":{"url":"https://img.test/panda.jpg"}},
            {"url":"https://zoo.test/cub","title":"Cub","properties":{"url":"https://img.test/cub.jpg"}}
        ]})))
        .expect(1)
        .mount(&server)
        .await;
    let service = SearchService::with_builtins(brave_and_searxng(&server));
    let tool = service
        .list_tools()
        .tools
        .into_iter()
        .find(|tool| tool.name == "web_image_search_tool")
        .ok_or("the image role is offered")?;
    assert_eq!(
        tool.parameters["properties"]["provider"]["enum"],
        json!(["brave", "searxng"])
    );

    let response = service
        .execute_tool(image_search(
            json!({"query":"red panda","max_results":2,"country":"gb"}),
        ))
        .await?;

    assert_eq!(response.provider, "brave");
    assert_eq!(response.role, Some(Role::Images));
    assert_eq!(response.status, SearchStatus::Ok);
    assert_eq!(response.results[1].url, "https://zoo.test/cub");
    assert_eq!(response.results[1].title, "Cub");
    let assets = &response.provider_data.as_ref().ok_or("provider data")?[IMAGE_ASSETS];
    assert_eq!(
        assets[0],
        json!({"source_url":"https://zoo.test/panda","image_url":"https://img.test/panda.jpg","title":"Red panda"})
    );
    let text = render_text(&response, "red panda", 5);
    assert!(text.starts_with("Image results for: red panda (via Brave)"));
    assert!(text.contains("   https://zoo.test/cub\n   Image: https://img.test/cub.jpg"));
    Ok(())
}

#[tokio::test]
async fn a_rate_limited_brave_falls_back_to_searxng_images() -> TestResult<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/images/search"))
        .respond_with(ResponseTemplate::new(429))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/searx/search"))
        .and(query_param("q", "red panda"))
        .and(query_param("categories", "images"))
        .and(query_param("format", "json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results":[
            {"url":"https://zoo.test/panda","title":"Red panda","img_src":"//img.test/panda.jpg","engine":"bing images"},
            {"url":"https://zoo.test/text","title":"No picture"}
        ]})))
        .expect(1)
        .mount(&server)
        .await;
    let service = SearchService::with_builtins(brave_and_searxng(&server));

    let response = service
        .execute_tool(image_search(json!({"query":"red panda","count":5})))
        .await?;

    assert_eq!(response.provider, "searxng");
    assert_eq!(response.fallback_from, ["brave"]);
    assert_eq!(response.results.len(), 1, "only results with an image");
    assert_eq!(response.results[0].url, "https://zoo.test/panda");
    let data = response.provider_data.as_ref().ok_or("provider data")?;
    assert_eq!(
        data[IMAGE_ASSETS][0]["image_url"],
        "https://img.test/panda.jpg"
    );
    assert!(
        render_text(&response, "red panda", 5).contains("Image: https://img.test/panda.jpg")
    );
    Ok(())
}
