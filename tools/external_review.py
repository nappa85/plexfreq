"""Run a read-only review through opencode on a sanitized snapshot, with fallback signalling.

Exit codes: 0 review written (the first output line names the model used); 3 every model
in the fallback order was unavailable (quota, auth, timeout, empty output); 2 usage errors.
The snapshot excludes secrets, user data, build output and caches. The opencode agent
has edit/write/bash/web tools denied, so it can only read the snapshot.
"""

import argparse
import json
import os
import re
import selectors
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

DEFAULT_MODEL = "opencode/muse-spark-1.3-contributor-free"
# User decision 2026-10-16: Sol and a Sonnet subagent are the main reviewers. The third
# reviewer is the first of these that works; if none works, the review proceeds with two.
SOL_MODEL = "openai/gpt-6.1-sol"
THIRD_REVIEWER_CHAIN = (
    "opencode/space-bunny-free",
    "opencode/deepseek-v4.1-flash-free",
    "opencode/muse-spark-1.3-contributor-free",
)
CHAINS = {"main": (SOL_MODEL,), "third": THIRD_REVIEWER_CHAIN}
# Basename patterns. Besides build output and VCS data, never ship Plex session state
# or credentials (session/queue/listen/history files, tokens), logs that may carry
# authenticated media URLs, packages, archives or generated translation binaries.
EXCLUDES = (
    ".env",
    ".env.*",
    "target",
    "build",
    "thirdparty",
    ".git",
    ".venv*",
    "__pycache__",
    ".ruff_cache",
    "session.json*",
    "queue.json*",
    "listens.jsonl*",
    "history.jsonl*",
    "credentials.json",
    "tokens.json",
    "*.pem",
    "*.key",
    "*.log",
    "*.rpm",
    "*.tar.gz",
    "*.a",
    "*.qm",
    "*.pdf",
)
UNAVAILABLE = re.compile(
    r"quota|credit|rate.?limit|insufficient|unauthori[sz]ed|forbidden|429|402|"
    r"exceeded|billing|not available|model not found",
    re.IGNORECASE,
)
AGENT_CONFIG = {
    "$schema": "https://opencode.ai/config.json",
    "agent": {
        "reviewer": {
            "mode": "primary",
            "description": "Read-only adversarial reviewer",
            # Only read/glob/grep/todowrite remain. websearch could carry repo text off-box;
            # task/skill could start agents with other permissions (seen 2026-10-16).
            "tools": {
                "write": False,
                "edit": False,
                "bash": False,
                "webfetch": False,
                "websearch": False,
                "task": False,
                "skill": False,
            },
            "permission": {
                "edit": "deny",
                "bash": "deny",
                "webfetch": "deny",
                "websearch": "deny",
                "task": "deny",
                "skill": "deny",
                # Reads outside the snapshot (seen: ~/.claude/CLAUDE.md) must never be asked.
                "external_directory": "deny",
            },
        }
    },
}


def snapshot(source, dest):
    ignore = shutil.ignore_patterns(*EXCLUDES)
    shutil.copytree(source, dest, ignore=ignore, symlinks=False)
    for leftover in dest.rglob(".env*"):
        if leftover.name != ".env.example":
            raise ValueError(f"Secret-like file survived snapshot: {leftover}")
    (dest / "opencode.json").write_text(json.dumps(AGENT_CONFIG, indent=2))


def opencode_command(model, snap, prompt):
    """opencode takes its project directory from `$PWD`, not the process cwd, so a run
    launched from the real repo would ignore the snapshot. Pin it with `--dir` too."""
    return ["opencode", "run", "--dir", str(snap), "-m", model, "--agent", "reviewer", prompt]


