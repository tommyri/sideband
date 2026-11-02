//! Source picker metadata and thumbnails.

use crate::Error;

/// Kind of shareable source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    /// A whole display.
    Display,
    /// One application window.
    Window,
    /// A compositor-owned picker, used by the Wayland portal.
    Portal,
}

/// A display or window offered to the source picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// Opaque native identifier.
    pub id: String,
    /// User-visible display or window name.
    pub label: String,
    /// Source kind.
    pub kind: SourceKind,
    /// Native width before preset scaling.
    pub width: u32,
    /// Native height before preset scaling.
    pub height: u32,
}

impl Source {
    pub(crate) fn native(&self) -> moq_video::capture::Source {
        match self.kind {
            SourceKind::Display => moq_video::capture::Source::Display(Some(self.id.clone())),
            SourceKind::Window => moq_video::capture::Source::Window(self.id.clone()),
            SourceKind::Portal => moq_video::capture::Source::Display(None),
        }
    }
}

/// Tightly packed RGBA source preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thumbnail {
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// RGBA8 rows, `width * 4` bytes each.
    pub rgba: Vec<u8>,
}

/// List displays and on-screen windows. Permission failures remain explicit.
pub async fn list_sources() -> Result<Vec<Source>, Error> {
    let (displays, windows) = tokio::join!(
        moq_video::capture::displays(),
        moq_video::capture::windows()
    );
    let mut sources = match displays {
        Ok(displays) => displays
            .into_iter()
            .map(|display| Source {
                id: display.id,
                label: display.name,
                kind: SourceKind::Display,
                width: display.width,
                height: display.height,
            })
            .collect::<Vec<_>>(),
        #[cfg(target_os = "linux")]
        Err(moq_video::Error::Unsupported(_)) => vec![Source {
            id: "portal".to_owned(),
            label: "Choose with system picker…".to_owned(),
            kind: SourceKind::Portal,
            width: 0,
            height: 0,
        }],
        Err(error) => return Err(error.into()),
    };
    let windows = match windows {
        Ok(windows) => windows,
        #[cfg(target_os = "linux")]
        Err(moq_video::Error::Unsupported(_)) => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    sources.extend(windows.into_iter().map(|window| Source {
        id: window.id,
        label: if window.title.is_empty() {
            window.app
        } else {
            format!("{} — {}", window.title, window.app)
        },
        kind: SourceKind::Window,
        width: window.width,
        height: window.height,
    }));
    Ok(sources)
}

/// Capture one frame and return a 320-pixel-wide source preview.
pub async fn thumbnail(source: &Source) -> Result<Thumbnail, Error> {
    let width = 320;
    let height = even_scaled_height(source.width, source.height, width);
    let mut config = moq_video::capture::Config::default();
    config.source = source.native();
    config.width = Some(width);
    config.height = Some(height);
    config.framerate = Some(moq_video::Rate::new(5, 1)?);
    config.cursor = false;
    let mut stream = moq_video::capture::open(&config).await?;
    let frame = stream.read().await?.ok_or(Error::SourceEnded)?;
    let size = moq_video::Size::new(width, height);
    let frame = frame.resize(size, &moq_video::resize::Config::default())?;
    let rgba = frame
        .surface
        .to_rgba(&moq_video::convert::Config::default())?;
    Ok(Thumbnail {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_data(),
    })
}

fn even_scaled_height(source_width: u32, source_height: u32, width: u32) -> u32 {
    if source_width == 0 || source_height == 0 {
        return 180;
    }
    let scaled = u64::from(source_height)
        .saturating_mul(u64::from(width))
        .checked_div(u64::from(source_width))
        .unwrap_or(180);
    u32::try_from(scaled).unwrap_or(180).max(2) & !1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_geometry_is_even_and_keeps_aspect() {
        assert_eq!(even_scaled_height(1920, 1080, 320), 180);
        assert_eq!(even_scaled_height(1024, 768, 320), 240);
        assert_eq!(even_scaled_height(0, 0, 320), 180);
    }

    #[tokio::test]
    #[ignore = "requires screen-recording permission and an interactive desktop"]
    async fn lists_sources_and_captures_a_thumbnail() {
        let sources = list_sources().await.unwrap();
        let source = sources.first().expect("at least one shareable source");
        let preview = thumbnail(source).await.unwrap();
        assert_eq!(
            preview.rgba.len(),
            preview.width as usize * preview.height as usize * 4
        );
    }
}
