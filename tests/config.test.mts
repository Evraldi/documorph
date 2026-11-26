import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

const root = fileURLToPath(new URL("..", import.meta.url));
const read = (path: string) => readFileSync(join(root, path), "utf8");
const json = (path: string) => JSON.parse(read(path));
const lockVersions = (name: string): string[] => {
  const lock = read("src-tauri/Cargo.lock").replace(/\r\n/g, "\n");
  const pattern = new RegExp(
    `\\[\\[package\\]\\]\\nname = "${name}"\\nversion = "([^"]+)"`,
    "g",
  );
  return [...lock.matchAll(pattern)].map((match) => match[1]);
};

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

test("Toolchain and dependency versions are pinned", () => {
  const pkg = json("package.json");
  assert.equal(pkg.engines.node, ">=24.21.0");
  assert.equal(pkg.engines.npm, ">=11.19.0");
  assert.equal(pkg.devDependencies.vite, "8.3.6");
  assert.equal(pkg.devDependencies.typescript, "6.0.3");
  assert.equal(pkg.dependencies.react, "19.3.1");
  assert.equal(pkg.scripts.audit, "npm audit --audit-level=high");
  assert.match(read("src-tauri/Cargo.toml"), /rust-version\s*=\s*"1\.98\.1"/);
  assert.equal(/panic\s*=\s*"abort"/.test(read("src-tauri/Cargo.toml")), false);
});

