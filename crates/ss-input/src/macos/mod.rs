//! Listen-only macOS push-to-talk. Input Monitoring is requested only by
//! start/capture; capability queries never prompt. Recheck on app focus.
//! A dedicated thread owns the tap, state machine, timer, and user handler.
mod keys;
mod native;

use crate::{
    Binding, CaptureSession, Code, Error, InputEvent, Permission, PttStateMachine,
    listener::{Caps, Handle, Handler, Listener, ListenerControl},
};
use objc2_core_graphics::CGEventType;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender},
    },
    thread::{self, JoinHandle, ThreadId},
    time::{Duration, Instant},
};

/// Native macOS input listener. Clones share one running/capture reservation.
#[derive(Clone, Default)]
pub struct MacListener {
    shared: Arc<Shared>,
}
#[derive(Default)]
struct Shared {
    usage: Mutex<Option<Usage>>,
    cancelled: AtomicBool,
}
#[derive(Clone, Copy)]
enum Usage {
    Running,
    Capture,
}
struct Lease {
    shared: Arc<Shared>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Ok(mut usage) = self.shared.usage.lock() {
            *usage = None;
        }
    }
}
impl MacListener {
    /// Construct without opening a tap or prompting for permission.
    pub fn new() -> Self {
        Self::default()
    }
    fn reserve(&self, mode: Usage) -> Result<Arc<Lease>, Error> {
        let mut usage = self.shared.usage.lock()?;
        match *usage {
            Some(Usage::Running) => return Err(Error::AlreadyRunning),
            Some(Usage::Capture) => return Err(Error::CaptureInProgress),
            None => {}
        }
        *usage = Some(mode);
        Ok(Arc::new(Lease {
            shared: Arc::clone(&self.shared),
        }))
    }
}
impl Listener for MacListener {
    fn capabilities(&self) -> Caps {
        let mut caps = Caps { mouse_buttons: true, passthrough: true,
            needs_permission: (!native::permitted()).then_some(Permission::InputMonitoring),
            binding_set_by_system: false,
            limitations: vec!["Secure input suspends global push-to-talk. Fn and media keys are not bindable; Caps Lock follows its locked state.".into()] };
        if native::secure_input() {
            caps.limitations.push("Secure input is currently active. Leave the secure field before starting push-to-talk.".into());
        }
        caps
    }
    fn start(&self, handler: Handler) -> Result<Handle, Error> {
        let lease = self.reserve(Usage::Running)?;
        native::require_permission()?;
        let worker = Worker::spawn(
            Session::Running {
                machine: PttStateMachine::new(Vec::new()),
                handler,
            },
            Arc::clone(&lease),
        )?;
        Ok(Handle::new(Box::new(Control {
            worker,
            lease: Mutex::new(Some(lease)),
        })))
    }
    fn capture_gesture(&self, timeout: Duration) -> Result<Binding, Error> {
        let lease = self.reserve(Usage::Capture)?;
        if self.shared.cancelled.swap(false, Ordering::AcqRel) {
            return Err(Error::CaptureCancelled);
        }
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(Error::CaptureTimeout)?;
        if timeout.is_zero() {
            return Err(Error::CaptureTimeout);
        }
        native::require_permission()?;
        let (tx, rx) = mpsc::sync_channel(1);
        let worker = Worker::spawn(
            Session::Capture {
                session: CaptureSession::new(true),
                result: tx,
                deadline,
            },
            lease,
        )?;
        let result = rx.recv().unwrap_or_else(|_| Err(worker.failure()));
        worker.stop()?;
        self.shared.cancelled.store(false, Ordering::Release);
        result
    }
    fn cancel_capture(&self) -> Result<(), Error> {
        self.shared.cancelled.store(true, Ordering::Release);
        Ok(())
    }
}

