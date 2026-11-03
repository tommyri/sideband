//! `config.toml` in the data directory.
//!
//! The file is created with comments on first use. A key that is not in the
//! template is an error, so a typo does not silently do nothing. Environment
//! variables override individual fields and ignore unrelated `SS_*` names.

use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use figment::{
    Figment,
    providers::{Format, Serialized, Toml},
};
use serde::{Deserialize, Serialize};

/// Comments written the first time the data directory is prepared.
pub const TEMPLATE: &str = r#"# Sideband. SS_SERVER_NAME, SS_BIND_QUIC, SS_BIND_HTTP, SS_PUBLIC_HOST,
# SS_TLS_MODE, SS_TLS_CERT, SS_TLS_KEY, SS_ACME_EMAIL, SS_ACME_DIRECTORY,
# SS_TLS_CHALLENGE, SS_HTTP_REDIRECT, SS_UPLOAD_MAX_BYTES, SS_LINK_PREVIEWS,
# SS_LINK_PREVIEW_ALLOW, SS_LINK_PREVIEW_DENY, SS_INTERNAL_BIND, and SS_LOG_JSON
# override this file.

[server]
name = "Sideband"
bind_quic = "0.0.0.0:443"
bind_http = "0.0.0.0:4430"
public_host = "localhost"

[tls]
# self-signed, acme, or files.
mode = "self-signed"
# files mode. PEM, leaf certificate first. Relative paths stay in the data directory.
cert_path = ""
key_path = ""
# acme mode. public_host must be a DNS name Let's Encrypt can reach.
acme_email = ""
acme_directory = "https://acme-v02.api.letsencrypt.org/directory"
# http-01 or tls-alpn-01.
challenge = "http-01"
# Compatibility setting. HTTP-01 now always binds TCP 80 and redirects other paths.
http_redirect = false

[limits]
upload_max_bytes = 26214400

[previews]
enabled = true
allow = ""
deny = ""

[internal]
bind = "127.0.0.1:9101"

[logging]
json = false
"#;

const ROOT_KEYS: &[&str] = &["server", "tls", "limits", "previews", "internal", "logging"];
const SERVER_KEYS: &[&str] = &["name", "bind_quic", "bind_http", "public_host"];
/// Let's Encrypt production directory. An empty `acme_directory` means this URL.
pub const ACME_DIRECTORY: &str = "https://acme-v02.api.letsencrypt.org/directory";

const TLS_KEYS: &[&str] = &[
    "mode",
    "cert_path",
    "key_path",
    "acme_email",
    "acme_directory",
    "challenge",
    "http_redirect",
];
const LIMIT_KEYS: &[&str] = &["upload_max_bytes"];
const PREVIEW_KEYS: &[&str] = &["enabled", "allow", "deny"];
const INTERNAL_KEYS: &[&str] = &["bind"];
const LOGGING_KEYS: &[&str] = &["json"];

/// How the process loads its certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TlsMode {
    /// Generate or reuse `tls/identity.json`.
    SelfSigned,
    /// Order a certificate for `public_host`.
    Acme,
    /// PEM files supplied by the operator.
    Files,
}

/// How an ACME server proves control of `public_host`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AcmeChallenge {
    /// `/.well-known/acme-challenge/` on a dedicated TCP 80 listener.
    #[default]
    #[serde(rename = "http-01")]
    Http01,
    /// TCP on the QUIC port, ALPN `acme-tls/1`.
    #[serde(rename = "tls-alpn-01")]
    TlsAlpn01,
}

impl TlsMode {
    fn parse(value: &str) -> Result<Self, Error> {
        match value.trim() {
            "self-signed" => Ok(Self::SelfSigned),
            "acme" => Ok(Self::Acme),
            "files" => Ok(Self::Files),
            _ => Err(Error::Invalid(
                "SS_TLS_MODE must be self-signed, acme, or files",
            )),
        }
    }
}

impl AcmeChallenge {
    fn parse(value: &str) -> Result<Self, Error> {
        match value.trim() {
            "http-01" => Ok(Self::Http01),
            "tls-alpn-01" => Ok(Self::TlsAlpn01),
            _ => Err(Error::Invalid(
                "SS_TLS_CHALLENGE must be http-01 or tls-alpn-01",
            )),
        }
    }
}

/// One process configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Name, bind addresses, and the host clients should use.
    pub server: ServerConfig,
    /// Certificate source.
    pub tls: TlsConfig,
    /// Upload cap.
    pub limits: LimitsConfig,
    /// Link preview settings copied into the database at startup.
    pub previews: PreviewsConfig,
    /// Loopback listener for health and metrics.
    pub internal: InternalConfig,
    /// Log shape.
    pub logging: LoggingConfig,
}

