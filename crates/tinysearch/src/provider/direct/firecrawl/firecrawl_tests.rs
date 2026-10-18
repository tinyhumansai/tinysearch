use super::*;
use crate::{PresentationMode, SearchConfig, SearchService};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

type TestResult<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Serves one canned `(status, body)` reply per connection, in order, and
/// returns the raw requests it received.
async fn mock(
    replies: Vec<(u16, Value)>,
) -> TestResult<(String, tokio::task::JoinHandle<TestResult<Vec<String>>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, body) in replies {
            let (mut stream, _) = listener.accept().await?;
            let mut data = vec![0; 65536];
            let mut used = 0;
            loop {
                let read = stream.read(&mut data[used..]).await?;
                if read == 0 {
                    break;
                }
                used += read;
                if let Some(end) = data[..used].windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&data[..end + 4]).to_ascii_lowercase();
                    let length = head
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length: ")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if used >= end + 4 + length {
                        break;
                    }
                }
            }
            requests.push(String::from_utf8_lossy(&data[..used]).into_owned());
            let body = body.to_string();
            let reply = format!(
                "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).await?;
        }
        Ok(requests)
    });
    Ok((url, task))
}

fn request(name: &str, arguments: Value) -> ExecuteToolRequest {
    ExecuteToolRequest {
        name: name.into(),
        arguments,
    }
}

fn config(base_url: &str) -> ProviderConfig {
    ProviderConfig {
        base_url: Some(base_url.into()),
        credential: Some("fc-test-key".into()),
        ..Default::default()
    }
}

fn header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

fn body(request: &str) -> TestResult<Value> {
    let (_, body) = request.split_once("\r\n\r\n").ok_or("missing body")?;
    Ok(serde_json::from_str(body)?)
}

#[tokio::test]
async fn search_posts_the_tagged_body_with_the_bearer_key() -> TestResult<()> {
    let (url, server) = mock(vec![(
        200,
        json!({"success":true,"data":{"web":[
            {"url":"https://rust.test","title":"Rust","description":"A language","position":1},
            {"url":"https://two.test","title":" ","description":" "},
            {"title":"No URL","description":"dropped"}
        ]},"creditsUsed":2,"id":"abc"}),
    )])
    .await?;
    let response = run(
        &Client::new(),
        &config(&url),
        " fc-test-key ",
        &request("firecrawl_search", json!({"query":"rust"})),
    )
    .await?;
    let sent = server.await??;
    let sent = sent.first().ok_or("no request")?;
    assert!(sent.starts_with("POST /v2/search "));
    assert_eq!(header(sent, "authorization"), Some("Bearer fc-test-key"));
    assert_eq!(
        body(sent)?,
        json!({"query":"rust","limit":5,"integration":"_tinysearch"})
    );
    assert_eq!(response.provider, "firecrawl");
    assert_eq!(response.results.len(), 2);
    assert_eq!(response.results[0].title, "Rust");
    assert_eq!(response.results[0].snippet.as_deref(), Some("A language"));
    assert_eq!(response.results[1].snippet, None);
    assert_eq!(response.citations.len(), 2);
    Ok(())
}

#[tokio::test]
async fn search_maps_count_filters_and_ignores_unknown_arguments() -> TestResult<()> {
    let (url, server) = mock(vec![(200, json!({"success":true,"data":{"web":[]}}))]).await?;
    let mut config = config(&url);
    config.max_results = Some(50);
    let response = run(
        &Client::new(),
        &config,
        "fc-test-key",
        &request(
            "firecrawl_search",
            json!({"query":"rust","time_range":"week","country":"US","include_domains":["rust-lang.org"],"exclude_domains":["spam.test"],"ignored":true}),
        ),
    )
    .await?;
    let sent = server.await??;
    let sent = sent.first().ok_or("no request")?;
    assert_eq!(
        body(sent)?,
        json!({"query":"rust","limit":20,"integration":"_tinysearch","tbs":"qdr:w","country":"US","includeDomains":["rust-lang.org"],"excludeDomains":["spam.test"]})
    );
    assert_eq!(response.status, crate::SearchStatus::Empty);
    Ok(())
}

#[tokio::test]
async fn every_time_range_maps_to_a_firecrawl_window() -> TestResult<()> {
    let (url, server) = mock(vec![
        (200, json!({"data":{"web":[]}})),
        (200, json!({"data":{"web":[]}})),
        (200, json!({"data":{"web":[]}})),
        (200, json!({"data":{"web":[]}})),
    ])
    .await?;
    for range in ["day", "week", "month", "year"] {
        run(
            &Client::new(),
            &config(&url),
            "fc-test-key",
            &request(
                "firecrawl_search",
                json!({"query":"rust","time_range":range}),
            ),
        )
        .await?;
    }
    let sent = server.await??;
    let windows: Vec<Value> = sent
        .iter()
        .map(|s| body(s).map(|b| b["tbs"].clone()))
        .collect::<TestResult<_>>()?;
    assert_eq!(windows, ["qdr:d", "qdr:w", "qdr:m", "qdr:y"]);
    Ok(())
}