#[derive(Default)]
struct Health {
    stop: AtomicBool,
    error: Mutex<Option<Error>>,
}
struct Worker {
    commands: Sender<Request>,
    health: Arc<Health>,
    thread_id: ThreadId,
    join: Mutex<Option<JoinHandle<()>>>,
}
struct Control {
    worker: Worker,
    lease: Mutex<Option<Arc<Lease>>>,
}
enum Command {
    Bindings(Vec<Binding>),
    Delay(Duration),
    Release,
}
struct Request {
    command: Command,
    response: SyncSender<Result<(), Error>>,
}
impl Worker {
    fn spawn(mut session: Session, lease: Arc<Lease>) -> Result<Self, Error> {
        let (commands, receiver) = mpsc::channel();
        let (ready, initialized) = mpsc::sync_channel(1);
        let health = Arc::new(Health::default());
        let worker_health = Arc::clone(&health);
        let join = thread::Builder::new()
            .name("ss-input-macos".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut tap = match native::Tap::open() {
                        Ok(tap) => tap,
                        Err(error) => {
                            let _ = ready.send(Err(error.clone()));
                            return Err(error);
                        }
                    };
                    if ready.send(Ok(())).is_err() {
                        return Ok(());
                    }
                    let result = run(
                        &mut tap,
                        &mut session,
                        &receiver,
                        &worker_health,
                        &lease.shared,
                    );
                    session.close(result.clone());
                    result
                }))
                .unwrap_or_else(|_| Err(Error::Unavailable("input handler panicked".into())));
                if let Err(error) = result
                    && let Ok(mut current) = worker_health.error.lock()
                {
                    *current = Some(error);
                }
                worker_health.stop.store(true, Ordering::Release);
            })
            .map_err(|error| Error::Unavailable(error.to_string()))?;
        let worker = Self {
            commands,
            health,
            thread_id: join.thread().id(),
            join: Mutex::new(Some(join)),
        };
        initialized.recv().unwrap_or_else(|_| {
            Err(Error::Unavailable(
                "input thread stopped during startup".into(),
            ))
        })?;
        Ok(worker)
    }
    fn failure(&self) -> Error {
        self.health
            .error
            .lock()
            .ok()
            .and_then(|error| error.clone())
            .unwrap_or(Error::NotRunning)
    }
    fn status(&self) -> Result<(), Error> {
        if let Some(error) = self.health.error.lock()?.clone() {
            return Err(error);
        }
        if self.health.stop.load(Ordering::Acquire) {
            return Err(Error::NotRunning);
        }
        Ok(())
    }
    fn command(&self, command: Command) -> Result<(), Error> {
        if thread::current().id() == self.thread_id {
            return Err(Error::ReentrantControl);
        }
        self.status()?;
        let (response, result) = mpsc::sync_channel(1);
        self.commands
            .send(Request { command, response })
            .map_err(|_| self.failure())?;
        result.recv().unwrap_or_else(|_| Err(self.failure()))
    }
    fn stop(&self) -> Result<(), Error> {
        self.health.stop.store(true, Ordering::Release);
        // A handler may drop its own handle: ask the loop to exit, never join self.
        if thread::current().id() != self.thread_id
            && let Some(join) = self.join.lock()?.take()
        {
            join.join()
                .map_err(|_| Error::Unavailable("input thread panicked".into()))?;
        }
        Ok(())
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
impl ListenerControl for Control {
    fn status(&self) -> Result<(), Error> {
        self.worker.status()
    }
    fn update_bindings(&self, bindings: &[Binding]) -> Result<(), Error> {
        self.worker.command(Command::Bindings(bindings.to_vec()))
    }
    fn set_release_delay(&self, delay: Duration) -> Result<(), Error> {
        self.worker.command(Command::Delay(delay))
    }
    fn force_release(&self) -> Result<(), Error> {
        self.worker.command(Command::Release)
    }
    fn stop(&self) -> Result<(), Error> {
        self.worker.stop()?;
        self.lease.lock()?.take();
        Ok(())
    }
}

enum Session {
    Running {
        machine: PttStateMachine,
        handler: Handler,
    },
    Capture {
        session: CaptureSession,
        result: SyncSender<Result<Binding, Error>>,
        deadline: Instant,
    },
}
impl Session {
    fn event(&mut self, event: InputEvent, now: Instant) -> bool {
        match self {
            Self::Running { machine, handler } => {
                for edge in machine.handle(event, now) {
                    handler(edge);
                }
                false
            }
            Self::Capture {
                session, result, ..
            } => {
                if let Some(binding) = session.push(event) {
                    let _ = result.try_send(Ok(binding));
                    true
                } else {
                    false
                }
            }
        }
    }
    fn release(&mut self) {
        match self {
            Self::Running { machine, handler } => {
                if let Some(edge) = machine.force_release() {
                    handler(edge);
                }
            }
            Self::Capture { session, .. } => *session = CaptureSession::new(true),
        }
    }
    fn close(&mut self, error: Result<(), Error>) {
        self.release();
        if let Self::Capture { result, .. } = self {
            let _ = result.try_send(Err(error.err().unwrap_or(Error::CaptureCancelled)));
        }
    }
    fn command(&mut self, command: Command, now: Instant) -> Result<(), Error> {
        let Self::Running { machine, handler } = self else {
            return Err(Error::NotRunning);
        };
        let edges = match command {
            Command::Bindings(bindings) => machine.update_bindings(bindings, now),
            Command::Delay(delay) => machine.set_release_delay(delay, now),
            Command::Release => machine.force_release().into_iter().collect(),
        };
        for edge in edges {
            handler(edge);
        }
        Ok(())
    }
}
fn run(
    tap: &mut native::Tap,
    session: &mut Session,
    commands: &Receiver<Request>,
    health: &Health,
    shared: &Shared,
) -> Result<(), Error> {
    let mut decoder = keys::Decoder::default();
    let mut last_health = Instant::now();
    while !health.stop.load(Ordering::Acquire) {
        while let Ok(request) = commands.try_recv() {
            let result = session.command(request.command, Instant::now());
            let _ = request.response.send(result);
        }
        let now = Instant::now();
        if let Session::Capture { deadline, .. } = session {
            if shared.cancelled.load(Ordering::Acquire) {
                return Err(Error::CaptureCancelled);
            }
            if now >= *deadline {
                return Err(Error::CaptureTimeout);
            }
        }
        if now.duration_since(last_health) >= Duration::from_millis(250) {
            native::health()?;
            last_health = now;
        }
        if let Session::Running { machine, handler } = session
            && let Some(edge) = machine.poll(now)
        {
            handler(edge);
        }
        tap.poll();
        if tap.overflowed() {
            recover(tap, session, &mut decoder)?;
        }
        while let Some(raw) = tap.next() {
            if matches!(
                raw.kind,
                CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
            ) {
                recover(tap, session, &mut decoder)?;
                break;
            }
            if health.stop.load(Ordering::Acquire) {
                return Ok(());
            }
            if process_raw(session, &mut decoder, raw) {
                return Ok(());
            }
        }
    }
    Ok(())
}
fn recover(
    tap: &mut native::Tap,
    session: &mut Session,
    decoder: &mut keys::Decoder,
) -> Result<(), Error> {
    session.release();
    tap.discard();
    if matches!(session, Session::Capture { .. }) {
        return Err(Error::CaptureCancelled);
    }
    native::health()?;
    decoder.reconcile(native::Tap::key_down, native::Tap::mouse_down, |event| {
        let _ = session.event(event, Instant::now());
    });
    tap.enable();
    Ok(())
}

fn process_raw(session: &mut Session, decoder: &mut keys::Decoder, raw: native::Raw) -> bool {
    let mut captured = false;
    decoder.modifiers(raw.flags, |event| {
        captured |= session.event(event, raw.at);
    });
    let event = match raw.kind {
        CGEventType::KeyDown if !raw.repeat => u16::try_from(raw.code)
            .ok()
            .and_then(|code| decoder.key(code, true)),
        CGEventType::KeyUp => u16::try_from(raw.code)
            .ok()
            .and_then(|code| decoder.key(code, false)),
        CGEventType::FlagsChanged
            if keys::key(u16::try_from(raw.code).unwrap_or(u16::MAX)) == Some(Code::CapsLock) =>
        {
            decoder.key(57, raw.flags & (1 << 16) != 0)
        }
        CGEventType::LeftMouseDown | CGEventType::RightMouseDown | CGEventType::OtherMouseDown => {
            decoder.mouse(raw.code, true)
        }
        CGEventType::LeftMouseUp | CGEventType::RightMouseUp | CGEventType::OtherMouseUp => {
            decoder.mouse(raw.code, false)
        }
        _ => None,
    };
    if let Some(event) = event {
        captured |= session.event(event, raw.at);
    }

    captured
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DEBOUNCE, ModifierSet, MouseButton, PttEvent};

    fn running(binding: Binding) -> (Session, Receiver<PttEvent>) {
        let (tx, rx) = mpsc::channel();
        (
            Session::Running {
                machine: PttStateMachine::with_release_delay(vec![binding], Duration::ZERO),
                handler: Box::new(move |edge| {
                    tx.send(edge).unwrap();
                }),
            },
            rx,
        )
    }
    fn raw(kind: CGEventType, code: i64, flags: u64, at: Instant) -> native::Raw {
        native::Raw {
            kind,
            code,
            flags,
            repeat: false,
            at,
        }
    }
    fn poll(session: &mut Session, at: Instant) {
        if let Session::Running { machine, handler } = session
            && let Some(edge) = machine.poll(at)
        {
            handler(edge);
        }
    }

    #[test]
    fn f13_and_mouse4_follow_the_shared_debounce_and_release_contract() {
        for (binding, down, up, code) in [
            (
                Binding::key(Code::F13),
                CGEventType::KeyDown,
                CGEventType::KeyUp,
                105,
            ),
            (
                Binding::mouse(MouseButton::Mouse4),
                CGEventType::OtherMouseDown,
                CGEventType::OtherMouseUp,
                3,
            ),
        ] {
            let (mut session, events) = running(binding);
            let mut decoder = keys::Decoder::default();
            let now = Instant::now();
            assert!(!process_raw(
                &mut session,
                &mut decoder,
                raw(down, code, 0, now)
            ));
            assert_eq!(events.try_recv().unwrap(), PttEvent::Pressed);
            process_raw(&mut session, &mut decoder, raw(up, code, 0, now));
            poll(
                &mut session,
                now + DEBOUNCE.saturating_sub(Duration::from_nanos(1)),
            );
            assert!(events.try_recv().is_err());
            poll(&mut session, now + DEBOUNCE);
            assert_eq!(events.try_recv().unwrap(), PttEvent::Released);
        }
    }
    #[test]
    fn repeat_cannot_reopen_after_force_release_and_recovery_repairs_lost_up() {
        let (mut session, events) = running(Binding::key(Code::F13));
        let mut decoder = keys::Decoder::default();
        let now = Instant::now();
        process_raw(
            &mut session,
            &mut decoder,
            raw(CGEventType::KeyDown, 105, 0, now),
        );
        assert_eq!(events.try_recv().unwrap(), PttEvent::Pressed);
        session.release();
        assert_eq!(events.try_recv().unwrap(), PttEvent::Released);
        let mut repeat = raw(CGEventType::KeyDown, 105, 0, now);
        repeat.repeat = true;
        process_raw(&mut session, &mut decoder, repeat);
        assert!(events.try_recv().is_err());
        decoder.reconcile(
            |_| false,
            |_| false,
            |e| {
                session.event(e, now);
            },
        );
        process_raw(
            &mut session,
            &mut decoder,
            raw(CGEventType::KeyDown, 105, 0, now),
        );
        assert_eq!(events.try_recv().unwrap(), PttEvent::Pressed);
        session.close(Err(native::permission_error()));
        assert_eq!(events.try_recv().unwrap(), PttEvent::Released);
        session.close(Ok(()));
        assert!(events.try_recv().is_err());
    }
    #[test]
    fn capture_includes_modifier_held_before_the_tap_started() {
        let (result, rx) = mpsc::sync_channel(1);
        let mut session = Session::Capture {
            session: CaptureSession::new(true),
            result,
            deadline: Instant::now() + Duration::from_secs(1),
        };
        let mut decoder = keys::Decoder::default();
        assert!(process_raw(
            &mut session,
            &mut decoder,
            raw(CGEventType::KeyDown, 105, (1 << 18) | 1, Instant::now())
        ));
        assert_eq!(
            rx.recv().unwrap().unwrap(),
            Binding::key(Code::F13).with_modifiers(ModifierSet::CTRL)
        );
    }
    #[test]
    fn modifier_capture_completes_on_release() {
        let (result, rx) = mpsc::sync_channel(1);
        let mut session = Session::Capture {
            session: CaptureSession::new(true),
            result,
            deadline: Instant::now() + Duration::from_secs(1),
        };
        let mut decoder = keys::Decoder::default();
        let now = Instant::now();
        assert!(!process_raw(
            &mut session,
            &mut decoder,
            raw(CGEventType::FlagsChanged, 54, (1 << 20) | 0x10, now)
        ));
        assert!(process_raw(
            &mut session,
            &mut decoder,
            raw(CGEventType::FlagsChanged, 54, 0, now)
        ));
        assert_eq!(rx.recv().unwrap().unwrap(), Binding::key(Code::MetaRight));
    }
    #[test]
    fn reservations_and_pre_cancel_do_not_open_native_taps() {
        let listener = MacListener::new();
        let lease = listener.reserve(Usage::Running).unwrap();
        assert!(matches!(
            listener.reserve(Usage::Capture),
            Err(Error::AlreadyRunning)
        ));
        drop(lease);
        let lease = listener.reserve(Usage::Capture).unwrap();
        assert!(matches!(
            listener.reserve(Usage::Running),
            Err(Error::CaptureInProgress)
        ));
        drop(lease);
        listener.cancel_capture().unwrap();
        assert_eq!(
            listener.capture_gesture(Duration::from_secs(1)),
            Err(Error::CaptureCancelled)
        );
        assert_eq!(
            listener.capture_gesture(Duration::ZERO),
            Err(Error::CaptureTimeout)
        );
    }
    #[test]
    fn runtime_failure_and_reentrant_controls_are_reported() {
        let (commands, _rx) = mpsc::channel();
        let health = Arc::new(Health::default());
        let worker = Worker {
            commands,
            health: Arc::clone(&health),
            thread_id: thread::current().id(),
            join: Mutex::new(None),
        };
        assert_eq!(
            worker.command(Command::Release),
            Err(Error::ReentrantControl)
        );
        *health.error.lock().unwrap() = Some(native::permission_error());
        assert_eq!(worker.status(), Err(native::permission_error()));
    }
}
