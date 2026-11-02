//! Passive Windows push-to-talk through Raw Input. A dedicated thread owns a
//! message-only window; no hotkey is registered and every event keeps flowing
//! to the focused application.

mod keys;
mod native;

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender},
    },
    thread::{self, JoinHandle, ThreadId},
    time::{Duration, Instant},
};

use crate::{
    Binding, CaptureSession, Error, InputEvent, PttStateMachine, Trigger,
    listener::{Caps, Handle, Handler, Listener, ListenerControl},
};

/// Native Windows Raw Input listener. Clones share one running/capture lease.
#[derive(Clone, Default)]
pub struct WinListener {
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

impl WinListener {
    /// Construct without creating a window or registering a device.
    #[must_use]
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

impl Listener for WinListener {
    fn capabilities(&self) -> Caps {
        let mut caps = Caps {
            mouse_buttons: true,
            passthrough: true,
            needs_permission: None,
            binding_set_by_system: false,
            limitations: Vec::new(),
        };
        if let Some(application) = native::elevated_foreground() {
            caps.limitations.push(format!(
                "Push-to-talk cannot see input while {application} runs as administrator unless SocialSpeak also runs as administrator."
            ));
        }
        caps
    }

    fn start(&self, handler: Handler) -> Result<Handle, Error> {
        let lease = self.reserve(Usage::Running)?;
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
        let (result, receiver) = mpsc::sync_channel(1);
        let worker = Worker::spawn(
            Session::Capture {
                session: CaptureSession::new(true),
                result,
                deadline,
            },
            lease,
        )?;
        let result = receiver.recv().unwrap_or_else(|_| Err(worker.failure()));
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
    rust_thread: ThreadId,
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
            .name("ss-input-windows".to_owned())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let window = match native::RawWindow::open() {
                        Ok(window) => window,
                        Err(error) => {
                            let _ = ready.send(Err(error.clone()));
                            return Err(error);
                        }
                    };
                    if ready.send(Ok(())).is_err() {
                        return Ok(());
                    }
                    let result = run(
                        &window,
                        &mut session,
                        &receiver,
                        &worker_health,
                        &lease.shared,
                    );
                    session.close(result.clone());
                    result
                }))
                .unwrap_or_else(|_| Err(Error::Unavailable("input handler panicked".to_owned())));
                if let Err(error) = result
                    && let Ok(mut current) = worker_health.error.lock()
                {
                    *current = Some(error);
                }
                worker_health.stop.store(true, Ordering::Release);
            })
            .map_err(|error| Error::Unavailable(error.to_string()))?;
        initialized.recv().unwrap_or_else(|_| {
            Err(Error::Unavailable(
                "input thread stopped during startup".to_owned(),
            ))
        })?;
        Ok(Self {
            commands,
            health,
            rust_thread: join.thread().id(),
            join: Mutex::new(Some(join)),
        })
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
        if thread::current().id() == self.rust_thread {
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
        if thread::current().id() != self.rust_thread
            && let Some(join) = self.join.lock()?.take()
        {
            join.join()
                .map_err(|_| Error::Unavailable("input thread panicked".to_owned()))?;
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

    fn poll(&mut self, now: Instant) {
        if let Self::Running { machine, handler } = self
            && let Some(edge) = machine.poll(now)
        {
            handler(edge);
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

#[derive(Default)]
struct StuckGuard {
    held: HashMap<Trigger, i32>,
}

impl StuckGuard {
    fn observe(&mut self, event: InputEvent, virtual_key: i32) {
        match event {
            InputEvent::Key { code, pressed } => self.set(Trigger::Key(code), pressed, virtual_key),
            InputEvent::Mouse { button, pressed } => {
                self.set(Trigger::Mouse(button), pressed, virtual_key);
            }
        }
    }

    fn set(&mut self, trigger: Trigger, pressed: bool, virtual_key: i32) {
        if pressed {
            self.held.insert(trigger, virtual_key);
        } else {
            self.held.remove(&trigger);
        }
    }

    fn missing_releases(&mut self) -> Vec<InputEvent> {
        let missing = self
            .held
            .iter()
            .filter_map(|(&trigger, &virtual_key)| {
                (!native::key_down(virtual_key)).then_some(trigger)
            })
            .collect::<Vec<_>>();
        for trigger in &missing {
            self.held.remove(trigger);
        }
        missing
            .into_iter()
            .map(|trigger| match trigger {
                Trigger::Key(code) => InputEvent::key_up(code),
                Trigger::Mouse(button) => InputEvent::mouse_up(button),
            })
            .collect()
    }
}

fn run(
    window: &native::RawWindow,
    session: &mut Session,
    commands: &Receiver<Request>,
    health: &Health,
    shared: &Shared,
) -> Result<(), Error> {
    let mut raw = Vec::new();
    let mut guard = StuckGuard::default();
    let mut last_guard = Instant::now();
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
        session.poll(now);
        if now.duration_since(last_guard) >= Duration::from_millis(250) {
            for event in guard.missing_releases() {
                if session.event(event, now) {
                    return Ok(());
                }
            }
            last_guard = now;
        }
        raw.clear();
        window.poll(&mut raw)?;
        for input in raw.drain(..) {
            let mut events = Vec::new();
            match input {
                native::Raw::Keyboard(keyboard) => {
                    if let Some(event) =
                        keys::keyboard(keyboard.MakeCode, keyboard.Flags, keyboard.VKey)
                    {
                        events.push(event);
                    }
                }
                native::Raw::Mouse(mouse) => {
                    // SAFETY: RAWMOUSE's active anonymous member is always the
                    // documented button flags/data pair for mouse input.
                    let flags = unsafe { mouse.Anonymous.Anonymous.usButtonFlags };
                    events.extend(keys::mouse(flags));
                }
            }
            for (event, virtual_key) in events {
                guard.observe(event, virtual_key);
                if session.event(event, Instant::now()) {
                    return Ok(());
                }
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Code, PttEvent};
    use windows::Win32::UI::{
        Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
            SendInput, VIRTUAL_KEY,
        },
        WindowsAndMessaging::GetForegroundWindow,
    };

    #[test]
    fn reservations_do_not_open_native_windows() {
        let listener = WinListener::new();
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
    fn raw_edges_follow_shared_state_machine() {
        let (sent, received) = mpsc::channel();
        let mut session = Session::Running {
            machine: PttStateMachine::with_release_delay(
                [Binding::key(Code::KeyV)],
                Duration::ZERO,
            ),
            handler: Box::new(move |event| sent.send(event).unwrap()),
        };
        let now = Instant::now();
        assert!(!session.event(InputEvent::key_down(Code::KeyV), now));
        assert_eq!(received.try_recv().unwrap(), PttEvent::Pressed);
        session.event(InputEvent::key_up(Code::KeyV), now);
        session.poll(now + crate::DEBOUNCE);
        assert_eq!(received.try_recv().unwrap(), PttEvent::Released);
    }

    #[test]
    fn send_input_reaches_raw_input_on_interactive_ci() {
        if std::env::var_os("CI").is_none() {
            return;
        }
        // SAFETY: read-only desktop query. A null foreground HWND means the
        // runner has no interactive window station, which the task permits us
        // to skip.
        if unsafe { GetForegroundWindow() }.0.is_null() {
            eprintln!("skipping SendInput check: no interactive window station");
            return;
        }
        let listener = WinListener::new();
        let (sent, received) = mpsc::channel();
        let handle = listener
            .start(Box::new(move |event| sent.send(event).unwrap()))
            .unwrap();
        handle.update_bindings(&[Binding::key(Code::KeyV)]).unwrap();
        let input = |flags| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: 0x2f,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let events = [
            input(KEYEVENTF_SCANCODE),
            input(KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP),
        ];
        // SAFETY: `events` is a live, correctly sized INPUT array.
        let sent = unsafe {
            SendInput(
                &events,
                i32::try_from(std::mem::size_of::<INPUT>()).unwrap(),
            )
        };
        if sent == 0 {
            eprintln!("skipping SendInput check: injection unavailable in this session");
            return;
        }
        assert_eq!(sent, 2);
        assert_eq!(
            received.recv_timeout(Duration::from_secs(1)).unwrap(),
            PttEvent::Pressed
        );
        assert_eq!(
            received.recv_timeout(Duration::from_secs(1)).unwrap(),
            PttEvent::Released
        );
        handle.stop().unwrap();
    }
}
