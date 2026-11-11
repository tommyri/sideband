#!/usr/bin/env python3
"""Tests for check-protocol-doc.py. Run: python3 scripts/check-protocol-doc.test.py"""

import importlib.util
from pathlib import Path
import tempfile
import textwrap
import unittest

_spec = importlib.util.spec_from_file_location(
    "check_protocol_doc", Path(__file__).resolve().parent / "check-protocol-doc.py"
)
check = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(check)

DOC = textwrap.dedent("""\
    ## 3. Messages

    ### Client → Server

    | Message | Fields | Purpose |
    |---|---|---|
    | `Hello` | `oneof auth { session_token }` | Must be first |
    | `CreateRole` / `UpdateRole` | `Role{name}` | |

    ### Server → Client

    | Message | Fields | Purpose |
    |---|---|---|
    | `Welcome` | `user` | |
    | `Ack` | `request_id` | |

    ## 6. REST

    | Method | Path | Purpose |
    |---|---|---|
    | GET / PUT | `/api/v1/server/settings` | Profile |
    | GET | `/api/v1/channels/{id}/messages?before=<id>&limit=50` | History |
    | GET | `/metrics` | internal listener only |
    """)

RUST = textwrap.dedent("""\
    pub fn router() -> Router {
        Router::new()
            .route("/api/v1/server/settings", get(read))
            .route("/api/v1/server/settings", put(update))
            .route(
                "/api/v1/channels/{channel_id}/messages",
                get(page),
            )
            .route("/metrics", get(metrics_text).post(nothing))
            .with_state(State { inner: (1, 2) })
    }
    """)

PROTO = textwrap.dedent("""\
    message Hello { string name = 1; }
    message CreateRole { string name = 1; }
    message UpdateRole { string name = 1; }
    message Welcome { bytes user = 1; }
    message Ack { uint32 request_id = 1; oneof result { Empty empty = 2; } }
    message ClientEnvelope {
      uint32 request_id = 1;
      oneof msg {
        Hello hello = 2;
        CreateRole create_role = 3;
        UpdateRole update_role = 4;
      }
    }
    message ServerEnvelope {
      uint32 request_id = 1;
      oneof msg {
        Welcome welcome = 2;
        Ack ack = 3;
      }
    }
    """)


def routes() -> dict:
    return {route: "x.rs" for route in check.code_routes_in(RUST)}


class Routes(unittest.TestCase):
    def test_doc_rows_split_methods_and_drop_queries_and_parameter_names(self) -> None:
        self.assertEqual(check.doc_routes(DOC), {
            ("GET", "/api/v1/server/settings"),
            ("PUT", "/api/v1/server/settings"),
            ("GET", "/api/v1/channels/{}/messages"),
            ("GET", "/metrics"),
        })

    def test_code_routes_span_lines_and_method_chains(self) -> None:
        self.assertEqual(check.code_routes_in(RUST), {
            ("GET", "/api/v1/server/settings"),
            ("PUT", "/api/v1/server/settings"),
            ("GET", "/api/v1/channels/{}/messages"),
            ("GET", "/metrics"),
            ("POST", "/metrics"),
        })

    def test_both_directions_are_reported(self) -> None:
        doc = DOC.replace("| GET | `/metrics` |", "| GET | `/api/v1/users/{id}/avatar` |")
        self.assertEqual(check.problems(doc, routes(), PROTO), [
            "documented route GET /api/v1/users/{}/avatar is not an axum route in ss-server",
            "route GET /metrics (x.rs) is not in 02-protocol.md",
            "route POST /metrics (x.rs) is not in 02-protocol.md",
        ])


class Messages(unittest.TestCase):
    def test_tables_are_read_per_direction(self) -> None:
        self.assertEqual(check.doc_messages(DOC), {
            "client": {"Hello", "CreateRole", "UpdateRole"},
            "server": {"Welcome", "Ack"},
        })

    def test_envelope_variants_ignore_other_oneofs(self) -> None:
        self.assertEqual(check.envelope_variants(PROTO, "ServerEnvelope"), {"Welcome", "Ack"})
        self.assertEqual(check.envelope_variants(PROTO, "Missing"), set())

    def test_matching_doc_and_code_pass(self) -> None:
        doc = DOC + "| POST | `/metrics` | |\n"
        self.assertEqual(check.problems(doc, routes(), PROTO), [])

    def test_missing_unknown_and_misplaced_messages_are_reported(self) -> None:
        doc = DOC.replace("| `Ack` |", "| `Ack` / `Hello` / `Gone` |").replace("| `Hello` |", "| `Ping` |")
        doc += "| POST | `/metrics` | |\n"
        self.assertEqual(check.problems(doc, routes(), PROTO), [
            "documented message Ping is not a protobuf message",
            "ClientEnvelope variant Hello is not in the 02-protocol.md client table",
            "documented message Gone is not a protobuf message",
            "documented message Hello is not a ServerEnvelope variant",
        ])

    def test_main_reads_the_repository_layout(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for path, text in [(check.DOC, DOC + "| POST | `/metrics` | |\n"),
                               (check.SERVER_SRC / "http/mod.rs", RUST),
                               (check.PROTO_DIR / "v1/envelope.proto", PROTO)]:
                (root / path).parent.mkdir(parents=True, exist_ok=True)
                (root / path).write_text(text, encoding="utf-8")
            self.assertEqual(check.main(["--root", tmp]), 0)
            (root / check.SERVER_SRC / "http/mod.rs").write_text(RUST.replace("put(update)", "patch(update)"))
            self.assertEqual(check.main(["--root", tmp]), 1)


if __name__ == "__main__":
    unittest.main()
