#!/usr/bin/env python3
"""Local browser QA pages. Standard library only; stores no production data."""

import argparse
from collections import deque
from http import cookies
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import threading
import time
from urllib.parse import urlparse


COOKIE_NAME = "mapbrowser_qa"
HEARTBEATS = {}
EVENTS = deque(maxlen=500)
LOCK = threading.Lock()
STARTED = time.monotonic()

PAGE = r"""<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Mapbrowser QA __TITLE__</title>
<style>
  :root { color-scheme: light; font: 17px/1.5 -apple-system, sans-serif; color: #23364a; background: #eef3f8; }
  body { max-width: 900px; margin: 32px auto; padding: 0 28px; }
  h1 { margin-bottom: 0; } h2 { font-size: 21px; }
  .card { padding: 20px 24px; margin: 24px 0; background: white; border: 1px solid #d4e0ed; border-radius: 14px; }
  input, button { font: inherit; padding: 10px 14px; border: 1px solid #a7bcd2; border-radius: 8px; }
  input { max-width: 70%; } button { cursor: pointer; background: #e8f0fc; }
  dt { font-weight: 600; } dd { margin: 0 0 10px; overflow-wrap: anywhere; }
  code { font-size: 14px; } a { color: #145db8; } #status { min-height: 1.5em; }
</style>
<h1>Browser test __TITLE__</h1>
<p>Local fixture at <code id="origin"></code>. Use this same address in each workspace.</p>
<nav><a href="/">First page</a> · <a href="/second">Second page</a> · <a href="/quiet">Quiet page</a> · <a href="#bottom">Scroll to bottom</a></nav>
<section class="card">
  <h2>Workspace storage</h2>
  <form id="save-form">
    <label for="value">Test value</label><br>
    <input id="value" name="value" autocomplete="off" placeholder="e.g. work-only" maxlength="80">
    <button type="submit">Save value</button>
    <button id="clear" type="button">Clear value</button>
  </form>
  <p id="status" role="status">Loading saved values...</p>
  <dl><dt>Persistent cookie, read by server</dt><dd id="cookie">Loading...</dd>
      <dt>Local storage, read by page</dt><dd id="local">Loading...</dd></dl>
</section>
<section class="card">
  <h2>Background activity</h2>
  <dl><dt>Page instance</dt><dd><code id="instance"></code></dd>
      <dt>Elapsed seconds since this document loaded</dt><dd id="elapsed">0</dd>
      <dt>Heartbeat ticks</dt><dd id="ticks">0</dd>
      <dt>Server acknowledged ticks</dt><dd id="ack">0</dd>
      <dt>Page visibility</dt><dd id="visibility"></dd></dl>
  <p>Each tick sends one request. Switch workspaces, wait, and inspect <a href="/api/heartbeats" target="_blank">server heartbeat history</a> from another browser or the terminal.</p>
</section>
<div id="long-content"></div>
<section class="card" id="bottom"><h2>Bottom of page</h2><p>Scroll and keyboard page navigation reached the end.</p><a href="#">Back to top</a></section>
<script>
  const $ = id => document.getElementById(id);
  const storageKey = 'mapbrowser-qa-value';
  const instance = crypto.randomUUID();
  const started = performance.now();
  let ticks = 0;
  $('origin').textContent = location.origin;
  $('instance').textContent = instance;
  const show = value => value === null || value === '' ? '(empty)' : value;
  async function refresh() {
    const response = await fetch('/api/state', {cache: 'no-store'});
    if (!response.ok) throw new Error('State request failed: ' + response.status);
    const state = await response.json();
    $('cookie').textContent = show(state.cookie);
    $('local').textContent = show(localStorage.getItem(storageKey));
  }
  $('save-form').addEventListener('submit', async event => {
    event.preventDefault();
    try {
      const value = $('value').value;
      const response = await fetch('/api/state', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({value})});
      if (!response.ok) throw new Error('Save failed: ' + response.status);
      localStorage.setItem(storageKey, value);
      await refresh();
      $('status').textContent = 'Saved. Reload, change workspace, or restart the browser to check persistence.';
    } catch (error) { $('status').textContent = String(error); }
  });
  $('clear').addEventListener('click', async () => {
    try {
      const response = await fetch('/api/state', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({value: ''})});
      if (!response.ok) throw new Error('Clear failed: ' + response.status);
      localStorage.removeItem(storageKey);
      $('value').value = '';
      await refresh();
      $('status').textContent = 'Cleared.';
    } catch (error) { $('status').textContent = String(error); }
  });
  addEventListener('storage', () => refresh().catch(error => $('status').textContent = String(error)));
  async function heartbeat() {
    ticks++;
    $('ticks').textContent = ticks;
    $('elapsed').textContent = ((performance.now() - started) / 1000).toFixed(1);
    $('visibility').textContent = document.visibilityState;
    try {
      const response = await fetch('/api/heartbeat', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({instance, ticks, visibility: document.visibilityState, path: location.pathname})});
      if (!response.ok) throw new Error('Heartbeat failed: ' + response.status);
      const state = await response.json();
      $('ack').textContent = state.ticks;
    } catch (error) { $('ack').textContent = String(error); }
  }
  document.addEventListener('visibilitychange', () => $('visibility').textContent = document.visibilityState);
  for (let i = 1; i <= 16; i++) {
    const section = document.createElement('section');
    section.className = 'card';
    const heading = document.createElement('h2'); heading.textContent = 'Scroll section ' + i;
    const paragraph = document.createElement('p'); paragraph.textContent = 'Use the trackpad, arrow keys, Page Down, and text selection. This content stays local and does not load external assets. Section ' + i + ' of 16.';
    section.append(heading, paragraph); $('long-content').append(section);
  }
  refresh().then(() => $('status').textContent = 'Ready.').catch(error => $('status').textContent = String(error));
  heartbeat();
  setInterval(heartbeat, 1000);
</script>
</html>"""

