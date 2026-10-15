use super::*;
use crate::ProviderConfig;
use reqwest::Client;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

type TestResult<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Serves one canned reply and returns the raw request it received.
async fn mock(
    status: u16,
    body: Value,
) -> TestResult<(String, tokio::task::JoinHandle<TestResult<String>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let mut data = vec![0; 8192];
        let mut used = 0;
        while !data[..used].windows(4).any(|w| w == b"\r\n\r\n") {
            let read = stream.read(&mut data[used..]).await?;
            if read == 0 {
                break;
            }
            used += read;
        }
        let body = body.to_string();
        let reply = format!(
            "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(reply.as_bytes()).await?;
        Ok(String::from_utf8_lossy(&data[..used]).into_owned())
    });
    Ok((url, task))
}

fn request(name: &str, arguments: Value) -> ExecuteToolRequest {
    ExecuteToolRequest {
        name: name.into(),
        arguments,
    }
}

fn config(base_url: String) -> ProviderConfig {
    ProviderConfig {
        base_url: Some(base_url),
        credential: Some("s-secret".into()),
        ..ProviderConfig::default()
    }
}

#[tokio::test]
async fn search_sends_the_key_header_and_maps_organic_results() -> TestResult<()> {
    let (url, server) = mock(
        200,
        json!({"results":[
            {"title":"Rust","link":"https://rust.test","description":"Systems language","position":1},
            {"title":"No link","description":"dropped"},
            {"title":"Book","link":"https://book.test","description":"The book","position":2},
            {"title":"Extra","link":"https://extra.test","description":"Past the limit","position":3}
        ],"ads":[{"link":"https://ad.test"}]}),
    )
    .await?;
    let result = super::super::run(
        &Client::new(),
        "serply",
        &config(url),
        &request(
            "serply_search",
            json!({"query":"rust","max_results":2,"country":"de"}),
        ),
    )
    .await?;
    let sent = server.await??;
    assert!(sent.starts_with("GET /search?q=rust&num=2&gl=de "));
    assert!(sent.to_ascii_lowercase().contains("x-api-key: s-secret"));
    assert_eq!(result.provider, "serply");
    let urls: Vec<&str> = result.results.iter().map(|r| r.url.as_str()).collect();
    assert_eq!(urls, ["https://rust.test", "https://book.test"]);
    assert_eq!(result.results[0].title, "Rust");
    assert_eq!(
        result.results[0].snippet.as_deref(),
        Some("Systems language")
    );
    Ok(())
}

#[tokio::test]
async fn rejected_key_is_unauthorized() -> TestResult<()> {
    let (url, server) = mock(401, json!({"detail":"Invalid API key"})).await?;
    let error = super::super::run(
        &Client::new(),
        "serply",
        &config(url),
        &request("serply_search", json!({"query":"rust"})),
    )
    .await
    .err()
    .ok_or("expected provider error")?;
    server.await??;
    assert_eq!(error, Error::Unauthorized(401));
    Ok(())
}

#[test]
fn page_size_is_capped_and_invalid_requests_are_rejected() -> TestResult<()> {
    let prepared = prepare(
        &request("serply_search", json!({"query":"rust","max_results":20})),
        "s-secret",
    )?;
    assert_eq!(prepared.4, [("q", "rust".into()), ("num", "10".into())]);
    assert_eq!(
        prepare(&request("serply_search", json!({"query":" "})), "s-secret")
            .err()
            .ok_or("expected provider error")?,
        Error::InvalidArguments
    );
    assert_eq!(
        prepare(&request("serply_news", json!({"query":"rust"})), "s-secret")
            .err()
            .ok_or("expected provider error")?,
        Error::UnavailableTool("serply_news".into())
    );
    Ok(())
}
