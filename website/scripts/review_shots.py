#!/usr/bin/env python3
"""Render review screenshots of the built site with headless Chrome.

Usage: python3 scripts/review_shots.py [--zola PATH]

Builds the site for a local base URL, serves it on 127.0.0.1 only, and writes
full-page PNGs plus a contact sheet into review/: desktop (1440 px) and phone
(390 px, emulated as a mobile device), each in dark and light. Chrome runs
headless through scripts/cdp.py with a throwaway profile, so no window opens.
Needs Pillow for the contact sheet.
"""

import argparse
import functools
import http.server
import shutil
import subprocess
import tempfile
import threading
from pathlib import Path

from PIL import Image, ImageDraw

import cdp

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "review"
PAGES = [
    ("home", "/"),
    ("compare", "/emulator/compare/"),
    ("projects", "/projects/"),
    ("walkthrough", "/docs/first-ps1-program/"),
    ("ethos", "/ethos/"),
    ("faq", "/faq/"),
    ("docs", "/docs/"),
]
# name, CSS width, device scale, colour scheme
VIEWS = [
    ("desktop-dark", 1440, 1, "dark"),
    ("desktop-light", 1440, 1, "light"),
    ("phone-dark", 390, 2, "dark"),
    ("phone-light", 390, 2, "light"),
]


def serve(directory):
    class Quiet(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *args):
            pass

    handler = functools.partial(Quiet, directory=str(directory))
    httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    return httpd


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--zola", default=shutil.which("zola") or "zola")
    args = ap.parse_args()

    OUT.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        public = Path(tmp) / "public"
        public.mkdir()
        httpd = serve(public)
        base = f"http://127.0.0.1:{httpd.server_address[1]}"
        subprocess.run([args.zola, "build", "--force", "--base-url", base, "--output-dir", str(public)],
                       cwd=ROOT, check=True, capture_output=True)
        chrome = cdp.Chrome(Path(tmp) / "profile")
        shots = []
        try:
            for view, width, scale, scheme in VIEWS:
                phone = view.startswith("phone")
                for name, path in PAGES:
                    target = OUT / f"{name}-{view}.png"
                    chrome.open(base + path, width, 844 if phone else 900, scale=scale,
                                mobile=phone, scheme=scheme)
                    chrome.screenshot_full(target)
                    shots.append(target)
                    print("wrote", target.relative_to(ROOT))
        finally:
            chrome.close()
            httpd.shutdown()
    contact_sheet(shots)


def contact_sheet(shots):
    """One image: a row per page, columns desktop dark/light then phone dark/light, tops only."""
    col_w = {"desktop": 360, "phone": 150}
    crop_h = {"desktop": 1500, "phone": 1600}  # source px from the top of each page
    rows = []
    for name, _ in PAGES:
        tiles = []
        for view, width, scale, _ in VIEWS:
            im = Image.open(OUT / f"{name}-{view}.png").convert("RGB")
            kind = view.split("-")[0]
            im = im.crop((0, 0, im.width, min(im.height, crop_h[kind] * scale)))
            w = col_w[kind]
            im = im.resize((w, round(im.height * w / im.width)), Image.LANCZOS)
            tiles.append((view, im))
        rows.append((name, tiles))
    pad, label_h = 14, 22
    widths = [t.width for _, t in rows[0][1]]
    sheet_w = sum(widths) + pad * (len(widths) + 1)
    row_hs = [max(t.height for _, t in tiles) + label_h + pad for _, tiles in rows]
    sheet = Image.new("RGB", (sheet_w, sum(row_hs) + pad), (24, 28, 33))
    draw = ImageDraw.Draw(sheet)
    y = pad
    for (name, tiles), rh in zip(rows, row_hs):
        x = pad
        for view, im in tiles:
            draw.text((x, y + 4), f"{name}  {view}", fill=(200, 210, 220))
            sheet.paste(im, (x, y + label_h))
            x += im.width + pad
        y += rh
    sheet.save(OUT / "contact-sheet.png", optimize=True)
    print("wrote", (OUT / "contact-sheet.png").relative_to(ROOT))


if __name__ == "__main__":
    main()
