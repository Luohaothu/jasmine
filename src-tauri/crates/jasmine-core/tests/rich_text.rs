use jasmine_core::{extract_urls, parse_mentions, MentionRef, UrlRef};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct FixtureMention {
    user_id: String,
    display_name: String,
    #[serde(default)]
    start: Option<usize>,
    #[serde(default)]
    end: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct FixtureUrl {
    url: String,
    #[serde(default)]
    start: Option<usize>,
    #[serde(default)]
    end: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct Fixture {
    input: String,
    expected_mentions: Vec<FixtureMention>,
    expected_urls: Vec<FixtureUrl>,
}

fn load_fixtures() -> Vec<Fixture> {
    let raw = include_str!("fixtures/rich_text_samples.json");
    serde_json::from_str(raw).expect("load rich text fixtures")
}

fn assert_mentions(actual: &[MentionRef], expected: &[FixtureMention]) {
    assert_eq!(actual.len(), expected.len());

    for (index, expected_entry) in expected.iter().enumerate() {
        let actual_entry = &actual[index];
        assert_eq!(actual_entry.user_id, expected_entry.user_id);
        assert_eq!(actual_entry.display_name, expected_entry.display_name);

        if let Some(start) = expected_entry.start {
            assert_eq!(actual_entry.start, start);
        }
        if let Some(end) = expected_entry.end {
            assert_eq!(actual_entry.end, end);
        }
    }
}

fn assert_urls(actual: &[UrlRef], expected: &[FixtureUrl]) {
    assert_eq!(actual.len(), expected.len());

    for (index, expected_entry) in expected.iter().enumerate() {
        let actual_entry = &actual[index];
        assert_eq!(actual_entry.url, expected_entry.url);

        if let Some(start) = expected_entry.start {
            assert_eq!(actual_entry.start, start);
        }
        if let Some(end) = expected_entry.end {
            assert_eq!(actual_entry.end, end);
        }
    }
}

#[test]
fn mention_fixture_parsing() {
    let fixtures = load_fixtures();

    for sample in fixtures {
        let actual = parse_mentions(&sample.input);
        assert_mentions(&actual, &sample.expected_mentions);
    }
}

#[test]
fn url_extract_fixture_parsing() {
    let fixtures = load_fixtures();

    for sample in fixtures {
        let actual = extract_urls(&sample.input);
        assert_urls(&actual, &sample.expected_urls);
    }
}

#[test]
fn mention_parser_keeps_mention_offsets_for_nested_markdown() {
    let result = parse_mentions("**@[Alice](user:uuid-1)** and @[Bob](user:uuid-2)");

    assert_eq!(
        result,
        vec![
            MentionRef {
                user_id: "uuid-1".to_string(),
                display_name: "Alice".to_string(),
                start: 2,
                end: 23,
            },
            MentionRef {
                user_id: "uuid-2".to_string(),
                display_name: "Bob".to_string(),
                start: 30,
                end: 49,
            },
        ]
    );
}
