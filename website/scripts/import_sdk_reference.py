#!/usr/bin/env python3
"""Generate crate structure and complete example sources from the player SDK pin."""
import argparse
import json
import re
import subprocess
import tomllib
from pathlib import Path

SITE = Path(__file__).resolve().parents[1]


def generate(repo):
    pins = tomllib.loads((SITE / 'data/examples.toml').read_text())
    revision = pins['sdk_revision']
    # Do not label a different source tree with the published example revision.
    subprocess.run(['git', 'diff', '--exit-code', revision, '--', 'sdk', 'crates'],
                   cwd=repo, check=True, stdout=subprocess.DEVNULL)
    tracked = subprocess.check_output(
        ['git', 'ls-tree', '-r', '--name-only', revision, 'sdk'], cwd=repo, text=True).splitlines()
    tracked = set(tracked)
    workspace = tomllib.loads((repo / 'sdk/Cargo.toml').read_text())
    names = {Path(member).name for member in workspace['workspace']['members']}

    def dependencies(manifest):
        result = []
        tables = [('all targets', manifest.get('dependencies', {}))]
        tables += [(target, value.get('dependencies', {}))
                   for target, value in manifest.get('target', {}).items()]
        for target, deps in tables:
            for name, spec in sorted(deps.items()):
                spec = {'version': spec} if isinstance(spec, str) else spec
                result.append({'name': name, 'target': target,
                               'optional': spec.get('optional', False),
                               'features': spec.get('features', []), 'sdk': name in names})
        return result

    crates = []
    for member in workspace['workspace']['members']:
        directory = repo / 'sdk' / member
        manifest = tomllib.loads((directory / 'Cargo.toml').read_text())
        files = []
        layout = [directory / 'Cargo.toml']
        layout += [p for p in [directory / 'README.md'] if p.exists()]
        layout += sorted(directory.glob('src/**/*.rs'))
        layout += sorted(directory.glob('tests/**/*.rs'))
        for path in layout:
            relative = path.relative_to(repo).as_posix()
            if relative not in tracked:
                raise ValueError(f'Unpinned source: {relative}')
            docs = re.findall(r'^//! ?(.*)$', path.read_text(), re.M)
            summary = next((line for line in docs if line.strip()), '')
            # Crate overviews link to the real API reference, not unresolved rustdoc shortcuts.
            summary = re.sub(r'\[(`[^`]+`)\]', r'\1', summary)
            files.append({'path': path.relative_to(directory).as_posix(), 'summary': summary})
        crates.append({'name': manifest['package']['name'],
                       'description': manifest['package']['description'],
                       'features': manifest.get('features', {}),
                       'dependencies': dependencies(manifest), 'files': files})

    examples = []
    for path in sorted((repo / 'sdk/examples').glob('*/Cargo.toml')):
        if path.relative_to(repo).as_posix() not in tracked:
            raise ValueError(f'Unpinned manifest: {path}')
        manifest = tomllib.loads(path.read_text())
        directory = path.parent
        files = [path] + sorted(directory.glob('src/**/*.rs'))
        for source in files:
            if source.relative_to(repo).as_posix() not in tracked:
                raise ValueError(f'Unpinned example source: {source}')
        examples.append({'name': manifest['package']['name'],
                         'dependencies': dependencies(manifest),
                         'files': [{'path': f.relative_to(directory).as_posix(),
                                    'language': 'toml' if f == path else 'rust',
                                    'lines': len(f.read_text().splitlines()),
                                    'code': f.read_text()} for f in files]})
    return {'revision': revision, 'crates': crates, 'examples': examples}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--sdk-root', type=Path, default=SITE.parent)
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    data = generate(args.sdk_root.resolve())
    text = json.dumps(data, indent=2, ensure_ascii=False) + '\n'
    destination = SITE / 'data/sdk-reference.json'
    if args.check:
        if not destination.exists() or destination.read_text() != text:
            raise SystemExit('SDK reference is stale; run scripts/import_sdk_reference.py')
    else:
        destination.write_text(text)
    for kind in ('crates', 'examples'):
        for item in data[kind]:
            if not (SITE / 'content/docs' / kind / (item['name'] + '.md')).is_file():
                raise SystemExit(f'Missing {kind} guide: {item["name"]}')
    print(f'{len(data["crates"])} crate structures; {len(data["examples"])} complete examples')


if __name__ == '__main__':
    main()
