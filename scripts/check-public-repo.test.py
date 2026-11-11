#!/usr/bin/env python3
"""Tests for check-public-repo.py. Run: python3 scripts/check-public-repo.test.py"""

import importlib.util
from pathlib import Path
import unittest

_spec = importlib.util.spec_from_file_location(
    "check_public_repo", Path(__file__).resolve().parent / "check-public-repo.py"
)
check = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(check)


class Violation(unittest.TestCase):
    def assertFlagged(self, name: str) -> None:
        self.assertIsNotNone(check.violation(name), name)

    def assertAllowed(self, name: str) -> None:
        self.assertIsNone(check.violation(name), name)

    def test_pin_files_by_any_prefix_and_directory(self) -> None:
        self.assertFlagged("pins.json")
        self.assertFlagged("socialspeak-pins.json")
        self.assertFlagged("sideband-pins.json")
        self.assertFlagged("apps/cli/data/socialspeak-pins.json")

    def test_identity_and_session_files_anywhere(self) -> None:
        self.assertFlagged("data/sessions.json")
        self.assertFlagged("x/identity.json")

    def test_recordings_and_exports_in_subdirectories(self) -> None:
        self.assertFlagged("capture.wav")
        self.assertFlagged("evidence.zip")
        self.assertFlagged("docs/plan/notes/run-3/capture.WAV")
        self.assertFlagged("deploy/out/evidence.zip")

    def test_test_fixtures_may_carry_audio(self) -> None:
        self.assertAllowed("crates/ss-audio/tests/fixtures/vad/speech-keyboard.wav")
        self.assertFlagged("crates/ss-audio/tests/fixtures/evidence.zip")

    def test_ordinary_files_pass(self) -> None:
        self.assertAllowed("crates/ss-net/src/pins.rs")
        self.assertAllowed("apps/desktop/package.json")
        self.assertAllowed("deploy/.env.example")
        self.assertFlagged("deploy/.env")


if __name__ == "__main__":
    unittest.main()
