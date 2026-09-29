#!/usr/bin/env python3
"""Build locally and check responsive layout, images, FAQ and theme persistence."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

import cdp
from review_shots import PAGES, ROOT, serve


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--zola', default=shutil.which('zola') or 'zola')
    args = ap.parse_args()
    results = []
    with tempfile.TemporaryDirectory() as tmp:
        public = Path(tmp) / 'public'
        public.mkdir()
        httpd = serve(public)
        base = f'http://127.0.0.1:{httpd.server_port}'
        subprocess.run([args.zola, 'build', '--base-url', base, '--output-dir', str(public), '--force'], cwd=ROOT, check=True)
        browser = cdp.Chrome(Path(tmp) / 'profile')
        try:
            browser.call('Network.enable', session=True)
            for width in (360, 390, 768, 1440):
                for scheme in ('light', 'dark'):
                    for name, route in PAGES:
                        browser.open(base + route, width, 900, mobile=width < 720, scheme=scheme)
                        browser.evaluate("document.querySelectorAll('img').forEach(i => i.loading='eager'); Promise.all([...document.images].map(i => i.decode().catch(() => null)))")
                        state = browser.evaluate("""({
                            overflow: document.documentElement.scrollWidth > innerWidth + 1,
                            images: [...document.images].filter(i => !i.complete || !i.naturalWidth).map(i => i.src),
                            h1: document.querySelectorAll('h1').length,
                            faqOpen: document.querySelectorAll('#faq details[open]').length
                        })""")
                        assert not state['overflow'], (name, width, scheme, 'horizontal overflow')
                        assert not state['images'], state['images']
                        assert state['h1'] == 1, (name, 'h1')
                        if name == 'home' and width < 720:
                            assert state['faqOpen'] == 0, 'phone quick answers should start collapsed'
                        failures = [e for e in browser._events if e.get('method') == 'Runtime.exceptionThrown' or
                                    (e.get('method') == 'Network.responseReceived' and e['params']['response']['status'] >= 400)]
                        assert not failures, (name, failures)
                        results.append({'page': name, 'width': width, 'theme': scheme, 'passed': True})
            browser.open(base + '/faq/#videos', 390, 844, mobile=True)
            assert browser.evaluate("document.getElementById('videos').open"), 'FAQ deep link did not open'
            browser.evaluate("document.querySelector('#videos summary').click()")
            assert not browser.evaluate("document.getElementById('videos').open"), 'FAQ did not close'
            browser.evaluate("document.querySelector('.theme-toggle').click()")
            theme = browser.evaluate('document.documentElement.dataset.theme')
            browser.open(base + '/projects/', 390, 844, mobile=True)
            assert browser.evaluate('document.documentElement.dataset.theme') == theme, 'theme did not persist'
            browser.open(base + '/emulator/', 1440, 900)
            assert browser.evaluate('location.pathname') == '/emulator/compare/', 'emulator redirect'
            # Previously shared project-site links keep query strings and anchors.
            for route in ('/', '/projects/', '/docs/', '/docs/first-ps1-program/', '/faq/', '/ethos/', '/emulator/', '/emulator/compare/'):
                browser.open(base + '/psoxide-site' + route + '?from=old#videos', 390, 844, mobile=True)
                expected = '/emulator/compare/' if route == '/emulator/' else route
                assert browser.evaluate('location.pathname + location.search + location.hash') == expected + '?from=old#videos', ('legacy redirect', route)
        finally:
            browser.close()
            httpd.shutdown()
    (ROOT / 'review').mkdir(exist_ok=True)
    (ROOT / 'review/browser-check.json').write_text(json.dumps(results, indent=2) + '\n')
    print(f'{len(results)} page/viewport/theme checks passed; FAQ, theme persistence and current/legacy redirects passed')


if __name__ == '__main__':
    main()
