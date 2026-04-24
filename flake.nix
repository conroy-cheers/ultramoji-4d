{
  description = "ultramoji-4d - 3D emoji rendering tools";

  inputs.nixpkgs.url = "https://flakehub.com/f/NixOS/nixpkgs/0";

  outputs =
    { self, ... }@inputs:
    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];

      forEachSupportedSystem =
        f:
        inputs.nixpkgs.lib.genAttrs supportedSystems (
          system:
          f {
            inherit system;
            pkgs = import inputs.nixpkgs {
              inherit system;
              config.allowUnfree = true;
            };
          }
        );
    in
    {
      packages = forEachSupportedSystem (
        { pkgs, system }:
        let
          wasmBindgenCliCompat = pkgs.rustPlatform.buildRustPackage {
            pname = "wasm-bindgen-cli";
            version = "0.2.118";
            src = pkgs.fetchurl {
              name = "wasm-bindgen-cli-0.2.118.tar.gz";
              url = "https://crates.io/api/v1/crates/wasm-bindgen-cli/0.2.118/download";
              hash = "sha256-T+W26BbjTikzh5794nNnOz7h7HwKQ+39kOKP1X2THsQ=";
            };
            cargoHash = "sha256-EYDfuBlH3zmTxACBL+sjicRna84CvoesKSQVcYiG9P0=";
            doCheck = false;
          };

          emojiBillboardServer = pkgs.rustPlatform.buildRustPackage {
            pname = "ultramoji-server";
            version = "0.1.0";
            src = ./.;
            cargoLock = {
              lockFile = ./Cargo.lock;
            };
            cargoRoot = "crates/emoji-web";
            postPatch = ''
              ln -sf ../../Cargo.lock crates/emoji-web/Cargo.lock
            '';
            nativeBuildInputs = with pkgs; [
              makeWrapper
              wasmBindgenCliCompat
              wasm-pack
              binaryen
              brotli
              gzip
              lld
            ];
            doCheck = false;
            buildPhase = ''
              runHook preBuild
              export HOME="$TMPDIR/home"
              mkdir -p "$HOME"
              export CARGO_TARGET_DIR="$TMPDIR/target"
              cd crates/emoji-web
              cargo build --release --bin ultramoji-server
              wasm-pack build --mode no-install --target web --out-dir "$TMPDIR/pkg"
              runHook postBuild
            '';
            installPhase = ''
              runHook preInstall
              mkdir -p "$out/share/ultramoji/static"
              cp -R ${./crates/emoji-web/static}/. "$out/share/ultramoji/static"
              chmod -R u+w "$out/share/ultramoji/static"
              rm -rf "$out/share/ultramoji/static/pkg"
              cp -R "$TMPDIR/pkg" "$out/share/ultramoji/static/pkg"
              static_dir="$out/share/ultramoji/static"
              slack_hosted_hash="$(sha256sum "$static_dir/slack_hosted.js" | cut -c1-16)"
              emoji_web_js_hash="$(sha256sum "$static_dir/pkg/emoji_web.js" | cut -c1-16)"
              emoji_web_wasm_hash="$(sha256sum "$static_dir/pkg/emoji_web_bg.wasm" | cut -c1-16)"
              printf '{"pkg/emoji_web.js":"%s","pkg/emoji_web_bg.wasm":"%s","slack_hosted.js":"%s"}\n' \
                "$emoji_web_js_hash" "$emoji_web_wasm_hash" "$slack_hosted_hash" \
                > "$static_dir/asset-manifest.json"
              find "$out/share/ultramoji/static" -type f \( -name '*.html' -o -name '*.js' -o -name '*.json' -o -name '*.wasm' \) -print0 \
                | while IFS= read -r -d "" file; do
                    brotli -f -q 11 "$file"
                    gzip -f -k -9 "$file"
                  done

              mkdir -p "$out/bin"
              install -Dm755 "$CARGO_TARGET_DIR/release/ultramoji-server" "$out/bin/.ultramoji-server-unwrapped"
              makeWrapper "$out/bin/.ultramoji-server-unwrapped" "$out/bin/ultramoji-server" \
                --set EMOJI_WEB_STATIC_DIR "$out/share/ultramoji/static"
              runHook postInstall
            '';
          };
        in
        {
          ultramoji-server = emojiBillboardServer;
          default = emojiBillboardServer;
        }
      );

      checks = forEachSupportedSystem (
        { pkgs, system }:
        let
          package = self.packages.${system}.ultramoji-server;
        in
        {
          package-smoke = pkgs.runCommand "ultramoji-server-smoke" { } ''
            set -euo pipefail
            static="${package}/share/ultramoji/static"
            test -x "${package}/bin/ultramoji-server"
            for file in \
              "$static/index.html" \
              "$static/asset-manifest.json" \
              "$static/slack_hosted.js" \
              "$static/pkg/emoji_web.js" \
              "$static/pkg/emoji_web_bg.wasm" \
              "$static/pkg/emoji_web_bg.wasm.br" \
              "$static/pkg/emoji_web_bg.wasm.gz"; do
              test -s "$file"
            done
            grep -q '"pkg/emoji_web.js"' "$static/asset-manifest.json"
            grep -q '"pkg/emoji_web_bg.wasm"' "$static/asset-manifest.json"
            grep -q '"slack_hosted.js"' "$static/asset-manifest.json"
            wasm_size="$(wc -c < "$static/pkg/emoji_web_bg.wasm" | tr -d ' ')"
            js_size="$(wc -c < "$static/pkg/emoji_web.js" | tr -d ' ')"
            test "$wasm_size" -le 8000000
            test "$js_size" -le 250000
            "${package}/bin/ultramoji-server" --help >/dev/null
            touch "$out"
          '';
        }
      );

      apps = forEachSupportedSystem (
        { system, ... }:
        {
          ultramoji-server = {
            type = "app";
            program = "${self.packages.${system}.ultramoji-server}/bin/ultramoji-server";
          };
          default = self.apps.${system}.ultramoji-server;
        }
      );

      devShells = forEachSupportedSystem (
        { pkgs, system }:
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              self.formatter.${system}
              rustc
              cargo
              rust-analyzer
              clippy
              rustfmt
              pkg-config
              mold
            ];
          };
        }
      );

      formatter = forEachSupportedSystem ({ pkgs, ... }: pkgs.nixfmt);
    };
}
