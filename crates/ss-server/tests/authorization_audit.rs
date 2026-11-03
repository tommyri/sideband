//! Authorization audit: every control command and HTTP route is classified,
//! and every classified handler reaches the checks its class demands.
//!
//! This reads the server's own source, so it is a lint, not a behavior test.
//! It fails when:
//!
//! - `ClientEnvelope` gains a command that [`COMMANDS`] does not list;
//! - a handler file gains a `Msg::X(..) =>` dispatch arm for a command that is
//!   not listed, or that [`COMMANDS`] says nothing handles yet;
//! - a listed handler no longer reaches `Permissions::BIT` plus a permission
//!   check call, or no longer reaches one of its guard calls;
//! - an HTTP route is added that [`ROUTES`] does not list, or a listed one
//!   stops reaching its checks.
//!
//! "Reaches" means the handler body, or any function in the same file it
//! calls, transitively. Strings and comments are stripped first, so a name
//! in a log message does not count as a check. Behavior is covered by the
//! table-driven tests in `permissions.rs` and the per-feature suites.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

/// What a handler must reach.
#[derive(Clone, Copy)]
struct Policy {
    /// `Permissions::BIT` names. Non-empty also requires a check call.
    perms: &'static [&'static str],
    /// Calls or comparisons that scope the command (membership, ownership,
    /// role hierarchy, token checks).
    guards: &'static [&'static str],
    /// Why the command needs no permission bit. Required when `perms` is empty.
    reason: &'static str,
}

const fn perms(perms: &'static [&'static str]) -> Policy {
    Policy {
        perms,
        guards: &[],
        reason: "",
    }
}

const fn perms_and(perms: &'static [&'static str], guards: &'static [&'static str]) -> Policy {
    Policy {
        perms,
        guards,
        reason: "",
    }
}

const fn scoped(guards: &'static [&'static str], reason: &'static str) -> Policy {
    Policy {
        perms: &[],
        guards,
        reason,
    }
}

const fn open(reason: &'static str) -> Policy {
    Policy {
        perms: &[],
        guards: &[],
        reason,
    }
}

/// How one `ClientEnvelope` variant is handled.
enum Command {
    /// Answered before or outside handler dispatch.
    Transport(&'static str),
    /// Dispatched in the listed files, each with its own policy. A dispatch
    /// arm in any other file fails until it is classified here.
    Handled(&'static [(&'static str, Policy)]),
}

const CHANNEL_ADMIN: &[&str] = &["MANAGE_CHANNELS"];

