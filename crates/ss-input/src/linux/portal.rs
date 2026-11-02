//! Wayland GlobalShortcuts sessions and system-owned shortcut settings.
use super::{SourceEvent, unavailable};

#[cfg(test)]
mod tests;
use crate::{Code, Error, InputEvent};
use ashpd::desktop::{
    Session,
    global_shortcuts::{GlobalShortcuts, NewShortcut},
};
use futures::StreamExt;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};
use tokio::runtime::Runtime;

/// An application action produced by a compositor shortcut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Toggle microphone mute.
    ToggleMute,
    /// Toggle speaker deafen.
    ToggleDeafen,
}

/// Shared settings handle. Keep a clone in the application settings model.
#[derive(Clone, Default)]
pub struct Settings(Arc<SettingsState>);
#[derive(Default)]
struct SettingsState {
    configure: AtomicBool,
    can_configure: AtomicBool,
    shortcuts: Mutex<Vec<(String, String)>>,
}
impl Settings {
    /// Ask the active portal v2 session to open its configuration UI.
    pub fn configure(&self) -> Result<(), Error> {
        if !self.0.can_configure.load(Ordering::Acquire) {
            return Err(unavailable(
                "Shortcut configuration requires an active portal v2 session",
            ));
        }
        self.0.configure.store(true, Ordering::Release);
        Ok(())
    }
    /// Actual compositor bindings as (shortcut id, trigger description).
    pub fn shortcuts(&self) -> Result<Vec<(String, String)>, Error> {
        Ok(self.0.shortcuts.lock()?.clone())
    }
}

/// Preferred triggers use the XDG shortcuts syntax; the compositor may change them.
#[derive(Clone, Default)]
pub struct Options {
    /// Preferred push-to-talk trigger, for example "F13".
    pub push_to_talk: Option<String>,
    /// Preferred toggle-mute trigger.
    pub toggle_mute: Option<String>,
    /// Preferred toggle-deafen trigger.
    pub toggle_deafen: Option<String>,
    /// Receives mute/deafen actions on the input worker, outside the portal runtime.
    pub on_action: Option<Arc<dyn Fn(Action) + Send + Sync>>,
    /// System settings and actual binding descriptions.
    pub settings: Settings,
}

fn runtime() -> Result<Runtime, Error> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(unavailable)
}

pub(super) fn available() -> bool {
    // Detection may be called inside the application's Tokio runtime.
    std::thread::spawn(|| {
        runtime().is_ok_and(|runtime| {
            runtime.block_on(async {
                tokio::time::timeout(Duration::from_secs(2), async {
                    let connection = ashpd::zbus::Connection::session().await?;
                    let portal = GlobalShortcuts::with_connection(connection).await?;
                    // ashpd defaults to version 1 for some missing-property errors.
                    // Require a real property reply before advertising availability.
                    portal
                        .get_property::<u32>("version")
                        .await
                        .map_err(ashpd::Error::from)
                })
                .await
                .is_ok_and(|result| result.is_ok_and(|version| version >= 1))
            })
        })
    })
    .join()
    .unwrap_or(false)
}

enum Message {
    Input(bool),
    Action(Action),
    Reset,
    Failed(Error),
}

pub(super) struct Source {
    runtime: Runtime,
    task: tokio::task::JoinHandle<()>,
    receiver: mpsc::Receiver<Message>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    on_action: Option<Arc<dyn Fn(Action) + Send + Sync>>,
}

impl Source {
    pub fn open(options: Options) -> Result<Self, Error> {
        // A multithread runtime keeps the D-Bus session alive between polls.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map_err(unavailable)?;
        let (sender, receiver) = mpsc::channel();
        let (ready, initialized) = mpsc::sync_channel(1);
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let on_action = options.on_action.clone();
        let task = runtime.spawn(async move {
            let result = run(options, &sender, ready, &mut stopped).await;
            if let Err(error) = result {
                let _ = sender.send(Message::Failed(error));
            }
        });
        let source = Self {
            runtime,
            task,
            receiver,
            stop: Some(stop),
            on_action,
        };
        initialized
            .recv_timeout(Duration::from_secs(125))
            .map_err(unavailable)??;
        Ok(source)
    }
    pub fn poll(&mut self) -> Result<Vec<SourceEvent>, Error> {
        let mut events = Vec::new();
        for message in self.receiver.try_iter().take(256) {
            match message {
                Message::Input(pressed) => {
                    events.push(SourceEvent::Input(InputEvent::key(Code::F24, pressed)));
                }
                Message::Reset => events.push(SourceEvent::Reset),
                Message::Action(action) => {
                    if let Some(handler) = &self.on_action {
                        handler(action);
                    }
                }
                Message::Failed(error) => return Err(error),
            }
        }
        if self.task.is_finished() {
            return Err(unavailable("portal input session ended"));
        }
        Ok(events)
    }
}
impl Drop for Source {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.runtime.block_on(async {
            if tokio::time::timeout(Duration::from_secs(2), &mut self.task)
                .await
                .is_err()
            {
                self.task.abort();
            }
        });
    }
}

