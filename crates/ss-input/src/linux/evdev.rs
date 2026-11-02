use super::{SourceEvent, keys, unavailable};
use crate::{Error, Permission};
use evdev::{Device, EventType};
use inotify::{Inotify, WatchMask};
use std::{
    collections::{HashMap, HashSet},
    io,
    path::PathBuf,
};

pub(super) struct Source {
    devices: HashMap<PathBuf, Device>,
    watcher: Inotify,
    held: HeldKeys,
}

#[derive(Default)]
struct HeldKeys(HashMap<PathBuf, HashSet<u16>>);
impl HeldKeys {
    fn edge(&mut self, device: &std::path::Path, code: u16, pressed: bool) -> bool {
        let before = self.0.values().any(|keys| keys.contains(&code));
        let keys = self.0.entry(device.to_owned()).or_default();
        if pressed {
            keys.insert(code);
        } else {
            keys.remove(&code);
        }
        let after = self.0.values().any(|keys| keys.contains(&code));
        before != after
    }
}
impl Source {
    pub fn open() -> Result<Self, Error> {
        let watcher = Inotify::init().map_err(unavailable)?;
        watcher
            .watches()
            .add(
                "/dev/input",
                WatchMask::CREATE
                    | WatchMask::DELETE
                    | WatchMask::ATTRIB
                    | WatchMask::MOVED_TO
                    | WatchMask::MOVED_FROM,
            )
            .map_err(permission)?;
        let mut source = Self {
            devices: HashMap::new(),
            watcher,
            held: HeldKeys::default(),
        };
        source.rescan()?;
        if source.devices.is_empty() {
            return Err(permission(io::Error::from(io::ErrorKind::PermissionDenied)));
        }
        Ok(source)
    }
    fn rescan(&mut self) -> Result<(), Error> {
        let paths = std::fs::read_dir("/dev/input")
            .map_err(permission)?
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with("event"))
            .map(|e| e.path())
            .collect::<HashSet<_>>();
        self.devices.retain(|path, _| paths.contains(path));
        for path in paths {
            if self.devices.contains_key(&path) {
                continue;
            }
            if let Ok(device) = Device::open(&path) {
                if device.supported_keys().is_none() {
                    continue;
                }
                device.set_nonblocking(true).map_err(unavailable)?;
                self.devices.insert(path, device);
            }
        }
        Ok(())
    }
    pub fn poll(&mut self) -> Result<Vec<SourceEvent>, Error> {
        let mut result = Vec::new();
        let mut buffer = [0; 4096];
        match self.watcher.read_events(&mut buffer) {
            Ok(mut events) => {
                if events.next().is_some() {
                    self.rescan()?;
                    self.held.0.clear();
                    result.push(SourceEvent::Reset);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(unavailable(error)),
        }
        let mut removed = Vec::new();
        for (path, device) in &mut self.devices {
            match device.fetch_events() {
                Ok(events) => {
                    for event in events {
                        // evdev's synchronized stream repairs SYN_DROPPED using device state.
                        if event.event_type() == EventType::KEY
                            && event.value() != 2
                            && self.held.edge(path, event.code(), event.value() != 0)
                            && let Some(input) = keys::evdev_event(event.code(), event.value() != 0)
                        {
                            result.push(SourceEvent::Input(input));
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(_) => removed.push(path.clone()),
            }
        }
        if !removed.is_empty() {
            for path in removed {
                self.devices.remove(&path);
            }
            result.push(SourceEvent::Reset);
            self.held.0.clear();
        }
        Ok(result)
    }
}

fn permission(error: io::Error) -> Error {
    if error.kind() == io::ErrorKind::PermissionDenied {
        Error::PermissionRequired {
            permission: Permission::InputGroup,
            settings_url: None,
        }
    } else {
        unavailable(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn releasing_one_keyboard_does_not_release_another() {
        let mut held = HeldKeys::default();
        let a = std::path::Path::new("a");
        let b = std::path::Path::new("b");
        assert!(held.edge(a, 30, true));
        assert!(!held.edge(a, 30, true));
        assert!(!held.edge(b, 30, true));
        assert!(!held.edge(a, 30, false));
        assert!(held.edge(b, 30, false));
        assert!(!held.edge(b, 30, false));
    }
}
