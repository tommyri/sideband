//! All native tap resources stay on their creating thread. The C callback
//! copies scalar fields to a fixed ring and always returns the original event.
use crate::{Error, Permission};
use objc2_core_foundation::{
    CFMachPort, CFRetained, CFRunLoop, CFRunLoopSource, kCFRunLoopDefaultMode,
};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventSource, CGEventSourceStateID, CGEventTapLocation,
    CGEventTapOptions, CGEventTapPlacement, CGEventTapProxy, CGEventType, CGMouseButton,
    CGPreflightListenEventAccess, CGRequestListenEventAccess,
};
use rtrb::{Consumer, Producer, RingBuffer};
use std::{cell::UnsafeCell, ffi::c_void, ptr::NonNull, time::Instant};

pub(super) const SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent";
const CAPACITY: usize = 256;
#[derive(Clone, Copy)]
pub(super) struct Raw {
    pub kind: CGEventType,
    pub code: i64,
    pub flags: u64,
    pub repeat: bool,
    pub at: Instant,
}
struct Callback {
    queue: Producer<Raw>,
    overflow: bool,
}

pub(super) fn permitted() -> bool {
    CGPreflightListenEventAccess()
}
pub(super) fn permission_error() -> Error {
    Error::PermissionRequired {
        permission: Permission::InputMonitoring,
        settings_url: Some(SETTINGS.to_owned()),
    }
}
pub(super) fn require_permission() -> Result<(), Error> {
    if !permitted() {
        let _ = CGRequestListenEventAccess();
        return Err(permission_error());
    }
    health()
}
#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn IsSecureEventInputEnabled() -> u8;
}
pub(super) fn secure_input() -> bool {
    // SAFETY: process-wide Carbon query has no pointer or thread preconditions.
    unsafe { IsSecureEventInputEnabled() != 0 }
}
pub(super) fn health() -> Result<(), Error> {
    if !permitted() {
        Err(permission_error())
    } else if secure_input() {
        Err(Error::SecureInput)
    } else {
        Ok(())
    }
}

