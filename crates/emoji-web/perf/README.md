`emoji-web` perf harness

Usage:

```bash
cd crates/emoji-web/perf
./run.sh
```

What it does:

- rebuilds the wasm bundle
- serves `crates/emoji-web/static` on a temporary local port
- opens the page in Playwright-controlled Chromium
- captures one gallery screenshot and one preview screenshot
- writes a JSON summary to `out/summary.json`

Outputs:

- `out/gallery.png`
- `out/preview.png`
- `out/summary.json`

Runtime metrics come from `window.__emojiPerf`, which is populated by the web app each frame.

Environment knobs:

- `CHROMIUM_PATH=/path/to/chrome ./run.sh`
  Use a specific browser binary instead of the nix-provided Chromium.
- `BROWSER_CDP_URL=http://127.0.0.1:9222 ./run.sh`
  Attach to an already-running Chrome/Chromium via the DevTools protocol instead of launching a new browser.
- `EMOJI_WEB_PORT=4179 ./run.sh`
  Force a specific local port instead of an ephemeral one.

If you want to attach to your own browser, start it with remote debugging enabled, for example:

```bash
google-chrome-stable --remote-debugging-port=9222
```

Automatic perf variants:

- baseline gallery
- gallery with CRT disabled
- gallery with transfer disabled
- gallery with nearest-neighbor terminal sampling
- baseline preview
- preview with billboard disabled
- preview with CRT disabled

Important:

This harness requires a browser environment with working WebGPU. In environments where automated Chromium cannot create a WebGPU adapter, the run will stop after page load with `init failed: no WebGPU adapter`.
