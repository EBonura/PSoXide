#!/usr/bin/env python3
"""Flag firmware artifacts and vendor text in homebrew EXE headers.

The default scans tracked working files (plus untracked, nonignored files).
--history scans objects reachable from fetched remote refs and tags. This is
a heuristic inventory, not a proof of source authorship; see the audit report.
"""
import argparse
from pathlib import Path
import re
import subprocess
import sys

ARTIFACT = re.compile(r"(?:^|/)(?:bios|scph|sce[aei]?)[^/]*\.(?:bin|rom|img|png|jpg)$|bios.*\.(?:png|jpg)$|(?:^|/)psyq(?:/|\.)", re.I)
VENDOR = b"sony computer entertainment"


def reasons(path, size, header):
    found = []
    if ARTIFACT.search(path):
        found.append("firmware/SDK artifact path")
    if size == 524288:
        found.append("512 KiB blob; inspect its provenance")
    if header.startswith(b"PS-X EXE") and VENDOR in header[0x4c:0x800].lower():
        found.append("vendor text in homebrew executable header")
    if header.startswith(b"PS-X EXE") and b"sony bios" in header.lower():
        found.append("vendor firmware prompt in homebrew executable")
    return found


def git(repo, *args, input=None):
    return subprocess.check_output(["git", "-C", str(repo), *args], input=input)


def scan_working(repo):
    paths = git(repo, "ls-files", "-z", "--cached", "--others", "--exclude-standard")
    for raw in sorted(set(paths.split(b"\0")) - {b""}):
        name = raw.decode("utf-8", "surrogateescape")
        path = repo / name
        if path.is_symlink() or not path.is_file():
            continue
        with path.open("rb") as stream:
            header = stream.read(4 * 1024 * 1024 if name.lower().endswith(".exe") else 2048)
        yield name, path.stat().st_size, header


def scan_history(repo):
    refs = git(repo, "for-each-ref", "--format=%(refname)", "refs/remotes", "refs/tags").splitlines()
    if not refs:
        raise RuntimeError("no fetched remote refs or tags; refusing an empty history audit")
    objects = git(repo, "rev-list", "--objects", "--stdin", input=b"\n".join(refs) + b"\n")
    metadata = git(repo, "cat-file", "--batch-check=%(objecttype) %(objectname) %(objectsize) %(rest)", input=objects)
    for raw in metadata.splitlines():
        kind, oid, size, *rest = raw.split(b" ", 3)
        if kind != b"blob":
            continue
        name = rest[0].decode("utf-8", "surrogateescape") if rest else oid.decode()
        size = int(size)
        # Inspect executable headers with a bounded read.
        process = subprocess.Popen(["git", "-C", str(repo), "cat-file", "blob", oid.decode()], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL) if name.lower().endswith(".exe") else None
        header = b""
        if process:
            header = process.stdout.read(4 * 1024 * 1024)
            process.stdout.close()
            result = process.wait()
            if result not in (0, -13, 141):
                raise RuntimeError(f"cannot inspect object {oid.decode()}")
        yield name, size, header


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    parser.add_argument("--history", action="store_true")
    args = parser.parse_args()
    repo = args.repo.resolve()
    failures = 0
    count = 0
    try:
        for name, size, header in (scan_history(repo) if args.history else scan_working(repo)):
            count += 1
            for reason in reasons(name, size, header):
                print(f"FINDING: {name}: {reason}")
                failures += 1
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"Audit failed: {error}", file=sys.stderr)
        return 2
    print(f"Scanned {count} files/blobs; {failures} heuristic findings. Source provenance still requires review.")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
