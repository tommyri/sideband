//! Screen-share presets and viewer-hint selection.

/// Aggregated viewer preference received from the server.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SubscriberHint {
    /// Highest requested height; zero means unrestricted.
    pub max_height: u32,
    /// Highest requested frame rate; zero means unrestricted.
    pub max_fps: u32,
    /// Number of active viewers.
    pub viewer_count: u32,
}

/// Supported capture/encode profiles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Preset {
    /// 1920×1080 at 60 fps and 8 Mb/s.
    FullHd60,
    /// 1920×1080 at 30 fps and 4 Mb/s.
    #[default]
    FullHd30,
    /// 1280×720 at 30 fps and 2 Mb/s.
    Hd30,
}

impl Preset {
    /// Select the largest preset that does not exceed a viewer hint.
    #[must_use]
    pub const fn from_hint(hint: SubscriberHint) -> Self {
        if hint.max_height != 0 && hint.max_height <= 720 {
            Self::Hd30
        } else if (hint.max_fps != 0 && hint.max_fps <= 30)
            || (hint.max_height == 0 && hint.max_fps == 0)
        {
            Self::FullHd30
        } else {
            Self::FullHd60
        }
    }

    /// Encoded width.
    #[must_use]
    pub const fn width(self) -> u32 {
        match self {
            Self::FullHd60 | Self::FullHd30 => 1920,
            Self::Hd30 => 1280,
        }
    }

    /// Encoded height.
    #[must_use]
    pub const fn height(self) -> u32 {
        match self {
            Self::FullHd60 | Self::FullHd30 => 1080,
            Self::Hd30 => 720,
        }
    }

    /// Encoded frame rate.
    #[must_use]
    pub const fn fps(self) -> u32 {
        match self {
            Self::FullHd60 => 60,
            Self::FullHd30 | Self::Hd30 => 30,
        }
    }

    /// Target bitrate in bits per second.
    #[must_use]
    pub const fn bitrate(self) -> u64 {
        match self {
            Self::FullHd60 => 8_000_000,
            Self::FullHd30 => 4_000_000,
            Self::Hd30 => 2_000_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_follow_viewer_limits() {
        assert_eq!(
            Preset::from_hint(SubscriberHint::default()),
            Preset::FullHd30
        );
        assert_eq!(
            Preset::from_hint(SubscriberHint {
                max_height: 720,
                max_fps: 60,
                viewer_count: 1,
            }),
            Preset::Hd30
        );
        assert_eq!(
            Preset::from_hint(SubscriberHint {
                max_height: 1080,
                max_fps: 30,
                viewer_count: 2,
            }),
            Preset::FullHd30
        );
        assert_eq!(
            Preset::from_hint(SubscriberHint {
                max_height: 1080,
                max_fps: 60,
                viewer_count: 2,
            }),
            Preset::FullHd60
        );
    }
}
