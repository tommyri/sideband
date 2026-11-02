//! In-process listener for tests and the headless CLI.
//!
//! There is no operating-system hook. Call [`MockListener::inject`] with the
//! edges the CLI or a test wants the state machine to see, and
//! [`MockListener::poll`] when the release delay should be allowed to elapse.

use std::{
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

use tracing::{debug, info};

use crate::{
    binding::{Binding, CaptureSession, InputEvent},
    error::Error,
    listener::{Caps, Handle, Handler, Listener, ListenerControl},
    state::{DEFAULT_RELEASE_DELAY, PttEvent, PttStateMachine},
};

type Callback = Arc<dyn Fn(PttEvent) + Send + Sync>;

/// Listener that feeds [`PttStateMachine`] from injected edges.
///
/// Available explicitly for tests and headless injection on every OS.
#[derive(Clone)]
pub struct MockListener {
    inner: Arc<MockInner>,
}

struct MockInner {
    caps: Caps,
    state: Mutex<MockState>,
    capture_wait: Condvar,
}

struct MockState {
    release_delay: Duration,
    running: Option<Running>,
    capture: CaptureQueue,
    capturing: bool,
    cancel_capture: bool,
}

struct Running {
    machine: PttStateMachine,
    handler: Callback,
}

struct CaptureQueue {
    allow_mouse: bool,
    session: CaptureSession,
    ready: Option<Binding>,
}

struct MockControl {
    inner: Arc<MockInner>,
}

impl MockListener {
    /// Mock with `caps` and [`DEFAULT_RELEASE_DELAY`].
    #[must_use]
    pub fn new(caps: Caps) -> Self {
        Self::with_release_delay(caps, DEFAULT_RELEASE_DELAY)
    }

    /// Mock with `caps` and an explicit release delay.
    ///
    /// The state machine still waits at least [`crate::DEBOUNCE`].
    #[must_use]
    pub fn with_release_delay(caps: Caps, release_delay: Duration) -> Self {
        let allow_mouse = caps.mouse_buttons;
        Self {
            inner: Arc::new(MockInner {
                caps,
                state: Mutex::new(MockState {
                    release_delay,
                    running: None,
                    capture: CaptureQueue::new(allow_mouse),
                    capturing: false,
                    cancel_capture: false,
                }),
                capture_wait: Condvar::new(),
            }),
        }
    }

    /// Push one edge.
    ///
    /// While the listener is running, the edge goes to the state machine and
    /// any resulting [`PttEvent`] is delivered to the handler. While it is
    /// stopped, the edge is queued for [`Listener::capture_next`]. `at` is the
    /// monotonic time the state machine should observe. Capture ignores it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::LockPoisoned`] if a callback panicked while the lock
    /// was held. Injecting while stopped is not an error: that is how capture
    /// is fed.
    pub fn inject(&self, event: InputEvent, at: Instant) -> Result<(), Error> {
        let pending = {
            let mut guard = self.inner.state.lock()?;
            if let Some(running) = guard.running.as_mut() {
                let events = running.machine.handle(event, at);
                Some((Arc::clone(&running.handler), events))
            } else {
                guard.capture.push(event);
                self.inner.capture_wait.notify_all();
                None
            }
        };
        if let Some((handler, events)) = pending {
            dispatch(&handler, events);
        }
        Ok(())
    }

    /// Let a delayed release fire if `now` has reached its deadline.
    ///
    /// Operating-system backends poll their own state machine on an input
    /// thread. The mock has no thread, so the CLI and tests call this.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotRunning`] when the listener is stopped, and
    /// [`Error::LockPoisoned`] if the lock was poisoned.
    pub fn poll(&self, now: Instant) -> Result<(), Error> {
        let (handler, event) = {
            let mut guard = self.inner.state.lock()?;
            let running = guard.running.as_mut().ok_or(Error::NotRunning)?;
            let event = running.machine.poll(now);
            (Arc::clone(&running.handler), event)
        };
        if let Some(event) = event {
            dispatch(&handler, vec![event]);
        }
        Ok(())
    }
}

impl std::fmt::Debug for MockListener {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MockListener")
            .field("caps", &self.inner.caps)
            .finish_non_exhaustive()
    }
}

impl Listener for MockListener {
    fn capabilities(&self) -> Caps {
        self.inner.caps.clone()
    }

    fn start(&self, handler: Handler) -> Result<Handle, Error> {
        let mut guard = self.inner.state.lock()?;
        if guard.running.is_some() {
            return Err(Error::AlreadyRunning);
        }
        if guard.capturing {
            return Err(Error::CaptureInProgress);
        }
        guard.capture.reset();
        guard.cancel_capture = false;
        guard.running = Some(Running {
            machine: PttStateMachine::with_release_delay(Vec::new(), guard.release_delay),
            handler: Arc::from(handler),
        });
        info!(backend = "mock", "push-to-talk listener started");
        drop(guard);
        Ok(Handle::new(Box::new(MockControl {
            inner: Arc::clone(&self.inner),
        })))
    }

