"""Minimal headless Chrome driver over --remote-debugging-pipe (stdlib only).

Used by review_shots.py and usability_check.py. It gives real device
emulation (a 390 px phone viewport, which a plain --window-size can't do
because headless Chrome keeps a minimum window width), colour-scheme
emulation, full-page screenshots and in-page measurement. Chrome runs
headless with a throwaway profile, so no window opens.
"""

import base64
import fcntl
import json
import os
import signal
import subprocess
import time

CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"


class Chrome:
    def __init__(self, profile_dir, chrome=CHROME):
        to_chrome_r, self._w = os.pipe()
        self._r, from_chrome_w = os.pipe()

        def child_fds():
            # Chrome reads commands on fd 3 and writes replies on fd 4. Move both
            # ends out of the way first so neither overwrites the other.
            a = fcntl.fcntl(to_chrome_r, fcntl.F_DUPFD, 10)
            b = fcntl.fcntl(from_chrome_w, fcntl.F_DUPFD, 10)
            os.dup2(a, 3)
            os.dup2(b, 4)

        self.proc = subprocess.Popen(
            [chrome, "--headless=new", "--remote-debugging-pipe", "--disable-gpu",
             "--hide-scrollbars", "--no-first-run", "--no-default-browser-check",
             "--use-mock-keychain", "--password-store=basic", "--mute-audio",
             f"--user-data-dir={profile_dir}", "about:blank"],
            pass_fds=(3, 4, to_chrome_r, from_chrome_w), preexec_fn=child_fds,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
        os.close(to_chrome_r)
        os.close(from_chrome_w)
        self._buf = b""
        self._id = 0
        self._events = []
        target = self.call("Target.createTarget", url="about:blank")["targetId"]
        self.session = self.call("Target.attachToTarget", targetId=target, flatten=True)["sessionId"]
        self.call("Page.enable", session=True)
        self.call("Runtime.enable", session=True)

    def _send(self, msg):
        os.write(self._w, json.dumps(msg).encode() + b"\0")

    def _recv(self, timeout=60):
        deadline = time.time() + timeout
        while b"\0" not in self._buf:
            if time.time() > deadline:
                raise TimeoutError("no reply from Chrome")
            chunk = os.read(self._r, 1 << 20)
            if not chunk:
                raise RuntimeError("Chrome closed the pipe")
            self._buf += chunk
        raw, self._buf = self._buf.split(b"\0", 1)
        return json.loads(raw)

    def call(self, method, session=False, **params):
        self._id += 1
        msg = {"id": self._id, "method": method, "params": params}
        if session:
            msg["sessionId"] = self.session
        self._send(msg)
        while True:
            reply = self._recv()
            if reply.get("id") == self._id:
                if "error" in reply:
                    raise RuntimeError(f"{method}: {reply['error']}")
                return reply.get("result", {})
            self._events.append(reply)

    def wait_event(self, name, timeout=60):
        deadline = time.time() + timeout
        while True:
            for i, ev in enumerate(self._events):
                if ev.get("method") == name:
                    return self._events.pop(i)
            if time.time() > deadline:
                raise TimeoutError(f"no {name}")
            self._events.append(self._recv(timeout))

    def open(self, url, width, height, scale=1, mobile=False, scheme="dark"):
        self.call("Emulation.setDeviceMetricsOverride", session=True, width=width, height=height,
                  deviceScaleFactor=scale, mobile=mobile)
        self.call("Emulation.setEmulatedMedia", session=True,
                  features=[{"name": "prefers-color-scheme", "value": scheme},
                            {"name": "prefers-reduced-motion", "value": "reduce"}])
        self._events.clear()
        self.call("Page.navigate", session=True, url=url)
        self.wait_event("Page.loadEventFired")
        # Let lazy images and web fonts settle.
        self.evaluate("document.fonts.ready.then(() => true)")
        time.sleep(0.3)

    def evaluate(self, expression):
        res = self.call("Runtime.evaluate", session=True, expression=expression,
                        awaitPromise=True, returnByValue=True)
        if "exceptionDetails" in res:
            raise RuntimeError(res["exceptionDetails"])
        return res["result"].get("value")

    def screenshot_full(self, path):
        # Load every lazy image first, then capture the whole page.
        self.evaluate("document.querySelectorAll('img[loading=lazy]').forEach(i => i.loading = 'eager');"
                      "Promise.all([...document.images].map(i => i.complete ? 0 : new Promise(r => { i.onload = i.onerror = r; })))")
        m = self.call("Page.getLayoutMetrics", session=True)["cssContentSize"]
        shot = self.call("Page.captureScreenshot", session=True, format="png", captureBeyondViewport=True,
                         clip={"x": 0, "y": 0, "width": m["width"], "height": m["height"], "scale": 1})
        with open(path, "wb") as f:
            f.write(base64.b64decode(shot["data"]))

    def close(self):
        if self.proc.poll() is None:
            os.killpg(self.proc.pid, signal.SIGTERM)
            self.proc.wait(timeout=10)
        os.close(self._w)
        os.close(self._r)