pub(super) struct Tap {
    port: CFRetained<CFMachPort>,
    source: CFRetained<CFRunLoopSource>,
    run_loop: CFRetained<CFRunLoop>,
    callback: Box<UnsafeCell<Callback>>,
    queue: Consumer<Raw>,
}
impl Tap {
    pub fn open() -> Result<Self, Error> {
        let (producer, queue) = RingBuffer::new(CAPACITY);
        let callback = Box::new(UnsafeCell::new(Callback {
            queue: producer,
            overflow: false,
        }));
        let mask = [
            CGEventType::KeyDown,
            CGEventType::KeyUp,
            CGEventType::FlagsChanged,
            CGEventType::LeftMouseDown,
            CGEventType::LeftMouseUp,
            CGEventType::RightMouseDown,
            CGEventType::RightMouseUp,
            CGEventType::OtherMouseDown,
            CGEventType::OtherMouseUp,
        ]
        .iter()
        .fold(0_u64, |mask, kind| mask | (1 << kind.0));
        // SAFETY: boxed context remains at the same address until the source is
        // removed and port invalidated in Drop; callback only runs on this loop.
        let port = unsafe {
            CGEvent::tap_create(
                CGEventTapLocation::SessionEventTap,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::ListenOnly,
                mask,
                Some(callback_fn),
                callback.get().cast(),
            )
        }
        .ok_or_else(|| {
            if permitted() {
                Error::Unavailable("could not create a session event tap".into())
            } else {
                permission_error()
            }
        })?;
        let source = CFMachPort::new_run_loop_source(None, Some(&port), 0).ok_or_else(|| {
            port.invalidate();
            Error::Unavailable("could not create the input run-loop source".into())
        })?;
        let run_loop = CFRunLoop::current().ok_or_else(|| {
            port.invalidate();
            Error::Unavailable("no input run loop".into())
        })?;
        // SAFETY: Core Foundation exports this immutable run-loop mode constant.
        run_loop.add_source(Some(&source), unsafe { kCFRunLoopDefaultMode });
        CGEvent::tap_enable(&port, true);
        Ok(Self {
            port,
            source,
            run_loop,
            callback,
            queue,
        })
    }
    #[allow(
        clippy::unused_self,
        reason = "keep the live tap borrowed while callbacks can run"
    )]
    pub fn poll(&mut self) {
        // SAFETY: immutable Core Foundation mode; only this thread runs its loop.
        CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, 0.01, true);
    }
    pub fn next(&mut self) -> Option<Raw> {
        self.queue.pop().ok()
    }
    pub fn overflowed(&mut self) -> bool {
        // SAFETY: callback only runs synchronously inside poll on this thread.
        std::mem::take(&mut unsafe { &mut *self.callback.get() }.overflow)
    }
    pub fn discard(&mut self) {
        while self.next().is_some() {}
    }
    pub fn enable(&self) {
        CGEvent::tap_enable(&self.port, true);
    }
    pub fn key_down(code: u16) -> bool {
        CGEventSource::key_state(CGEventSourceStateID::CombinedSessionState, code)
    }
    pub fn mouse_down(button: u8) -> bool {
        CGEventSource::button_state(
            CGEventSourceStateID::CombinedSessionState,
            CGMouseButton(u32::from(button)),
        )
    }
}
impl Drop for Tap {
    fn drop(&mut self) {
        CGEvent::tap_enable(&self.port, false);
        // SAFETY: immutable Core Foundation mode, same source/loop as open.
        self.run_loop
            .remove_source(Some(&self.source), unsafe { kCFRunLoopDefaultMode });
        self.port.invalidate();
        // The boxed callback can now be freed: no source can invoke it again.
    }
}
unsafe extern "C-unwind" fn callback_fn(
    _proxy: CGEventTapProxy,
    kind: CGEventType,
    event: NonNull<CGEvent>,
    info: *mut c_void,
) -> *mut CGEvent {
    // SAFETY: Tap::open gives this callback its live, exclusively accessed box.
    let context = unsafe { &mut *info.cast::<Callback>() };
    // Disabled notifications carry no readable event fields. Never dereference
    // their event fields.
    let raw = if kind == CGEventType::TapDisabledByTimeout
        || kind == CGEventType::TapDisabledByUserInput
    {
        Raw {
            kind,
            code: 0,
            flags: 0,
            repeat: false,
            at: Instant::now(),
        }
    } else {
        // SAFETY: Quartz supplies a borrowed event valid for this callback.
        let input = Some(unsafe { event.as_ref() });
        let field = if matches!(
            kind,
            CGEventType::KeyDown | CGEventType::KeyUp | CGEventType::FlagsChanged
        ) {
            CGEventField::KeyboardEventKeycode
        } else {
            CGEventField::MouseEventButtonNumber
        };
        Raw {
            kind,
            code: CGEvent::integer_value_field(input, field),
            flags: CGEvent::flags(input).bits(),
            repeat: CGEvent::integer_value_field(input, CGEventField::KeyboardEventAutorepeat) != 0,
            at: Instant::now(),
        }
    };
    if context.queue.push(raw).is_err() {
        context.overflow = true;
    }
    event.as_ptr()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_keeps_event_identity_and_bounds_its_queue() {
        // Construct only an in-memory event: never post it or open an event tap.
        let event = CGEvent::new_keyboard_event(None, 105, true).unwrap();
        let original = NonNull::from(&*event);
        let (queue, mut consumer) = RingBuffer::new(1);
        let mut context = Callback {
            queue,
            overflow: false,
        };
        // SAFETY: pointers remain live through these direct synchronous calls;
        // callback never uses the proxy or keeps a borrowed event pointer.
        let returned = unsafe {
            callback_fn(
                std::ptr::null_mut(),
                CGEventType::KeyDown,
                original,
                (&raw mut context).cast(),
            )
        };
        assert_eq!(returned, original.as_ptr());
        // SAFETY: same live event/context as above; full ring must not block.
        unsafe {
            callback_fn(
                std::ptr::null_mut(),
                CGEventType::KeyUp,
                original,
                (&raw mut context).cast(),
            );
        }
        assert!(context.overflow);
        let raw = consumer.pop().unwrap();
        assert_eq!(raw.code, 105);
        assert_eq!(raw.kind, CGEventType::KeyDown);
        assert!(consumer.pop().is_err());
        // SAFETY: disabled notification should queue without reading event fields.
        unsafe {
            callback_fn(
                std::ptr::null_mut(),
                CGEventType::TapDisabledByTimeout,
                original,
                (&raw mut context).cast(),
            );
        }
        assert_eq!(
            consumer.pop().unwrap().kind,
            CGEventType::TapDisabledByTimeout
        );
    }
}
