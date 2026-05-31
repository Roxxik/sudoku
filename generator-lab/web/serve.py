#!/usr/bin/env python3
"""Static file server for the mobile bench page + a /report POST endpoint.

Serves generator-lab/web/ (so the phone can load index.html + generator_lab.wasm) and
accepts the benchmark results the page POSTs back to /report: each is printed to
this terminal live and appended to generator-lab/web/results.jsonl (gitignored, and
readable from the dev box). No more hand-typing numbers off the phone.

Usage: python generator-lab/web/serve.py [port=8000]
"""
import datetime
import json
import os
import sys
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8000
WEB_DIR = os.path.dirname(os.path.abspath(__file__))
RESULTS = os.path.join(WEB_DIR, "results.jsonl")


class Handler(SimpleHTTPRequestHandler):
    def __init__(self, *a, **k):
        super().__init__(*a, directory=WEB_DIR, **k)

    def do_POST(self):
        if self.path.rstrip("/") != "/report":
            self.send_error(404)
            return
        length = int(self.headers.get("Content-Length", 0))
        try:
            data = json.loads(self.rfile.read(length))
        except Exception:
            self.send_error(400, "bad json")
            return
        data["received"] = datetime.datetime.now().isoformat(timespec="seconds")
        with open(RESULTS, "a") as f:
            f.write(json.dumps(data) + "\n")
        print_report(data)
        self.send_response(204)
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()

    def do_OPTIONS(self):
        self.send_response(204)
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Methods", "POST, OPTIONS")
        self.send_header("Access-Control-Allow-Headers", "Content-Type")
        self.end_headers()

    def log_message(self, *a):
        pass  # quiet the per-request GET noise; reports are what we want to see


def print_report(d):
    note = f" [{d['note']}]" if d.get("note") else ""
    print(f"\n=== report: {d.get('engine','?')}{note}  ({d.get('received')}) ===")
    print(f"    attempts={d.get('attempts')} seed={d.get('seed')}")
    for r in d.get("results", []):
        atts_per = r.get("atts_per")
        atts_str = f"{atts_per:>9.0f}" if isinstance(atts_per, (int, float)) else "      inf"
        print(
            f"    {r['mode']:<7}{r['us']:>8.1f} us/attempt   "
            f"puzzles {r.get('puzzles', 0):>4}   atts/puzzle {atts_str}"
        )
    print(f"    ua: {d.get('ua','')}", flush=True)


def main():
    httpd = ThreadingHTTPServer(("0.0.0.0", PORT), Handler)
    print(f"serving generator-lab/web/ on 0.0.0.0:{PORT} — reports will appear below. Ctrl-C to stop.")
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        print("\nstopped.")


if __name__ == "__main__":
    main()
