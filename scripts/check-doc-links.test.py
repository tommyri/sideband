#!/usr/bin/env python3
"""Tests for check-doc-links.py. Run: python3 scripts/check-doc-links.test.py"""

import importlib.util
import io
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
import tempfile
import textwrap
import unittest

_spec = importlib.util.spec_from_file_location(
    "check_doc_links", Path(__file__).resolve().parent / "check-doc-links.py"
)
links = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(links)


class Slug(unittest.TestCase):
    def test_github_rules(self):
        cases = {
            "Verify a download": "verify-a-download",
            "1.1 Session tokens": "11-session-tokens",
            "Client → Server": "client--server",
            "D1 — Rust for the server": "d1--rust-for-the-server",
            "snake_case stays": "snake_case-stays",
            "Members & moderation": "members--moderation",
            "Æsir og Ånd": "æsir-og-ånd",
        }
        for text, expected in cases.items():
            self.assertEqual(links.slug(text), expected, text)

    def test_inline_markup_is_rendered_first(self):
        self.assertEqual(
            links.slug(links._heading_text("3. Schema (`crates/a_b/*.proto`)")),
            "3-schema-cratesa_bproto",
        )
        self.assertEqual(links.slug(links._heading_text("See [the guide](x.md) ##")), "see-the-guide")
        self.assertEqual(links.slug(links._heading_text("_Emphasis_ and **bold**")), "emphasis-and-bold")


class Anchors(unittest.TestCase):
    def test_duplicates_setext_and_html(self):
        text = textwrap.dedent("""\
            # Title

            ## Notes
            ## Notes

            Setext heading
            --------------

            - item
            ---

            <a id="custom"></a>

            ```md
            ## Not a heading
            ```
            """)
        found = links.anchors(text)
        self.assertEqual(found, {"title", "notes", "notes-1", "setext-heading", "custom"})


class Extraction(unittest.TestCase):
    def test_ignores_code_and_comments(self):
        text = textwrap.dedent("""\
            [real](a.md) and `[code](b.md)` and
            ```
            [fenced](c.md)
            ```
            <!-- [comment](d.md) -->
            ~~~~
            [tilde](e.md)
            ~~~~
            [wrapped
            text](f.md "Title") ![img](<g h.png>)
            [ref]: i.md#x
            <a href="j.md">j</a> <img src='k.svg'>
            """)
        self.assertEqual(
            links.links(text),
            [(1, "a.md"), (10, "f.md"), (10, "g h.png"), (11, "i.md#x"), (12, "j.md"), (12, "k.svg")],
        )

    def test_parentheses_in_destination(self):
        self.assertEqual(links.links("[x](a_(b).md)"), [(1, "a_(b).md")])


class Checking(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        (self.root / "docs").mkdir()
        (self.root / "docs" / "guide.md").write_text("# Guide\n\n## Install it\n", encoding="utf-8")
        (self.root / "docs" / "Image.png").write_bytes(b"")
        (self.root / "crate").mkdir()
        (self.root / "crate" / "README.md").write_text("# Crate\n", encoding="utf-8")
        (self.root / "src.rs").write_text("fn main() {}\n", encoding="utf-8")

    def tearDown(self):
        self.tmp.cleanup()

    def run_on(self, body: str):
        page = self.root / "README.md"
        page.write_text(textwrap.dedent(body), encoding="utf-8")
        checker = links.Checker(self.root)
        return checker, [(p.line, p.target, p.reason) for p in checker.check_file(page)]

    def test_valid_links(self):
        checker, problems = self.run_on("""\
            # Top
            [a](docs/guide.md) [b](docs/guide.md#install-it) [c](#top) [d](crate/)
            [e](crate#crate) [f](src.rs#L1) [g](/docs/guide.md) [h](docs/Image.png)
            [i](https://example.com/x) [j](mailto:a@example.com) [k](//cdn.example)
            [l](docs/guide.md#Install-It)
            """)
        self.assertEqual(problems, [])
        self.assertEqual(checker.external, 3)
        self.assertEqual(checker.links, 9)

    def test_broken_links(self):
        _, problems = self.run_on("""\
            [a](docs/missing.md)
            [b](docs/guide.md#nope)
            [c](#nope)
            [d](docs/image.png)
            [e](../outside.md)
            """)
        self.assertEqual(
            problems,
            [
                (1, "docs/missing.md", "no such file or directory"),
                (2, "docs/guide.md#nope", "no anchor #nope in docs/guide.md"),
                (3, "#nope", "no anchor #nope in README.md"),
                (4, "docs/image.png", "case differs on disk (Image.png)"),
                (5, "../outside.md", "points outside the repository"),
            ],
        )

    def test_main_walks_a_non_git_root_and_fails(self):
        (self.root / "README.md").write_text("[x](gone.md)\n", encoding="utf-8")
        (self.root / "target").mkdir()
        (self.root / "target" / "skip.md").write_text("[x](gone.md)\n", encoding="utf-8")
        err, out = io.StringIO(), io.StringIO()
        with redirect_stderr(err), redirect_stdout(out):
            code = links.main(["--root", str(self.root)])
        self.assertEqual(code, 1)
        self.assertIn("README.md:1: gone.md: no such file or directory", err.getvalue())
        self.assertNotIn("skip.md", err.getvalue())
        self.assertIn("1 broken links", err.getvalue())


if __name__ == "__main__":
    unittest.main()
