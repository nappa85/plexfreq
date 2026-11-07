"""External reviewers (the external-agents skill) receive the tracked and untracked
non-ignored files of the checkout. These tests keep private PlexFreq data out of it."""

import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Names the app writes in its data/cache directories, plus credentials, logs and
# build or package output that must never reach a reviewer.
PRIVATE = (
    "session.json",
    "session.json.2.tmp",
    ".session-1.tmp",
    "queue.json",
    "queue.json.3.tmp",
    "queue.json.corrupt-6",
    "resume.json",
    "listens.jsonl",
    "listens.jsonl.4.tmp",
    "history.jsonl",
    "history.jsonl.5.tmp",
    "audio-cache/1.flac",
    "artwork-cache/1.jpg",
    "library-cache/index.json",
    "credentials.json",
    "tokens.json",
    ".env",
    "key.pem",
    "plexfreq.log",
    "app/RPMS/harbour-plexfreq.rpm",
    "target/debug/plexfreq",
    "build/CMakeCache.txt",
    "review-notes/plan.md",
)


class SnapshotExclusionTests(unittest.TestCase):
    def test_private_files_are_ignored_anywhere(self):
        for name in PRIVATE:
            for path in (name, f"data/{name}"):
                with self.subTest(path=path):
                    if path.startswith("data/") and name.split("/")[0] in (
                        "app", "target", "build", "review-notes"
                    ):
                        continue  # rooted patterns
                    result = subprocess.run(
                        ["git", "check-ignore", "--no-index", "-q", path], cwd=ROOT
                    )
                    self.assertEqual(result.returncode, 0, f"{path} is not ignored")


if __name__ == "__main__":
    unittest.main()
