# ultramoji-4d

3D emoji rendering crates and preview tools extracted from `slackslack`.

## Crates

- `emoji-renderer`: shared CPU/GPU emoji billboard renderer.
- `emoji-web`: hosted WebGPU/WebGL emoji gallery and Slack emoji viewer.

## Tools

- `emoji_preview_viewer`
- `emoji_billboard_native`
- `contour_viewer`

## Build

```sh
cargo check
```

Build the web package:

```sh
cd crates/emoji-web
wasm-pack build --target web --out-dir static/pkg
```
