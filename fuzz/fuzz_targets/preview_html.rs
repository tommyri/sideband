#![no_main]

//! The OpenGraph/Twitter/oEmbed scanner over untrusted fetched pages.
//!
//! A NUL byte, when present, splits the input into an HTML page and an oEmbed
//! JSON document. libFuzzer's `-timeout` catches non-terminating scans.

use libfuzzer_sys::fuzz_target;
use ss_server::fuzzing::preview_card;

fn http_url(value: Option<&String>) {
    if let Some(url) = value {
        assert!(url.starts_with("https://") || url.starts_with("http://"), "{url}");
    }
}

fn clipped(value: Option<&String>, max: usize) {
    if let Some(text) = value {
        assert!(!text.is_empty());
        assert!(text.chars().count() <= max);
    }
}

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let (html, oembed) = match text.split_once('\0') {
        Some((html, oembed)) => (html, Some(oembed)),
        None => (text.as_ref(), None),
    };
    let card = preview_card(html, oembed, "https://example.com/a/page?q=1")
        .expect("the page URL is valid");
    clipped(card.title.as_ref(), 300);
    clipped(card.description.as_ref(), 500);
    clipped(card.site_name.as_ref(), 300);
    http_url(card.image.as_ref());
    http_url(card.oembed.as_ref());
});
