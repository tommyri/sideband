//! Spacebar gate. Terminals that report key-up use hold-to-talk. Others toggle,
//! because a cooked or vanilla Unix terminal never delivers the release.

use std::{
    io::{self, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    terminal,
};

/// Read the terminal until `stop` is set or the user presses q or Esc.
pub(crate) fn spawn(talking: Arc<AtomicBool>, stop: Arc<AtomicBool>) -> io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("ss-cli-keys".to_owned())
        .spawn(move || {
            if let Err(error) = run(&talking, &stop) {
                tracing::warn!(%error, "terminal input stopped");
                stop.store(true, Ordering::Relaxed);
            }
        })
}

fn run(talking: &AtomicBool, stop: &AtomicBool) -> io::Result<()> {
    let _raw = RawMode::enable()?;
    let mut hold = cfg!(windows);
    let stderr = io::stderr();
    let mut stderr = stderr.lock();
    if hold {
        writeln!(stderr, "hold space to talk; q quits")?;
    } else {
        writeln!(
            stderr,
            "space toggles the microphone (this terminal does not report key release); q quits"
        )?;
    }
    while !stop.load(Ordering::Relaxed) {
        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            hold = true;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                stop.store(true, Ordering::Relaxed);
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                stop.store(true, Ordering::Relaxed);
            }
            KeyCode::Char(' ') => match key.kind {
                KeyEventKind::Release => talking.store(false, Ordering::Relaxed),
                KeyEventKind::Press | KeyEventKind::Repeat if hold => {
                    talking.store(true, Ordering::Relaxed);
                }
                KeyEventKind::Press => {
                    let current = talking.load(Ordering::Relaxed);
                    talking.store(!current, Ordering::Relaxed);
                }
                KeyEventKind::Repeat => {}
            },
            _ => {}
        }
    }
    talking.store(false, Ordering::Relaxed);
    Ok(())
}

struct RawMode;

impl RawMode {
    fn enable() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}
