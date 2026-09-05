#!/usr/bin/env python3
"""Materialize locked source components at the paths expected by Cargo.

Imported files are build inputs, not a second maintained copy of the source.
The receipt checks their content before reuse or replacement. Local sources
export the locked Git revision, never unstaged files.
"""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import tarfile
import tempfile
import urllib.request


def digest(data):
    return hashlib.sha256(data).hexdigest()


def relative(value):
    path = PurePosixPath(value)
    if path.is_absolute() or ".." in path.parts or not path.parts:
        raise ValueError(f"Unsafe component path: {value}")
    return str(path)


def verify(root, receipt):
    for name, expected in receipt.get("files", {}).items():
        path = root / relative(name)
        if not path.is_file() or path.is_symlink() or digest(path.read_bytes()) != expected:
            raise RuntimeError(f"Imported file changed or missing: {path}. Preserve your edits before refreshing components.")


def materialize(root, sources, check=False, lock_path=None):
    root = root.resolve()
    if not check:
        root.mkdir(parents=True, exist_ok=True)
    lock_path = Path(lock_path) if lock_path else root / "components.lock.json"
    lock_bytes = lock_path.read_bytes()
    lock = json.loads(lock_bytes)
    if lock.get("schema") != 1:
        raise ValueError("Unsupported component lock schema")
    receipt_path = root / ".components-receipt.json"
    previous = json.loads(receipt_path.read_text()) if receipt_path.exists() else {}
    verify(root, previous)
    if previous.get("lock_sha256") == digest(lock_bytes):
        print("Components match the lock and content receipt")
        return
    if check:
        raise RuntimeError("Components are not bootstrapped at the locked revisions")

    incoming = {}
    for name, spec in lock["components"].items():
        rev = spec["revision"]
        if not re.fullmatch(r"[0-9a-f]{40}", rev):
            raise ValueError(f"{name}: require a full Git commit")
        paths = [relative(path) for path in spec["paths"]]
        if name in sources:
            data = subprocess.check_output(["git", "-C", sources[name], "archive", rev, "--", *paths])
            strip = False
        else:
            repo = spec["repository"]
            if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo):
                raise ValueError(f"Invalid GitHub repository: {repo}")
            with urllib.request.urlopen(f"https://api.github.com/repos/{repo}/tarball/{rev}", timeout=120) as response:
                data = response.read()
            strip = True
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:*") as archive:
            found = set()
            for member in archive:
                filename = member.name.split("/", 1)[1] if strip and "/" in member.name else member.name
                if not any(filename == path or filename.startswith(path + "/") for path in paths):
                    continue
                if member.isdir():
                    continue
                if not member.isfile():
                    raise ValueError(f"Unsupported component entry: {filename}")
                filename = relative(filename)
                found.update(path for path in paths if filename == path or filename.startswith(path + "/"))
                if filename in incoming:
                    raise ValueError(f"Components overlap: {filename}")
                incoming[filename] = (archive.extractfile(member).read(), member.mode & 0o777)
            missing = set(paths) - found
            if missing:
                raise ValueError(f"{name}: locked source paths missing: {sorted(missing)}")
        print(f"Resolved {name} at {rev}")

    # Validate every collision before changing any files.
    tracked = set(subprocess.check_output(["git", "-C", str(root), "ls-files", "-z"]).decode().split("\0"))
    for name in incoming:
        path = root / name
        if name in tracked or (path.exists() and name not in previous.get("files", {})):
            raise RuntimeError(f"Refusing to replace an owned file: {name}")
        if any(parent.is_symlink() for parent in path.parents if parent.is_relative_to(root)):
            raise RuntimeError(f"Refusing a symlink parent: {name}")
    receipt = {"schema": 1, "lock_sha256": digest(lock_bytes), "components": lock["components"], "files": {}}
    for name, (data, mode) in incoming.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as temp:
            temp.write(data)
            temporary = Path(temp.name)
        temporary.chmod(mode)
        os.replace(temporary, path)
        receipt["files"][name] = digest(data)
    for name in previous.get("files", {}):
        if name not in incoming:
            (root / name).unlink()
    temporary = receipt_path.with_suffix(".tmp")
    temporary.write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n")
    os.replace(temporary, receipt_path)
    print(f"Bootstrapped {len(incoming)} files; component receipt recorded")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--source", action="append", default=[], metavar="NAME=GIT_CHECKOUT")
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--lock", type=Path, help="Lock outside an ignored generated source tree")
    args = parser.parse_args()
    sources = dict(item.split("=", 1) for item in args.source)
    materialize(args.root.resolve(), sources, args.check, args.lock)


if __name__ == "__main__":
    main()
