//! OpenGraph, Twitter, and oEmbed cards.
//!
//! The scanner only needs the tags a preview uses. It does not build a DOM.

use reqwest::Url;

const TITLE_CHARS: usize = 300;
const TEXT_CHARS: usize = 500;

/// Fields collected from one page.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Card {
    /// Page title.
    pub title: Option<String>,
    /// Page description.
    pub description: Option<String>,
    /// Absolute image URL.
    pub image: Option<String>,
    /// Site name.
    pub site_name: Option<String>,
    /// Discovered oEmbed document.
    pub oembed: Option<String>,
}

/// Reads preview fields from an HTML document.
#[must_use]
pub(crate) fn parse_html(body: &str, page: &Url) -> Card {
    let mut og_title = None;
    let mut twitter_title = None;
    let mut doc_title = None;
    let mut og_description = None;
    let mut twitter_description = None;
    let mut meta_description = None;
    let mut og_image = None;
    let mut twitter_image = None;
    let mut favicon = None;
    let mut site_name = None;
    let mut oembed = None;
    let mut rest = body;
    while let Some(start) = rest.find('<') {
        rest = rest.get(start..).unwrap_or("");
        if title_open(rest) {
            doc_title.get_or_insert_with(|| text_until(rest, "</title>"));
        }
        if let Some(tag) = element(rest) {
            if tag.name == "meta" {
                take_meta(
                    &tag,
                    &mut MetaSlots {
                        og_title: &mut og_title,
                        twitter_title: &mut twitter_title,
                        og_description: &mut og_description,
                        twitter_description: &mut twitter_description,
                        meta_description: &mut meta_description,
                        og_image: &mut og_image,
                        twitter_image: &mut twitter_image,
                        site_name: &mut site_name,
                    },
                );
            }
            if tag.name == "link" {
                take_link(&tag, page, &mut favicon, &mut oembed);
            }
            rest = tag.rest;
        } else {
            rest = rest.get(1..).unwrap_or("");
        }
    }
    Card {
        title: clip(og_title.or(twitter_title).or(doc_title), TITLE_CHARS),
        description: clip(
            og_description.or(twitter_description).or(meta_description),
            TEXT_CHARS,
        ),
        image: absolute(page, og_image.or(twitter_image).or(favicon).as_deref()),
        site_name: clip(site_name, TITLE_CHARS),
        oembed: oembed.and_then(|href| absolute(page, Some(&href))),
    }
}

/// Fills gaps in `card` from an oEmbed JSON document.
pub(crate) fn merge_oembed(card: &mut Card, body: &str, page: &Url) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return;
    };
    let text = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    if let Some(title) = text("title") {
        card.title = clip(Some(title), TITLE_CHARS);
    }
    if card.description.is_none() {
        card.description = clip(text("description"), TEXT_CHARS);
    }
    if card.image.is_none() {
        card.image = absolute(page, text("thumbnail_url").as_deref());
    }
    if card.site_name.is_none() {
        card.site_name = clip(text("provider_name"), TITLE_CHARS);
    }
}

struct Tag<'a> {
    name: String,
    attrs: Vec<(String, String)>,
    rest: &'a str,
}

struct MetaSlots<'a> {
    og_title: &'a mut Option<String>,
    twitter_title: &'a mut Option<String>,
    og_description: &'a mut Option<String>,
    twitter_description: &'a mut Option<String>,
    meta_description: &'a mut Option<String>,
    og_image: &'a mut Option<String>,
    twitter_image: &'a mut Option<String>,
    site_name: &'a mut Option<String>,
}

fn take_meta(tag: &Tag<'_>, slots: &mut MetaSlots<'_>) {
    let key = tag
        .attr("property")
        .or_else(|| tag.attr("name"))
        .unwrap_or("")
        .to_ascii_lowercase();
    let Some(content) = tag.attr("content") else {
        return;
    };
    let slot = match key.as_str() {
        "og:title" => &mut *slots.og_title,
        "twitter:title" => &mut *slots.twitter_title,
        "og:description" => &mut *slots.og_description,
        "twitter:description" => &mut *slots.twitter_description,
        "description" => &mut *slots.meta_description,
        "og:image" => &mut *slots.og_image,
        "twitter:image" => &mut *slots.twitter_image,
        "og:site_name" => &mut *slots.site_name,
        _ => return,
    };
    if slot.is_none() {
        *slot = Some(decode(content));
    }
}

fn take_link(tag: &Tag<'_>, page: &Url, favicon: &mut Option<String>, oembed: &mut Option<String>) {
    let rel = tag.attr("rel").unwrap_or("").to_ascii_lowercase();
    let Some(href) = tag.attr("href") else {
        return;
    };
    if oembed.is_none() {
        let kind = tag.attr("type").unwrap_or("").to_ascii_lowercase();
        if rel.split_whitespace().any(|item| item == "alternate") && kind.contains("oembed") {
            *oembed = Some(href.to_owned());
            return;
        }
    }
    if favicon.is_none()
        && rel
            .split_whitespace()
            .any(|item| item == "icon" || item == "shortcut")
    {
        *favicon = absolute(page, Some(href));
    }
}