/// Every `ClientEnvelope` variant, by message name.
const COMMANDS: &[(&str, Command)] = &[
    (
        "Hello",
        Command::Transport("authentication itself; connection.rs runs it before dispatch"),
    ),
    (
        "Ping",
        Command::Transport("keepalive answered inline; no state change"),
    ),
    (
        "JoinVoice",
        Command::Handled(&[(
            "voice/session.rs",
            perms(&["VIEW_CHANNEL", "CONNECT", "SPEAK"]),
        )]),
    ),
    (
        "LeaveVoice",
        Command::Handled(&[(
            "voice/session.rs",
            open("only removes the caller's own connection from voice"),
        )]),
    ),
    (
        "SetVoiceState",
        Command::Handled(&[(
            "voice/session.rs",
            open("only changes the caller's own self-mute and self-deaf flags"),
        )]),
    ),
    (
        "SendMessage",
        Command::Handled(&[(
            "handlers/messages.rs",
            perms_and(
                &[
                    "VIEW_CHANNEL",
                    "SEND_MESSAGES",
                    "ATTACH_FILES",
                    "MENTION_EVERYONE",
                ],
                &["claimable_files("],
            ),
        )]),
    ),
    (
        "EditMessage",
        Command::Handled(&[(
            "handlers/messages.rs",
            perms_and(
                &["VIEW_CHANNEL", "MENTION_EVERYONE"],
                &["author_id != Some(actor)"],
            ),
        )]),
    ),
    (
        "DeleteMessage",
        Command::Handled(&[(
            "handlers/messages.rs",
            perms_and(
                &["VIEW_CHANNEL", "MANAGE_MESSAGES"],
                &["author_id != Some(actor)"],
            ),
        )]),
    ),
    (
        "Typing",
        Command::Handled(&[(
            "handlers/messages.rs",
            perms(&["VIEW_CHANNEL", "SEND_MESSAGES"]),
        )]),
    ),
    (
        "MarkRead",
        Command::Handled(&[("handlers/messages.rs", perms(&["VIEW_CHANNEL"]))]),
    ),
    (
        "CreateDm",
        Command::Handled(&[(
            "handlers/dm.rs",
            scoped(
                &["participants(", "reachable("],
                "membership is the grant; disabled and banned users cannot be added",
            ),
        )]),
    ),
    (
        "LeaveDm",
        Command::Handled(&[(
            "handlers/dm.rs",
            scoped(
                &["membership("],
                "only removes the caller from a DM they are in",
            ),
        )]),
    ),
    (
        "AddDmMembers",
        Command::Handled(&[(
            "handlers/dm.rs",
            scoped(
                &["membership(", "created_by != actor"],
                "only the group DM's creator may add people",
            ),
        )]),
    ),
    (
        "CreateWebhook",
        Command::Handled(&[("handlers/webhooks.rs", perms(&["MANAGE_SERVER"]))]),
    ),
    (
        "DeleteWebhook",
        Command::Handled(&[("handlers/webhooks.rs", perms(&["MANAGE_SERVER"]))]),
    ),
    (
        "CreateChannel",
        Command::Handled(&[("handlers/channels.rs", perms(CHANNEL_ADMIN))]),
    ),
    (
        "UpdateChannel",
        Command::Handled(&[
            ("handlers/channels.rs", perms(CHANNEL_ADMIN)),
            (
                "handlers/dm.rs",
                scoped(&["membership("], "any member may rename their group DM"),
            ),
        ]),
    ),
    (
        "DeleteChannel",
        Command::Handled(&[("handlers/channels.rs", perms(CHANNEL_ADMIN))]),
    ),
    (
        "MoveChannel",
        Command::Handled(&[("handlers/channels.rs", perms(CHANNEL_ADMIN))]),
    ),
    (
        "CreateInvite",
        Command::Handled(&[("handlers/moderation.rs", perms(&["CREATE_INVITES"]))]),
    ),
    (
        "RevokeInvite",
        Command::Handled(&[(
            "handlers/moderation.rs",
            perms_and(
                &["CREATE_INVITES", "MANAGE_SERVER"],
                &["created_by == actor"],
            ),
        )]),
    ),
    (
        "KickUser",
        Command::Handled(&[(
            "handlers/moderation.rs",
            perms_and(&["KICK_MEMBERS"], &["can_act_on("]),
        )]),
    ),
    (
        "BanUser",
        Command::Handled(&[(
            "handlers/moderation.rs",
            perms_and(&["BAN_MEMBERS"], &["can_act_on("]),
        )]),
    ),
    (
        "UnbanUser",
        Command::Handled(&[(
            "handlers/moderation.rs",
            perms_and(&["BAN_MEMBERS"], &["can_act_on("]),
        )]),
    ),
    (
        "MoveUser",
        Command::Handled(&[(
            "handlers/moderation.rs",
            perms_and(&["MOVE_MEMBERS"], &["can_act_on("]),
        )]),
    ),
    (
        "ServerMuteUser",
        Command::Handled(&[(
            "voice/session.rs",
            perms_and(&["MUTE_MEMBERS"], &["can_act_on("]),
        )]),
    ),
    (
        "CreateRole",
        Command::Handled(&[(
            "handlers/roles.rs",
            perms_and(
                &["MANAGE_ROLES"],
                &["ensure_position(", "ensure_admin_bits("],
            ),
        )]),
    ),
    (
        "UpdateRole",
        Command::Handled(&[(
            "handlers/roles.rs",
            perms_and(&["MANAGE_ROLES"], &["ensure_can_edit("]),
        )]),
    ),
    (
        "DeleteRole",
        Command::Handled(&[(
            "handlers/roles.rs",
            perms_and(&["MANAGE_ROLES"], &["ensure_can_edit("]),
        )]),
    ),
    (
        "SetUserRoles",
        Command::Handled(&[(
            "handlers/roles.rs",
            perms_and(&["MANAGE_ROLES"], &["can_act_on(", "ensure_can_edit("]),
        )]),
    ),
    (
        "SetChannelOverride",
        Command::Handled(&[(
            "handlers/roles.rs",
            perms_and(&["MANAGE_CHANNELS"], &["ensure_can_edit("]),
        )]),
    ),
    (
        "UpdateProfile",
        Command::Handled(&[(
            "handlers/profile.rs",
            perms_and(&["CHANGE_NICKNAME"], &["owned_avatar("]),
        )]),
    ),
    (
        "SetPresence",
        Command::Handled(&[(
            "handlers/presence.rs",
            open("only changes the caller's own presence"),
        )]),
    ),
    (
        "StartStream",
        Command::Handled(&[(
            "media/streams.rs",
            perms_and(&["VIEW_CHANNEL", "STREAM"], &["seat("]),
        )]),
    ),
    (
        "StopStream",
        Command::Handled(&[(
            "media/streams.rs",
            scoped(
                &["owner == conn"],
                "removes only the caller's own publication; stays idempotent after \
                 permissions are lost",
            ),
        )]),
    ),
    (
        "SubscribeTrack",
        Command::Handled(&[("media/streams.rs", VIDEO_VIEWER)]),
    ),
    (
        "UnsubscribeTrack",
        Command::Handled(&[("media/streams.rs", VIDEO_VIEWER)]),
    ),
    (
        "RequestKeyframe",
        Command::Handled(&[("media/streams.rs", VIDEO_VIEWER)]),
    ),
    (
        "SetPreferredQuality",
        Command::Handled(&[("media/streams.rs", VIDEO_VIEWER)]),
    ),
];