QUIET_PAGE = """<!doctype html><html lang="en"><meta charset="utf-8">
<title>Mapbrowser QA quiet</title><style>body{font:20px/1.5 -apple-system,sans-serif;background:#eef3f8;color:#23364a;padding:48px}</style>
<h1>Quiet page</h1><p>This document has no JavaScript, timers, animation, images, or external requests.</p>
<p>Use identical copies in each browser for an idle baseline.</p><a href="/">Return to interactive test</a></html>"""


class Handler(BaseHTTPRequestHandler):
    server_version = "MapbrowserQA/1.0"

    def log_message(self, format, *args):
        if getattr(self.server, "verbose", False):
            super().log_message(format, *args)

    def cookie_value(self):
        jar = cookies.SimpleCookie()
        try:
            jar.load(self.headers.get("Cookie", ""))
        except cookies.CookieError:
            return ""
        return jar[COOKIE_NAME].value if COOKIE_NAME in jar else ""

    def send(self, status, content, content_type="application/json; charset=utf-8", cookie=None):
        data = content.encode("utf-8") if isinstance(content, str) else content
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        if cookie is not None:
            self.send_header("Set-Cookie", cookie)
        self.end_headers()
        self.wfile.write(data)

    def send_json(self, status, data, cookie=None):
        self.send(status, json.dumps(data, ensure_ascii=True), cookie=cookie)

    def do_GET(self):
        path = urlparse(self.path).path
        if path in ("/", "/second"):
            self.send(200, PAGE.replace("__TITLE__", "one" if path == "/" else "two"), "text/html; charset=utf-8")
        elif path == "/quiet":
            self.send(200, QUIET_PAGE, "text/html; charset=utf-8")
        elif path == "/api/state":
            self.send_json(200, {"cookie": self.cookie_value()})
        elif path == "/api/heartbeats":
            with LOCK:
                data = {"server_elapsed_seconds": round(time.monotonic() - STARTED, 3), "instances": list(HEARTBEATS.values()), "recent_events": list(EVENTS)}
            self.send_json(200, data)
        elif path == "/favicon.ico":
            self.send(204, b"", "image/x-icon")
        else:
            self.send_json(404, {"error": "Not found"})

    def do_POST(self):
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if not 0 < length <= 4096:
                raise ValueError("Request body must contain 1 to 4096 bytes")
            body = json.loads(self.rfile.read(length))
            if not isinstance(body, dict):
                raise ValueError("Expected a JSON object")
            path = urlparse(self.path).path
            if path == "/api/state":
                value = body.get("value")
                if not isinstance(value, str) or len(value) > 80:
                    raise ValueError("value must be a string of at most 80 characters")
                if any(ord(char) > 127 or ord(char) < 32 for char in value):
                    raise ValueError("Use printable ASCII for this cookie test")
                jar = cookies.SimpleCookie()
                jar[COOKIE_NAME] = value
                jar[COOKIE_NAME]["path"] = "/"
                jar[COOKIE_NAME]["max-age"] = 86400 * 365 if value else 0
                jar[COOKIE_NAME]["httponly"] = True
                jar[COOKIE_NAME]["samesite"] = "Lax"
                self.send_json(200, {"cookie": value}, cookie=jar.output(header="").strip())
            elif path == "/api/heartbeat":
                instance = body.get("instance")
                ticks = body.get("ticks")
                if not isinstance(instance, str) or not 1 <= len(instance) <= 80:
                    raise ValueError("instance must be a string of 1 to 80 characters")
                if not isinstance(ticks, int) or isinstance(ticks, bool) or ticks < 1:
                    raise ValueError("ticks must be a positive integer")
                event = {"instance": instance, "ticks": ticks, "visibility": str(body.get("visibility", ""))[:32], "path": str(body.get("path", ""))[:100], "cookie": self.cookie_value(), "server_elapsed_seconds": round(time.monotonic() - STARTED, 3)}
                with LOCK:
                    previous = HEARTBEATS.get(instance)
                    if previous and ticks <= previous["ticks"]:
                        self.send_json(409, {"error": "Heartbeat ticks must increase"})
                        return
                    HEARTBEATS[instance] = event
                    EVENTS.append(event)
                self.send_json(200, {"ticks": ticks})
            else:
                self.send_json(404, {"error": "Not found"})
        except (ValueError, TypeError, json.JSONDecodeError) as error:
            self.send_json(400, {"error": str(error)})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=8765)
    parser.add_argument("--verbose", action="store_true", help="Print every HTTP request")
    args = parser.parse_args()
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    server.verbose = args.verbose
    print(f"Browser QA fixture: http://127.0.0.1:{server.server_port}/", flush=True)
    print("Heartbeat evidence: /api/heartbeats | Idle baseline: /quiet", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
