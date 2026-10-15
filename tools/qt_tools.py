"""Reference-style wheel bootstrap, pinned to the catalogue compiler version."""
import contextlib
import fcntl
import hashlib
import os
from pathlib import Path, PurePosixPath
import subprocess
import sys
import tempfile
import zipfile

VERSION = "6.11.2"
# sha256 of the Linux PySide6-Essentials wheels published on PyPI for VERSION
# (https://pypi.org/pypi/PySide6-Essentials/6.11.2/json). pip enforces them
# via --require-hashes and the extracted wheel is re-verified below.
WHEEL_SHA256 = {
    f"pyside6_essentials-{VERSION}-cp310-abi3-manylinux_2_34_x86_64.whl":
        "aaf9f25f0f324874085fa5b26a610318db8a8e243cf85bb3e5400595191c7778",
    f"pyside6_essentials-{VERSION}-cp310-abi3-manylinux_2_39_aarch64.whl":
        "d3ec6e1885c46e57f16364f38ea8040ffc5dc0b18341058f608fede3ba930567",
}

def matches(folder):
    for name in ("lupdate", "lrelease"):
        binary = folder / name
        if not binary.is_file():
            return False
        try:
            result = subprocess.run([str(binary), "-version"], capture_output=True, text=True)
        except OSError:
            return False
        if result.returncode or f"version {VERSION}" not in result.stdout + result.stderr:
            return False
    return True

def ensure_tools():
    cache = Path(os.environ.get("XDG_CACHE_HOME", str(Path.home() / ".cache"))) / "qt-tools"
    # Reuse a matching reference cache without replacing its contents.
    legacy = cache / "PySide6"
    if matches(legacy):
        return legacy
    folder = cache / f"plexfreq-{VERSION}" / "PySide6"
    if matches(folder):
        return folder
    cache.mkdir(parents=True, exist_ok=True)
    # Serialize concurrent clean-runner bootstraps: without a lock two
    # processes could download/extract over each other and leave a half
    # written tree that passes is_file() but fails the version check.
    with open(cache / f"plexfreq-{VERSION}.lock", "a") as lock_file:
        try:
            fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX)
        except OSError:
            pass
        try:
            if matches(folder):
                return folder
            return _fetch(folder, cache)
        finally:
            with contextlib.suppress(OSError):
                fcntl.flock(lock_file.fileno(), fcntl.LOCK_UN)


def _fetch(folder, cache):
    folder.parent.mkdir(parents=True, exist_ok=True)
    print(f"Fetching Qt Linguist {VERSION} (PySide6-Essentials wheel)", flush=True)
    with tempfile.TemporaryDirectory(prefix="plexfreq-qt-tools-", dir=cache) as work:
        requirements = Path(work) / "requirements.txt"
        hashes = " ".join(f"--hash=sha256:{digest}" for digest in sorted(WHEEL_SHA256.values()))
        requirements.write_text(f"PySide6-Essentials=={VERSION} {hashes}\n")
        downloads = Path(work) / "wheels"
        subprocess.run([sys.executable, "-m", "pip", "download", "--no-deps", "--only-binary=:all:", "--require-hashes", "--dest", str(downloads), "-r", str(requirements)], check=True)
        wheels = list(downloads.glob("*.whl"))
        if len(wheels) != 1:
            raise RuntimeError("Expected exactly one Qt tools wheel")
        expected = WHEEL_SHA256.get(wheels[0].name)
        if expected is None or hashlib.sha256(wheels[0].read_bytes()).hexdigest() != expected:
            raise RuntimeError(f"Unpinned or corrupted Qt tools wheel: {wheels[0].name}")
        with zipfile.ZipFile(wheels[0]) as wheel:
            names = [name for name in wheel.namelist() if name in ("PySide6/lupdate", "PySide6/lrelease") or name.startswith("PySide6/Qt/lib/")]
            if any(".." in PurePosixPath(name).parts for name in names):
                raise RuntimeError("Invalid Qt tools archive path")
            wheel.extractall(folder.parent, members=names)
        for name in ("lupdate", "lrelease"):
            (folder / name).chmod(0o755)
    if not matches(folder):
        raise RuntimeError("Qt Linguist bootstrap failed; check bundled library dependencies")
    return folder