    fn capture_gesture(&self, timeout: Duration) -> Result<Binding, Error> {
        let Some(deadline) = Instant::now().checked_add(timeout) else {
            return Err(Error::CaptureTimeout);
        };
        let mut guard = self.inner.state.lock()?;
        if guard.running.is_some() {
            return Err(Error::AlreadyRunning);
        }
        if guard.capturing {
            return Err(Error::CaptureInProgress);
        }
        guard.capturing = true;
        let result = loop {
            if guard.cancel_capture {
                break Err(Error::CaptureCancelled);
            }
            if let Some(binding) = guard.capture.take() {
                break Ok(binding);
            }
            let now = Instant::now();
            if now >= deadline {
                break Err(Error::CaptureTimeout);
            }
            let wait = deadline.saturating_duration_since(now);
            let (next, _) = self.inner.capture_wait.wait_timeout(guard, wait)?;
            guard = next;
        };
        guard.capturing = false;
        guard.cancel_capture = false;
        guard.capture.reset();
        result
    }

    fn cancel_capture(&self) -> Result<(), Error> {
        let mut guard = self.inner.state.lock()?;
        guard.cancel_capture = true;
        self.inner.capture_wait.notify_all();
        Ok(())
    }
}

impl ListenerControl for MockControl {
    fn update_bindings(&self, bindings: &[Binding]) -> Result<(), Error> {
        let (handler, events) = {
            let mut guard = self.inner.state.lock()?;
            let running = guard.running.as_mut().ok_or(Error::NotRunning)?;
            let events = running
                .machine
                .update_bindings(bindings.to_vec(), Instant::now());
            (Arc::clone(&running.handler), events)
        };
        debug!(count = bindings.len(), "updated push-to-talk bindings");
        dispatch(&handler, events);
        Ok(())
    }

    fn set_release_delay(&self, delay: Duration) -> Result<(), Error> {
        let (handler, events) = {
            let mut guard = self.inner.state.lock()?;
            if guard.running.is_none() {
                return Err(Error::NotRunning);
            }
            guard.release_delay = delay;
            // `running` is `Some`: the check above returned otherwise.
            let running = guard.running.as_mut().ok_or(Error::NotRunning)?;
            let events = running.machine.set_release_delay(delay, Instant::now());
            (Arc::clone(&running.handler), events)
        };
        dispatch(&handler, events);
        Ok(())
    }

    fn force_release(&self) -> Result<(), Error> {
        let (handler, event) = {
            let mut guard = self.inner.state.lock()?;
            let running = guard.running.as_mut().ok_or(Error::NotRunning)?;
            let event = running.machine.force_release();
            (Arc::clone(&running.handler), event)
        };
        if let Some(event) = event {
            info!("push-to-talk force-released");
            dispatch(&handler, vec![event]);
        }
        Ok(())
    }

    fn stop(&self) -> Result<(), Error> {
        let running = {
            let mut guard = self.inner.state.lock()?;
            guard.running.take()
        };
        let Some(mut running) = running else {
            return Err(Error::NotRunning);
        };
        info!("push-to-talk listener stopped");
        if let Some(event) = running.machine.force_release() {
            dispatch(&running.handler, vec![event]);
        }
        Ok(())
    }
}

impl CaptureQueue {
    fn new(allow_mouse: bool) -> Self {
        Self {
            allow_mouse,
            session: CaptureSession::new(allow_mouse),
            ready: None,
        }
    }

    fn push(&mut self, event: InputEvent) {
        if self.ready.is_some() {
            return;
        }
        if let Some(binding) = self.session.push(event) {
            self.ready = Some(binding);
        }
    }

    fn take(&mut self) -> Option<Binding> {
        self.ready.take()
    }

    fn reset(&mut self) {
        self.session = CaptureSession::new(self.allow_mouse);
        self.ready = None;
    }
}

