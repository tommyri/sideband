#!/usr/bin/env python3
"""Check relative links and anchors in the repository's Markdown files.

For every Markdown file Git tracks (plus untracked files that are not
ignored), each relative link must name a file or directory that exists with
exactly that spelling (Linux and GitHub are case-sensitive even when macOS
is not), and a `#fragment` on a Markdown target must match one of its
headings or an explicit `id`/`name` anchor. Heading anchors follow GitHub's
rules: lowercase, punctuation removed, spaces become hyphens, and repeated
headings get `-1`, `-2`, ...

Checked: inline links and images, reference definitions (`[x]: path`), and
`href`/`src` attributes of `<a>` and `<img>`. Links inside fenced code,
inline code and HTML comments are ignored, as on GitHub.

Not checked: external URLs (any `scheme:` or `//host`), and fragments on
files that are not Markdown (GitHub's `#L10` line anchors, for example).

Run: python3 scripts/check-doc-links.py [--root DIR] [FILE ...]
Tests: python3 scripts/check-doc-links.test.py
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import os
from pathlib import Path
import re
import subprocess
import sys
import unicodedata
from urllib.parse import unquote

ROOT = Path(__file__).resolve().parent.parent
# Walked only when the root is not a Git checkout.
SKIP_DIRS = {".git", "target", "node_modules", ".worktrees"}
SCHEME = re.compile(r"^[A-Za-z][A-Za-z0-9+.-]*:")
FENCE = re.compile(r"^ {0,3}(`{3,}|~{3,})")
ATX = re.compile(r"^ {0,3}(#{1,6})(?:[ \t]+(.*?))?[ \t]*$")
SETEXT = re.compile(r"^ {0,3}(=+|-+)[ \t]*$")
REFERENCE = re.compile(r"^ {0,3}\[[^\]\n]+\]:[ \t]*(<[^>\n]*>|\S+)", re.MULTILINE)
HTML_LINK = re.compile(
    r"<(?:a|img)\b[^>]*?\b(?:href|src)\s*=\s*(?:\"([^\"]*)\"|'([^']*)')", re.IGNORECASE
)
HTML_ANCHOR = re.compile(r"<[a-z][^>]*?\b(?:id|name)\s*=\s*(?:\"([^\"]*)\"|'([^']*)')", re.IGNORECASE)


@dataclass(frozen=True)
class Problem:
    file: str
    line: int
    target: str
    reason: str

    def __str__(self) -> str:
        return f"{self.file}:{self.line}: {self.target}: {self.reason}"


def slug(text: str) -> str:
    """GitHub's anchor for rendered heading text (github-slugger)."""
    kept = []
    for ch in text.lower():
        category = unicodedata.category(ch)
        if ch in " -" or category[0] in "LMN" or category == "Pc":
            kept.append(ch)
    return "".join(kept).replace(" ", "-")


def _blank(match: re.Match[str]) -> str:
    return re.sub(r"[^\n]", " ", match.group(0))


def _strip_code(text: str) -> str:
    """Blank out fenced code, HTML comments and inline code, keeping offsets."""
    lines = text.split("\n")
    fence: str | None = None
    for i, line in enumerate(lines):
        opening = FENCE.match(line)
        if fence is None:
            if opening:
                fence = opening.group(1)
                lines[i] = " " * len(line)
        else:
            closing = FENCE.match(line)
            if (
                closing
                and closing.group(1)[0] == fence[0]
                and len(closing.group(1)) >= len(fence)
                and not line.strip()[len(closing.group(1)) :].strip()
            ):
                fence = None
            lines[i] = " " * len(line)
    text = "\n".join(lines)
    text = re.sub(r"<!--.*?-->", _blank, text, flags=re.DOTALL)
    return _strip_code_spans(text)


def _strip_code_spans(text: str) -> str:
    out = list(text)
    i = 0
    while i < len(text):
        if text[i] == "\\":
            i += 2
            continue
        if text[i] != "`":
            i += 1
            continue
        j = i
        while j < len(text) and text[j] == "`":
            j += 1
        ticks = text[i:j]
        # A code span cannot cross a blank line (it would end the paragraph).
        end = text.find(ticks, j)
        while end != -1 and (text[end + len(ticks) : end + len(ticks) + 1] == "`" or text[end - 1] == "`"):
            end = text.find(ticks, end + len(ticks))
        if end == -1 or "\n\n" in text[j:end]:
            i = j
            continue
        for k in range(i, end + len(ticks)):
            if out[k] != "\n":
                out[k] = " "
        i = end + len(ticks)
    return "".join(out)


