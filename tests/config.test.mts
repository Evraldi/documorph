import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

const root = fileURLToPath(new URL("..", import.meta.url));
const read = (path: string) => readFileSync(join(root, path), "utf8");
const json = (path: string) => JSON.parse(read(path));

test("production CSP and dev CSP are both configured", () => {
  const config = json("src-tauri/tauri.conf.json");
  assert.ok(config.app.security.csp, "CSP produksi tidak boleh null");
  assert.ok(config.app.security.devCsp, "devCsp tidak boleh null");
  assert.match(config.app.security.csp["script-src"], /'self'/);
  assert.equal(config.app.security.csp["object-src"], "'none'");
  assert.match(config.app.security.devCsp["connect-src"], /ws:\/\/localhost:1420/);
});

test("bundle is Windows-only and ships PDFium licenses", () => {
  const config = json("src-tauri/tauri.conf.json");
  assert.deepEqual(config.bundle.targets, ["nsis", "msi"]);
  assert.equal(config.bundle.resources["bin/pdfium.dll"], "bin/pdfium.dll");
  assert.equal(config.bundle.resources["licenses/*"], "licenses/");
  assert.ok(config.bundle.windows.wix.upgradeCode);
  assert.equal(config.bundle.windows.nsis.installMode, "currentUser");
  assert.equal(config.bundle.windows.webviewInstallMode.type, "downloadBootstrapper");
});

test("updater and code signing stay disabled without keys or endpoint", () => {
  const config = json("src-tauri/tauri.conf.json");
  assert.equal("updater" in config.app, false);
  assert.equal("certificateThumbprint" in config.bundle.windows, false);
  assert.equal("signCommand" in config.bundle.windows, false);
});

test("frontend has no opener capability", () => {
  const capability = json("src-tauri/capabilities/default.json");
  assert.deepEqual(capability.permissions, [
    "core:default",
    "dialog:allow-open",
  ]);
  assert.equal(
    capability.permissions.some((permission: string) =>
      permission.startsWith("opener:"),
    ),
    false,
  );
});

test("engine discovery no longer searches PATH", () => {
  const active = read("src-tauri/Cargo.toml")
    .split("\n")
    .filter((line) => !line.trimStart().startsWith("#"))
    .join("\n");
  assert.equal(/\bwhich\s*=/.test(active), false);
  assert.match(active, /sha2\s*=/);
});

test("frontend test and typecheck scripts exist", () => {
  const scripts = json("package.json").scripts;
  assert.equal(scripts.test, 'node --test "tests/*.test.mts"');
  assert.equal(scripts.typecheck, "tsc --noEmit");
});

test("README documents explicit engine locations after PATH was removed", () => {
  const readme = read("README.md");
  for (const hint of [
    "WinGet",
    "npm global",
    "PYTHON_SCRIPTS_PATH",
    "--version",
    "--help",
  ]) {
    assert.ok(readme.includes(hint), `README harus menyebut ${hint}`);
  }
});
