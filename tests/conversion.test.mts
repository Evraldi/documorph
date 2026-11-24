import assert from "node:assert/strict";
import { test } from "node:test";

import {
  extensionsForTab,
  runnableQueue,
  targetsForTab,
} from "../src/lib/conversion.ts";

const allEngines = {
  pandoc: true,
  anymd: true,
  pdf2docx: true,
  libreoffice: true,
  typst: true,
  ocrmypdf: true,
  tesseract: true,
  ghostscript: true,
};

test("Other formats only advertises routes the backend really has", () => {
  const extensions = extensionsForTab("other");
  assert.ok(extensions.includes("md"));
  assert.ok(extensions.includes("html"));
  assert.ok(extensions.includes("txt"));
  for (const unsupported of [
    "csv",
    "tsv",
    "json",
    "yaml",
    "yml",
    "toml",
    "xml",
    "xlsx",
    "xls",
    "pptx",
    "ods",
    "odp",
  ]) {
    assert.equal(
      extensions.includes(unsupported),
      false,
      `.${unsupported} harusnya tidak ditawarkan`,
    );
  }
});

test("Other formats offers real Pandoc targets, not an empty select", () => {
  const targets = targetsForTab("notes.md", allEngines, "other");
  assert.ok(targets.length > 0);
  assert.equal(targets.includes("Markdown"), false);
  assert.ok(targets.includes("DOCX"));
  assert.ok(targets.includes("TXT"));
});

test("A format without any backend route has no target", () => {
  assert.deepEqual(targetsForTab("data.csv", allEngines, "other"), []);
});

test("Queue excludes items without target or with failed preflight", () => {
  const queue = [
    { id: "ok", path: "C:\\in\\a.pdf", target: "DOCX", status: "Ready" },
    { id: "no-target", path: "C:\\in\\b.csv", target: "", status: "No supported route / engine" },
    { id: "failed", path: "C:\\in\\c.pdf", target: "DOCX", status: "Preflight failed: broken" },
    { id: "picker", path: "", target: "PDF", status: "Run in the Tauri desktop app" },
  ];
  assert.deepEqual(
    runnableQueue(queue).map((item) => item.id),
    ["ok"],
  );
});
