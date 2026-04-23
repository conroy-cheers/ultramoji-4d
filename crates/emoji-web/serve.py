#!/usr/bin/env python3
from __future__ import annotations

import argparse
import contextlib
import functools
import http.server
import os
import socketserver
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

STATIC_DIR = Path(
    os.environ.get("EMOJI_WEB_STATIC_DIR", Path(__file__).resolve().parent / "static")
).resolve()
ALLOWED_HOST_SUFFIXES = (
    'slack-edge.com',
    'slack-files.com',
)
FORWARDED_HEADERS = (
    'Content-Type',
    'Content-Length',
    'ETag',
    'Last-Modified',
    'Cache-Control',
)


def is_allowed_asset_url(raw_url: str) -> bool:
    try:
        parsed = urllib.parse.urlparse(raw_url)
    except ValueError:
        return False
    if parsed.scheme != 'https':
        return False
    hostname = (parsed.hostname or '').lower()
    if not hostname:
        return False
    return any(hostname == suffix or hostname.endswith(f'.{suffix}') for suffix in ALLOWED_HOST_SUFFIXES)


class SlackEmojiHandler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self) -> None:
        parsed = urllib.parse.urlparse(self.path)
        if parsed.path == '/emoji-asset':
            self.handle_emoji_asset(parsed)
            return
        super().do_GET()

    def do_HEAD(self) -> None:
        parsed = urllib.parse.urlparse(self.path)
        if parsed.path == '/emoji-asset':
            self.send_error(405, 'HEAD not supported for emoji relay')
            return
        super().do_HEAD()

    def handle_emoji_asset(self, parsed_path: urllib.parse.ParseResult) -> None:
        params = urllib.parse.parse_qs(parsed_path.query)
        raw_url = params.get('url', [''])[0]
        if not raw_url or not is_allowed_asset_url(raw_url):
            self.send_error(400, 'invalid Slack emoji asset URL')
            return

        request = urllib.request.Request(
            raw_url,
            headers={
              'User-Agent': 'ultramoji-4d-emoji-web/1.0',
                'Accept': 'image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8',
            },
            method='GET',
        )
        try:
            with contextlib.closing(urllib.request.urlopen(request, timeout=20.0)) as upstream:
                status = getattr(upstream, 'status', 200)
                self.send_response(status)
                for header in FORWARDED_HEADERS:
                    value = upstream.headers.get(header)
                    if value:
                        self.send_header(header, value)
                if 'Cache-Control' not in upstream.headers:
                    self.send_header('Cache-Control', 'public, max-age=86400')
                self.end_headers()
                while True:
                    chunk = upstream.read(64 * 1024)
                    if not chunk:
                        break
                    self.wfile.write(chunk)
        except urllib.error.HTTPError as err:
            message = f'upstream error {err.code}'
            self.send_error(err.code, message)
        except Exception as err:  # noqa: BLE001
            self.send_error(502, f'emoji relay failed: {err}')

    def log_message(self, format: str, *args: object) -> None:
        sys.stderr.write(f'[{self.log_date_time_string()}] {self.address_string()} {format % args}\n')


class ThreadingTCPServer(socketserver.ThreadingMixIn, socketserver.TCPServer):
    daemon_threads = True
    allow_reuse_address = True


def main() -> int:
    parser = argparse.ArgumentParser(description='Serve emoji-web with Slack emoji asset relay')
    parser.add_argument('--bind', default='127.0.0.1', help='Bind address')
    parser.add_argument('--port', type=int, default=int(os.environ.get('EMOJI_WEB_PORT', '8765')), help='Port to listen on')
    args = parser.parse_args()

    handler = functools.partial(SlackEmojiHandler, directory=str(STATIC_DIR))
    with ThreadingTCPServer((args.bind, args.port), handler) as httpd:
        print(f'Serving emoji-web on http://{args.bind}:{args.port}/ from {STATIC_DIR}')
        httpd.serve_forever()
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
