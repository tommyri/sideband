//! Entry points for the `cargo-fuzz` targets in `fuzz/`.
//!
//! These wrap private parsers so the fuzz crate can reach them without the
//! rest of the server becoming public. They are not a stable API.

use axum::{
    body::Body,
    extract::{FromRequest, Multipart},
    http::{Request, header},
};

/// Preview fields read from one HTML page and an optional oEmbed document.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PreviewCard {
    /// Page title, at most 300 characters.
    pub title: Option<String>,
    /// Description, at most 500 characters.
    pub description: Option<String>,
    /// Absolute http(s) image URL.
    pub image: Option<String>,
    /// Site name, at most 300 characters.
    pub site_name: Option<String>,
    /// Absolute http(s) oEmbed URL discovered in the page.
    pub oembed: Option<String>,
}

/// Runs the link-preview HTML scanner on `html` as if fetched from `page`,
/// then merges `oembed` when it is present. `page` must be an absolute URL.
#[must_use]
pub fn preview_card(html: &str, oembed: Option<&str>, page: &str) -> Option<PreviewCard> {
    let page = reqwest::Url::parse(page).ok()?;
    let mut card = crate::previews::parse_html(html, &page);
    if let Some(body) = oembed {
        crate::previews::merge_oembed(&mut card, body, &page);
    }
    Some(PreviewCard {
        title: card.title,
        description: card.description,
        image: card.image,
        site_name: card.site_name,
        oembed: card.oembed,
    })
}

/// URLs the preview worker would fetch for a message body.
#[must_use]
pub fn preview_urls(content: &str) -> Vec<String> {
    crate::previews::preview_urls(content)
}

/// User ids mentioned with `<@ULID>` (sorted, unique) and whether the body
/// mentions `@everyone`. `Err` is the wire message for too many mentions.
pub fn mentions(content: &str) -> Result<(Vec<ss_core::Id>, bool), String> {
    crate::handlers::messages::parse_mentions(content).map_err(|error| error.to_string())
}

/// One parsed upload form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadForm {
    /// Raw `channel_id` field text.
    pub channel_id: String,
    /// Client-supplied file name, before normalization.
    pub filename: String,
    /// File bytes. Never empty and never above the cap.
    pub bytes: Vec<u8>,
}

/// Parses a `multipart/form-data` upload body exactly as
/// `POST /api/v1/attachments` does. `Err` is the HTTP error message.
pub async fn upload_form(
    content_type: &str,
    body: Vec<u8>,
    max_bytes: u64,
) -> Result<UploadForm, &'static str> {
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/attachments")
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(body))
        .map_err(|_| "request is not valid")?;
    let mut multipart = Multipart::from_request(request, &())
        .await
        .map_err(|_| "upload is not valid multipart")?;
    let (channel_id, filename, bytes) =
        crate::http::attachments::read_upload(&mut multipart, max_bytes)
            .await
            .map_err(crate::http::attachments::HttpError::message)?;
    Ok(UploadForm {
        channel_id,
        filename,
        bytes,
    })
}

/// File name as it would be stored for an upload.
#[must_use]
pub fn stored_filename(raw: &str) -> String {
    crate::storage::normalize_filename(raw)
}
