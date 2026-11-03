#!/usr/bin/env python3
"""Check tracked paths for local artifacts. Gitleaks handles secret contents."""

from pathlib import Path
import subprocess
import sys


def violation(name: str) -> str | None:
    path = Path(name)
    if any(part in {".worktrees", ".codex", ".codebase-memory", "node_modules", "target"}
           for part in path.parts):
        return "local workspace or generated output"
    if path.name.startswith(".env") and not path.name.endswith(".example"):
        return "environment data"
    if path.suffix.lower() in {".pem", ".key", ".p12", ".pfx", ".db", ".sqlite", ".sqlite3"}:
        return "credential or runtime database"
    if path.name in {"sessions.json", "pins.json", "identity.json"}:
        return "local identity or session data"
    if len(path.parts) == 1 and path.suffix.lower() in {".wav", ".zip"}:
        return "local recording or evidence export"
    return None


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    names = subprocess.check_output(["git", "ls-files", "-z"], cwd=root).decode().split("\0")
    failures = []
    for name in filter(None, names):
        reason = violation(name)
        path = root / name
        if not reason and path.is_file() and path.stat().st_size > 10 * 1024 * 1024:
            reason = "asset exceeds 10 MiB; review before publishing"
        if reason:
            failures.append(f"{name}: {reason}")
    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    print("Tracked-file publication checks passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
