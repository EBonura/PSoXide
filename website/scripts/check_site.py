#!/usr/bin/env python3
"""Check built HTML, assets and fragments, including GitHub Pages subpaths.

Usage: python3 scripts/check_site.py --base-url https://ebonura.github.io/
External availability is deliberately checked separately from deterministic CI.
"""
import argparse
import json
import re
from collections import Counter
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urljoin, urlsplit


class Page(HTMLParser):
    def __init__(self, text):
        super().__init__()
        self.ids, self.links, self.errors = [], [], []
        self.listings, self.current_listing = {}, None
        self.feed(text)
        self.errors += [f'duplicate id: {key}' for key, n in Counter(self.ids).items() if n > 1]

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if tag == 'pre' and attrs.get('aria-label', '').endswith(' source'):
            self.current_listing = attrs['aria-label'][:-7]
            self.listings[self.current_listing] = ''
        if 'id' in attrs:
            self.ids.append(attrs['id'])
        for key in ('href', 'src'):
            if key in attrs:
                self.links.append(attrs[key])
        if tag == 'img' and not attrs.get('alt'):
            self.errors.append('image has no descriptive alt text')
        if {'flag', 'draft-banner'} & set(attrs.get('class', '').split()):
            self.errors.append('unresolved review note')

    def handle_endtag(self, tag):
        if tag == 'pre':
            self.current_listing = None

    def handle_data(self, data):
        if self.current_listing is not None:
            self.listings[self.current_listing] += data


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--base-url', required=True)
    ap.add_argument('--output-dir', default='public')
    args = ap.parse_args()
    root = Path(args.output_dir).resolve()
    base = args.base_url.rstrip('/') + '/'
    origin = urlsplit(base)
    pages = {p: Page(p.read_text()) for p in root.rglob('*.html')}
    errors = []
    if not pages:
        errors.append('no built HTML found')
    for path, page in pages.items():
        name = path.relative_to(root)
        errors.extend(f'{name}: {e}' for e in page.errors)
        for link in page.links:
            url = urlsplit(urljoin(base + str(name), link))
            if url.hostname in ('localhost', '127.0.0.1') and origin.hostname not in ('localhost', '127.0.0.1'):
                errors.append(f'{name}: local URL in production: {link}')
            if url.netloc != origin.netloc or url.scheme not in ('http', 'https'):
                continue
            if url.path.rstrip('/') == origin.path.rstrip('/'):
                target = root / 'index.html'
            elif url.path.startswith(origin.path):
                target = root / unquote(url.path[len(origin.path):])
                if target.is_dir():
                    target /= 'index.html'
            else:
                errors.append(f'{name}: escapes site base path: {link}')
                continue
            if not target.exists():
                errors.append(f'{name}: missing target: {link}')
            elif url.fragment and target in pages:
                ids = pages[target].ids
                valid = url.fragment in ids or unquote(url.fragment) in ids
                # Rustdoc uses literal percent-encoded ids for generics, and
                # its source viewer resolves #start-end to two numbered lines.
                if not valid and target.is_relative_to(root / 'api') and '/src/' in target.as_posix():
                    span = re.fullmatch(r'(\d+)-(\d+)', url.fragment)
                    valid = bool(span and all(line in ids for line in span.groups()))
                if not valid:
                    errors.append(f'{name}: missing fragment: {link}')
    reference = Path(__file__).resolve().parents[1] / 'data/sdk-reference.json'
    if reference.exists():
        for example in json.loads(reference.read_text())['examples']:
            path = root / 'docs/examples' / example['name'] / 'index.html'
            for source in example['files']:
                if path not in pages or pages[path].listings.get(source['path']) != source['code']:
                    errors.append(f'{path.relative_to(root)}: complete source differs: {source["path"]}')
    for error in errors:
        print(error)
    print(f'{len(pages)} HTML pages checked; {len(errors)} errors')
    raise SystemExit(bool(errors))


if __name__ == '__main__':
    main()