/// `[server]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerConfig {
    /// Shown in `Welcome`.
    pub name: String,
    /// QUIC UDP address.
    pub bind_quic: SocketAddr,
    /// Public HTTP address for history, uploads, and webhooks.
    pub bind_http: SocketAddr,
    /// Host name operators tell clients. ACME orders a certificate for this name.
    pub public_host: String,
}

/// `[tls]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TlsConfig {
    /// `self-signed`, `acme`, or `files`.
    pub mode: TlsMode,
    /// PEM certificate chain for `files` mode. Leaf first.
    pub cert_path: String,
    /// PEM private key for `files` mode.
    pub key_path: String,
    /// Account contact for `acme` mode.
    pub acme_email: String,
    /// ACME directory URL. Empty selects Let's Encrypt production.
    pub acme_directory: String,
    /// HTTP-01 or TLS-ALPN-01.
    pub challenge: AcmeChallenge,
    /// Also bind TCP port 80 for the HTTP-01 challenge and redirect other paths.
    pub http_redirect: bool,
}

/// `[limits]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LimitsConfig {
    /// Largest accepted upload, in bytes.
    pub upload_max_bytes: u64,
}

/// `[previews]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PreviewsConfig {
    /// Fetch Open Graph cards.
    pub enabled: bool,
    /// Comma-separated host allow list. Empty allows every public host.
    pub allow: String,
    /// Comma-separated host deny list.
    pub deny: String,
}

/// `[internal]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct InternalConfig {
    /// `/healthz` and `/metrics`.
    pub bind: SocketAddr,
}

/// `[logging]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct LoggingConfig {
    /// JSON lines instead of the text format.
    pub json: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            name: "Sideband".to_owned(),
            bind_quic: SocketAddr::from(([0, 0, 0, 0], 443)),
            bind_http: SocketAddr::from(([0, 0, 0, 0], 4430)),
            public_host: "localhost".to_owned(),
        }
    }
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            mode: TlsMode::SelfSigned,
            cert_path: String::new(),
            key_path: String::new(),
            acme_email: String::new(),
            acme_directory: ACME_DIRECTORY.to_owned(),
            challenge: AcmeChallenge::Http01,
            http_redirect: false,
        }
    }
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            upload_max_bytes: 25 * 1024 * 1024,
        }
    }
}

impl Default for PreviewsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allow: String::new(),
            deny: String::new(),
        }
    }
}

impl Default for InternalConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 9101)),
        }
    }
}

/// A config file or environment value the process should not start with.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The file could not be read or written.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// A key is not part of the template.
    #[error("unknown config key `{0}`")]
    Unknown(String),
    /// A value could not be parsed.
    #[error("{0}")]
    Invalid(&'static str),
    /// Figment rejected the document.
    #[error("{0}")]
    Figment(Box<figment::Error>),
    /// The TOML document is not a table.
    #[error("{0}")]
    Toml(#[from] toml::de::Error),
}

/// `$XDG_DATA_HOME/socialspeak`, or `./data` when that variable is unset.
#[must_use]
pub fn default_data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME").map_or_else(
        || PathBuf::from("./data"),
        |home| PathBuf::from(home).join("socialspeak"),
    )
}

/// Path of the config file inside a data directory.
#[must_use]
pub fn path_in(data_dir: &Path) -> PathBuf {
    data_dir.join("config.toml")
}

/// Writes [`TEMPLATE`] when the file is absent. Does not overwrite.
pub fn ensure_file(data_dir: &Path) -> Result<PathBuf, Error> {
    fs::create_dir_all(data_dir)?;
    let path = path_in(data_dir);
    if !path.is_file() {
        fs::write(&path, TEMPLATE)?;
    }
    Ok(path)
}

/// Loads `data_dir/config.toml` and applies `SS_*` overrides.
pub fn load(data_dir: &Path) -> Result<Config, Error> {
    let path = path_in(data_dir);
    let text = fs::read_to_string(&path)?;
    parse(&text)
}

/// Parses TOML and applies `SS_*` overrides.
pub fn parse(text: &str) -> Result<Config, Error> {
    reject_unknown(text)?;
    let mut config: Config = Figment::from(Serialized::defaults(Config::default()))
        .merge(Toml::string(text))
        .extract()
        .map_err(|error| Error::Figment(Box::new(error)))?;
    apply_env(&mut config)?;
    Ok(config)
}