def _heading_text(raw: str) -> str:
    """Approximate the rendered text of a heading's inline Markdown."""
    raw = re.sub(r"[ \t]+#+[ \t]*$", "", raw)
    if re.fullmatch(r"#+", raw.strip()):
        return ""
    parts = re.split(r"(`+)(.+?)\1", raw)
    text = []
    # re.split with two groups yields [plain, ticks, code, plain, ticks, code, ...].
    for index in range(0, len(parts), 3):
        plain = parts[index]
        plain = re.sub(r"!\[([^\]]*)\]\([^)]*\)", "", plain)
        plain = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", plain)
        plain = re.sub(r"\[([^\]]*)\]\[[^\]]*\]", r"\1", plain)
        plain = re.sub(r"<[^>]+>", "", plain)
        plain = re.sub(r"(?<!\w)_+|_+(?!\w)", "", plain)
        plain = re.sub(r"\\(.)", r"\1", plain)
        text.append(plain)
        if index + 2 < len(parts):
            text.append(parts[index + 2])
    return "".join(text).strip()


def anchors(text: str) -> set[str]:
    """Every anchor a rendered Markdown file offers."""
    found: set[str] = set()
    seen: dict[str, int] = {}
    lines = _strip_fences_only(text).split("\n")
    previous = ""
    for line in lines:
        heading = None
        atx = ATX.match(line)
        if atx:
            heading = atx.group(2) or ""
        elif SETEXT.match(line) and previous.strip() and not ATX.match(previous):
            # `---` under a list item or table row is not a heading; good enough here.
            if not re.match(r"^ {0,3}([-*+]|\d+[.)]|\|)", previous):
                heading = previous.strip()
        if heading is not None:
            base = slug(_heading_text(heading))
            count = seen.get(base, 0)
            found.add(base if count == 0 else f"{base}-{count}")
            seen[base] = count + 1
        previous = line
    for match in HTML_ANCHOR.finditer(_strip_code(text)):
        found.add(match.group(1) if match.group(1) is not None else match.group(2))
    return found


def _strip_fences_only(text: str) -> str:
    lines = text.split("\n")
    fence: str | None = None
    for i, line in enumerate(lines):
        mark = FENCE.match(line)
        if fence is None:
            if mark:
                fence = mark.group(1)
                lines[i] = ""
        else:
            if mark and mark.group(1)[0] == fence[0] and len(mark.group(1)) >= len(fence):
                fence = None
            lines[i] = ""
    return "\n".join(lines)


def _destination(text: str, start: int) -> str | None:
    """The link destination that begins at `start`, just after `](`."""
    i = start
    while i < len(text) and text[i] in " \t\n":
        i += 1
    if i < len(text) and text[i] == "<":
        end = text.find(">", i)
        return None if end == -1 else text[i + 1 : end]
    depth = 0
    j = i
    while j < len(text):
        ch = text[j]
        if ch == "\\":
            j += 2
            continue
        if ch in " \t\n":
            break
        if ch == "(":
            depth += 1
        elif ch == ")":
            if depth == 0:
                break
            depth -= 1
        j += 1
    return text[i:j]


def links(text: str) -> list[tuple[int, str]]:
    """(line, destination) for every link outside code."""
    clean = _strip_code(text)
    found: list[tuple[int, int]] = []
    out: list[tuple[int, str]] = []
    for match in re.finditer(r"\]\(", clean):
        dest = _destination(clean, match.end())
        if dest is not None:
            found.append((match.start(), dest))
    for match in REFERENCE.finditer(clean):
        dest = match.group(1)
        if dest.startswith("<"):
            dest = dest[1:-1]
        found.append((match.start(1), dest))
    for match in HTML_LINK.finditer(clean):
        dest = match.group(1) if match.group(1) is not None else match.group(2)
        found.append((match.start(), dest))
    for offset, dest in sorted(found):
        out.append((clean.count("\n", 0, offset) + 1, dest))
    return out


