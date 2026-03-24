use std::time::{Duration, Instant};

use jasmine_core::{fetch_og_metadata, CoreError, OgMetadata};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct TestResponse {
    status_line: &'static str,
    content_type: Option<&'static str>,
    body: String,
    delay_before_response: Option<Duration>,
}

async fn spawn_test_server(response: TestResponse) -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind OG test server");
    let address = listener.local_addr().expect("test server local address");

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept OG test connection");
        read_request(&mut stream).await;

        if let Some(delay) = response.delay_before_response {
            tokio::time::sleep(delay).await;
        }

        let mut headers = vec![
            format!("Content-Length: {}", response.body.len()),
            "Connection: close".to_string(),
        ];

        if let Some(content_type) = response.content_type {
            headers.push(format!("Content-Type: {content_type}"));
        }

        let raw_response = format!(
            "HTTP/1.1 {}\r\n{}\r\n\r\n{}",
            response.status_line,
            headers.join("\r\n"),
            response.body
        );

        let _ = stream.write_all(raw_response.as_bytes()).await;
        let _ = stream.shutdown().await;
    });

    format!("http://{address}/")
}

async fn read_request(stream: &mut tokio::net::TcpStream) {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];

    loop {
        let read = match stream.read(&mut buffer).await {
            Ok(read) => read,
            Err(_) => return,
        };

        if read == 0 {
            return;
        }

        request.extend_from_slice(&buffer[..read]);

        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn og_fetches_full_metadata_from_html() {
    let url = spawn_test_server(TestResponse {
        status_line: "200 OK",
        content_type: Some("text/html; charset=utf-8"),
        body: r#"
            <html>
                <head>
                    <meta property="og:title" content="Jasmine OG title" />
                    <meta property="og:description" content="A link preview description." />
                    <meta property="og:image" content="https://cdn.example.com/preview.png" />
                    <meta property="og:site_name" content="Example Site" />
                </head>
                <body>hello</body>
            </html>
        "#
        .to_string(),
        delay_before_response: None,
    })
    .await;

    let metadata = fetch_og_metadata(&url)
        .await
        .expect("fetch full OG metadata");

    assert_eq!(
        metadata,
        OgMetadata {
            url,
            title: Some("Jasmine OG title".to_string()),
            description: Some("A link preview description.".to_string()),
            image_url: Some("https://cdn.example.com/preview.png".to_string()),
            site_name: Some("Example Site".to_string()),
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn og_fetches_partial_metadata_when_some_tags_are_missing() {
    let url = spawn_test_server(TestResponse {
        status_line: "200 OK",
        content_type: Some("text/html"),
        body: r#"
            <html>
                <head>
                    <meta property="og:title" content="Only title" />
                    <meta property="og:image" content="https://cdn.example.com/only-title.png" />
                </head>
            </html>
        "#
        .to_string(),
        delay_before_response: None,
    })
    .await;

    let metadata = fetch_og_metadata(&url)
        .await
        .expect("fetch partial OG metadata");

    assert_eq!(metadata.url, url);
    assert_eq!(metadata.title.as_deref(), Some("Only title"));
    assert_eq!(
        metadata.image_url.as_deref(),
        Some("https://cdn.example.com/only-title.png")
    );
    assert_eq!(metadata.description, None);
    assert_eq!(metadata.site_name, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn og_returns_empty_metadata_when_tags_are_missing() {
    let url = spawn_test_server(TestResponse {
        status_line: "200 OK",
        content_type: Some("text/html"),
        body: r#"
            <html>
                <head><title>No OpenGraph here</title></head>
                <body><p>plain page</p></body>
            </html>
        "#
        .to_string(),
        delay_before_response: None,
    })
    .await;

    let metadata = fetch_og_metadata(&url)
        .await
        .expect("fetch page without OG metadata");

    assert_eq!(metadata, OgMetadata::empty(url));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn og_returns_empty_metadata_when_request_times_out() {
    let url = spawn_test_server(TestResponse {
        status_line: "200 OK",
        content_type: Some("text/html"),
        body: r#"
            <html>
                <head><meta property="og:title" content="Too slow" /></head>
            </html>
        "#
        .to_string(),
        delay_before_response: Some(Duration::from_secs(10)),
    })
    .await;

    let started_at = Instant::now();
    let metadata = fetch_og_metadata(&url)
        .await
        .expect("timeout should degrade to empty metadata");

    assert!(
        started_at.elapsed() < Duration::from_secs(6),
        "timeout should resolve within the 5 second request timeout buffer"
    );
    assert_eq!(metadata, OgMetadata::empty(url));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn og_returns_error_for_non_success_responses() {
    let url = spawn_test_server(TestResponse {
        status_line: "404 Not Found",
        content_type: Some("text/html"),
        body: "missing".to_string(),
        delay_before_response: None,
    })
    .await;

    let error = fetch_og_metadata(&url)
        .await
        .expect_err("non-success responses should return an error");

    assert!(matches!(error, CoreError::Persistence(_)));
    assert!(error.to_string().contains("404 Not Found"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn og_invalid_non_html_content_type_returns_empty_metadata() {
    let url = spawn_test_server(TestResponse {
        status_line: "200 OK",
        content_type: Some("application/pdf"),
        body: "%PDF-1.7 fake".to_string(),
        delay_before_response: None,
    })
    .await;

    let metadata = fetch_og_metadata(&url)
        .await
        .expect("non-html content should degrade gracefully");

    assert_eq!(metadata, OgMetadata::empty(url));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn og_invalid_malformed_html_does_not_panic_and_best_effort_parses() {
    let url = spawn_test_server(TestResponse {
        status_line: "200 OK",
        content_type: Some("text/html"),
        body: "<html><head><meta property=\"og:title\" content=\"Broken title\"><meta property=\"og:description\" content=\"Broken description\"".to_string(),
        delay_before_response: None,
    })
    .await;

    let metadata = fetch_og_metadata(&url)
        .await
        .expect("malformed html should not fail");

    assert_eq!(metadata.title.as_deref(), Some("Broken title"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn og_invalid_large_body_truncates_before_late_og_tags() {
    let filler = "a".repeat(520 * 1024);
    let body = format!(
        "<html><head>{filler}<meta property=\"og:title\" content=\"Too late\" /></head><body></body></html>"
    );
    let url = spawn_test_server(TestResponse {
        status_line: "200 OK",
        content_type: Some("text/html"),
        body,
        delay_before_response: None,
    })
    .await;

    let metadata = fetch_og_metadata(&url)
        .await
        .expect("large body should truncate safely");

    assert_eq!(metadata, OgMetadata::empty(url));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn og_invalid_malformed_url_returns_empty_metadata() {
    let metadata = fetch_og_metadata("not a url")
        .await
        .expect("malformed url should degrade gracefully");

    assert_eq!(metadata, OgMetadata::empty("not a url"));
}
