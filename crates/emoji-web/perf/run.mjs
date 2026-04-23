import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";

import { chromium } from "playwright";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "../../..");
const webRoot = path.resolve(repoRoot, "crates/emoji-web");
const staticRoot = path.resolve(webRoot, "static");
const outRoot = path.resolve(__dirname, "out");
const requestedPort = process.env.EMOJI_WEB_PORT
  ? Number(process.env.EMOJI_WEB_PORT)
  : 0;

async function main() {
  await mkdir(outRoot, { recursive: true });

  await runLogged(
    "bash",
    [
      "-lc",
      "nix shell nixpkgs#wasm-pack nixpkgs#binaryen nixpkgs#lld -c bash -lc 'cd crates/emoji-web && wasm-pack build --target web --out-dir static/pkg'",
    ],
    repoRoot,
  );

  const server = await startStaticServer(staticRoot, requestedPort);
  const address = server.address();
  const port =
    typeof address === "object" && address ? Number(address.port) : requestedPort;
const url = `http://localhost:${port}/index.html`;

  const cleanup = async () => {
    await new Promise((resolve, reject) => {
      server.close((err) => {
        if (err) {
          reject(err);
        } else {
          resolve();
        }
      });
    });
  };
  process.on("exit", cleanup);
  process.on("SIGINT", async () => {
    await cleanup();
    process.exit(130);
  });
  process.on("SIGTERM", async () => {
    await cleanup();
    process.exit(143);
  });

  try {
    await waitForHttp(url, 10_000);
    const browser = await openBrowser();
    try {
      const page = await browser.newPage({
        viewport: { width: 1280, height: 960 },
        deviceScaleFactor: 1,
      });
      page.on("console", (msg) => {
        if (msg.type() === "error") {
          console.error(`[browser] ${msg.text()}`);
        }
      });

      await page.goto(url, { waitUntil: "networkidle" });
      await page.waitForSelector("#emoji-canvas");
      const zoomInfo = await verifyPageZoom(page, 0.5);
      await waitForPerf(page, false);
      await delay(2_000);

      const gallery = await captureScenario(page, "gallery", false, {}, zoomInfo);
      const galleryNoCrt = await captureScenario(page, "gallery_no_crt", false, {
        crt: false,
      }, zoomInfo);
      const galleryNoTransfer = await captureScenario(page, "gallery_no_transfer", false, {
        transfer: false,
      }, zoomInfo);
      const galleryNearest = await captureScenario(page, "gallery_nearest_overlay", false, {
        overlayFilter: false,
      }, zoomInfo);

      await page.keyboard.press("Enter");
      await waitForPerf(page, true);
      await delay(2_000);

      const preview = await captureScenario(page, "preview", true, {}, zoomInfo);
      const previewNoBillboard = await captureScenario(page, "preview_no_billboard", true, {
        billboard: false,
      }, zoomInfo);
      const previewNoCrt = await captureScenario(page, "preview_no_crt", true, {
        crt: false,
      }, zoomInfo);

      const summary = {
        url,
        capturedAt: new Date().toISOString(),
        gallery,
        galleryNoCrt,
        galleryNoTransfer,
        galleryNearest,
        preview,
        previewNoBillboard,
        previewNoCrt,
      };

      const summaryPath = path.join(outRoot, "summary.json");
      await writeFile(summaryPath, JSON.stringify(summary, null, 2));
      console.log(JSON.stringify(summary, null, 2));
      console.log(`wrote ${summaryPath}`);
    } finally {
      await browser.close();
    }
  } finally {
    await cleanup();
  }
}

async function captureScenario(page, name, expectPreview, toggles, zoomInfo) {
  await setPerfToggles(page, {
    crt: true,
    transfer: true,
    overlayFilter: true,
    billboard: true,
  });
  await setPerfToggles(page, toggles);
  await delay(1_500);
  await waitForPerf(page, expectPreview);
  const metrics = await page.evaluate(() => window.__emojiPerf ?? null);
  if (!metrics) {
    throw new Error(`missing __emojiPerf for ${name}`);
  }

  const screenshotPath = path.join(outRoot, `${name}.png`);
  await page.screenshot({ path: screenshotPath });

  return {
    ...metrics,
    ...zoomInfo,
    expectPreview,
    screenshot: screenshotPath,
  };
}

