#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionRef {
    pub user_id: String,
    pub display_name: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlRef {
    pub url: String,
    pub start: usize,
    pub end: usize,
}

const USER_MENTION_PREFIX: &str = "user:";

pub fn parse_mentions(content: &str) -> Vec<MentionRef> {
    let mut mentions = Vec::new();
    let mut cursor = 0;

    while let Some(match_offset) = content[cursor..].find("@[") {
        let at = cursor + match_offset;
        let after_open = &content[at + 2..];

        let link_open = match after_open.find("](") {
            Some(idx) => at + 2 + idx,
            None => {
                cursor = at + 2;
                continue;
            }
        };

        let link_start = link_open + 2;
        let after_link = &content[link_start..];

        let link_close = match after_link.find(')') {
            Some(idx) => link_start + idx,
            None => {
                cursor = at + 2;
                continue;
            }
        };

        let display_name = &content[at + 2..link_open];
        let user_id_raw = &content[link_start..link_close];

        if !display_name.is_empty()
            && user_id_raw.len() > USER_MENTION_PREFIX.len()
            && user_id_raw.starts_with(USER_MENTION_PREFIX)
        {
            mentions.push(MentionRef {
                user_id: user_id_raw[USER_MENTION_PREFIX.len()..].to_string(),
                display_name: display_name.to_string(),
                start: at,
                end: link_close + 1,
            });
        }

        cursor = link_close + 1;
    }

    mentions
}

pub fn extract_urls(content: &str) -> Vec<UrlRef> {
    let mut urls = Vec::new();
    let mut cursor = 0;

    while cursor < content.len() {
        let rest = &content[cursor..];
        let has_http = rest.starts_with("http://");
        let has_https = rest.starts_with("https://");

        if !has_http && !has_https {
            cursor += 1;
            continue;
        }

        let start = cursor;

        let mut end = start;
        while end < content.len() {
            let ch = content.as_bytes()[end] as char;
            if is_url_terminator(ch) {
                break;
            }
            end += 1;
        }

        let trimmed_end = trim_url_end(content, start, end);
        if trimmed_end > start {
            urls.push(UrlRef {
                url: content[start..trimmed_end].to_string(),
                start,
                end: trimmed_end,
            });
        }

        cursor = end + 1;
    }

    urls
}

fn is_url_terminator(ch: char) -> bool {
    ch.is_whitespace() || ch == ')' || ch == ']' || ch == '}' || ch == '>'
}

fn is_url_trailing_char(ch: char) -> bool {
    ch == '.'
        || ch == ','
        || ch == ';'
        || ch == ':'
        || ch == '!'
        || ch == '?'
        || ch == '"'
        || ch == '\''
}

fn trim_url_end(content: &str, start: usize, end: usize) -> usize {
    let mut trimmed_end = end;
    while trimmed_end > start {
        let last = content.as_bytes()[trimmed_end - 1] as char;
        if is_url_trailing_char(last) {
            trimmed_end -= 1;
            continue;
        }
        break;
    }

    trimmed_end
}