class Checker:
    def __init__(self, root: Path):
        self.root = root.resolve()
        self._anchors: dict[Path, set[str]] = {}
        self._listing: dict[Path, set[str]] = {}
        self.links = 0
        self.external = 0

    def _names(self, directory: Path) -> set[str]:
        if directory not in self._listing:
            try:
                self._listing[directory] = set(os.listdir(directory))
            except OSError:
                self._listing[directory] = set()
        return self._listing[directory]

    def _exact(self, path: Path) -> str | None:
        """None when `path` exists with this exact spelling, else the reason."""
        try:
            relative = path.relative_to(self.root)
        except ValueError:
            return "points outside the repository"
        current = self.root
        for part in relative.parts:
            names = self._names(current)
            if part not in names:
                lower = {n.lower(): n for n in names}
                if part.lower() in lower:
                    return f"case differs on disk ({lower[part.lower()]})"
                return "no such file or directory"
            current = current / part
        return None

    def _file_anchors(self, path: Path) -> set[str]:
        if path not in self._anchors:
            self._anchors[path] = anchors(path.read_text(encoding="utf-8", errors="replace"))
        return self._anchors[path]

    def check_file(self, path: Path) -> list[Problem]:
        problems = []
        path = path.resolve()
        name = path.relative_to(self.root).as_posix()
        text = path.read_text(encoding="utf-8", errors="replace")
        for line, dest in links(text):
            problem = self._check_link(path, dest)
            if problem:
                problems.append(Problem(name, line, dest, problem))
        return problems

    def _check_link(self, source: Path, dest: str) -> str | None:
        dest = dest.strip()
        if not dest:
            return None
        if SCHEME.match(dest) or dest.startswith("//"):
            self.external += 1
            return None
        self.links += 1
        target_part, _, fragment = dest.partition("#")
        target_part = target_part.split("?", 1)[0]
        target_part = unquote(target_part)
        if not target_part:
            target = source
        elif target_part.startswith("/"):
            target = Path(os.path.normpath(self.root / target_part.lstrip("/")))
        else:
            target = Path(os.path.normpath(source.parent / target_part))
        if target != source:
            reason = self._exact(target)
            if reason:
                return reason
        if not fragment:
            return None
        if target.is_dir():
            readme = target / "README.md"
            if not readme.is_file():
                return None
            target = readme
        if target.suffix.lower() not in (".md", ".markdown"):
            return None
        wanted = unquote(fragment)
        offered = self._file_anchors(target)
        if wanted in offered or wanted.lower() in {a.lower() for a in offered}:
            return None
        return f"no anchor #{wanted} in {target.relative_to(self.root).as_posix()}"


def markdown_files(root: Path) -> list[Path]:
    try:
        result = subprocess.run(
            ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", "*.md", "*.markdown"],
            cwd=root,
            capture_output=True,
            check=True,
        )
    except (OSError, subprocess.CalledProcessError):
        result = None
    if result is not None and (root / ".git").exists():
        names = sorted({n for n in result.stdout.decode().split("\0") if n})
        return [root / n for n in names if (root / n).is_file()]
    found = []
    for directory, dirs, files in os.walk(root):
        dirs[:] = sorted(d for d in dirs if d not in SKIP_DIRS)
        for f in sorted(files):
            if f.lower().endswith((".md", ".markdown")):
                found.append(Path(directory) / f)
    return found


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("files", nargs="*", type=Path, help="Markdown files (default: every one)")
    args = parser.parse_args(argv)
    root = args.root.resolve()
    files = [f.resolve() for f in args.files] if args.files else markdown_files(root)
    checker = Checker(root)
    problems: list[Problem] = []
    for path in files:
        problems.extend(checker.check_file(path))
    for problem in problems:
        print(problem, file=sys.stderr)
    summary = (
        f"{len(files)} Markdown files, {checker.links} relative links checked, "
        f"{checker.external} external links not checked"
    )
    if problems:
        print(f"{len(problems)} broken links. {summary}.", file=sys.stderr)
        return 1
    print(f"Links ok. {summary}.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