fn title_open(rest: &str) -> bool {
    let Some(bytes) = rest.as_bytes().get(..6) else {
        return false;
    };
    bytes.eq_ignore_ascii_case(b"<title")
        && rest
            .as_bytes()
            .get(6)
            .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b'>')
}

fn text_until(rest: &str, end_tag: &str) -> String {
    let after = rest
        .find('>')
        .and_then(|index| rest.get(index.saturating_add(1)..))
        .unwrap_or("");
    let lower = after.to_ascii_lowercase();
    let text = lower
        .find(&end_tag.to_ascii_lowercase())
        .and_then(|end| after.get(..end))
        .unwrap_or("");
    decode(text).trim().to_owned()
}

fn element(input: &str) -> Option<Tag<'_>> {
    let bytes = input.as_bytes();
    if bytes.first() != Some(&b'<') {
        return None;
    }
    let mut index = 1;
    while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
        index = index.saturating_add(1);
    }
    let name_at = index;
    while bytes.get(index).is_some_and(u8::is_ascii_alphanumeric) {
        index = index.saturating_add(1);
    }
    if index == name_at {
        return None;
    }
    let name = input.get(name_at..index)?.to_ascii_lowercase();
    let mut attrs = Vec::new();
    while let Some(byte) = bytes.get(index) {
        if *byte == b'>' {
            index = index.saturating_add(1);
            break;
        }
        if byte.is_ascii_whitespace() {
            index = index.saturating_add(1);
            continue;
        }
        if *byte == b'/' {
            index = index.saturating_add(1);
            continue;
        }
        let key_at = index;
        while bytes
            .get(index)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'-' || *byte == b':')
        {
            index = index.saturating_add(1);
        }
        let key = input.get(key_at..index)?.to_ascii_lowercase();
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index = index.saturating_add(1);
        }
        let value = if bytes.get(index) == Some(&b'=') {
            index = index.saturating_add(1);
            while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
                index = index.saturating_add(1);
            }
            read_value(input, &mut index)
        } else {
            String::new()
        };
        if !key.is_empty() {
            attrs.push((key, value));
        }
    }
    Some(Tag {
        name,
        attrs,
        rest: input.get(index..).unwrap_or(""),
    })
}

fn read_value(input: &str, index: &mut usize) -> String {
    let bytes = input.as_bytes();
    let quote = bytes.get(*index).copied();
    if matches!(quote, Some(b'"' | b'\'')) {
        *index = index.saturating_add(1);
        let start = *index;
        while bytes.get(*index).is_some_and(|byte| Some(*byte) != quote) {
            *index = index.saturating_add(1);
        }
        let value = input.get(start..*index).unwrap_or("").to_owned();
        if bytes.get(*index) == quote.as_ref() {
            *index = index.saturating_add(1);
        }
        value
    } else {
        let start = *index;
        while bytes
            .get(*index)
            .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'>')
        {
            *index = index.saturating_add(1);
        }
        input.get(start..*index).unwrap_or("").to_owned()
    }
}

fn absolute(page: &Url, raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    let url = page.join(raw).ok()?;
    if url.scheme() == "http" || url.scheme() == "https" {
        Some(url.to_string())
    } else {
        None
    }
}

fn clip(value: Option<String>, max: usize) -> Option<String> {
    let text = value?.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        None
    } else {
        Some(text.chars().take(max).collect())
    }
}

fn decode(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

impl Tag<'_> {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

#[cfg(test)]
mod tests {
    use reqwest::Url;

    use super::{merge_oembed, parse_html};

    #[test]
    fn open_graph_beats_title_and_favicon() {
        let page = Url::parse("https://example.com/post").unwrap();
        let card = parse_html(
            r#"<html><head>
            <title>Fallback</title>
            <meta name="description" content="plain">
            <meta property="og:title" content="Hello &amp; co">
            <meta property="og:description" content="A page">
            <meta property="og:image" content="/cover.png">
            <meta property="og:site_name" content="Example">
            <link rel="icon" href="/favicon.ico">
            </head></html>"#,
            &page,
        );
        assert_eq!(card.title.as_deref(), Some("Hello & co"));
        assert_eq!(card.description.as_deref(), Some("A page"));
        assert_eq!(card.image.as_deref(), Some("https://example.com/cover.png"));
        assert_eq!(card.site_name.as_deref(), Some("Example"));
    }

    #[test]
    fn oembed_fills_missing_image() {
        let page = Url::parse("https://example.com/watch").unwrap();
        let mut card = parse_html("<title>Watch</title>", &page);
        merge_oembed(
            &mut card,
            r#"{"title":"Clip","provider_name":"Tube","thumbnail_url":"https://i.example/t.jpg"}"#,
            &page,
        );
        assert_eq!(card.title.as_deref(), Some("Clip"));
        assert_eq!(card.site_name.as_deref(), Some("Tube"));
        assert_eq!(card.image.as_deref(), Some("https://i.example/t.jpg"));
    }
}
