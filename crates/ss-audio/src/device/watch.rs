//! Device inventory diffs. The poll thread turns these into events. Tests
//! cover unplug and replug without a hardware device.

use std::fmt;

/// Capture or playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Microphone, or another input device.
    Capture,
    /// Speaker, or another output device.
    Playback,
}

/// Stable id from `cpal`, serialized as `host:device`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeviceId(String);

impl DeviceId {
    pub(crate) fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// The `host:device` string `cpal` uses for this device.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One enumerated device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Stable id.
    pub id: DeviceId,
    /// Name shown in a device picker.
    pub name: String,
    /// The device can capture.
    pub input: bool,
    /// The device can play.
    pub output: bool,
}

/// Something the settings screen or the voice session should react to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceEvent {
    /// A device appeared.
    Added(DeviceInfo),
    /// A device disappeared from the inventory.
    Removed(DeviceId),
    /// The host's default device changed.
    DefaultChanged {
        /// Which direction changed.
        direction: Direction,
        /// The new default, or `None` when that direction has no device.
        id: Option<DeviceId>,
    },
    /// The open stream's device is gone. Drop the stream and open another.
    Lost {
        /// Which open stream died.
        direction: Direction,
        /// The device that was open.
        id: DeviceId,
    },
    /// The capture device is on an 8 or 16 kHz hands-free path.
    LowQualityInput {
        /// The device that was opened.
        id: DeviceId,
        /// The rate the stream actually opened at.
        sample_rate: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Defaults {
    pub(crate) input: Option<DeviceId>,
    pub(crate) output: Option<DeviceId>,
}

/// `true` when a capture rate, or a hands-free name at a narrow rate, cannot
/// carry fullband speech.
pub(crate) fn is_low_quality_input(name: &str, sample_rate: u32) -> bool {
    if matches!(sample_rate, 8_000 | 16_000) {
        return true;
    }
    if sample_rate > 16_000 {
        return false;
    }
    let lowered = name.to_ascii_lowercase();
    lowered.contains("hands-free") || lowered.contains("handsfree") || lowered.contains("hfp")
}

pub(crate) fn diff_devices(
    previous: &[DeviceInfo],
    current: &[DeviceInfo],
    previous_defaults: &Defaults,
    current_defaults: &Defaults,
    active_capture: Option<&DeviceId>,
    active_playback: Option<&DeviceId>,
) -> Vec<DeviceEvent> {
    let mut events = Vec::new();
    for device in current {
        if !previous.iter().any(|earlier| earlier.id == device.id) {
            events.push(DeviceEvent::Added(device.clone()));
        }
    }
    for device in previous {
        if current.iter().any(|later| later.id == device.id) {
            continue;
        }
        if active_capture == Some(&device.id) {
            events.push(DeviceEvent::Lost {
                direction: Direction::Capture,
                id: device.id.clone(),
            });
        }
        if active_playback == Some(&device.id) {
            events.push(DeviceEvent::Lost {
                direction: Direction::Playback,
                id: device.id.clone(),
            });
        }
        events.push(DeviceEvent::Removed(device.id.clone()));
    }
    if previous_defaults.input != current_defaults.input {
        events.push(DeviceEvent::DefaultChanged {
            direction: Direction::Capture,
            id: current_defaults.input.clone(),
        });
    }
    if previous_defaults.output != current_defaults.output {
        events.push(DeviceEvent::DefaultChanged {
            direction: Direction::Playback,
            id: current_defaults.output.clone(),
        });
    }
    events
}

#[cfg(test)]
mod tests {
    use super::{
        Defaults, DeviceEvent, DeviceId, DeviceInfo, Direction, diff_devices, is_low_quality_input,
    };

    fn device(id: &str) -> DeviceInfo {
        DeviceInfo {
            id: DeviceId::new(id),
            name: id.to_owned(),
            input: true,
            output: false,
        }
    }

    #[test]
    fn unplug_emits_lost_and_replug_emits_added() {
        let mic = device("coreaudio:mic");
        let active = mic.id.clone();
        let defaults = Defaults {
            input: Some(active.clone()),
            output: None,
        };
        let none = Defaults {
            input: None,
            output: None,
        };
        let removed = diff_devices(
            std::slice::from_ref(&mic),
            &[],
            &defaults,
            &none,
            Some(&active),
            None,
        );
        assert!(removed.iter().any(|event| matches!(
            event,
            DeviceEvent::Lost {
                direction: Direction::Capture,
                id,
            } if id == &active
        )));
        assert!(
            removed
                .iter()
                .any(|event| matches!(event, DeviceEvent::Removed(id) if id == &active))
        );
        let returned = diff_devices(&[], &[mic], &none, &defaults, None, None);
        assert!(
            returned
                .iter()
                .any(|event| matches!(event, DeviceEvent::Added(_)))
        );
        assert!(returned.iter().any(|event| matches!(
            event,
            DeviceEvent::DefaultChanged {
                direction: Direction::Capture,
                id: Some(_),
            }
        )));
    }

    #[test]
    fn hands_free_rates_are_low_quality() {
        assert!(is_low_quality_input("Built-in Microphone", 16_000));
        assert!(is_low_quality_input("Headset Hands-Free", 8_000));
        assert!(!is_low_quality_input("Headset Hands-Free", 48_000));
        assert!(!is_low_quality_input("Built-in Microphone", 48_000));
    }
}
