#!/usr/bin/env python3
"""Audit every registered public itch.io deployment workflow."""

from __future__ import annotations

import argparse
import os
import re
import sys
import tomllib
import urllib.error
import urllib.request
from pathlib import Path


class AuditError(Exception):
    """A registered workflow no longer satisfies the deployment contract."""


def parse_roots(values: list[str]) -> dict[str, Path]:
    roots: dict[str, Path] = {}
    for value in values:
        repository, separator, root = value.partition("=")
        if not separator or not repository or not root:
            raise AuditError(f"invalid --root value: {value!r}")
        roots[repository] = Path(root)
    return roots


def workflow_text(repository: str, workflow: str, roots: dict[str, Path]) -> str:
    if repository in roots:
        return (roots[repository] / workflow).read_text(encoding="utf-8")

    url = f"https://raw.githubusercontent.com/{repository}/main/{workflow}"
    request = urllib.request.Request(url)
    token = os.environ.get("GH_TOKEN")
    if token:
        request.add_header("Authorization", f"Bearer {token}")
    request.add_header("User-Agent", "psoxide-itch-contract-audit")
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.read().decode("utf-8")
    except (OSError, UnicodeError, urllib.error.HTTPError) as error:
        raise AuditError(f"cannot read {repository}/{workflow}: {error}") from error


def require(workflow: str, needle: str, context: str) -> None:
    if needle not in workflow:
        raise AuditError(f"{context}: missing {needle!r}")


def audit_deployment(
    deployment: dict[str, object], action_sha: str, roots: dict[str, Path]
) -> None:
    repository = str(deployment["repository"])
    workflow_path = str(deployment["workflow"])
    target = str(deployment["target"])
    version_source = str(deployment["version_source"])
    context = f"{repository}/{workflow_path}"
    workflow = workflow_text(repository, workflow_path, roots)

    for needle in (
        "workflow_dispatch:",
        "default: false",
        "permissions:",
        "contents: read",
        "secrets.BUTLER_API_KEY",
        target,
        version_source,
        "cancel-in-progress: false",
    ):
        require(workflow, needle, context)

    if repository == "EBonura/PSoXide":
        require(workflow, "uses: ./.github/actions/itch-publish", context)
    else:
        require(
            workflow,
            f"uses: EBonura/PSoXide/.github/actions/itch-publish@{action_sha}",
            context,
        )

    for match in re.finditer(r"uses:\s+(actions/[^@\s]+)@([^\s#]+)", workflow):
        action, revision = match.groups()
        if not re.fullmatch(r"[0-9a-f]{40}", revision):
            raise AuditError(
                f"{context}: external action {action} is not pinned by full SHA"
            )

    print(f"PASS {repository} -> {target}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--registry",
        type=Path,
        default=Path(".github/itch-deployments.toml"),
    )
    parser.add_argument(
        "--root",
        action="append",
        default=[],
        metavar="REPOSITORY=PATH",
        help="read one repository from a local checkout instead of GitHub",
    )
    args = parser.parse_args()
    try:
        registry = tomllib.loads(args.registry.read_text(encoding="utf-8"))
        if registry.get("schema") != 1:
            raise AuditError("unsupported deployment registry schema")
        action_sha = str(registry["publisher_action_sha"])
        if not re.fullmatch(r"[0-9a-f]{40}", action_sha):
            raise AuditError("publisher_action_sha must be a full commit SHA")
        roots = parse_roots(args.root)
        deployments = registry.get("deployment", [])
        if not isinstance(deployments, list) or not deployments:
            raise AuditError("deployment registry is empty")
        for deployment in deployments:
            audit_deployment(deployment, action_sha, roots)
    except (AuditError, KeyError, OSError, tomllib.TOMLDecodeError) as error:
        print(f"itch deployment audit failed: {error}", file=sys.stderr)
        return 1
    print(f"itch deployment audit: PASS ({len(deployments)} workflows)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