/// Viewer-side video commands: a voice seat in the track's channel plus
/// `VIEW_CHANNEL` there; a publisher cannot subscribe to itself.
const VIDEO_VIEWER: Policy = perms_and(
    &["VIEW_CHANNEL"],
    &["seat(", "track.channel != channel", "track.owner == conn"],
);

/// Every HTTP route, by `(file, method, path)`.
const ROUTES: &[(&str, &str, &str, Policy)] = &[
    (
        "http/messages.rs",
        "get",
        "/api/v1/channels/{channel_id}/messages",
        perms_and(&["VIEW_CHANNEL"], &["caller("]),
    ),
    (
        "http/messages.rs",
        "get",
        "/api/v1/channels/{channel_id}/messages/around/{message_id}",
        perms_and(&["VIEW_CHANNEL"], &["caller("]),
    ),
    (
        "http/attachments.rs",
        "post",
        "/api/v1/attachments",
        perms_and(&["VIEW_CHANNEL", "ATTACH_FILES"], &["caller("]),
    ),
    (
        "http/attachments.rs",
        "get",
        "/api/v1/attachments/{attachment_id}/{filename}",
        perms_and(&["VIEW_CHANNEL"], &["authorize(", "caller(", "may_read("]),
    ),
    (
        "http/audit.rs",
        "get",
        "/api/v1/audit-log",
        perms_and(&["VIEW_AUDIT_LOG"], &["caller("]),
    ),
    (
        "http/webhooks.rs",
        "post",
        "/api/v1/webhooks/{webhook_id}/{token}",
        scoped(
            &["webhooks.execute("],
            "no bearer; Webhooks::execute compares the token hash before posting",
        ),
    ),
    (
        "http/info.rs",
        "get",
        "/api/v1/server",
        open("public server description and certificate fingerprint"),
    ),
    (
        "tls.rs",
        "get",
        "/.well-known/acme-challenge/{token}",
        open("ACME HTTP-01 key authorization; public by protocol design"),
    ),
    (
        "http/internal.rs",
        "get",
        "/healthz",
        open("internal operator listener only; loopback by default"),
    ),
    (
        "http/internal.rs",
        "get",
        "/metrics",
        open("internal operator listener only; loopback by default"),
    ),
    (
        "http/internal.rs",
        "get",
        "/connections",
        open("redacted diagnostics on the internal operator listener; loopback by default"),
    ),
];

/// Files that contain `Msg::X(..) =>` arms but are not authenticated
/// handlers. The spike accepts any username by design and binds loopback.
const NOT_HANDLERS: &[(&str, &str)] = &[
    ("lib.rs", "unauthenticated voice spike, loopback only"),
    (
        "connection.rs",
        "authenticated dispatcher; Ping and Hello only",
    ),
];

/// Calls that perform a permission check.
const CHECK_CALLS: &[&str] = &[".check(", ".require(", ".allow("];

