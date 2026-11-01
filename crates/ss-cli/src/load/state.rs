//! Bot accounts kept between runs so a soak does not re-register.
//!
//! Registration costs an argon2 hash and one of the five per-minute login
//! slots per source address. A session token skips both. The file is plain
//! text, one record per line:
//!
//! ```text
//! invite <code>
//! user <username> <password> <token>
//! ```

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs,
    hash::{BuildHasher, Hasher},
    io::{self, Write},
    path::{Path, PathBuf},
};

/// Credentials for one bot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// Random password used at registration and as a fallback login.
    pub password: String,
    /// Session token from the last `Welcome`.
    pub token: String,
}

/// Parsed state file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    /// Multi-use invite the bots register with.
    pub invite: Option<String>,
    /// Accounts by username.
    pub accounts: BTreeMap<String, Account>,
}

/// A line the parser did not understand.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    /// Read or write failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Malformed record.
    #[error("{path}:{line}: expected `invite CODE` or `user NAME PASSWORD TOKEN`")]
    Syntax {
        /// File that failed.
        path: PathBuf,
        /// One-based line number.
        line: usize,
    },
}

impl State {
    /// Reads `path`. A missing file is an empty state.
    pub fn load(path: &Path) -> Result<Self, StateError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error.into()),
        };
        Self::parse(&text).map_err(|line| StateError::Syntax {
            path: path.to_owned(),
            line,
        })
    }

    /// Parses file text. The error is the one-based failing line.
    pub fn parse(text: &str) -> Result<Self, usize> {
        let mut state = Self::default();
        for (index, line) in text.lines().enumerate() {
            let fields: Vec<&str> = line.split_whitespace().collect();
            match fields.as_slice() {
                [] => {}
                [first, ..] if first.starts_with('#') => {}
                ["invite", code] => state.invite = Some((*code).to_owned()),
                ["user", name, password, token] => {
                    state.accounts.insert(
                        (*name).to_owned(),
                        Account {
                            password: (*password).to_owned(),
                            token: (*token).to_owned(),
                        },
                    );
                }
                _ => return Err(index + 1),
            }
        }
        Ok(state)
    }

    /// File text for [`Self::parse`].
    pub fn render(&self) -> String {
        let mut text =
            String::from("# ss-cli load accounts. Tokens are credentials; keep private.\n");
        if let Some(invite) = &self.invite {
            text.push_str("invite ");
            text.push_str(invite);
            text.push('\n');
        }
        for (name, account) in &self.accounts {
            let _ = writeln!(text, "user {name} {} {}", account.password, account.token);
        }
        text
    }

    /// Writes atomically next to `path` with owner-only permissions on Unix.
    pub fn save(&self, path: &Path) -> Result<(), StateError> {
        let temporary = path.with_extension("tmp");
        {
            let mut options = fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(self.render().as_bytes())?;
            file.sync_all()?;
        }
        fs::rename(&temporary, path)?;
        Ok(())
    }
}

/// 128 random bits as hex, from the standard library's OS-seeded hasher keys.
pub fn random_secret() -> String {
    let mut text = String::with_capacity(32);
    for round in 0_u8..2 {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u8(round);
        let _ = write!(text, "{:016x}", hasher.finish());
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_and_parse_round_trip() {
        let mut state = State {
            invite: Some("abc".to_owned()),
            ..State::default()
        };
        state.accounts.insert(
            "load001".to_owned(),
            Account {
                password: "p".repeat(16),
                token: "t0k".to_owned(),
            },
        );
        assert_eq!(State::parse(&state.render()), Ok(state));
    }

    #[test]
    fn blank_and_comment_lines_are_ignored_and_junk_is_located() {
        assert_eq!(State::parse("\n# note\n"), Ok(State::default()));
        assert_eq!(State::parse("invite a\nuser x y\n"), Err(2));
    }

    #[test]
    fn save_then_load_and_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.txt");
        assert_eq!(State::load(&path).unwrap(), State::default());
        let state = State {
            invite: Some("code".to_owned()),
            ..State::default()
        };
        state.save(&path).unwrap();
        assert_eq!(State::load(&path).unwrap(), state);
    }

    #[test]
    fn secrets_are_long_and_distinct() {
        let first = random_secret();
        assert_eq!(first.len(), 32);
        assert_ne!(first, random_secret());
    }
}