#[tokio::test]
async fn scrape_reads_each_url_as_tagged_markdown() -> TestResult<()> {
    let (url, server) = mock(vec![
        (
            200,
            json!({"success":true,"data":{"markdown":"# A","metadata":{"title":"\n  A\n","sourceURL":"https://a.test/final","statusCode":200}}}),
        ),
        (
            200,
            json!({"success":true,"data":{"markdown":"# B","metadata":{}}}),
        ),
    ])
    .await?;
    let response = run(
        &Client::new(),
        &config(&url),
        "fc-test-key",
        &request(
            "firecrawl_scrape",
            json!({"urls":["https://a.test/","https://b.test/"]}),
        ),
    )
    .await?;
    let sent = server.await??;
    assert_eq!(sent.len(), 2);
    for (sent, page) in sent.iter().zip(["https://a.test/", "https://b.test/"]) {
        assert!(sent.starts_with("POST /v2/scrape "));
        assert_eq!(header(sent, "authorization"), Some("Bearer fc-test-key"));
        assert_eq!(
            body(sent)?,
            json!({"url":page,"formats":["markdown"],"integration":"_tinysearch"})
        );
    }
    assert_eq!(response.results.len(), 2);
    assert_eq!(response.results[0].url, "https://a.test/final");
    assert_eq!(response.results[0].title, "A");
    assert_eq!(response.results[0].snippet.as_deref(), Some("# A"));
    assert_eq!(response.results[1].url, "https://b.test/");
    assert_eq!(response.results[1].title, "");
    assert_eq!(
        response.provider_data.ok_or("missing provider data")?["failed_count"],
        0
    );
    Ok(())
}

#[tokio::test]
async fn scrape_returns_the_pages_it_read_and_counts_the_rest() -> TestResult<()> {
    let (url, server) = mock(vec![
        (503, json!({"error":"private body"})),
        (200, json!({"success":true,"data":{"markdown":" "}})),
        (200, json!({"success":true,"data":{"markdown":"# C"}})),
    ])
    .await?;
    let response = run(
        &Client::new(),
        &config(&url),
        "fc-test-key",
        &request(
            "firecrawl_scrape",
            json!({"urls":["https://a.test/","https://b.test/","https://c.test/"]}),
        ),
    )
    .await?;
    server.await??;
    assert_eq!(response.results.len(), 1);
    assert_eq!(response.results[0].url, "https://c.test/");
    assert_eq!(
        response.provider_data.ok_or("missing provider data")?["failed_count"],
        2
    );
    Ok(())
}

#[tokio::test]
async fn http_failures_keep_their_classification() -> TestResult<()> {
    for status in [401, 402, 429, 400, 500] {
        for (name, arguments) in [
            ("firecrawl_search", json!({"query":"rust"})),
            ("firecrawl_scrape", json!({"urls":["https://a.test/"]})),
        ] {
            let (url, server) = mock(vec![(
                status,
                json!({"success":false,"error":"secret detail"}),
            )])
            .await?;
            let error = run(
                &Client::new(),
                &config(&url),
                "fc-test-key",
                &request(name, arguments),
            )
            .await
            .err()
            .ok_or("expected an error")?;
            server.await??;
            let matched = match status {
                401 => matches!(error, Error::Unauthorized(401)),
                402 => matches!(error, Error::InsufficientBalance),
                429 => matches!(error, Error::RateLimited),
                400 => matches!(error, Error::RejectedArguments(_)),
                _ => matches!(error, Error::ProviderUnavailable(_)),
            };
            assert!(matched, "{name} {status}: {error:?}");
            assert!(!format!("{error:?}").contains("secret detail"));
        }
    }
    Ok(())
}

#[tokio::test]
async fn an_unreachable_server_is_unavailable() -> TestResult<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    drop(listener);
    let error = run(
        &Client::new(),
        &config(&url),
        "fc-test-key",
        &request("firecrawl_search", json!({"query":"rust"})),
    )
    .await
    .err()
    .ok_or("expected an error")?;
    assert!(matches!(error, Error::ProviderUnavailable(_)));
    Ok(())
}

#[tokio::test]
async fn a_slow_server_times_out_as_unavailable() -> TestResult<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let hold = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        tokio::time::sleep(Duration::from_secs(5)).await;
        drop(stream);
        Ok::<_, std::io::Error>(())
    });
    let mut config = config(&url);
    config.timeout_secs = Some(1);
    let error = run(
        &Client::new(),
        &config,
        "fc-test-key",
        &request("firecrawl_search", json!({"query":"rust"})),
    )
    .await
    .err()
    .ok_or("expected an error")?;
    hold.abort();
    assert!(matches!(error, Error::ProviderUnavailable(_)));
    Ok(())
}

