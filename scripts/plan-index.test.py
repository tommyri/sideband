#!/usr/bin/env python3
"""Tests for plan-index.py. Run: python3 scripts/plan-index.test.py"""

import importlib.util
import sys
from pathlib import Path
import tempfile
import textwrap
import unittest

_spec = importlib.util.spec_from_file_location(
    "plan_index", Path(__file__).resolve().parent / "plan-index.py"
)
plan = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = plan  # dataclasses looks the module up here
_spec.loader.exec_module(plan)

INDEX = textwrap.dedent("""\
    # Task index

    | ID | Task | Size | Depends on | Status |
    |---|---|---|---|---|
    | [T-001](T-001-a.md) | First | S | — | done |
    | [T-002](T-002-b.md) | Second | S | T-001 | in-progress |
    | [T-045](T-045-c.md) | Parent | L | T-001 | awaiting-acceptance |

    Prose between tables is ignored.

    | ID | Task | Size | Depends on | Status |
    |---|---|---|---|---|
    | [T-045A](T-045A-d.md) | Split | M | T-002, T-045 | todo |
    | [T-099](T-099-e.md) | Alone | S | — | todo |

    ## Dependency graph

    <!-- plan-index:graph:start -->
    stale
    <!-- plan-index:graph:end -->

    ## Waves

    <!-- plan-index:waves:start -->
    <!-- plan-index:waves:end -->

    Text after the waves stays.
    """)


class Parse(unittest.TestCase):
    def test_reads_every_task_table_and_ignores_others(self) -> None:
        tasks = plan.parse_tasks(INDEX + "\n| Wave | Tasks |\n|---|---|\n| 1 | T-001 |\n")
        self.assertEqual([t.id for t in tasks], ["T-001", "T-002", "T-045", "T-045A", "T-099"])
        self.assertEqual(tasks[3].deps, ("T-002", "T-045"))
        self.assertEqual(tasks[0].deps, ())
        self.assertEqual(tasks[2].status, "awaiting-acceptance")

    def test_columns_are_found_by_header_name(self) -> None:
        text = "| Status | ID | Depends on |\n|---|---|---|\n| todo | [T-003](x.md) | T-001 |\n"
        (task,) = plan.parse_tasks(text)
        self.assertEqual((task.id, task.deps, task.status), ("T-003", ("T-001",), "todo"))

    def test_unreadable_dependencies_fail(self) -> None:
        with self.assertRaisesRegex(plan.PlanError, "dependencies"):
            plan.parse_tasks(INDEX.replace("T-002, T-045 |", "T-002 and soon |"))

    def test_ids_sort_numerically_then_by_suffix(self) -> None:
        ids = ["T-046", "T-045B", "T-100", "T-045", "T-045A", "T-099"]
        self.assertEqual(sorted(ids, key=plan.sort_key),
                         ["T-045", "T-045A", "T-045B", "T-046", "T-099", "T-100"])


