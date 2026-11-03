//! Linux input listeners with explicit evdev opt-in.

mod evdev;
mod keys;
pub mod portal;
mod x11;

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender},
    },
    thread::{self, JoinHandle, ThreadId},
    time::{Duration, Instant},
};

use crate::{
    Binding, CaptureSession, Error, InputEvent, PttStateMachine,
    listener::{Caps, Handle, Handler, Listener, ListenerControl},
};

/// Linux listener. Clones share one running/capture lease.
#[derive(Clone)]
pub struct LinuxListener {
    shared: Arc<Shared>,
    selected: Selected,
    options: Options,
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

impl LinuxListener {
    /// Detect the current session without opening input devices or requesting consent.
    #[must_use]
    pub fn new(options: Options) -> Self {
        let selected = select(
            std::env::var_os("WAYLAND_DISPLAY").is_some(),
            std::env::var_os("DISPLAY").is_some(),
            options.allow_evdev,
            portal::available,
        );
        Self {
            shared: Arc::default(),
            selected,
            options,
        }
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

impl Listener for LinuxListener {
    fn backend_name(&self) -> &'static str {
        match self.selected {
            Selected::X11 => "X11 XI2",
            Selected::Evdev => "Linux evdev",
            Selected::Portal => "Wayland GlobalShortcuts portal",
            Selected::Unavailable => "Unavailable",
        }
    }

    fn capabilities(&self) -> Caps {
        match self.selected {
            Selected::X11 => passive_caps(),
            Selected::Evdev => {
                let mut caps = passive_caps();
                caps.needs_permission = Some(crate::Permission::InputGroup);
                caps.limitations.push("Opt-in evdev access can read every key in every application. Grant input-group access only if you accept this.".to_owned());
                caps
            }
            Selected::Portal => Caps {
                mouse_buttons: false, passthrough: false, needs_permission: None,
                binding_set_by_system: true,
                limitations: vec!["The compositor owns these shortcuts. KDE/GNOME may consume them; Hyprland requires a pass-through bindn configuration.".to_owned()],
            },
            Selected::Unavailable => Caps::none(),
        }
    }

    fn start(&self, handler: Handler) -> Result<Handle, Error> {
        let lease = self.reserve(Usage::Running)?;
        let worker = Worker::spawn(
            Session::Running {
                machine: PttStateMachine::new(if self.selected == Selected::Portal {
                    vec![Binding::key(crate::Code::F24)]
                } else {
                    Vec::new()
                }),
                handler,
            },
            Arc::clone(&lease),
            self.selected,
            self.options.clone(),
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
            self.selected,
            self.options.clone(),
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
    fn spawn(
        mut session: Session,
        lease: Arc<Lease>,
        selected: Selected,
        options: Options,
    ) -> Result<Self, Error> {
        let (commands, receiver) = mpsc::channel();
        let (ready, initialized) = mpsc::sync_channel(1);
        let health = Arc::new(Health::default());
        let worker_health = Arc::clone(&health);
        let join = thread::Builder::new()
            .name("ss-input-linux".to_owned())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut window = match Source::open(selected, options) {
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
                        &mut window,
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

fn run(
    source: &mut Source,
    session: &mut Session,
    commands: &Receiver<Request>,
    health: &Health,
    shared: &Shared,
) -> Result<(), Error> {
    while !health.stop.load(Ordering::Acquire) {
        while let Ok(request) = commands.try_recv() {
            let result = if matches!(source, Source::Portal(_))
                && matches!(request.command, Command::Bindings(_))
            {
                Err(Error::Unavailable(
                    "Configure the system-owned shortcut through the portal".to_owned(),
                ))
            } else {
                session.command(request.command, Instant::now())
            };
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
        for event in source.poll()? {
            match event {
                SourceEvent::Input(input) => {
                    if session.event(input, Instant::now()) {
                        return Ok(());
                    }
                }
                SourceEvent::Reset => {
                    session.release();
                    // A new device/session has no knowledge of the old held keys.
                    // Feed releases for every physical key to clear force-release suppression.
                    for usage in 0..=255 {
                        if let Some(code) = crate::Code::from_hid_usage(usage) {
                            session.event(InputEvent::key_up(code), Instant::now());
                        }
                    }
                    for button in [
                        crate::MouseButton::Left,
                        crate::MouseButton::Right,
                        crate::MouseButton::Middle,
                        crate::MouseButton::Mouse4,
                        crate::MouseButton::Mouse5,
                    ] {
                        session.event(InputEvent::mouse_up(button), Instant::now());
                    }
                }
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

#[derive(Clone, Default)]
/// Linux input preferences. evdev is never enabled implicitly.
pub struct Options {
    /// Allow reading all system input devices when the Wayland portal is missing.
    pub allow_evdev: bool,
    /// Portal shortcut preferences and action callbacks.
    pub portal: portal::Options,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Selected {
    X11,
    Portal,
    Evdev,
    Unavailable,
}

fn select(wayland: bool, x11: bool, evdev: bool, portal: impl FnOnce() -> bool) -> Selected {
    if wayland {
        if portal() {
            Selected::Portal
        } else if evdev {
            Selected::Evdev
        } else {
            Selected::Unavailable
        }
    } else if x11 {
        Selected::X11
    } else {
        Selected::Unavailable
    }
}

fn passive_caps() -> Caps {
    Caps {
        mouse_buttons: true,
        passthrough: true,
        needs_permission: None,
        binding_set_by_system: false,
        limitations: Vec::new(),
    }
}

enum SourceEvent {
    Input(InputEvent),
    Reset,
}
enum Source {
    X11(Box<x11::Source>),
    Evdev(evdev::Source),
    Portal(portal::Source),
}
impl Source {
    fn open(selected: Selected, options: Options) -> Result<Self, Error> {
        match selected {
            Selected::X11 => x11::Source::open().map(Box::new).map(Self::X11),
            Selected::Evdev => evdev::Source::open().map(Self::Evdev),
            Selected::Portal => portal::Source::open(options.portal).map(Self::Portal),
            Selected::Unavailable => {
                Err(Error::Unavailable(Caps::UNAVAILABLE_LIMITATION.to_owned()))
            }
        }
    }
    fn poll(&mut self) -> Result<Vec<SourceEvent>, Error> {
        match self {
            Self::X11(source) => source.poll(),
            Self::Evdev(source) => source.poll(),
            Self::Portal(source) => source.poll(),
        }
    }
}
fn unavailable(error: impl std::fmt::Display) -> Error {
    Error::Unavailable(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wayland_never_falls_back_to_xwayland_or_implicit_evdev() {
        assert!(select(true, true, false, || false) == Selected::Unavailable);
        assert!(select(true, true, true, || false) == Selected::Evdev);
        assert!(select(true, true, true, || true) == Selected::Portal);
        assert!(
            select(false, true, false, || panic!("portal must not be probed")) == Selected::X11
        );
        assert!(select(false, false, true, || false) == Selected::Unavailable);
    }
}