def run_with_idle_timeout(command, cwd, total, idle):
    """Kill the run if it exceeds `total` seconds or emits nothing for `idle` seconds."""
    env = {**os.environ, "PWD": str(cwd)}
    proc = subprocess.Popen(
        command,
        cwd=cwd,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    chunks, start, last = [], time.monotonic(), time.monotonic()
    sel = selectors.DefaultSelector()
    sel.register(proc.stdout, selectors.EVENT_READ)
    while True:
        now = time.monotonic()
        if now - start > total or now - last > idle:
            proc.kill()
            proc.wait()
            chunks.append(f"\n[killed: {'total' if now - start > total else 'idle'} timeout]")
            return 124, "".join(chunks)
        if sel.select(timeout=1.0):
            line = proc.stdout.readline()
            if not line:
                break
            chunks.append(line)
            last = time.monotonic()
        elif proc.poll() is not None:
            break
    rest = proc.stdout.read()
    chunks.append(rest)
    return proc.wait(), "".join(chunks)


def preflight(model, cwd, timeout=45, attempts=3, base_delay=10):
    """Retry transient failures briefly and log each one. An exhausted quota hangs instead.
    `UnknownError` at startup usually means opencode did not load the snapshot's
    `reviewer` agent (see `opencode_command`)."""
    command = opencode_command(model, cwd, "Reply with exactly: PONG")
    for attempt in range(attempts):
        code, output = run_with_idle_timeout(command, cwd, timeout, timeout)
        if code == 0 and "PONG" in output:
            return True
        tail = " ".join(output.split())[-300:]
        print(
            f"preflight attempt {attempt + 1}/{attempts} failed (exit {code}): {tail}",
            file=sys.stderr,
        )
        if attempt + 1 < attempts:
            time.sleep(base_delay * 2**attempt)
    return False


def classify(returncode, output):
    text = output.strip()
    if returncode != 0 or not text or UNAVAILABLE.search(text[-2000:]) and len(text) < 4000:
        return "unavailable"
    return "ok"


def review_with_fallback(models, snap, prompt, total, idle):
    """Try each model in order; return (model, output) from the first usable review, or
    (None, log) when every model is unavailable."""
    attempts = []
    for model in models:
        if not preflight(model, snap):
            attempts.append(f"{model}: preflight failed")
            print(f"{model}: preflight failed; trying the next model.", file=sys.stderr)
            continue
        code, output = run_with_idle_timeout(
            opencode_command(model, snap, prompt), snap, total, idle
        )
        if classify(code, output) == "ok":
            return model, output
        attempts.append(f"{model}: unavailable (exit {code})")
        print(f"{model}: unavailable (exit {code}); trying the next model.", file=sys.stderr)
    return None, "\n".join(attempts)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True, help="Repo or worktree to snapshot")
    parser.add_argument("--prompt-file", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--diff", action="store_true", help="Include `git diff` vs HEAD")
    group = parser.add_mutually_exclusive_group()
    group.add_argument("--model", action="append", help="Model to try; repeat for a fallback order")
    group.add_argument("--chain", choices=sorted(CHAINS), help="Preset fallback order")
    parser.add_argument("--timeout", type=int, default=1800)
    parser.add_argument("--idle-timeout", type=int, default=120)
    args = parser.parse_args()
    models = args.model or CHAINS.get(args.chain) or (DEFAULT_MODEL,)
    if not args.prompt_file.is_file() or not (args.source / "AGENTS.md").is_file():
        print("Prompt file or PlexFreq source missing", file=sys.stderr)
        return 2
    with tempfile.TemporaryDirectory(prefix="plexfreq-review-") as tmp:
        snap = Path(tmp) / "snapshot"
        snapshot(args.source, snap)
        if args.diff:
            diff = subprocess.run(
                ["git", "-C", str(args.source), "diff", "HEAD"],
                capture_output=True,
                text=True,
                check=True,
            ).stdout
            status = subprocess.run(
                ["git", "-C", str(args.source), "status", "--short"],
                capture_output=True,
                text=True,
                check=True,
            ).stdout
            (snap / "REVIEW_DIFF.patch").write_text(diff)
            (snap / "REVIEW_STATUS.txt").write_text(status)
        prompt = args.prompt_file.read_text()
        model, output = review_with_fallback(models, snap, prompt, args.timeout, args.idle_timeout)
    if model is None:
        args.output.write_text(f"no reviewer available:\n{output}\n")
        print("No external reviewer available; continue without this review.", file=sys.stderr)
        return 3
    args.output.write_text(f"reviewer-model: {model}\n{output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
