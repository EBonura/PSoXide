#!/usr/bin/env python3
"""Publish searchable rustdoc for the PS1 target and the host GTE backend."""
import argparse
import html
import shutil
import subprocess
import tempfile
import tomllib
from pathlib import Path

SITE = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--sdk-root', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--base-url', default='https://ebonura.github.io/PSoXide')
    args = parser.parse_args()
    repo, target = args.sdk_root.resolve(), args.target_dir.resolve()
    pins = tomllib.loads((SITE / 'data/examples.toml').read_text())
    subprocess.run(['git', 'diff', '--exit-code', pins['sdk_revision'], '--', 'sdk', 'crates'],
                   cwd=repo, check=True, stdout=subprocess.DEVNULL)
    for generated in (target / 'ps1/mipsel-sony-psx/doc', target / 'host/doc'):
        if generated.exists():
            shutil.rmtree(generated)
    # Keep a route back to the guides above rustdoc's own navigation.
    with tempfile.TemporaryDirectory(prefix='sdk-rustdoc-') as scratch:
        banner = Path(scratch) / 'navigation.html'
        banner.write_text('<div style="padding:12px 24px;border-bottom:1px solid #888">'
                          f'<a href="{html.escape(args.base_url.rstrip("/"))}/docs/sdk/">'
                          '← PSoXide SDK guides and examples</a> · '
                          'SDK ' + pins['sdk_revision'][:12] +
                          ' · All Cargo features enabled; check feature gates before use.</div>')
        import os
        env = dict(os.environ, RUSTDOCFLAGS=f'--html-before-content {banner}')
        common = ['cargo', 'doc', '--locked', '--no-deps', '--manifest-path', 'sdk/Cargo.toml']
        subprocess.run(common + ['--workspace', '--all-features', '--target', 'mipsel-sony-psx',
                                 '-Z', 'build-std=core,alloc', '--target-dir', str(target / 'ps1')],
                       cwd=repo, env=env, check=True)
        subprocess.run(['cargo', 'doc', '--locked', '--no-deps', '-p', 'psx-hw', '-p', 'psxed-format',
                        '--target', 'mipsel-sony-psx', '-Z', 'build-std=core,alloc',
                        '--target-dir', str(target / 'ps1')], cwd=repo, env=env, check=True)
        subprocess.run(common + ['--all-features', '-p', 'psx-gte', '-p', 'psx-gte-core', '-p', 'psx-math', '--target-dir', str(target / 'host')],
                       cwd=repo, env=env, check=True)
    output = SITE / 'static/api'
    output.mkdir(exist_ok=True)
    for name, source in [('ps1', target / 'ps1/mipsel-sony-psx/doc'), ('host', target / 'host/doc')]:
        destination = output / name
        if destination.exists():
            shutil.rmtree(destination)  # Only this script's generated rustdoc output.
        shutil.copytree(source, destination)
        # Cargo does not create a root index without --enable-index-page. Supply
        # one so rustdoc's Help/Settings breadcrumb has a real destination.
        entries = sorted(p.parent.name for p in destination.glob('*/index.html')
                         if p.parent.name.startswith(('psx_', 'psxed_')))
        (destination / 'index.html').write_text(
            '<!doctype html><html lang="en"><meta charset="utf-8">'
            '<meta name="viewport" content="width=device-width,initial-scale=1">'
            '<title>PSoXide API reference</title><h1>PSoXide API reference</h1>'
            '<p><a href="../../docs/sdk/">SDK guides and examples</a></p><ul>' +
            ''.join(f'<li><a href="{crate}/index.html">{crate}</a></li>' for crate in entries) + '</ul></html>')
        # Correct two upstream documentation links without changing the pinned
        # SDK source. Ord links come from core's inherited iterator docs.
        for document in destination.rglob('*.html'):
            content = document.read_text()
            content = content.replace('href="Ord#lexicographical-comparison"',
                                      'href="https://doc.rust-lang.org/core/cmp/trait.Ord.html#lexicographical-comparison"')
            if document.relative_to(destination).as_posix() == 'psx_mc/sio/index.html':
                content = content.replace('href="../psx_pad"', 'href="../../psx_pad/index.html"')
            document.write_text(content)
        # rustdoc emits an async implementor script reference even for a trait
        # with no emitted implementations. Supply its empty registry.
        import re
        for document in destination.rglob('*.html'):
            for relative in re.findall(r'<script src="([^"]*trait\.impl/[^"]+\.js)"', document.read_text()):
                script = (document.parent / relative).resolve()
                if not script.is_relative_to(destination.resolve()):
                    raise ValueError('Implementor script escapes rustdoc output')
                if not script.exists():
                    script.parent.mkdir(parents=True, exist_ok=True)
                    script.write_text('if(window.register_implementors){window.register_implementors({});}else{window.pending_implementors={};}\n')
    shutil.copyfile(repo / 'LICENSE', output / 'LICENSE.txt')
    print('Staged PS1 API and host GTE API under static/api/')


if __name__ == '__main__':
    main()
