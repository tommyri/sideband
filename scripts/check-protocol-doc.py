#!/usr/bin/env python3
"""Check that docs/plan/02-protocol.md and the code name the same routes and messages.

- Every HTTP route in a `| Method | Path | ... |` table of the protocol
  document is an axum `.route(..)` under `crates/ss-server/src/`, with the
  same method, and every such route is documented.
- Every message in the "Client → Server" table is a `ClientEnvelope.msg`
  variant and every one in the "Server → Client" table a `ServerEnvelope.msg`
  variant, both ways, and each is a `message` in the `.proto` files.

Path parameter names and query strings are ignored: `/a/{id}?x=` matches
`/a/{channel_id}`.

Run: python3 scripts/check-protocol-doc.py [--root DIR]
Tests: python3 scripts/check-protocol-doc.test.py
"""

from __future__ import annotations

import argparse
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parent.parent
DOC = Path("docs/plan/02-protocol.md")
SERVER_SRC = Path("crates/ss-server/src")
PROTO_DIR = Path("crates/ss-proto/proto")
METHODS = ("get", "post", "put", "delete", "patch", "head", "options")

Route = tuple[str, str]


def _cells(line: str) -> list[str]:
    return [cell.strip() for cell in line.strip().strip("|").split("|")]


def _tables(text: str):
    """Yield (heading, header cells, body rows) for each Markdown table."""
    heading = ""
    lines = text.splitlines()
    i = 0
    while i < len(lines):
        line = lines[i].strip()
        if line.startswith("#"):
            heading = line.lstrip("#").strip()
        if line.startswith("|") and i + 1 < len(lines) and set(lines[i + 1].strip()) <= set("|-: ") \
                and lines[i + 1].strip():
            header = _cells(line)
            rows = []
            i += 2
            while i < len(lines) and lines[i].strip().startswith("|"):
                rows.append(_cells(lines[i]))
                i += 1
            yield heading, header, rows
            continue
        i += 1


def normalize_path(path: str) -> str:
    path = path.strip().strip("`").split("?", 1)[0]
    return re.sub(r"\{[^}]*\}", "{}", path)


def doc_routes(text: str) -> set[Route]:
    routes: set[Route] = set()
    for _, header, rows in _tables(text):
        if header[:2] != ["Method", "Path"]:
            continue
        for row in rows:
            path = normalize_path(row[1])
            for method in re.split(r"[/,]", row[0]):
                routes.add((method.strip().upper(), path))
    return routes


def doc_messages(text: str) -> dict[str, set[str]]:
    """Message names per direction ("client" or "server")."""
    found = {"client": set(), "server": set()}
    for heading, header, rows in _tables(text):
        if not header or header[0] != "Message":
            continue
        if "Client → Server" in heading:
            direction = "client"
        elif "Server → Client" in heading:
            direction = "server"
        else:
            continue
        for row in rows:
            found[direction].update(re.findall(r"`([A-Z]\w*)`", row[0]))
    return found


def _call_args(text: str, start: int) -> str:
    """Text between the parenthesis at `start` and its match."""
    depth = 0
    for i in range(start, len(text)):
        if text[i] == "(":
            depth += 1
        elif text[i] == ")":
            depth -= 1
            if depth == 0:
                return text[start + 1:i]
    raise ValueError("unbalanced .route( call")


def code_routes_in(source: str) -> set[Route]:
    routes: set[Route] = set()
    for match in re.finditer(r"\.route\s*\(", source):
        args = _call_args(source, match.end() - 1)
        path = re.match(r'\s*"([^"]+)"\s*,', args)
        if not path:
            continue
        handler = args[path.end():]
        for method in re.findall(r"\b(" + "|".join(METHODS) + r")\s*\(", handler):
            routes.add((method.upper(), normalize_path(path.group(1))))
    return routes


def code_routes(root: Path) -> dict[Route, str]:
    routes: dict[Route, str] = {}
    for path in sorted((root / SERVER_SRC).rglob("*.rs")):
        for route in code_routes_in(path.read_text(encoding="utf-8")):
            routes.setdefault(route, str(path.relative_to(root)))
    return routes


def _block(text: str, opener: str) -> str:
    start = text.find(opener)
    if start < 0:
        return ""
    brace = text.index("{", start)
    depth = 0
    for i in range(brace, len(text)):
        if text[i] == "{":
            depth += 1
        elif text[i] == "}":
            depth -= 1
            if depth == 0:
                return text[brace + 1:i]
    raise ValueError(f"unbalanced {opener}")


def proto_messages(proto: str) -> set[str]:
    return set(re.findall(r"^\s*message\s+(\w+)\s*\{", proto, re.MULTILINE))


def envelope_variants(proto: str, envelope: str) -> set[str]:
    """Message types of the `oneof msg` fields in `message <envelope>`."""
    match = re.search(r"^\s*message\s+" + envelope + r"\s*\{", proto, re.MULTILINE)
    if not match:
        return set()
    body = _block(_block(proto[match.start():], "message"), "oneof msg")
    return set(re.findall(r"^\s*(\w+)\s+\w+\s*=\s*\d+\s*;", body, re.MULTILINE))


def problems(doc: str, routes: dict[Route, str], proto: str) -> list[str]:
    found: list[str] = []
    documented = doc_routes(doc)
    if not documented:
        found.append("02-protocol.md has no `| Method | Path |` table")
    for method, path in sorted(documented - routes.keys(), key=lambda r: (r[1], r[0])):
        found.append(f"documented route {method} {path} is not an axum route in ss-server")
    for method, path in sorted(routes.keys() - documented, key=lambda r: (r[1], r[0])):
        found.append(f"route {method} {path} ({routes[(method, path)]}) is not in 02-protocol.md")

    messages = doc_messages(doc)
    known = proto_messages(proto)
    for direction, envelope in (("client", "ClientEnvelope"), ("server", "ServerEnvelope")):
        variants = envelope_variants(proto, envelope)
        if not variants:
            found.append(f"no `oneof msg` in {envelope}")
        if not messages[direction]:
            found.append(f"02-protocol.md has no {direction} message table")
        for name in sorted(messages[direction] - known):
            found.append(f"documented message {name} is not a protobuf message")
        for name in sorted((messages[direction] & known) - variants):
            found.append(f"documented message {name} is not a {envelope} variant")
        for name in sorted(variants - messages[direction]):
            found.append(f"{envelope} variant {name} is not in the 02-protocol.md {direction} table")
    return found


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args(argv)
    doc = (args.root / DOC).read_text(encoding="utf-8")
    proto = "\n".join(p.read_text(encoding="utf-8") for p in sorted((args.root / PROTO_DIR).rglob("*.proto")))
    found = problems(doc, code_routes(args.root), proto)
    for problem in found:
        print(f"check-protocol-doc: {problem}", file=sys.stderr)
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
