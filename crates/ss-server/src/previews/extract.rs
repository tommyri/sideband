//! URLs worth a preview card.
//!
//! The first three distinct http(s) URLs are kept. A URL wrapped in `<...>`
//! is a deliberate opt-out and is skipped. Fragments and userinfo are dropped.

use reqwest::Url;

const MAX_URLS: usize = 3;
const SCHEMES: [&str; 2] = ["https://", "http://"];

/// Normalized preview URLs, in the order they appear.
#[must_use]
pub(crate) fn preview_urls(content: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut index = 0;
    while found.len() < MAX_URLS {
        let Some((start, scheme_len)) = next_scheme(content, index) else {
            break;
        };
        let end = url_end(content, start);
        let suppressed = content.as_bytes().get(start.wrapping_sub(1)) == Some(&b'<')
            && content.as_bytes().get(end) == Some(&b'>');
        if !suppressed
            && let Some(raw) = content.get(start..end)
            && let Some(url) = normalize(raw)
            && !found.contains(&url)
        {
            found.push(url);
        }
        index = end.saturating_add(usize::from(suppressed));
        if index <= start {
            index = start.saturating_add(scheme_len);
        }
    }
    found
}

fn next_scheme(content: &str, from: usize) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    for scheme in SCHEMES {
        if let Some(start) = find_scheme(content, from, scheme) {
            let replace = best.is_none_or(|(at, _)| start < at);
            if replace {
                best = Some((start, scheme.len()));
            }
        }
    }
    best
}

fn find_scheme(content: &str, from: usize, scheme: &str) -> Option<usize> {
    content
        .char_indices()
        .skip_while(|(index, _)| *index < from)
        .find_map(|(index, _)| {
            content
                .get(index..)
                .and_then(|tail| tail.as_bytes().get(..scheme.len()))
                .is_some_and(|bytes| {
                    bytes
                        .iter()
                        .zip(scheme.as_bytes())
                        .all(|(got, want)| got.to_ascii_lowercase() == *want)
                })
                .then_some(index)
        })
}

fn url_end(content: &str, start: usize) -> usize {
    let bytes = content.as_bytes();
    let mut end = start;
    while let Some(byte) = bytes.get(end) {
        if byte.is_ascii_whitespace() || matches!(byte, b'<' | b'>' | b'"' | b'\'') {
            break;
        }
        if !byte.is_ascii() {
            break;
        }
        end = end.saturating_add(1);
    }
    while end > start {
        let Some(byte) = bytes.get(end - 1) else {
            break;
        };
        if matches!(byte, b'.' | b',' | b';' | b':' | b'!' | b'?' | b')') {
            end = end.saturating_sub(1);
        } else {
            break;
        }
    }
    end
}

fn normalize(raw: &str) -> Option<String> {
    let mut url = Url::parse(raw).ok()?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return None;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    url.set_fragment(None);
    url.host_str()?;
    Some(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::preview_urls;

    #[test]
    fn keeps_three_unique_urls_and_skips_angle_brackets() {
        let content = "see https://Example.com/a, <https://secret.example/no> \
            https://example.com/a https://example.com/b http://example.com/c https://example.com/d";
        assert_eq!(
            preview_urls(content),
            vec![
                "https://example.com/a".to_owned(),
                "https://example.com/b".to_owned(),
                "http://example.com/c".to_owned(),
            ]
        );
    }
}
