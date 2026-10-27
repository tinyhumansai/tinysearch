use super::*;
use crate::{Citation, SearchResult};

fn response(role: Option<Role>) -> ExecuteToolResponse {
    ExecuteToolResponse {
        provider: "exa".into(),
        results: vec![SearchResult {
            title: "Tokio select".into(),
            url: "https://tokio.rs/select".into(),
            snippet: Some("Waits on multiple futures".into()),
            published: Some("2026-09-01".into()),
        }],
        citations: Vec::new(),
        answer: None,
        status: SearchStatus::Ok,
        provider_data: None,
        role,
        fallback_from: Vec::new(),
    }
}

fn answer_fixture() -> ExecuteToolResponse {
    let mut answer = response(Some(Role::Answer));
    answer.provider = "gemini".into();
    answer.results.clear();
    answer.answer = Some("It returns the first future to complete.".into());
    answer.citations = vec![
        Citation {
            url: "https://docs.rs/tokio".into(),
            title: Some("tokio docs".into()),
        },
        Citation {
            url: "https://tokio.rs".into(),
            title: None,
        },
    ];
    answer.fallback_from = vec!["exa".into()];
    answer
}

#[test]
fn search_results_carry_the_via_marker() {
    let text = render_text(&response(Some(Role::Search)), "tokio select", 5);
    assert!(text.starts_with("Search results for: tokio select (via Exa)"));
    assert!(text.contains("1. Tokio select"));
}

#[test]
fn answers_render_text_citations_and_fallbacks() {
    let text = render_text(&answer_fixture(), "what does select do", 5);
    assert!(text.starts_with("Answer for: what does select do (via Gemini, after Exa)"));
    assert!(text.contains("It returns the first future"));
    assert!(text.contains("[1] tokio docs — https://docs.rs/tokio"));
    assert!(render_markdown(&answer_fixture(), "q", 5).contains("### Sources"));
}

#[test]
fn empty_results_still_attribute_the_provider() {
    let mut empty = response(None);
    empty.results.clear();
    empty.status = SearchStatus::Empty;
    assert_eq!(
        render_text(&empty, "nothing", 5),
        "No results found for: nothing (via Exa)"
    );
}

#[test]
fn in_progress_research_is_flagged() {
    let mut running = response(Some(Role::Answer));
    running.provider = "gemini_deep_research".into();
    running.results.clear();
    running.status = SearchStatus::InProgress;
    assert!(
        render_text(&running, "deep topic", 5)
            .starts_with("Research still running for: deep topic (via Gemini Deep Research)")
    );
}

#[test]
fn results_are_capped() {
    let mut many = response(None);
    many.results = (0..8)
        .map(|i| SearchResult {
            title: format!("r{i}"),
            url: format!("https://e.example/{i}"),
            snippet: None,
            published: None,
        })
        .collect();
    let text = render_text(&many, "q", 3);
    assert!(text.contains("3. r2"));
    assert!(!text.contains("4. r3"));
}

#[test]
fn long_snippets_are_truncated() {
    let mut long = response(None);
    long.results[0].snippet = Some("x".repeat(600));
    let text = render_text(&long, "q", 5);
    assert!(text.contains(&format!("{}…", "x".repeat(500))));
    assert!(!text.contains(&"x".repeat(501)));
}

#[test]
fn subject_reads_query_urls_or_objective() {
    assert_eq!(subject(&serde_json::json!({"query": " q "})), "q");
    assert_eq!(
        subject(&serde_json::json!({"urls": ["https://a", "https://b"]})),
        "https://a, https://b"
    );
    assert_eq!(subject(&serde_json::json!({"objective": " o "})), "o");
    assert_eq!(subject(&serde_json::json!({})), "");
}

fn image_fixture() -> ExecuteToolResponse {
    let page = |title: &str, url: &str| SearchResult {
        title: title.into(),
        url: url.into(),
        snippet: None,
        published: None,
    };
    let mut images = response(Some(Role::Images));
    images.provider = "brave".into();
    images.results = vec![
        page("Red panda", "https://zoo.example/panda"),
        page("Red panda, again", "https://zoo.example/panda"),
        page("No image", "https://zoo.example/text"),
    ];
    images.provider_data = Some(serde_json::json!({IMAGE_ASSETS:[
        {"source_url":"https://zoo.example/panda","image_url":"https://img.example/1.jpg"},
        {"source_url":"https://zoo.example/panda","image_url":"https://img.example/2.jpg"},
        {"source_url":"https://zoo.example/other","image_url":" "}
    ]}));
    images
}

#[test]
fn image_results_list_each_image_under_its_page() {
    let text = render_text(&image_fixture(), "red panda", 5);
    assert!(text.starts_with("Image results for: red panda (via Brave)"));
    assert!(text.contains(
        "1. Red panda\n   https://zoo.example/panda\n   Image: https://img.example/1.jpg"
    ));
    assert!(text.contains(
        "2. Red panda, again\n   https://zoo.example/panda\n   Image: https://img.example/2.jpg"
    ));
    assert!(text.contains("3. No image\n   https://zoo.example/text"));
    assert_eq!(text.matches("Image: ").count(), 2);

    let markdown = render_markdown(&image_fixture(), "red panda", 5);
    assert!(markdown.contains(
        "## [Red panda](https://zoo.example/panda)\n![Red panda](https://img.example/1.jpg)"
    ));
    assert!(markdown.contains("![Red panda, again](https://img.example/2.jpg)"));
}

#[test]
fn an_image_search_without_images_says_so() {
    let mut none = image_fixture();
    none.results.clear();
    none.provider_data = None;
    none.status = SearchStatus::Empty;
    assert_eq!(
        render_text(&none, "red panda", 5),
        "No images found for: red panda (via Brave)"
    );
}
