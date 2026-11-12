#!/usr/bin/env python3
"""Tests for proto-doc.py. Run: python3 scripts/proto-doc.test.py"""

import importlib.util
import io
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
import tempfile
import textwrap
import unittest

_spec = importlib.util.spec_from_file_location(
    "proto_doc", Path(__file__).resolve().parent / "proto-doc.py"
)
doc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(doc)

SOURCE = textwrap.dedent("""\
    syntax = "proto3";

    package demo.v1;

    import "demo/v1/other.proto";
    option java_package = "x";

    // File note.
    // Second line.

    // Bits:
    //
    // 0 FIRST
    // 1 SECOND

    // A thing.
    message Thing {
      // Identifier.
      bytes id = 1;
      repeated Kind kinds = 2; // Trailing note.
      optional string name = 3;
      map<string, Thing> children = 4;
      // Exactly one.
      oneof body {
        // Text | body.
        string text = 5;
        Inner inner = 6;
      }
      reserved 7, 8;

      // Nested.
      message Inner {
        bool flag = 1;
      }
    }

    message Empty {}

    // Kinds.
    enum Kind {
      KIND_UNSPECIFIED = 0;
      // The first.
      KIND_A = 1;
    }
    """)


class Parse(unittest.TestCase):
    def test_structure(self):
        f = doc.parse("demo.proto", SOURCE)
        self.assertEqual(f.package, "demo.v1")
        self.assertEqual(f.notes, [["File note.", "Second line."], ["Bits:", "", "0 FIRST", "1 SECOND"]])
        thing, empty, kind = f.types
        self.assertEqual((thing.name, thing.doc), ("Thing", ["A thing."]))
        self.assertEqual(
            [(x.name, x.type, x.number, x.label, x.doc) for x in thing.fields],
            [
                ("id", "bytes", 1, "", ["Identifier."]),
                ("kinds", "Kind", 2, "repeated", ["Trailing note."]),
                ("name", "string", 3, "optional", []),
                ("children", "map<string,Thing>", 4, "", []),
                ("text", "string", 5, "oneof body", ["Text | body."]),
                ("inner", "Inner", 6, "oneof body", []),
            ],
        )
        self.assertEqual(thing.notes, [["`body` (oneof): Exactly one."]])
        self.assertEqual(thing.reserved, ["7, 8"])
        self.assertEqual([n.name for n in thing.nested], ["Inner"])
        self.assertEqual(thing.nested[0].doc, ["Nested."])
        self.assertEqual(empty.fields, [])
        self.assertEqual([(v.name, v.number, v.doc) for v in kind.values], [("KIND_UNSPECIFIED", 0, []), ("KIND_A", 1, ["The first."])])

    def test_unsupported_syntax_is_an_error(self):
        for bad in (
            'syntax = "proto2";',
            "service Api {\n}",
            "message A {\n  extensions 100 to 199;\n}",
            "message A {\n  bytes id = 1;\n",
            "}",
            "message A {\n  oneof x {\n    optional bytes y = 1;\n  }\n}",
        ):
            with self.subTest(bad=bad), self.assertRaises(doc.ProtoError):
                doc.parse("bad.proto", bad)

    def test_error_names_the_line(self):
        with self.assertRaisesRegex(doc.ProtoError, r"^bad.proto:3: unsupported statement"):
            doc.parse("bad.proto", 'syntax = "proto3";\nmessage A {\n  rpc Get(A) returns (A);\n}\n')


class Render(unittest.TestCase):
    def test_markdown(self):
        text = doc.render([doc.parse("demo.proto", SOURCE)], "proto/")
        self.assertIn("- [demo.proto](#demoproto): [`Thing`](#thing), [`Empty`](#empty), [`Kind`](#kind)", text)
        self.assertIn("- 0 FIRST\n- 1 SECOND\n", text)
        self.assertIn("| `kinds` | 2 | [`Kind`](#kind) | `repeated` | Trailing note. |", text)
        self.assertIn("| `children` | 4 | map<`string`, [`Thing`](#thing)> |  |  |", text)
        self.assertIn("| `text` | 5 | `string` | `oneof body` | Text \\| body. |", text)
        self.assertIn("| `inner` | 6 | [`Inner`](#thinginner) | `oneof body` |  |", text)
        self.assertIn("### `Thing.Inner`", text)
        self.assertIn("Reserved: `7, 8`.", text)
        self.assertIn("### `Empty`\n\nNo description.\n\nNo fields.", text)
        self.assertIn("| `KIND_A` | 1 | The first. |", text)
        self.assertTrue(text.endswith("|\n"))

    def test_unknown_type_is_an_error(self):
        f = doc.parse("x.proto", 'syntax = "proto3";\npackage p;\nmessage A {\n  Missing m = 1;\n}\n')
        with self.assertRaisesRegex(doc.ProtoError, "unknown type Missing"):
            doc.render([f], "proto/")


class Main(unittest.TestCase):
    def test_write_then_check(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            proto = root / doc.PROTO_DIR / "demo" / "v1"
            proto.mkdir(parents=True)
            (proto / "demo.proto").write_text(SOURCE, encoding="utf-8")
            sink = io.StringIO()
            with redirect_stdout(sink), redirect_stderr(sink):
                self.assertEqual(doc.main(["--root", tmp, "--check"]), 1)
                self.assertEqual(doc.main(["--root", tmp]), 0)
                self.assertEqual(doc.main(["--root", tmp, "--check"]), 0)
                (proto / "demo.proto").write_text(SOURCE.replace("A thing.", "Changed."), encoding="utf-8")
                self.assertEqual(doc.main(["--root", tmp, "--check"]), 1)
            self.assertIn("is stale", sink.getvalue())

    def test_repository_reference_is_current(self):
        sink = io.StringIO()
        with redirect_stdout(sink), redirect_stderr(sink):
            self.assertEqual(doc.main(["--check"]), 0, sink.getvalue())


if __name__ == "__main__":
    unittest.main()