#[tokio::test]
async fn an_unsuccessful_scrape_envelope_counts_as_a_failed_page() -> TestResult<()> {
    let (url, server) = mock(vec![(
        200,
        json!({"success":false,"data":{"markdown":"ignored"}}),
    )])
    .await?;
    let error = run(
        &Client::new(),
        &config(&url),
        "fc-test-key",
        &request("firecrawl_scrape", json!({"urls":["https://a.test/"]})),
    )
    .await
    .err()
    .ok_or("expected an error")?;
    server.await??;
    assert!(matches!(error, Error::Provider(_)));
    Ok(())
}

#[tokio::test]
async fn invalid_requests_fail_before_any_call() -> TestResult<()> {
    let config = config("http://127.0.0.1:9");
    let client = Client::new();
    let too_many: Vec<String> = (0..11).map(|i| format!("https://{i}.test/")).collect();
    for (name, arguments) in [
        ("firecrawl_search", json!({"query":" "})),
        (
            "firecrawl_search",
            json!({"query":"q","time_range":"decade"}),
        ),
        ("firecrawl_search", json!({"query":"q","country":" "})),
        (
            "firecrawl_search",
            json!({"query":"q","include_domains":[]}),
        ),
        (
            "firecrawl_search",
            json!({"query":"q","exclude_domains":"a.test"}),
        ),
        (
            "firecrawl_search",
            json!({"query":"q","include_domains":[" "]}),
        ),
        ("firecrawl_scrape", json!({"urls":[]})),
        ("firecrawl_scrape", json!({"urls":too_many})),
    ] {
        let result = run(&client, &config, "fc-test-key", &request(name, arguments)).await;
        assert!(matches!(result, Err(Error::InvalidArguments)), "{name}");
    }
    let unknown = run(
        &client,
        &config,
        "fc-test-key",
        &request("firecrawl_crawl", json!({})),
    )
    .await;
    assert!(matches!(unknown, Err(Error::UnavailableTool(name)) if name == "firecrawl_crawl"));
    let bad_base = run(
        &client,
        &ProviderConfig {
            base_url: Some("ftp://firecrawl.test".into()),
            ..Default::default()
        },
        "fc-test-key",
        &request("firecrawl_search", json!({"query":"rust"})),
    )
    .await;
    assert!(matches!(bad_base, Err(Error::Provider(_))));
    Ok(())
}

#[tokio::test]
async fn the_search_and_contents_roles_route_to_firecrawl() -> TestResult<()> {
    let (url, server) = mock(vec![
        (
            200,
            json!({"success":true,"data":{"web":[{"url":"https://rust.test","title":"Rust","description":"A language"}]}}),
        ),
        (
            200,
            json!({"success":true,"data":{"markdown":"# Page","metadata":{"title":"Page"}}}),
        ),
    ])
    .await?;
    let mut config = SearchConfig::default();
    config
        .providers
        .insert("firecrawl".into(), self::config(&url));
    let service = SearchService::with_builtins(config);
    let names: Vec<String> = service
        .list_tools()
        .tools
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    assert_eq!(names, ["web_search_tool", "web_contents_tool"]);
    let search = service
        .execute_tool(request(
            "web_search_tool",
            json!({"query":"rust","max_results":3}),
        ))
        .await?;
    assert_eq!(search.provider, "firecrawl");
    assert_eq!(search.results[0].url, "https://rust.test");
    let contents = service
        .execute_tool(request(
            "web_contents_tool",
            json!({"urls":["https://page.test/"],"query":"ignored"}),
        ))
        .await?;
    assert_eq!(contents.provider, "firecrawl");
    assert_eq!(contents.results[0].snippet.as_deref(), Some("# Page"));
    let sent = server.await??;
    assert_eq!(body(&sent[0])?["limit"], 3);
    assert_eq!(body(&sent[1])?["integration"], "_tinysearch");
    Ok(())
}

#[test]
fn firecrawl_is_listed_only_with_a_credential_on_the_direct_route() {
    let tool_names = |config: &SearchConfig| -> Vec<String> {
        SearchService::with_builtins(config.clone())
            .list_tools()
            .tools
            .into_iter()
            .map(|tool| tool.name)
            .collect()
    };
    let mut config = SearchConfig::default();
    config.presentation.mode = PresentationMode::AllTools;
    config.providers.insert(
        "firecrawl".into(),
        ProviderConfig {
            credential: Some(" ".into()),
            ..Default::default()
        },
    );
    assert_eq!(tool_names(&config).len(), 0);
    config.providers.insert(
        "firecrawl".into(),
        ProviderConfig {
            credential: Some("fc-test-key".into()),
            ..Default::default()
        },
    );
    assert_eq!(
        tool_names(&config),
        ["firecrawl_search", "firecrawl_scrape"]
    );
    config.providers.insert(
        "firecrawl".into(),
        ProviderConfig {
            credential: Some("fc-test-key".into()),
            route: crate::ProviderRoute::Backend,
            ..Default::default()
        },
    );
    assert_eq!(tool_names(&config).len(), 0);
}
