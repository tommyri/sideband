use super::{SourceEvent, keys, unavailable};
use crate::{Code, Error, InputEvent, MouseButton};
use std::collections::HashMap;
use x11rb::{
    connection::Connection,
    protocol::{
        Event,
        xinput::{self, ConnectionExt as _, XIEventMask},
        xkb::{self, ConnectionExt as _},
    },
    rust_connection::RustConnection,
};

pub(super) struct Source {
    connection: RustConnection,
    keys: HashMap<u32, Code>,
}

impl Source {
    pub fn open() -> Result<Self, Error> {
        let (connection, _) = x11rb::connect(None).map_err(unavailable)?;
        connection
            .xinput_xi_query_version(2, 0)
            .map_err(unavailable)?
            .reply()
            .map_err(unavailable)?;
        let xkb = connection
            .xkb_use_extension(1, 0)
            .map_err(unavailable)?
            .reply()
            .map_err(unavailable)?;
        if !xkb.supported {
            return Err(unavailable("XKB is unavailable"));
        }
        let mut source = Self {
            connection,
            keys: HashMap::new(),
        };
        source.refresh_keys()?;
        for screen in &source.connection.setup().roots {
            source
                .connection
                .xinput_xi_select_events(
                    screen.root,
                    &[xinput::EventMask {
                        deviceid: xinput::Device::ALL_MASTER.into(),
                        mask: vec![
                            XIEventMask::RAW_KEY_PRESS
                                | XIEventMask::RAW_KEY_RELEASE
                                | XIEventMask::RAW_BUTTON_PRESS
                                | XIEventMask::RAW_BUTTON_RELEASE,
                        ],
                    }],
                )
                .map_err(unavailable)?
                .check()
                .map_err(unavailable)?;
        }
        source.connection.flush().map_err(unavailable)?;
        Ok(source)
    }

    fn refresh_keys(&mut self) -> Result<(), Error> {
        let names = self
            .connection
            .xkb_get_names(xkb::ID::USE_CORE_KBD.into(), xkb::NameDetail::KEY_NAMES)
            .map_err(unavailable)?
            .reply()
            .map_err(unavailable)?;
        self.keys.clear();
        for (offset, name) in names
            .value_list
            .key_names
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            if let Some(code) = keys::xkb_name(name.name) {
                self.keys.insert(
                    u32::from(names.first_key) + u32::try_from(offset).map_err(unavailable)?,
                    code,
                );
            }
        }
        Ok(())
    }

    pub fn poll(&mut self) -> Result<Vec<SourceEvent>, Error> {
        let mut events = Vec::new();
        // Bound each batch so input floods cannot starve stop or release timers.
        for _ in 0..256 {
            let Some(event) = self.connection.poll_for_event().map_err(unavailable)? else {
                break;
            };
            let input = match event {
                Event::XinputRawKeyPress(e) => self
                    .keys
                    .get(&e.detail)
                    .map(|&code| InputEvent::key_down(code)),
                Event::XinputRawKeyRelease(e) => self
                    .keys
                    .get(&e.detail)
                    .map(|&code| InputEvent::key_up(code)),
                Event::XinputRawButtonPress(e) => mouse(e.detail, true),
                Event::XinputRawButtonRelease(e) => mouse(e.detail, false),
                Event::MappingNotify(_) => {
                    self.refresh_keys()?;
                    events.push(SourceEvent::Reset);
                    None
                }
                _ => None,
            };
            if let Some(input) = input {
                events.push(SourceEvent::Input(input));
            }
        }
        Ok(events)
    }
}

fn mouse(detail: u32, pressed: bool) -> Option<InputEvent> {
    let button = match detail {
        1 => MouseButton::Left,
        2 => MouseButton::Middle,
        3 => MouseButton::Right,
        8 => MouseButton::Mouse4,
        9 => MouseButton::Mouse5,
        _ => return None,
    };
    Some(InputEvent::mouse(button, pressed))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wheel_events_are_not_side_buttons() {
        for code in 4..=7 {
            assert_eq!(mouse(code, true), None);
        }
        assert_eq!(
            mouse(9, true),
            Some(InputEvent::mouse_down(MouseButton::Mouse5))
        );
    }

    #[test]
    #[ignore = "requires an isolated Xvfb display and xdotool"]
    fn x11_native_key_mouse_capture_and_stop() {
        use super::super::{LinuxListener, Options, Selected, Shared};
        use crate::{Binding, Listener, PttEvent};
        use std::{sync::mpsc, time::Duration};
        let listener = LinuxListener {
            shared: std::sync::Arc::new(Shared::default()),
            selected: Selected::X11,
            options: Options::default(),
        };
        let (sender, receiver) = mpsc::channel();
        let handle = listener
            .start(Box::new(move |event| {
                sender.send(event).unwrap();
            }))
            .unwrap();
        handle.update_bindings(&[Binding::key(Code::KeyQ)]).unwrap();
        handle.set_release_delay(Duration::ZERO).unwrap();
        let send = |args: &[&str]| {
            assert!(
                std::process::Command::new("xdotool")
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        send(&["keydown", "q"]);
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(3)).unwrap(),
            PttEvent::Pressed
        );
        send(&["keyup", "q"]);
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(3)).unwrap(),
            PttEvent::Released
        );
        handle
            .update_bindings(&[Binding::mouse(MouseButton::Mouse5)])
            .unwrap();
        send(&["mousedown", "9"]);
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(3)).unwrap(),
            PttEvent::Pressed
        );
        handle.stop().unwrap();
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(3)).unwrap(),
            PttEvent::Released
        );
        send(&["mouseup", "9"]);
        listener.cancel_capture().unwrap();
        assert_eq!(
            listener.capture_gesture(Duration::from_secs(1)),
            Err(Error::CaptureCancelled)
        );
        let capture = std::thread::spawn(move || listener.capture_gesture(Duration::from_secs(3)));
        std::thread::sleep(Duration::from_millis(200));
        send(&["key", "q"]);
        assert_eq!(capture.join().unwrap().unwrap(), Binding::key(Code::KeyQ));
    }
}
