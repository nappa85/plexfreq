import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import external_review


class FakeProc:
    def __init__(self):
        read_fd, write_fd = os.pipe()  # selectors cannot poll regular files
        os.write(write_fd, b"PONG\n")
        os.close(write_fd)
        self.stdout = os.fdopen(read_fd)

    def poll(self):
        return 0

    def wait(self):
        return 0


class OpencodeDirectoryTests(unittest.TestCase):
    def test_runs_are_pinned_to_the_snapshot_not_the_launch_pwd(self):
        """Regression: opencode uses `$PWD`, so a run launched from the real repo ignored
        the snapshot's `reviewer` agent and failed with UnknownError (2026-10-16)."""
        calls = []

        def fake_popen(command, **kwargs):
            calls.append((command, kwargs))
            return FakeProc()

        with (
            tempfile.TemporaryDirectory() as tmp,
            mock.patch.dict("os.environ", {"PWD": "/real/repo"}),
            mock.patch.object(subprocess, "Popen", fake_popen),
        ):
            snap = Path(tmp)
            self.assertTrue(external_review.preflight("m", snap, attempts=1))
        command, kwargs = calls[0]
        self.assertEqual(command[command.index("--dir") + 1], str(snap))
        self.assertEqual(kwargs["env"]["PWD"], str(snap))
        self.assertEqual(kwargs["cwd"], snap)
        self.assertIs(kwargs["stdin"], subprocess.DEVNULL)


class ReviewerAgentTests(unittest.TestCase):
    def test_reviewer_cannot_reach_network_shell_writes_or_subagents(self):
        agent = external_review.AGENT_CONFIG["agent"]["reviewer"]
        for tool in ("write", "edit", "bash", "webfetch", "websearch", "task", "skill"):
            self.assertIs(agent["tools"][tool], False, tool)
        for tool in (
            "edit",
            "bash",
            "webfetch",
            "websearch",
            "task",
            "skill",
            "external_directory",
        ):
            self.assertEqual(agent["permission"][tool], "deny", tool)


class SnapshotTests(unittest.TestCase):
    def test_snapshot_never_ships_session_state_logs_packages_or_builds(self):
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "repo"
            kept = ["AGENTS.md", "src/lib.rs", "app/translations/harbour-plexfreq_it.ts"]
            dropped = [
                "session.json",
                "session.json.1234.tmp",
                "queue.json",
                "listens.jsonl",
                "history.jsonl",
                "logs/plexfreq-2026-10-16.log",
                "build/sailfish/RPMS/harbour-plexfreq-0.1.6-1.aarch64.rpm",
                "app/thirdparty/libplexfreq_core.a",
                "app/translations/harbour-plexfreq_it.qm",
                "target/debug/x",
                ".env",
            ]
            for name in kept + dropped:
                path = source / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("x")
            snap = Path(tmp) / "snap"
            external_review.snapshot(source, snap)
            for name in kept:
                self.assertTrue((snap / name).exists(), name)
            for name in dropped:
                self.assertFalse((snap / name).exists(), name)


class FallbackTests(unittest.TestCase):
    def run_chain(self, working):
        """`working` is the set of models whose preflight and review succeed."""
        calls = []

        def fake_preflight(model, snap):
            calls.append(("preflight", model))
            return model in working

        def fake_run(command, cwd, total, idle):
            model = command[command.index("-m") + 1]
            calls.append(("review", model))
            return 0, f"review by {model}"

        with (
            mock.patch.object(external_review, "preflight", fake_preflight),
            mock.patch.object(external_review, "run_with_idle_timeout", fake_run),
        ):
            result = external_review.review_with_fallback(
                external_review.THIRD_REVIEWER_CHAIN, Path("/snap"), "p", 10, 10
            )
        return result, calls

    def test_third_reviewer_order_is_bunny_deepseek_muse(self):
        self.assertEqual(
            external_review.THIRD_REVIEWER_CHAIN,
            (
                "opencode/space-bunny-free",
                "opencode/deepseek-v4.1-flash-free",
                "opencode/muse-spark-1.3-contributor-free",
            ),
        )
        self.assertEqual(external_review.CHAINS["main"], ("openai/gpt-6.1-sol",))

    def test_first_working_model_is_used_and_later_ones_not_tried(self):
        bunny, deepseek, muse = external_review.THIRD_REVIEWER_CHAIN
        (model, output), calls = self.run_chain({bunny, deepseek})
        self.assertEqual(model, bunny)
        self.assertNotIn(("preflight", deepseek), calls)

    def test_falls_through_to_muse(self):
        bunny, deepseek, muse = external_review.THIRD_REVIEWER_CHAIN
        (model, output), calls = self.run_chain({muse})
        self.assertEqual(model, muse)
        self.assertEqual(
            [c for c in calls if c[0] == "preflight"],
            [("preflight", bunny), ("preflight", deepseek), ("preflight", muse)],
        )

    def test_no_model_available_returns_none(self):
        (model, log), _ = self.run_chain(set())
        self.assertIsNone(model)
        self.assertEqual(log.count("preflight failed"), 3)


if __name__ == "__main__":
    unittest.main()
