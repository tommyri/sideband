#![no_main]

//! Mention parsing and preview URL extraction over message bodies.
//!
//! The server has no Markdown parser: it stores raw text. These are the two
//! scanners that read user-written content.

use libfuzzer_sys::fuzz_target;
use ss_server::fuzzing::{mentions, preview_urls};

const MAX_MENTIONS: usize = 50;

fuzz_target!(|data: &[u8]| {
    let Ok(content) = std::str::from_utf8(data) else {
        return;
    };
    match mentions(content) {
        Ok((ids, _everyone)) => {
            assert!(ids.len() <= MAX_MENTIONS);
            assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "sorted and unique");
            // ULIDs parse case-insensitively with Crockford aliases, so
            // compare parsed ids, not the canonical text.
            let written: Vec<ss_core::Id> = content
                .split("<@")
                .skip(1)
                .filter_map(|tail| tail.split_once('>'))
                .filter_map(|(id, _)| id.parse().ok())
                .collect();
            for id in &ids {
                assert!(written.contains(id), "{id} is not written in the body");
            }
        }
        Err(message) => assert_eq!(message, "too many mentions"),
    }
    let urls = preview_urls(content);
    assert!(urls.len() <= 3);
    for (index, url) in urls.iter().enumerate() {
        assert!(url.starts_with("https://") || url.starts_with("http://"), "{url}");
        assert!(!url.contains('#'), "fragments are dropped: {url}");
        assert!(!urls[..index].contains(url), "unique: {url}");
    }
});