#[allow(
    clippy::default_trait_access,
    reason = "ashpd does not publicly export CreateSessionOptions"
)]
async fn run(
    options: Options,
    sender: &mpsc::Sender<Message>,
    ready: mpsc::SyncSender<Result<(), Error>>,
    stopped: &mut tokio::sync::oneshot::Receiver<()>,
) -> Result<(), Error> {
    let mut ready = Some(ready);
    loop {
        let connection = ashpd::zbus::Connection::session()
            .await
            .map_err(unavailable)?;
        let portal = match GlobalShortcuts::with_connection(connection).await {
            Ok(portal) => portal,
            Err(error) => {
                let error = unavailable(error);
                if let Some(ready) = ready.take() {
                    let _ = ready.send(Err(error.clone()));
                }
                return Err(error);
            }
        };
        let session = portal
            .create_session(Default::default())
            .await
            .map_err(unavailable)?;
        let result = tokio::select! {
            _ = &mut *stopped => Ok(false),
            result = session_loop(&portal, &session, &options, sender, &mut ready) => result,
        };
        // Release before waiting on a possibly unresponsive service to close.
        if !matches!(result, Ok(false)) {
            let _ = sender.send(Message::Reset);
        }
        let _ = tokio::time::timeout(Duration::from_secs(2), session.close()).await;
        options.settings.0.shortcuts.lock()?.clear();
        options
            .settings
            .0
            .can_configure
            .store(false, Ordering::Release);
        match result {
            Ok(true) => {}
            Ok(false) => return Ok(()),
            Err(error) => {
                if let Some(ready) = ready.take() {
                    let _ = ready.send(Err(error.clone()));
                }
                return Err(error);
            }
        }
    }
}

async fn session_loop(
    portal: &GlobalShortcuts,
    session: &Session<GlobalShortcuts>,
    options: &Options,
    sender: &mpsc::Sender<Message>,
    ready: &mut Option<mpsc::SyncSender<Result<(), Error>>>,
) -> Result<bool, Error> {
    let mut activated = portal.receive_activated().await.map_err(unavailable)?;
    let mut deactivated = portal.receive_deactivated().await.map_err(unavailable)?;
    let mut changed = portal
        .receive_shortcuts_changed()
        .await
        .map_err(unavailable)?;
    let mut closed = session.receive_closed().await.map_err(unavailable)?;
    let mut owner_changed = portal.receive_owner_changed().await.map_err(unavailable)?;
    // Session serializes as its D-Bus object path.
    let path = serde_json::to_value(session)
        .map_err(unavailable)?
        .as_str()
        .ok_or_else(|| unavailable("invalid portal session path"))?
        .to_owned();
    let shortcuts = [
        NewShortcut::new("push-to-talk", "Push to talk")
            .preferred_trigger(options.push_to_talk.as_deref()),
        NewShortcut::new("toggle-mute", "Toggle microphone mute")
            .preferred_trigger(options.toggle_mute.as_deref()),
        NewShortcut::new("toggle-deafen", "Toggle deafen")
            .preferred_trigger(options.toggle_deafen.as_deref()),
    ];
    let request = tokio::time::timeout(
        Duration::from_secs(120),
        portal.bind_shortcuts(
            session,
            &shortcuts,
            None,
            ashpd::desktop::global_shortcuts::BindShortcutsOptions::default(),
        ),
    )
    .await
    .map_err(unavailable)?
    .map_err(unavailable)?;
    request.response().map_err(unavailable)?;
    let listed = portal
        .list_shortcuts(
            session,
            ashpd::desktop::global_shortcuts::ListShortcutsOptions::default(),
        )
        .await
        .map_err(unavailable)?
        .response()
        .map_err(unavailable)?;
    *options.settings.0.shortcuts.lock()? = listed
        .shortcuts()
        .iter()
        .map(|s| (s.id().to_owned(), s.trigger_description().to_owned()))
        .collect();
    options
        .settings
        .0
        .can_configure
        .store(portal.version() >= 2, Ordering::Release);
    if let Some(ready) = ready.take() {
        let _ = ready.send(Ok(()));
    }
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    let mut held = std::collections::HashSet::new();
    loop {
        tokio::select! {
            event = activated.next() => {
                let event = event.ok_or_else(|| unavailable("portal activation stream closed"))?;
                if event.session_handle().as_str() == path && held.insert(event.shortcut_id().to_owned()) {
                    let message = match event.shortcut_id() {
                        "push-to-talk" => Some(Message::Input(true)),
                        "toggle-mute" => Some(Message::Action(Action::ToggleMute)),
                        "toggle-deafen" => Some(Message::Action(Action::ToggleDeafen)), _ => None,
                    };
                    if let Some(message) = message { sender.send(message).map_err(unavailable)?; }
                }
            }
            event = deactivated.next() => {
                let event = event.ok_or_else(|| unavailable("portal deactivation stream closed"))?;
                if event.session_handle().as_str() == path {
                    held.remove(event.shortcut_id());
                    if event.shortcut_id() == "push-to-talk" {
                        sender.send(Message::Input(false)).map_err(unavailable)?;
                    }
                }
            }
            event = changed.next() => {
                let event = event.ok_or_else(|| unavailable("portal bindings stream closed"))?;
                if event.session_handle().as_str() == path {
                    *options.settings.0.shortcuts.lock()? = event.shortcuts().iter()
                        .map(|s| (s.id().to_owned(), s.trigger_description().to_owned())).collect();
                    sender.send(Message::Reset).map_err(unavailable)?;
                    held.clear();
                }
            }
            _ = closed.next() => return Ok(true),
            _ = owner_changed.next() => return Err(unavailable("portal service disconnected; restart the input listener")),
            _ = tick.tick() => {
                if options.settings.0.configure.swap(false, Ordering::AcqRel) {
                    portal.configure_shortcuts(session, None, ashpd::desktop::global_shortcuts::ConfigureShortcutsOptions::default()).await.map_err(unavailable)?;
                }
            }
        }
    }
}