class Generate(unittest.TestCase):
    def test_graph_has_every_edge_and_isolated_tasks(self) -> None:
        out = plan.generate(INDEX)
        self.assertIn("    T001 --> T002\n", out)
        self.assertIn("    T001 --> T045\n", out)
        self.assertIn("    T002 & T045 --> T045A\n", out)
        self.assertIn("    T099\n", out)
        self.assertNotIn("stale", out)
        self.assertTrue(out.endswith("Text after the waves stays.\n"))

    def test_waves_follow_the_longest_dependency_chain(self) -> None:
        out = plan.generate(INDEX)
        self.assertIn("| 1 | T-001, T-099 |", out)
        self.assertIn("| 2 | T-002, T-045 |", out)
        self.assertIn("| 3 | T-045A |", out)

    def test_generation_is_idempotent(self) -> None:
        once = plan.generate(INDEX)
        self.assertEqual(plan.generate(once), once)

    def test_status_edits_do_not_make_the_output_stale(self) -> None:
        once = plan.generate(INDEX)
        edited = once.replace("Second | S | T-001 | in-progress", "Second | S | T-001 | done")
        self.assertNotEqual(edited, once)
        self.assertEqual(plan.generate(edited), edited)

    def test_done_may_depend_on_awaiting_acceptance(self) -> None:
        text = INDEX.replace("| Split | M | T-002, T-045 | todo |", "| Split | M | T-045 | done |")
        self.assertIn("| 3 | T-045A |", plan.generate(text).replace("| 2 | T-002, T-045 |\n", ""))

    def test_a_new_dependency_makes_the_output_stale(self) -> None:
        once = plan.generate(INDEX)
        edited = once.replace("| Alone | S | — |", "| Alone | S | T-045A |")
        self.assertNotEqual(plan.generate(edited), edited)

    def test_cycles_unknown_tasks_statuses_and_duplicates_fail(self) -> None:
        cases = {
            "cycle": INDEX.replace("| First | S | — |", "| First | S | T-045A |"),
            "has no row": INDEX.replace("T-002, T-045 |", "T-002, T-777 |"),
            "unknown status": INDEX.replace("| todo |", "| started |", 1),
            "more than one row": INDEX.replace("[T-099](T-099-e.md)", "[T-001](T-099-e.md)"),
            "exactly one": INDEX.replace("<!-- plan-index:waves:end -->", ""),
            "is done but depends on T-002": INDEX.replace("| Parent | L | T-001 |", "| Parent | L | T-002 |")
                                                 .replace("| awaiting-acceptance |", "| done |"),
        }
        for message, text in cases.items():
            with self.subTest(message), self.assertRaisesRegex(plan.PlanError, message):
                plan.generate(text)


class TaskFiles(unittest.TestCase):
    def write(self, directory: Path, name: str, status: str, deps: str) -> None:
        (directory / name).write_text(
            f"# {name}\n\nStatus: {status}\nPhase: 1\nDepends on: {deps}\n", encoding="utf-8")

    def test_rows_must_match_their_task_files(self) -> None:
        tasks = plan.parse_tasks(INDEX)
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            self.write(directory, "T-001-a.md", "done", "—")
            self.write(directory, "T-002-b.md", "done", "T-001")
            self.write(directory, "T-045-c.md", "awaiting-acceptance", "T-001")
            self.write(directory, "T-045A-d.md", "todo", "T-045, T-002")
            problems = plan.check_task_files(tasks, directory)
        self.assertEqual(problems, [
            "T-002: INDEX says 'in-progress', T-002-b.md says 'done'",
            "T-099: T-099-e.md does not exist",
        ])

    def test_awaiting_acceptance_tasks_need_a_matrix_row(self) -> None:
        tasks = plan.parse_tasks(INDEX)
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            self.assertEqual(plan.check_acceptance(tasks, directory),
                             ["T-045 is awaiting-acceptance but ACCEPTANCE.md has no row for it"])
            (directory / "ACCEPTANCE.md").write_text("| Check | [T-045](T-045-c.md) |\n")
            self.assertEqual(plan.check_acceptance(tasks, directory), [])

    def test_check_mode_fails_on_stale_output_without_writing(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            index = Path(tmp) / "INDEX.md"
            index.write_text(INDEX, encoding="utf-8")
            (Path(tmp) / "ACCEPTANCE.md").write_text("[T-045](T-045-c.md)\n", encoding="utf-8")
            for name, status, deps in [("T-001-a.md", "done", "—"), ("T-002-b.md", "in-progress", "T-001"),
                                       ("T-045-c.md", "awaiting-acceptance", "T-001"),
                                       ("T-045A-d.md", "todo", "T-002, T-045"), ("T-099-e.md", "todo", "—")]:
                self.write(Path(tmp), name, status, deps)
            self.assertEqual(plan.main(["--check", "--index", str(index)]), 1)
            self.assertEqual(index.read_text(encoding="utf-8"), INDEX)
            self.assertEqual(plan.main(["--index", str(index)]), 0)
            self.assertEqual(plan.main(["--check", "--index", str(index)]), 0)


if __name__ == "__main__":
    unittest.main()