#[test]
fn every_client_command_is_classified() {
    let proto = fs::read_to_string(proto_dir().join("envelope.proto")).unwrap();
    let variants = client_variants(&proto);
    assert!(variants.len() > 30, "parsed {variants:?}");
    let listed: BTreeSet<&str> = COMMANDS.iter().map(|(name, _)| *name).collect();
    let missing: Vec<_> = variants.difference(&listed).collect();
    let stale: Vec<_> = listed.difference(&variants).collect();
    assert!(
        missing.is_empty(),
        "ClientEnvelope commands without an authorization policy in \
         tests/authorization_audit.rs: {missing:?}"
    );
    assert!(
        stale.is_empty(),
        "policies for commands that no longer exist: {stale:?}"
    );
    for (name, command) in COMMANDS {
        match command {
            Command::Transport(reason) => assert!(!reason.is_empty(), "{name}"),
            Command::Handled(handlers) => {
                for (file, policy) in *handlers {
                    assert!(
                        !policy.perms.is_empty() || !policy.reason.is_empty(),
                        "{name} in {file}: a policy without permission bits needs a reason"
                    );
                }
            }
        }
    }
}

#[test]
fn every_dispatch_arm_reaches_its_checks() {
    let sources = Sources::load();
    let arms = sources.dispatch_arms();
    let mut failures = Vec::new();
    let mut seen = BTreeSet::new();
    for arm in &arms {
        seen.insert((arm.command.clone(), arm.file.clone()));
        let Some((_, command)) = COMMANDS.iter().find(|(name, _)| *name == arm.command) else {
            failures.push(format!(
                "{}: `Msg::{}` is dispatched but has no policy",
                arm.file, arm.command
            ));
            continue;
        };
        let policy = match command {
            Command::Handled(handlers) => handlers
                .iter()
                .find(|(file, _)| *file == arm.file)
                .map(|(_, policy)| *policy),
            Command::Transport(_) => None,
        };
        let Some(policy) = policy else {
            failures.push(format!(
                "{}: `Msg::{}` is dispatched to `{}` but the audit does not list this file \
                 for it; classify it in COMMANDS",
                arm.file, arm.command, arm.handler
            ));
            continue;
        };
        let reached = sources.reachable(&arm.file, &arm.handler);
        failures.extend(policy_failures(
            &format!("{}: `Msg::{}` -> `{}`", arm.file, arm.command, arm.handler),
            policy,
            &reached,
        ));
    }
    for (name, command) in COMMANDS {
        if let Command::Handled(handlers) = command {
            for (file, _) in *handlers {
                if !seen.contains(&((*name).to_owned(), (*file).to_owned())) {
                    failures.push(format!(
                        "{file}: expected a dispatch arm for `Msg::{name}`; the audit is stale"
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "authorization audit failed:\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn every_http_route_reaches_its_checks() {
    let sources = Sources::load();
    let routes = sources.routes();
    let mut failures = Vec::new();
    for route in &routes {
        let Some((_, _, _, policy)) = ROUTES.iter().find(|(file, method, path, _)| {
            *file == route.file && *method == route.method && *path == route.path
        }) else {
            failures.push(format!(
                "{}: `{} {}` -> `{}` has no policy in ROUTES",
                route.file,
                route.method.to_uppercase(),
                route.path,
                route.handler
            ));
            continue;
        };
        let reached = sources.reachable(&route.file, &route.handler);
        failures.extend(policy_failures(
            &format!(
                "{}: `{} {}` -> `{}`",
                route.file,
                route.method.to_uppercase(),
                route.path,
                route.handler
            ),
            *policy,
            &reached,
        ));
    }
    for (file, method, path, _) in ROUTES {
        if !routes
            .iter()
            .any(|route| route.file == *file && route.method == *method && route.path == *path)
        {
            failures.push(format!(
                "{file}: expected route `{method} {path}`; the audit is stale"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "authorization audit failed:\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn no_file_hides_commands_behind_an_alias() {
    let sources = Sources::load();
    let hidden: Vec<_> = sources
        .stripped
        .iter()
        .filter(|(_, source)| source.text.contains("client_envelope::Msg as"))
        .map(|(file, _)| file.clone())
        .collect();
    assert!(
        hidden.is_empty(),
        "the audit looks for `Msg::`; these files alias the command enum: {hidden:?}"
    );
}

#[test]
fn the_scanner_follows_or_patterns() {
    let text = strip(
        "match msg { Msg::A(_) | Msg::B(_) => Some(self.handle(msg)), \
         Msg::C(c) => c.id, _ => None }",
    );
    let arms: Vec<_> = arms_in("demo.rs", &text)
        .iter()
        .map(|arm| format!("{}->{}", arm.command, arm.handler))
        .collect();
    assert_eq!(arms, ["A->handle", "B->handle"]);
    assert!(or_pattern_call("| Msg::B(_)) { return; } => self.x(").is_none());
}

#[test]
fn the_scanner_sees_through_helpers_and_ignores_strings() {
    let source = r##"
        fn handler(&self) {
            tracing::info!("Permissions::KICK_MEMBERS in a string does not count");
            // Permissions::BAN_MEMBERS in a comment does not count
            let _ = r#"{"Permissions::MUTE_MEMBERS"}"#;
            let _ = '{';
            self.require(actor, Permissions::MANAGE_ROLES)?;
            helper();
        }
        fn helper() { guard_call(); }
        fn require(&self) { self.perms.check(actor, None, permission) }
    "##;
    let stripped = strip(source);
    let file = SourceFile::new(stripped);
    let reached = file.reachable("handler");
    assert!(reached.contains("Permissions::MANAGE_ROLES"));
    assert!(reached.contains("guard_call("));
    assert!(reached.contains(".check("));
    for hidden in ["KICK_MEMBERS", "BAN_MEMBERS", "MUTE_MEMBERS"] {
        assert!(!reached.contains(hidden), "{hidden} leaked from a literal");
    }
    let failures = policy_failures("demo", perms(&["KICK_MEMBERS"]), &reached);
    assert_eq!(failures.len(), 1, "{failures:?}");
}

fn policy_failures(label: &str, policy: Policy, reached: &str) -> Vec<String> {
    let mut failures = Vec::new();
    for bit in policy.perms {
        if !reached.contains(&format!("Permissions::{bit}")) {
            failures.push(format!("{label} never names Permissions::{bit}"));
        }
    }
    if !policy.perms.is_empty() && !CHECK_CALLS.iter().any(|call| reached.contains(call)) {
        failures.push(format!("{label} never calls a permission check"));
    }
    for guard in policy.guards {
        if !reached.contains(guard) {
            failures.push(format!("{label} never reaches `{guard}`"));
        }
    }
    failures
}

fn proto_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../ss-proto/proto/socialspeak/v1")
}

/// Message names inside `oneof msg { ... }` of `message ClientEnvelope`.
fn client_variants(proto: &str) -> BTreeSet<&str> {
    let start = proto
        .find("message ClientEnvelope")
        .expect("ClientEnvelope");
    let body = &proto[start..];
    let oneof = body.find("oneof msg").expect("oneof msg");
    let body = &body[oneof..];
    let open = body.find('{').expect("oneof body");
    let close = body.find('}').expect("oneof end");
    body[open + 1..close]
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .filter_map(|line| line.split_whitespace().next())
        .collect()
}

struct Arm {
    file: String,
    command: String,
    handler: String,
}

struct Route {
    file: String,
    method: String,
    path: String,
    handler: String,
}

/// Every `.rs` file under `src/`, keyed by its path relative to `src/`.
struct Sources {
    raw: BTreeMap<String, String>,
    stripped: BTreeMap<String, SourceFile>,
}

impl Sources {
    fn load() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut raw = BTreeMap::new();
        walk(&root, &root, &mut raw);
        let stripped = raw
            .iter()
            .map(|(name, text)| (name.clone(), SourceFile::new(strip(text))))
            .collect();
        Self { raw, stripped }
    }

    fn reachable(&self, file: &str, function: &str) -> String {
        self.stripped
            .get(file)
            .map(|source| source.reachable(function))
            .unwrap_or_default()
    }

    /// `Msg::Name(..) => ... self.method(` arms outside [`NOT_HANDLERS`].
    fn dispatch_arms(&self) -> Vec<Arm> {
        self.stripped
            .iter()
            .filter(|(file, _)| !NOT_HANDLERS.iter().any(|(name, _)| name == *file))
            .flat_map(|(file, source)| arms_in(file, &source.text))
            .collect()
    }

    /// `.route("path", method(handler))` calls. Read from the raw text
    /// because the path is a string literal.
    fn routes(&self) -> Vec<Route> {
        let mut routes = Vec::new();
        for (file, text) in &self.raw {
            let compact: String = text.split_whitespace().collect();
            let mut from = 0;
            while let Some(found) = compact[from..].find(".route(\"") {
                let start = from + found + ".route(\"".len();
                from = start;
                let Some(path_end) = compact[start..].find('"') else {
                    continue;
                };
                let path = &compact[start..start + path_end];
                let rest = compact[start + path_end + 1..]
                    .strip_prefix(',')
                    .unwrap_or_default();
                let method: String = rest.chars().take_while(char::is_ascii_lowercase).collect();
                let Some(handler) = rest[method.len()..].strip_prefix('(') else {
                    continue;
                };
                let handler: String = handler
                    .chars()
                    .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                    .collect();
                routes.push(Route {
                    file: file.clone(),
                    method,
                    path: path.to_owned(),
                    handler,
                });
            }
        }
        routes
    }
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            walk(root, &path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(name, fs::read_to_string(&path).unwrap());
        }
    }
}

/// `Msg::Name(..) => ... self.method(` arms in one stripped file. An
/// or-pattern (`Msg::A(..) | Msg::B(..) => ...`) yields one arm per
/// alternative, all with the arm's handler.
fn arms_in(file: &str, text: &str) -> Vec<Arm> {
    let mut arms = Vec::new();
    let mut from = 0;
    while let Some(found) = text[from..].find("Msg::") {
        let at = from + found;
        from = at + 5;
        if text[..at].ends_with("server_envelope::") {
            continue;
        }
        let name: String = text[at + 5..]
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
            .collect();
        let after = &text[at + 5 + name.len()..];
        if !after.starts_with('(') {
            continue;
        }
        let Some(close) = after.find(')') else {
            continue;
        };
        let rest = after[close + 1..].trim_start();
        let handler = if rest.starts_with("=>") || rest.starts_with('|') {
            // `Msg::X(command) => Some(self.handler(..))`, possibly after
            // further `| Msg::Y(..)` alternatives.
            or_pattern_call(rest)
        } else if rest.starts_with('=') {
            // `let Msg::X(command) = msg else { return None };
            //  Some(self.handler(..))`
            let_else_call(rest)
        } else {
            None
        };
        let Some(handler) = handler else {
            continue;
        };
        arms.push(Arm {
            file: file.to_owned(),
            command: name,
            handler,
        });
    }
    arms
}

/// The handler of a match arm, starting at its `=>` or at the `|` after one
/// alternative of an or-pattern. Only further `Msg::Name(..)` alternatives may
/// precede the `=>`; anything else is not a dispatch arm.
fn or_pattern_call(rest: &str) -> Option<String> {
    let mut text = rest;
    loop {
        text = text.trim_start();
        if let Some(arm) = text.strip_prefix("=>") {
            let end = arm
                .find("Msg::")
                .into_iter()
                .chain(arm.find("_ =>"))
                .min()
                .unwrap_or(arm.len());
            return first_self_call(&arm[..end]);
        }
        text = text.strip_prefix('|')?.trim_start().strip_prefix("Msg::")?;
        let name_len = text
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
            .count();
        text = text[name_len..].strip_prefix('(')?;
        text = &text[text.find(')')? + 1..];
    }
}

/// The first `self.method(` after a `let ... else { ... }` block, before the
/// enclosing function ends.
fn let_else_call(rest: &str) -> Option<String> {
    let statement_end = rest.find(';')?;
    let else_at = rest[..statement_end].find("else")?;
    let block = body_after(&rest[else_at..])?;
    let after = &rest[else_at + rest[else_at..].find(block)? + block.len()..];
    let end = after.find("fn ").unwrap_or(after.len());
    first_self_call(&after[..end])
}

fn first_self_call(text: &str) -> Option<String> {
    let at = text.find("self.")?;
    let name: String = text[at + 5..]
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect();
    text[at + 5 + name.len()..].starts_with('(').then_some(name)
}

/// One file with strings and comments blanked, plus its function bodies.
struct SourceFile {
    text: String,
    functions: BTreeMap<String, String>,
}

impl SourceFile {
    fn new(text: String) -> Self {
        let mut functions = BTreeMap::new();
        let mut from = 0;
        while let Some(found) = text[from..].find("fn ") {
            let at = from + found;
            from = at + 3;
            let boundary = at == 0
                || !text[..at]
                    .chars()
                    .next_back()
                    .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_');
            if !boundary {
                continue;
            }
            let name: String = text[at + 3..]
                .chars()
                .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                .collect();
            if name.is_empty() {
                continue;
            }
            if let Some(body) = body_after(&text[at..]) {
                functions
                    .entry(name)
                    .and_modify(|existing: &mut String| existing.push_str(body))
                    .or_insert_with(|| body.to_owned());
            }
        }
        Self { text, functions }
    }

    /// The bodies of `function` and of every same-file function it calls.
    fn reachable(&self, function: &str) -> String {
        let mut seen = BTreeSet::new();
        let mut queue = vec![function.to_owned()];
        let mut out = String::new();
        while let Some(name) = queue.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            let Some(body) = self.functions.get(&name) else {
                continue;
            };
            out.push_str(body);
            out.push('\n');
            for callee in self.functions.keys() {
                if !seen.contains(callee) && calls(body, callee) {
                    queue.push(callee.clone());
                }
            }
        }
        out
    }
}

/// Whether `body` calls `name(` as a function or method.
fn calls(body: &str, name: &str) -> bool {
    let pattern = format!("{name}(");
    body.match_indices(&pattern).any(|(at, _)| {
        !body[..at]
            .chars()
            .next_back()
            .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    })
}

/// From `fn name ...` to the matching close brace of its body. `None` for a
/// declaration without a body (a trait method signature).
fn body_after(text: &str) -> Option<&str> {
    let open = text.find(['{', ';'])?;
    if text.as_bytes()[open] == b';' {
        return None;
    }
    let mut depth = 0_usize;
    for (index, byte) in text.bytes().enumerate().skip(open) {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[open..=index]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Blanks string, raw-string, byte-string, and char literals and comments,
/// keeping everything else. Good enough for this crate's source.
fn strip(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut index = 0;
    while index < bytes.len() {
        let rest = &source[index..];
        if rest.starts_with("//") {
            let end = rest.find('\n').unwrap_or(rest.len());
            index += end;
            continue;
        }
        if rest.starts_with("/*") {
            let end = rest.find("*/").map_or(rest.len(), |end| end + 2);
            index += end;
            continue;
        }
        let prefix_raw = rest.starts_with("r#") || rest.starts_with("r\"");
        let prefix_byte_raw = rest.starts_with("br#") || rest.starts_with("br\"");
        let starts_ident =
            index > 0 && (bytes[index - 1].is_ascii_alphanumeric() || bytes[index - 1] == b'_');
        if (prefix_raw || prefix_byte_raw) && !starts_ident {
            let skip = usize::from(prefix_byte_raw) + 1;
            let hashes = rest[skip..].chars().take_while(|ch| *ch == '#').count();
            let quote = skip + hashes;
            if rest.as_bytes().get(quote) == Some(&b'"') {
                let terminator = format!("\"{}", "#".repeat(hashes));
                let end = rest[quote + 1..]
                    .find(&terminator)
                    .map_or(rest.len(), |end| quote + 1 + end + terminator.len());
                out.push_str("\"\"");
                index += end;
                continue;
            }
        }
        match bytes[index] {
            b'"' => {
                let mut end = index + 1;
                while end < bytes.len() && bytes[end] != b'"' {
                    end += if bytes[end] == b'\\' { 2 } else { 1 };
                }
                out.push_str("\"\"");
                index = end + 1;
            }
            b'\'' => {
                // A char literal closes within a few bytes; a lifetime does not.
                let close = rest[1..]
                    .char_indices()
                    .skip(1)
                    .take(10)
                    .find(|(_, ch)| *ch == '\'')
                    .map(|(at, _)| at + 1);
                let escaped = rest[1..].starts_with('\\');
                let single = rest[1..].chars().nth(1) == Some('\'');
                match close {
                    Some(close) if escaped || single => {
                        out.push_str("' '");
                        index += close + 1;
                    }
                    _ => {
                        out.push('\'');
                        index += 1;
                    }
                }
            }
            _ => {
                let ch = rest.chars().next().unwrap_or(' ');
                out.push(ch);
                index += ch.len_utf8();
            }
        }
    }
    out
}