fn dispatch(handler: &Callback, events: Vec<PttEvent>) {
    for event in events {
        debug!(%event, "push-to-talk edge");
        handler(event);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };

    use super::MockListener;
    use crate::{
        binding::{Binding, Code, InputEvent, ModifierSet, MouseButton},
        error::Error,
        listener::{Caps, CapturedBinding, Listener},
        state::{DEBOUNCE, PttEvent},
    };

    #[test]
    fn reports_press_release_and_stop() {
        let listener = MockListener::with_release_delay(Caps::mock(), Duration::ZERO);
        let (tx, rx) = mpsc::channel();
        let handle = listener
            .start(Box::new(move |event| tx.send(event).unwrap()))
            .unwrap();
        handle.update_bindings(&[Binding::key(Code::KeyV)]).unwrap();
        let t0 = Instant::now();
        listener
            .inject(InputEvent::key_down(Code::KeyV), t0)
            .unwrap();
        assert_eq!(rx.try_recv().unwrap(), PttEvent::Pressed);
        listener.inject(InputEvent::key_up(Code::KeyV), t0).unwrap();
        assert!(rx.try_recv().is_err());
        listener.poll(t0 + DEBOUNCE).unwrap();
        assert_eq!(rx.try_recv().unwrap(), PttEvent::Released);

        listener
            .inject(InputEvent::key_down(Code::KeyV), t0 + DEBOUNCE)
            .unwrap();
        assert_eq!(rx.try_recv().unwrap(), PttEvent::Pressed);
        handle.stop().unwrap();
        assert_eq!(rx.try_recv().unwrap(), PttEvent::Released);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn update_bindings_moves_the_chord() {
        let listener = MockListener::with_release_delay(Caps::mock(), Duration::ZERO);
        let (tx, rx) = mpsc::channel();
        let handle = listener
            .start(Box::new(move |event| tx.send(event).unwrap()))
            .unwrap();
        handle.update_bindings(&[Binding::key(Code::KeyV)]).unwrap();
        let t0 = Instant::now();
        listener
            .inject(InputEvent::key_down(Code::KeyV), t0)
            .unwrap();
        assert_eq!(rx.try_recv().unwrap(), PttEvent::Pressed);
        handle.update_bindings(&[Binding::key(Code::KeyB)]).unwrap();
        assert!(rx.try_recv().is_err());
        listener
            .poll(Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert_eq!(rx.try_recv().unwrap(), PttEvent::Released);
        listener
            .inject(InputEvent::key_down(Code::KeyB), Instant::now())
            .unwrap();
        assert_eq!(rx.try_recv().unwrap(), PttEvent::Pressed);
        handle.force_release().unwrap();
        assert_eq!(rx.try_recv().unwrap(), PttEvent::Released);
    }

    #[test]
    fn second_start_and_capture_while_running_fail() {
        let listener = MockListener::new(Caps::mock());
        let handle = listener.start(Box::new(|_event| {})).unwrap();
        assert_eq!(
            listener.start(Box::new(|_event| {})).unwrap_err(),
            Error::AlreadyRunning,
        );
        assert_eq!(
            listener.capture_next(Duration::ZERO).unwrap_err(),
            Error::AlreadyRunning,
        );
        handle.stop().unwrap();
    }

    #[test]
    fn capture_returns_a_queued_gesture() {
        let listener = MockListener::new(Caps::mock());
        let now = Instant::now();
        listener
            .inject(InputEvent::key_down(Code::ControlLeft), now)
            .unwrap();
        listener
            .inject(InputEvent::key_down(Code::KeyV), now)
            .unwrap();
        assert_eq!(
            listener.capture_next(Duration::from_millis(50)).unwrap(),
            CapturedBinding::Binding(Binding::key(Code::KeyV).with_modifiers(ModifierSet::CTRL),),
        );
    }

    #[test]
    fn capture_skips_mouse_when_the_backend_cannot_bind_it() {
        let caps = Caps {
            mouse_buttons: false,
            ..Caps::mock()
        };
        let listener = MockListener::new(caps);
        listener
            .inject(InputEvent::mouse_down(MouseButton::Mouse4), Instant::now())
            .unwrap();
        assert_eq!(
            listener.capture_next(Duration::ZERO).unwrap_err(),
            Error::CaptureTimeout,
        );
        listener
            .inject(InputEvent::key_down(Code::F13), Instant::now())
            .unwrap();
        assert_eq!(
            listener.capture_next(Duration::ZERO).unwrap(),
            CapturedBinding::Binding(Binding::key(Code::F13)),
        );
    }

    #[test]
    fn capture_reports_the_compositor_instead_of_a_queued_key() {
        let caps = Caps {
            binding_set_by_system: true,
            ..Caps::none()
        };
        let listener = MockListener::new(caps);
        listener
            .inject(InputEvent::key_down(Code::KeyV), Instant::now())
            .unwrap();
        let started = Instant::now();
        assert_eq!(
            listener.capture_next(Duration::from_secs(30)).unwrap(),
            CapturedBinding::SetBySystem,
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn cancel_unblocks_capture() {
        let listener = MockListener::new(Caps::mock());
        listener.cancel_capture().unwrap();
        assert_eq!(
            listener.capture_next(Duration::from_secs(30)).unwrap_err(),
            Error::CaptureCancelled,
        );

        let waiting = listener.clone();
        let worker = thread::spawn(move || waiting.capture_next(Duration::from_secs(5)));
        listener.cancel_capture().unwrap();
        assert_eq!(worker.join().unwrap().unwrap_err(), Error::CaptureCancelled);
    }
}