fn reject_unknown(text: &str) -> Result<(), Error> {
    let value: toml::Value = toml::from_str(text)?;
    let Some(root) = value.as_table() else {
        return Err(Error::Invalid("config.toml must be a table"));
    };
    for (key, value) in root {
        let allowed = match key.as_str() {
            "server" => SERVER_KEYS,
            "tls" => TLS_KEYS,
            "limits" => LIMIT_KEYS,
            "previews" => PREVIEW_KEYS,
            "internal" => INTERNAL_KEYS,
            "logging" => LOGGING_KEYS,
            other if ROOT_KEYS.contains(&other) => continue,
            other => return Err(Error::Unknown(other.to_owned())),
        };
        let Some(table) = value.as_table() else {
            return Err(Error::Invalid("each config section must be a table"));
        };
        for child in table.keys() {
            if !allowed.contains(&child.as_str()) {
                return Err(Error::Unknown(format!("{key}.{child}")));
            }
        }
    }
    Ok(())
}

fn apply_env(config: &mut Config) -> Result<(), Error> {
    if let Some(value) = env("SS_SERVER_NAME") {
        config.server.name = value;
    }
    if let Some(value) = env("SS_BIND_QUIC") {
        config.server.bind_quic = parse_addr(&value, "SS_BIND_QUIC")?;
    }
    if let Some(value) = env("SS_BIND_HTTP") {
        config.server.bind_http = parse_addr(&value, "SS_BIND_HTTP")?;
    }
    if let Some(value) = env("SS_PUBLIC_HOST") {
        config.server.public_host = value;
    }
    if let Some(value) = env("SS_TLS_MODE") {
        config.tls.mode = TlsMode::parse(&value)?;
    }
    if let Some(value) = env("SS_TLS_CERT") {
        config.tls.cert_path = value;
    }
    if let Some(value) = env("SS_TLS_KEY") {
        config.tls.key_path = value;
    }
    if let Some(value) = env("SS_ACME_EMAIL") {
        config.tls.acme_email = value;
    }
    if let Some(value) = env("SS_ACME_DIRECTORY") {
        config.tls.acme_directory = value;
    }
    if let Some(value) = env("SS_TLS_CHALLENGE") {
        config.tls.challenge = AcmeChallenge::parse(&value)?;
    }
    if let Some(value) = env("SS_HTTP_REDIRECT") {
        config.tls.http_redirect = matches!(value.as_str(), "1" | "on" | "true");
    }
    if let Some(value) = env("SS_UPLOAD_MAX_BYTES") {
        config.limits.upload_max_bytes = value
            .parse()
            .map_err(|_| Error::Invalid("SS_UPLOAD_MAX_BYTES must be a number"))?;
    }
    if let Some(value) = env("SS_LINK_PREVIEWS") {
        config.previews.enabled = matches!(value.as_str(), "1" | "on" | "true");
    }
    if let Some(value) = env("SS_LINK_PREVIEW_ALLOW") {
        config.previews.allow = value;
    }
    if let Some(value) = env("SS_LINK_PREVIEW_DENY") {
        config.previews.deny = value;
    }
    if let Some(value) = env("SS_INTERNAL_BIND") {
        config.internal.bind = parse_addr(&value, "SS_INTERNAL_BIND")?;
    }
    if let Some(value) = env("SS_LOG_JSON") {
        config.logging.json = matches!(value.as_str(), "1" | "true" | "on");
    }
    Ok(())
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

fn parse_addr(value: &str, name: &'static str) -> Result<SocketAddr, Error> {
    value.parse().map_err(|_| match name {
        "SS_BIND_QUIC" => Error::Invalid("SS_BIND_QUIC must be host:port"),
        "SS_BIND_HTTP" => Error::Invalid("SS_BIND_HTTP must be host:port"),
        _ => Error::Invalid("SS_INTERNAL_BIND must be host:port"),
    })
}

#[cfg(test)]
mod tests {
    use super::{ACME_DIRECTORY, AcmeChallenge, TEMPLATE, TlsMode, parse};

    #[test]
    fn the_template_loads_and_a_typo_is_rejected() {
        let config = parse(TEMPLATE).unwrap();
        assert_eq!(config.server.name, "Sideband");
        assert_eq!(config.tls.mode, TlsMode::SelfSigned);
        assert_eq!(config.tls.challenge, AcmeChallenge::Http01);
        assert_eq!(config.tls.acme_directory, ACME_DIRECTORY);
        assert!(!config.tls.http_redirect);
        assert_eq!(config.internal.bind.port(), 9101);
        let typo = TEMPLATE.replace("public_host", "public_hst");
        let error = parse(&typo).unwrap_err();
        assert!(error.to_string().contains("server.public_hst"), "{error}");
    }
}