test("Rust lockfile stays above patched versions from 2026 advisories", () => {
  const minimums: Record<string, string> = {
    cookie: "0.18.3",
    "libsqlite3-sys": "0.39.0",
    url: "2.5.9",
    zip: "9.0.0",
  };
  const atLeast = (version: string, minimum: string) => {
    const left = version.split(".").map(Number);
    const right = minimum.split(".").map(Number);
    for (let i = 0; i < right.length; i += 1) {
      if ((left[i] || 0) !== right[i]) return (left[i] || 0) > right[i];
    }
    return true;
  };
  for (const [name, minimum] of Object.entries(minimums)) {
    const versions = lockVersions(name);
    assert.ok(versions.length > 0, `${name} harus tercatat di Cargo.lock`);
    for (const version of versions) {
      assert.ok(
        atLeast(version, minimum),
        `${name} ${version} harus >= ${minimum}`,
      );
    }
  }
  const cargo = read("src-tauri/Cargo.toml");
  assert.match(cargo, /^zip = \{ version = "9"/m);
  assert.match(cargo, /^rusqlite = \{ version = "0\.41"/m);
});

test("CI runs tests, lint, audit and installer build", () => {
  const workflow = read(".github/workflows/ci.yml");
  for (const step of [
    "npm run typecheck",
    "npm test",
    "npm run audit",
    "cargo fmt --check",
    "cargo clippy --all-targets -- -D warnings",
    "cargo test",
    "cargo audit",
    "npm run tauri build",
  ]) {
    assert.ok(workflow.includes(step), `CI harus menjalankan ${step}`);
  }
});

test("README replaces the template with prerequisites and conversion matrix", () => {
  const readme = read("README.md");
  for (const hint of [
    "Prasyarat",
    "Matriks konversi",
    "Build & test",
    "Code signing",
    "unsigned",
  ]) {
    assert.ok(readme.includes(hint), `README harus menyebut ${hint}`);
  }
});

test("app metadata contains no template placeholders", () => {
  const pkg = json("package.json");
  assert.equal(pkg.name, "documorph");
  assert.equal(pkg.description, "Konversi dokumen offline untuk Windows");
  assert.equal(pkg.license, "UNLICENSED");
  assert.equal(json("src-tauri/tauri.conf.json").productName, "documorph");
  const cargo = read("src-tauri/Cargo.toml");
  assert.match(cargo, /^description = "Konversi dokumen offline untuk Windows"$/m);
  assert.match(cargo, /^authors = \["Evraldi"\]$/m);
  const html = read("index.html");
  assert.match(html, /<title>documorph<\/title>/);
  assert.match(html, /<html lang="id">/);
  assert.equal(/Tauri \+ React|vite\.svg|react\.svg/.test(html), false);
});

test("UI reads the version from Tauri instead of hardcoding it", () => {
  const app = read("src/App.tsx");
  assert.match(app, /getVersion\(\)/);
  assert.equal(/v0\.1\.0/.test(app), false);
  assert.equal(
    json("src-tauri/tauri.conf.json").version,
    json("package.json").version,
  );
});

test("vite config no longer suppresses a missing type error", () => {
  assert.equal(read("vite.config.ts").includes("@ts-expect-error"), false);
  assert.equal(json("package.json").devDependencies["@types/node"], "26.6.7");
});

test("dead template dependencies and assets are removed", () => {
  const cargo = read("src-tauri/Cargo.toml");
  for (const dependency of ["serde_json", "quick-xml", "which"]) {
    assert.equal(
      new RegExp(`^${dependency}\\s*=`, "m").test(cargo),
      false,
      `${dependency} tidak boleh dipertahankan`,
    );
  }
  for (const asset of [
    "public/tauri.svg",
    "public/vite.svg",
    "src/assets/react.svg",
    "src-tauri/include/fpdfview.h.orig",
  ]) {
    assert.equal(existsSync(join(root, asset)), false, `${asset} harus dihapus`);
  }
  assert.ok(existsSync(join(root, "public/app-icon.svg")));
});

test("UI strings are Indonesian and history deletion is confirmed", () => {
  const sources = ["src/App.tsx", "src/lib/conversion.ts"].map(read).join("\n");
  assert.equal(sources.includes("window.alert"), false);
  assert.match(read("src/App.tsx"), /confirmClear/);
  for (const english of [
    "No supported route / engine",
    "Clear history",
    "Preflight failed",
    "Ready",
  ]) {
    assert.equal(
      sources.includes(english),
      false,
      `Masih ada UI Inggris: ${english}`,
    );
  }
  assert.match(sources, /Tidak ada rute\/engine yang didukung/);
  assert.match(sources, /Preflight gagal/);
});

test("history schema and query persist preflight warnings", () => {
  const db = read("src-tauri/src/db.rs");
  const lib = read("src-tauri/src/lib.rs");
  assert.match(db, /ADD COLUMN warnings/);
  assert.match(lib, /warnings: db::decode_warnings/);
  assert.match(lib, /SELECT name, output, engine, tier, at, warnings/);
  assert.match(lib, /record_history\(&app, source, &report, &warnings\)/);
});

test("engine stderr is redacted and Explorer opens are guarded", () => {
  const process = read("src-tauri/src/process.rs");
  assert.match(process, /fn redact_paths/);
  assert.match(process, /redact_paths\(&raw\)/);
  const config = json("src-tauri/tauri.conf.json");
  assert.ok(
    config.bundle.fileAssociations.some((entry: { ext: string[] }) =>
      entry.ext.includes("pdf"),
    ),
  );
  const lib = read("src-tauri/src/lib.rs");
  assert.match(lib, /fn openable_path/);
  assert.match(lib, /fn take_open_files/);
  assert.match(lib, /take_open_files,/);
  assert.match(read("src/App.tsx"), /take_open_files/);
});

test("adding files is allowed while a conversion is running", () => {
  const app = read("src/App.tsx");
  assert.equal(
    app.includes("if (busyRef.current) return;"),
    false,
    "addPaths tidak boleh diam-diam menolak file saat konversi berjalan",
  );
  assert.match(app, /freshPaths\(itemsRef\.current, acceptedPaths, tab\)/);
});

test("history deletion uses a real confirmation dialog", () => {
  const app = read("src/App.tsx");
  assert.equal(app.includes("Yakin hapus?"), false);
  assert.match(app, /role="alertdialog"/);
  assert.match(app, /Ya, hapus riwayat/);
  assert.match(app, /confirm-clear-desc/);
});

test("completed conversions are visible without reading a status line", () => {
  const app = read("src/App.tsx");
  const css = read("src/App.css");
  assert.match(app, /success-panel/);
  assert.match(app, /file berhasil dikonversi/);
  assert.match(app, /success-badge/);
  assert.match(app, /Buka hasil terakhir/);
  assert.equal(app.includes('className="result-actions"'), false);
  assert.match(css, /\.success-badge\{/);
  assert.match(css, /\.file-row-done\{/);
});

test("engine status is centered and reports partial OCR readiness", () => {
  const app = read("src/App.tsx");
  const css = read("src/App.css");
  assert.equal(css.includes("position:absolute;top:10px;right:14px"), false);
  assert.match(css, /\.engine-status\{display:flex/);
  assert.match(app, /ocrMissing\.join\(" \+ "\)/);
  assert.match(app, /OCRMYPDF_PATH/);
  assert.match(app, /PYTHON_SCRIPTS_PATH/);
});

test("Alat PDF exposes DOCX and explains missing OCR engines", () => {
  const conversion = read("src/lib/conversion.ts");
  const app = read("src/App.tsx");
  const readme = read("README.md");
  assert.match(conversion, /\["DOCX", "Searchable PDF", "OCR Text"\]/);
  assert.match(conversion, /function missingOcrEngines/);
  assert.match(app, /OCR belum siap:/);
  assert.match(app, /Alat PDF butuh/);
  assert.match(readme, /\| Alat PDF \| PDF \| DOCX \|/);
});

test("PDF to Writer documents uses real engine routes", () => {
  const lib = read("src-tauri/src/lib.rs");
  const readme = read("README.md");
  assert.match(lib, /fn pdf2docx_convert_args/);
  assert.equal(lib.includes('arg("-d").arg(&output)'), false);
  assert.match(lib, /fn pdf_writer_route/);
  assert.match(lib, /--infilter=HTML \(StarWriter\)/);
  assert.match(readme, /LibreOffice Draw → Writer/);
});

test("searchable PDF rewrites an invisible word layer after correction", () => {
  const lib = read("src-tauri/src/lib.rs");
  const layer = read("src-tauri/src/ocr_layer.rs");
  const postprocess = read("src-tauri/src/ocr_postprocess.rs");
  const readme = read("README.md");
  assert.match(lib, /target_key == "searchable pdf"/);
  const skipAt = lib.indexOf('command.arg("--skip-text")');
  assert.ok(skipAt > 0, "halaman searchable harus memakai --skip-text");
  const branch = lib.slice(skipAt, lib.indexOf("} else {", skipAt));
  assert.equal(branch.includes("--deskew"), false);
  assert.equal(branch.includes("--rotate-pages"), false);
  assert.match(lib, /rewrite_searchable_layer\(/);
  assert.match(lib, /Halaman \{\}: \{\} → \{\} \(\{\}\)/);
  assert.match(layer, /PdfPageTextRenderMode::Invisible/);
  assert.match(layer, /word_separator/);
  assert.match(layer, /pdftotext/);
  assert.match(layer, /pymupdf/);
  assert.match(layer, /DOCUMORPH_OCR_RAW_BGR/);
  assert.match(layer, /sample_image_only_pdf_gets_correct_invisible_text_layer/);
  assert.match(postprocess, /DOCUMORPH_OCR_POSTPROCESS/);
  for (const flag of [
    "DOCUMORPH_OCR_ROTATION",
    "DOCUMORPH_OCR_DICTIONARY",
    "DOCUMORPH_OCR_FONT_RERUN",
    "DOCUMORPH_OCR_FONT_RERUN_SKIP_SAFE",
    "DOCUMORPH_OCR_PARALLEL_PAGES",
    "DOCUMORPH_OCR_PARALLEL_FONT_RERUN",
    "DOCUMORPH_OCR_LAYOUT",
    "DOCUMORPH_OCR_BULLET",
    "DOCUMORPH_OCR_HYPHEN",
    "DOCUMORPH_OCR_FRACTION",
    "DOCUMORPH_OCR_RENDER_DPI",
    "DOCUMORPH_OCR_RERENDER_DPI",
    "DOCUMORPH_OCR_DICTIONARY_EXTRA",
  ]) {
    assert.ok(postprocess.includes(flag), `${flag} harus ada di backend`);
    assert.ok(readme.includes(flag), `${flag} harus didokumentasikan`);
  }
});

test("OCR corrections keep body text accuracy guarded by ground truth", () => {
  const postprocess = read("src-tauri/src/ocr_postprocess.rs");
  const layer = read("src-tauri/src/ocr_layer.rs");
  const metrics = read("src-tauri/src/ocr_metrics.rs");
  const readme = read("README.md");
  const fullTruth = read("tests/fixtures/linn-sequencer.ground-truth.txt");
  const bodyTruth = read("tests/fixtures/linn-sequencer.body-ground-truth.txt");

  assert.ok(fullTruth.includes("The LinnSequencer"));
  assert.ok(fullTruth.includes("LINN UR"));
  assert.ok(bodyTruth.includes("professional musician"));
  assert.equal(fullTruth.includes("Sequence Recorder"), true);

  assert.match(postprocess, /dictionary-explicit/);
  assert.match(postprocess, /bullet-insert/);
  assert.match(layer, /text_font\(/);

  for (const token of [
    "or",
    "is",
    "why",
    "designed",
    "existing",
    "gone",
    "channels",
    "last",
    "them",
  ]) {
    assert.ok(postprocess.includes(`"${token}"`), `${token} harus dilindungi kamus`);
  }
  assert.match(postprocess, /MIN_CANDIDATE_FREQUENCY: usize = 3/);
  assert.match(postprocess, /MAX_CORRECTABLE_CONFIDENCE: f32 = 90\.0/);
  assert.match(postprocess, /bounded_edit_distance\(&key, candidate, 1\) != Some\(1\)/);
  assert.match(postprocess, /same_inflection_shape/);
  assert.match(postprocess, /word\.text\.trim\(\) != "32"/);
  assert.match(postprocess, /in_disk_drive_context/);
  assert.match(postprocess, /fn preserve_hyphens/);
  assert.equal(postprocess.includes("hyphen-join-search"), false);
  assert.match(postprocess, /fn geometric_lines/);
  assert.match(postprocess, /groups\.entry\(line_key\(word\)\)/);
  assert.match(postprocess, /if gaps\.len\(\) < 2/);
  assert.match(postprocess, /fn split_bands/);
  assert.match(postprocess, /fn split_columns/);
  assert.match(postprocess, /parent_bullet/);

  assert.match(layer, /ground_truth_metrics_track_preprocessing_and_each_stage/);
  assert.match(layer, /fn binarized/);
  assert.match(layer, /fn denoised/);
  assert.match(layer, /fn upscaled_nearest/);
  assert.match(layer, /crop_upscaled/);
  assert.match(metrics, /pub fn cer/);
  assert.match(metrics, /pub fn wer/);
  assert.match(readme, /ground_truth_metrics/);
  assert.match(readme, /CER/);
  assert.match(readme, /body text/);
});

test("engine probing and conversion run off the UI thread", () => {
  const lib = read("src-tauri/src/lib.rs");
  for (const command of [
    "engine_status",
    "refresh_dependencies",
    "preflight_file",
    "convert_file",
  ]) {
    assert.match(
      lib,
      new RegExp(`#\\[tauri::command\\(async\\)\\]\\s*fn ${command}\\b`),
      `${command} tidak boleh menjadi command blocking`,
    );
  }
});
