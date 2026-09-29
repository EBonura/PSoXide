#!/usr/bin/env python3
"""Task-based usability check: clicks and scrolls from the home page to each answer.

Usage:
    python3 scripts/usability_check.py --set after [--site DIR] [--zola PATH]

Reads the questions and routes from scripts/usability_routes.toml, builds the
site in DIR (default: this repo), serves it on 127.0.0.1, loads each page in
headless Chrome (scripts/cdp.py) at a desktop (1440x900) and an emulated phone
(390x844) viewport, and measures where every element on each route sits. Prints a markdown table.

Cost model, per step of a route:
  - clicks: one per link followed (the last step is the answer, not a click);
  - scrolls: how many 90%-of-viewport scrolls it takes, from where the page
    opened, until the element's top plus 80 px is on screen. Elements in the
    sticky header cost 0. A link with a #fragment opens the next page at that
    element, so an answer at the anchor costs 0 scrolls.
A question can list several routes; the cheapest (fewest clicks plus scrolls,
then fewest clicks) is reported. A step on page "=" is a click that stays on the
same page and doesn't move it, such as opening a collapsed answer. "no" means no route answers it on that site.
"""

import argparse
import functools
import http.server
import json
import math
import shutil
import subprocess
import tempfile
import threading
import tomllib
from pathlib import Path

import cdp

ROOT = Path(__file__).resolve().parent.parent
VIEWPORTS = {"desktop": (1440, 900), "phone": (390, 844)}  # CSS px; the phone is emulated as a mobile device
VISIBLE_PX = 80

MEASURE_JS = r"""
(function (sels) {
  // checkVisibility also sees content hidden inside a closed <details>.
  function vis(el) { return el.checkVisibility ? el.checkVisibility() : el.getClientRects().length > 0; }
  function find(sel) {
    if (sel.indexOf("text=") === 0) {
      var want = sel.slice(5).trim();
      var els = document.querySelectorAll("h1,h2,h3,h4,a,p,span,summary,li,td,dt,button");
      for (var i = 0; i < els.length; i++) {
        var t = (els[i].textContent || "").replace(/\s+/g, " ").trim();
        if (t.indexOf(want) === 0 && vis(els[i])) return els[i];
      }
      return null;
    }
    if (/^#[^ .\[]+$/.test(sel)) {
      var byId = document.getElementById(sel.slice(1));
      return byId && vis(byId) ? byId : null;
    }
    var all;
    try { all = document.querySelectorAll(sel); } catch (e) { return null; }
    for (var j = 0; j < all.length; j++) if (vis(all[j])) return all[j];
    return null;
  }
  var out = {vh: innerHeight, vw: innerWidth, header: 0, els: {}};
  var hdr = document.querySelector(".site-header");
  if (hdr) out.header = hdr.getBoundingClientRect().height;
  sels.forEach(function (sel) {
    var el = find(sel);
    if (!el) { out.els[sel] = null; return; }
    var r = el.getBoundingClientRect();
    out.els[sel] = {top: r.top + scrollY, sticky: !!el.closest(".site-header")};
  });
  return out;
})
"""


def serve(directory):
    class Quiet(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *args):
            pass

    httpd = http.server.ThreadingHTTPServer(
        ("127.0.0.1", 0), functools.partial(Quiet, directory=str(directory)))
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    return httpd


def scrolls(top, view_top, vh):
    need = top - view_top + VISIBLE_PX - vh
    return 0 if need <= 0 else math.ceil(need / (0.9 * vh))


def route_cost(route, layouts, view):
    clicks = scr = 0
    view_top = 0
    for i, (page, sel) in enumerate(route):
        if page == "=":
            page = route[i - 1][0]
        info = layouts[(page.split("#")[0], view)]
        el = info["els"].get(sel)
        if el is None and route[i][0] == "=" and i > 0:
            # Revealed by the click (a collapsed answer): it opens right below.
            el = info["els"].get(route[i - 1][1])
        if el is None:
            return None
        if not el["sticky"]:
            n = scrolls(el["top"], view_top, info["vh"])
            scr += n
            if n:
                view_top = el["top"] + VISIBLE_PX - info["vh"]
        if i == len(route) - 1:
            break
        clicks += 1
        nxt_page = route[i + 1][0]
        if nxt_page == "=":
            continue
        view_top = 0
        if "#" in nxt_page:
            nbase, frag = nxt_page.split("#", 1)
            ninfo = layouts[(nbase, view)]
            target = ninfo["els"].get("#" + frag)
            if target:
                view_top = max(0, target["top"] - ninfo["header"])
    return clicks, scr


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--set", required=True, help="route set in usability_routes.toml: before or after")
    ap.add_argument("--site", default=str(ROOT), help="site source directory to build")
    ap.add_argument("--zola", default=shutil.which("zola") or "zola")
    ap.add_argument("--json", help="also write raw results here")
    args = ap.parse_args()

    spec = tomllib.loads((ROOT / "scripts" / "usability_routes.toml").read_text())
    questions = spec["q"]
    # page -> selectors used on it, including #fragment targets
    needed = {}
    for q in questions:
        for route in q.get(args.set, []):
            prev = None
            for page, sel in route:
                if page == "=":
                    needed.setdefault(prev, set()).add(sel)
                    continue
                base, _, frag = page.partition("#")
                prev = base
                needed.setdefault(base, set()).add(sel)
                if frag:
                    needed.setdefault(base, set()).add("#" + frag)

    with tempfile.TemporaryDirectory() as tmp:
        public = Path(tmp) / "public"
        public.mkdir()
        httpd = serve(public)
        base_url = f"http://127.0.0.1:{httpd.server_address[1]}"
        subprocess.run([args.zola, "build", "--force", "--base-url", base_url, "--output-dir", str(public)],
                       cwd=args.site, check=True, capture_output=True)
        layouts = {}
        chrome = cdp.Chrome(Path(tmp) / "profile")
        try:
            for page, sels in needed.items():
                for view, (w, h) in VIEWPORTS.items():
                    phone = view == "phone"
                    chrome.open(base_url + page, w, h, scale=2 if phone else 1, mobile=phone)
                    layouts[(page, view)] = chrome.evaluate(f"{MEASURE_JS}({json.dumps(sorted(sels))})")
        finally:
            chrome.close()
            httpd.shutdown()

    rows, raw = [], []
    for q in questions:
        cells = {}
        for view in VIEWPORTS:
            best = None
            for route in q.get(args.set, []):
                cost = route_cost([tuple(s) for s in route], layouts, view)
                if cost and (best is None or (sum(cost), cost[0]) < (sum(best), best[0])):
                    best = cost
            cells[view] = best
        raw.append({"id": q["id"], "question": q["question"], **{v: cells[v] for v in cells}})
        fmt = lambda c: "no" if c is None else f"{c[0]} / {c[1]}"
        rows.append(f"| {q['question']} | {q['audience']} | {fmt(cells['desktop'])} | {fmt(cells['phone'])} |")
    print(f"| Question | Who asks | Desktop clicks / scrolls | Phone clicks / scrolls |")
    print("|---|---|---:|---:|")
    print("\n".join(rows))
    if args.json:
        dump = {"results": raw, "layouts": {f"{p} {v}": l for (p, v), l in layouts.items()}}
        Path(args.json).write_text(json.dumps(dump, indent=1))


if __name__ == "__main__":
    main()
