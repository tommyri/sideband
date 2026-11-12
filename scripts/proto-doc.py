#!/usr/bin/env python3
"""Generate docs/protocol/messages.md from the comments in the `.proto` files.

The output lists every file, message, field, oneof and enum under
`crates/ss-proto/proto/`, with the `//` comment written above each
declaration. A comment block followed by a blank line belongs to the file
(or the enclosing message), not to the next declaration. Field types that
name a message or enum link to it.

The parser accepts the subset of proto3 the schema uses: `syntax`, `package`,
`import`, `option`, `message`, `enum`, `oneof`, nested messages and enums,
`optional`/`repeated`/`map<>` fields, `reserved`, and trailing `//`
comments. Anything else is an error, so a new construct is never dropped
silently. This is the stand-in for `protoc-gen-doc`, which the project does
not install.

Run: python3 scripts/proto-doc.py [--check] [--root DIR]
Tests: python3 scripts/proto-doc.test.py
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass, field
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parent.parent
PROTO_DIR = Path("crates/ss-proto/proto")
OUTPUT = Path("docs/protocol/messages.md")
SCALARS = {
    "double", "float", "int32", "int64", "uint32", "uint64", "sint32", "sint64",
    "fixed32", "fixed64", "sfixed32", "sfixed64", "bool", "string", "bytes",
}
IDENT = r"[A-Za-z_][A-Za-z0-9_]*"
FIELD = re.compile(
    rf"^(?:(optional|repeated)\s+)?(map<\s*{IDENT}\s*,\s*[A-Za-z_.][A-Za-z0-9_.]*\s*>|[A-Za-z_.][A-Za-z0-9_.]*)"
    rf"\s+({IDENT})\s*=\s*(\d+)\s*(?:\[[^\]]*\])?\s*;$"
)
ENUM_VALUE = re.compile(rf"^({IDENT})\s*=\s*(-?\d+)\s*(?:\[[^\]]*\])?\s*;$")
OPEN = re.compile(rf"^(message|enum|oneof)\s+({IDENT})\s*\{{\s*(\}})?$")


class ProtoError(ValueError):
    pass


@dataclass
class Field:
    name: str
    type: str
    number: int
    label: str
    doc: list[str]


@dataclass
class EnumValue:
    name: str
    number: int
    doc: list[str]


@dataclass
class Type:
    kind: str  # "message" or "enum"
    name: str
    doc: list[str]
    notes: list[list[str]] = field(default_factory=list)
    fields: list[Field] = field(default_factory=list)
    values: list[EnumValue] = field(default_factory=list)
    reserved: list[str] = field(default_factory=list)
    nested: list["Type"] = field(default_factory=list)


@dataclass
class File:
    name: str
    package: str
    notes: list[list[str]] = field(default_factory=list)
    types: list[Type] = field(default_factory=list)


def _split_comment(line: str) -> tuple[str, str | None]:
    """Code and trailing comment. The schema has no `//` inside strings."""
    if line.lstrip().startswith("//"):
        return "", line.lstrip()[2:]
    code, sep, comment = line.partition("//")
    if '"' in code and code.count('"') % 2:
        raise ProtoError("`//` inside a string literal is not supported")
    return code.strip(), (comment if sep else None)


def _clean(text: str) -> str:
    return text[1:] if text.startswith(" ") else text


def parse(name: str, source: str) -> File:
    result = File(name=name, package="")
    stack: list[Type] = []
    oneof: str | None = None
    pending: list[str] = []
    pending_started = False

    def where(number: int) -> str:
        return f"{name}:{number}"

    def detach() -> None:
        nonlocal pending, pending_started
        if pending_started:
            (stack[-1].notes if stack else result.notes).append(pending)
        pending, pending_started = [], False

    for number, raw in enumerate(source.splitlines(), 1):
        code, comment = _split_comment(raw)
        if not code:
            if comment is not None:
                pending.append(_clean(comment).rstrip())
                pending_started = True
            elif not raw.strip():
                detach()
            continue
        doc = pending
        pending, pending_started = [], False
        if comment is not None:
            doc = doc + [_clean(comment).strip()]
        try:
            if re.match(r"^syntax\s*=", code):
                if '"proto3"' not in code:
                    raise ProtoError("only proto3 is supported")
            elif match := re.match(r"^package\s+([A-Za-z0-9_.]+)\s*;$", code):
                result.package = match.group(1)
            elif re.match(r'^(import|option)\s', code) and code.endswith(";"):
                pass
            elif match := OPEN.match(code):
                kind, ident, closed = match.groups()
                if kind == "oneof":
                    if not stack or stack[-1].kind != "message" or oneof is not None:
                        raise ProtoError("oneof outside a message")
                    oneof = ident
                    if doc:
                        stack[-1].notes.append([f"`{ident}` (oneof): " + " ".join(doc)])
                    if closed:
                        oneof = None
                    continue
                if oneof is not None:
                    raise ProtoError(f"{kind} inside a oneof")
                if stack and stack[-1].kind == "enum":
                    raise ProtoError(f"{kind} inside an enum")
                declared = Type(kind=kind, name=ident, doc=doc)
                (stack[-1].nested if stack else result.types).append(declared)
                if not closed:
                    stack.append(declared)
            elif code == "}":
                if oneof is not None:
                    oneof = None
                elif stack:
                    stack.pop()
                else:
                    raise ProtoError("unbalanced `}`")
            elif match := re.match(r"^reserved\s+(.+);$", code):
                if not stack:
                    raise ProtoError("reserved outside a declaration")
                stack[-1].reserved.append(match.group(1).strip())
            elif stack and stack[-1].kind == "enum" and (match := ENUM_VALUE.match(code)):
                stack[-1].values.append(EnumValue(match.group(1), int(match.group(2)), doc))
            elif stack and stack[-1].kind == "message" and (match := FIELD.match(code)):
                label, kind, ident, tag = match.groups()
                if oneof is not None:
                    if label:
                        raise ProtoError("a oneof field cannot be optional or repeated")
                    label = f"oneof {oneof}"
                stack[-1].fields.append(
                    Field(ident, re.sub(r"\s+", "", kind), int(tag), label or "", doc)
                )
            else:
                raise ProtoError(f"unsupported statement: {code}")
        except ProtoError as error:
            raise ProtoError(f"{where(number)}: {error}") from None
    if stack or oneof is not None:
        raise ProtoError(f"{name}: unclosed declaration at end of file")
    detach()
    return result


def _paragraphs(lines: list[str]) -> list[list[str]]:
    paragraphs: list[list[str]] = [[]]
    for line in lines:
        if line.strip():
            paragraphs[-1].append(line.strip())
        elif paragraphs[-1]:
            paragraphs.append([])
    return [p for p in paragraphs if p]


def _prose(lines: list[str]) -> list[str]:
    """Markdown for a comment: paragraphs, with list-like runs as bullets."""
    out: list[str] = []
    for paragraph in _paragraphs(lines):
        if len(paragraph) > 1 and all(re.match(r"^(\d+\s|[-*]\s)", l) for l in paragraph):
            out.extend(l if l[0] in "-*" else f"- {l}" for l in paragraph)
        else:
            out.extend(paragraph)
        out.append("")
    return out


def _cell(lines: list[str]) -> str:
    text = " ".join(l.strip() for l in lines if l.strip())
    return text.replace("|", "\\|")


def _all_types(files: list[File]) -> dict[str, str]:
    """Type name (short and qualified) to its anchor."""
    found: dict[str, str] = {}

    def visit(package: str, prefix: str, types: list[Type]) -> None:
        for t in types:
            name = f"{prefix}{t.name}"
            anchor = _anchor(name)
            if name in found:
                raise ProtoError(f"type {name} is declared twice")
            found[name] = anchor
            found[f"{package}.{name}"] = anchor
            visit(package, f"{name}.", t.nested)

    for f in files:
        visit(f.package, "", f.types)
    return found


def _anchor(name: str) -> str:
    # The heading is `` `Name` ``; GitHub drops the backticks and the dots.
    return name.lower().replace(".", "")


def _type_link(kind: str, scope: str, known: dict[str, str]) -> str:
    match = re.fullmatch(r"map<([^,]+),(.+)>", kind)
    if match:
        return f"map<{_type_link(match.group(1), scope, known)}, {_type_link(match.group(2), scope, known)}>"
    if kind in SCALARS:
        return f"`{kind}`"
    name = kind.lstrip(".")
    candidates = [f"{scope}.{name}", name] if scope else [name]
    for candidate in candidates:
        if candidate in known:
            return f"[`{kind}`](#{known[candidate]})"
    raise ProtoError(f"unknown type {kind}")


def _render_type(t: Type, prefix: str, known: dict[str, str], out: list[str]) -> None:
    name = f"{prefix}{t.name}"
    out.append(f"### `{name}`")
    out.append("")
    if t.kind == "enum":
        out.append("Enum.")
        out.append("")
    out.extend(_prose(t.doc) if t.doc else ["No description.", ""])
    for note in t.notes:
        out.extend(_prose(note))
    if t.kind == "message":
        if t.fields:
            out.append("| Field | Number | Type | Label | Description |")
            out.append("|---|---|---|---|---|")
            for f in t.fields:
                label = f"`{f.label}`" if f.label else ""
                out.append(
                    f"| `{f.name}` | {f.number} | {_type_link(f.type, name, known)} | {label} | {_cell(f.doc)} |"
                )
        else:
            out.append("No fields.")
        out.append("")
    else:
        out.append("| Value | Number | Description |")
        out.append("|---|---|---|")
        for v in t.values:
            out.append(f"| `{v.name}` | {v.number} | {_cell(v.doc)} |")
        out.append("")
    if t.reserved:
        out.append("Reserved: " + "; ".join(f"`{r}`" for r in t.reserved) + ".")
        out.append("")
    for nested in t.nested:
        _render_type(nested, f"{name}.", known, out)


def render(files: list[File], source_dir: str) -> str:
    known = _all_types(files)
    packages = sorted({f.package for f in files})
    out = [
        "# Protocol message reference",
        "",
        "<!-- Generated by scripts/proto-doc.py. Do not edit: change the .proto",
        "comments and run `just proto-doc`. `just plan-check` fails when stale. -->",
        "",
        f"Generated from the comments in `{source_dir}`. Package "
        + ", ".join(f"`{p}`" for p in packages)
        + ". Field numbers are permanent: fields are only ever added. Framing,",
        "connection lifecycle, limits and REST are in the",
        "[wire protocol](../plan/02-protocol.md); start from the",
        "[protocol overview](README.md).",
        "",
        "## Contents",
        "",
    ]
    for f in files:
        names = ", ".join(f"[`{t.name}`](#{_anchor(t.name)})" for t in f.types)
        out.append(f"- [{f.name}](#{f.name.lower().replace('.', '')}): {names}")
    out.append("")
    for f in files:
        out.append(f"## {f.name}")
        out.append("")
        for note in f.notes:
            out.extend(_prose(note))
        for t in f.types:
            _render_type(t, "", known, out)
    while out and out[-1] == "":
        out.pop()
    return "\n".join(out) + "\n"


def generate(root: Path) -> str:
    directory = root / PROTO_DIR
    paths = sorted(directory.rglob("*.proto"))
    if not paths:
        raise ProtoError(f"no .proto files under {directory}")
    files = [parse(p.name, p.read_text(encoding="utf-8")) for p in paths]
    names = [f.name for f in files]
    if len(set(names)) != len(names):
        raise ProtoError("two .proto files share a file name")
    return render(files, PROTO_DIR.as_posix() + "/")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--check", action="store_true", help="fail when the file is stale")
    args = parser.parse_args(argv)
    try:
        text = generate(args.root)
    except ProtoError as error:
        print(f"proto-doc: {error}", file=sys.stderr)
        return 1
    output = args.root / OUTPUT
    if args.check:
        current = output.read_text(encoding="utf-8") if output.exists() else ""
        if current != text:
            print(f"{OUTPUT} is stale: run `just proto-doc`.", file=sys.stderr)
            return 1
        print(f"{OUTPUT} is current.")
        return 0
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(text, encoding="utf-8")
    print(f"Wrote {OUTPUT}.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
