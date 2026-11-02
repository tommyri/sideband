#![allow(
    clippy::unused_self,
    clippy::needless_pass_by_value,
    reason = "D-Bus interface methods require instance receivers and owned wire arguments"
)]
use super::*;
use ashpd::zbus::{
    self, Connection,
    message::Header,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};
use std::{collections::HashMap, sync::atomic::AtomicUsize, time::Instant};

type Dict = HashMap<String, OwnedValue>;
type Shortcuts = Vec<(String, Dict)>;

#[derive(Default)]
struct State {
    sessions: Mutex<Vec<String>>,
    configured: AtomicUsize,
}
struct Desktop(Arc<State>);
struct MockSession;

#[zbus::interface(name = "org.freedesktop.portal.Session")]
impl MockSession {
    fn close(&self) {}
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }
}

fn owned(value: &str) -> OwnedValue {
    Value::from(value).try_into().unwrap()
}

fn shortcuts() -> Shortcuts {
    ["push-to-talk", "toggle-mute", "toggle-deafen"]
        .into_iter()
        .map(|id| {
            (
                id.to_owned(),
                HashMap::from([
                    ("description".to_owned(), owned(id)),
                    ("trigger_description".to_owned(), owned("F13")),
                ]),
            )
        })
        .collect()
}

async fn response(
    connection: &Connection,
    header: &Header<'_>,
    options: &Dict,
    result: Dict,
) -> zbus::fdo::Result<OwnedObjectPath> {
    let sender = header.sender().unwrap().as_str();
    let token = <&str>::try_from(options.get("handle_token").unwrap()).unwrap();
    let path = format!(
        "/org/freedesktop/portal/desktop/request/{}/{token}",
        sender.trim_start_matches(':').replace('.', "_")
    );
    connection
        .emit_signal(
            Some(sender),
            path.as_str(),
            "org.freedesktop.portal.Request",
            "Response",
            &(0_u32, result),
        )
        .await
        .unwrap();
    Ok(path.try_into().unwrap())
}

#[zbus::interface(name = "org.freedesktop.portal.GlobalShortcuts")]
impl Desktop {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
    }

    async fn create_session(
        &self,
        options: Dict,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let sender = header.sender().unwrap().as_str();
        let token = <&str>::try_from(options.get("session_handle_token").unwrap()).unwrap();
        let path = format!(
            "/org/freedesktop/portal/desktop/session/{}/{token}",
            sender.trim_start_matches(':').replace('.', "_")
        );
        connection
            .object_server()
            .at(path.as_str(), MockSession)
            .await
            .unwrap();
        self.0.sessions.lock().unwrap().push(path.clone());
        response(
            connection,
            &header,
            &options,
            HashMap::from([("session_handle".to_owned(), owned(&path))]),
        )
        .await
    }
    async fn bind_shortcuts(
        &self,
        session: OwnedObjectPath,
        shortcuts: Shortcuts,
        parent_window: String,
        options: Dict,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        assert_ne!(session.as_str(), "");
        assert_eq!(parent_window, "");
        assert_eq!(
            shortcuts.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(),
            ["push-to-talk", "toggle-mute", "toggle-deafen"]
        );
        response(connection, &header, &options, HashMap::new()).await
    }
    async fn list_shortcuts(
        &self,
        session: OwnedObjectPath,
        options: Dict,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        assert_ne!(session.as_str(), "");
        let value = Value::from(shortcuts());
        response(
            connection,
            &header,
            &options,
            HashMap::from([("shortcuts".to_owned(), value.try_into().unwrap())]),
        )
        .await
    }
    fn configure_shortcuts(&self, session: OwnedObjectPath, parent_window: String, options: Dict) {
        assert_ne!(session.as_str(), "");
        assert_eq!(parent_window, "");
        assert!(options.is_empty());
        self.0.configured.fetch_add(1, Ordering::SeqCst);
    }
}

#[track_caller]
fn wait_for(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for portal event"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn signal(runtime: &Runtime, connection: &Connection, path: &str, signal: &str, id: &str) {
    let path = OwnedObjectPath::try_from(path).unwrap();
    runtime
        .block_on(connection.emit_signal(
            None::<&str>,
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.GlobalShortcuts",
            signal,
            &(path, id, 0_u64, Dict::new()),
        ))
        .unwrap();
}

#[test]
#[ignore = "run under dbus-run-session on Linux; owns the test portal bus name"]
fn bind_activate_deactivate_session_loss_and_rebind() {
    assert!(!available());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let state = Arc::new(State::default());
    let connection = runtime.block_on(async {
        zbus::connection::Builder::session()
            .unwrap()
            .name("org.freedesktop.portal.Desktop")
            .unwrap()
            .serve_at("/org/freedesktop/portal/desktop", Desktop(state.clone()))
            .unwrap()
            .build()
            .await
            .unwrap()
    });
    let options = Options {
        push_to_talk: Some("F13".to_owned()),
        ..Options::default()
    };
    assert!(available());
    assert!(runtime.block_on(async { available() }));
    let settings = options.settings.clone();
    let mut source = Source::open(options).unwrap();
    assert_eq!(settings.shortcuts().unwrap().len(), 3);
    let first = state.sessions.lock().unwrap()[0].clone();
    signal(&runtime, &connection, &first, "Activated", "push-to-talk");
    wait_for(|| {
        source.poll().unwrap().iter().any(|e| {
            matches!(
                e,
                SourceEvent::Input(InputEvent::Key {
                    code: Code::F24,
                    pressed: true
                })
            )
        })
    });
    signal(&runtime, &connection, &first, "Deactivated", "push-to-talk");
    wait_for(|| {
        source.poll().unwrap().iter().any(|e| {
            matches!(
                e,
                SourceEvent::Input(InputEvent::Key {
                    code: Code::F24,
                    pressed: false
                })
            )
        })
    });
    settings.configure().unwrap();
    wait_for(|| {
        source.poll().unwrap();
        state.configured.load(Ordering::SeqCst) == 1
    });
    runtime
        .block_on(connection.emit_signal(
            None::<&str>,
            first.as_str(),
            "org.freedesktop.portal.Session",
            "Closed",
            &Dict::new(),
        ))
        .unwrap();
    wait_for(|| {
        source
            .poll()
            .unwrap()
            .iter()
            .any(|e| matches!(e, SourceEvent::Reset))
    });
    wait_for(|| {
        state.sessions.lock().unwrap().len() == 2 && settings.shortcuts().unwrap().len() == 3
    });
    let second = state.sessions.lock().unwrap()[1].clone();
    // Stale sessions cannot reopen transmit after reconnect.
    signal(&runtime, &connection, &first, "Activated", "push-to-talk");
    signal(&runtime, &connection, &second, "Activated", "push-to-talk");
    wait_for(|| {
        source
            .poll()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, SourceEvent::Input(InputEvent::Key { pressed: true, .. })))
            .count()
            == 1
    });
    drop(source);
    assert_eq!(settings.shortcuts().unwrap().len(), 0);
    assert!(settings.configure().is_err());
    // A stopped listener must not leave a connection tied to its dead runtime.
    drop(Source::open(Options::default()).unwrap());
}