async function waitForPerf(page, previewing) {
  await page.waitForFunction(
    (expected) => {
      const perf = window.__emojiPerf;
      return Boolean(perf && perf.frame > 10 && perf.previewing === expected);
    },
    previewing,
    { timeout: 10_000 },
  );
}

async function setPerfToggles(page, partial) {
  await page.evaluate((next) => {
    window.__emojiPerfControls?.setToggles(next);
  }, partial);
}

async function verifyPageZoom(page, zoomFactor) {
  const effectiveZoom = await page.evaluate(() => ({
    devicePixelRatio: window.devicePixelRatio,
    innerWidth: window.innerWidth,
    innerHeight: window.innerHeight,
  }));
  const expectedWidth = 1280 / zoomFactor;
  const expectedHeight = 960 / zoomFactor;
  const widthOk = Math.abs(effectiveZoom.innerWidth - expectedWidth) <= 64;
  const heightOk = Math.abs(effectiveZoom.innerHeight - expectedHeight) <= 64;
  if (!widthOk || !heightOk) {
    throw new Error(
      `browser zoom verification failed: requested ${zoomFactor}, observed=${JSON.stringify(effectiveZoom)}`,
    );
  }
  return {
    requestedZoom: zoomFactor,
    ...effectiveZoom,
  };
}

async function waitForHttp(targetUrl, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(targetUrl, { method: "GET" });
      if (response.ok) {
        return;
      }
    } catch (_) {
    }
    await delay(200);
  }
  throw new Error(`timed out waiting for ${targetUrl}`);
}

async function runLogged(cmd, args, cwd) {
  await new Promise((resolve, reject) => {
    const child = spawn(cmd, args, {
      cwd,
      stdio: "inherit",
    });
    child.on("exit", (code) => {
      if (code === 0) {
        resolve();
      } else {
        reject(new Error(`${cmd} exited with code ${code}`));
      }
    });
    child.on("error", reject);
  });
}

async function openBrowser() {
  if (process.env.BROWSER_CDP_URL) {
    return chromium.connectOverCDP(process.env.BROWSER_CDP_URL);
  }

  return chromium.launch({
    headless: process.env.PLAYWRIGHT_HEADLESS !== "0",
    executablePath: process.env.CHROMIUM_PATH || undefined,
    args: [
      "--enable-unsafe-webgpu",
      "--ignore-gpu-blocklist",
      "--disable-gpu-sandbox",
      "--enable-features=Vulkan,UseSkiaRenderer",
      "--use-angle=vulkan",
      "--use-vulkan=swiftshader",
      "--disable-vulkan-surface",
    ],
  });
}

async function startStaticServer(root, portNumber) {
  const server = createServer(async (req, res) => {
    try {
      const reqPath = new URL(req.url ?? "/", "http://127.0.0.1")
        .pathname;
      const relativePath = reqPath === "/" ? "/index.html" : reqPath;
      const filePath = path.join(root, relativePath);
      const body = await readFile(filePath);
      res.statusCode = 200;
      res.setHeader("Cache-Control", "no-cache");
      res.setHeader("Content-Type", mimeType(filePath));
      res.end(body);
    } catch (_) {
      res.statusCode = 404;
      res.end("not found");
    }
  });

  await new Promise((resolve, reject) => {
    server.on("error", reject);
    server.listen(portNumber, "127.0.0.1", resolve);
  });

  return server;
}

function mimeType(filePath) {
  if (filePath.endsWith(".html")) return "text/html; charset=utf-8";
  if (filePath.endsWith(".js")) return "text/javascript; charset=utf-8";
  if (filePath.endsWith(".wasm")) return "application/wasm";
  if (filePath.endsWith(".css")) return "text/css; charset=utf-8";
  if (filePath.endsWith(".json")) return "application/json; charset=utf-8";
  if (filePath.endsWith(".png")) return "image/png";
  return "application/octet-stream";
}

await main();
