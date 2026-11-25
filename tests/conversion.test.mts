import assert from "node:assert/strict";
import { test } from "node:test";

import {
  extensionsForTab,
  freshPaths,
  missingOcrEngines,
  pdfDocxAvailable,
  readyOcrEngines,
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

test("PDF tools offers DOCX even without OCR engines", () => {
  assert.deepEqual(targetsForTab("scan.pdf", allEngines, "pdf"), [
    "DOCX",
    "Searchable PDF",
    "OCR Text",
  ]);

  const noOcr = {
    ...allEngines,
    ocrmypdf: false,
    tesseract: false,
    ghostscript: false,
  };
  assert.deepEqual(targetsForTab("scan.pdf", noOcr, "pdf"), ["DOCX"]);
  assert.deepEqual(missingOcrEngines(noOcr), [
    "OCRmyPDF",
    "Tesseract",
    "Ghostscript",
  ]);

  const writerOnly = { ...noOcr, pdf2docx: false, anymd: false, pandoc: false };
  assert.deepEqual(targetsForTab("scan.pdf", writerOnly, "pdf"), ["DOCX"]);

  const markdownPipeline = {
    ...noOcr,
    pdf2docx: false,
    libreoffice: false,
  };
  assert.deepEqual(targetsForTab("scan.pdf", markdownPipeline, "pdf"), ["DOCX"]);

  const withoutDocxEngine = {
    ...noOcr,
    pdf2docx: false,
    anymd: false,
    pandoc: false,
    libreoffice: false,
  };
  assert.deepEqual(targetsForTab("scan.pdf", withoutDocxEngine, "pdf"), []);
});

test("OCR readiness separates the missing engine from installed ones", () => {
  const partial = { ...allEngines, ocrmypdf: false };
  assert.deepEqual(missingOcrEngines(partial), ["OCRmyPDF"]);
  assert.deepEqual(readyOcrEngines(partial), ["Tesseract", "Ghostscript"]);
  assert.equal(pdfDocxAvailable(partial), true);

  assert.equal(pdfDocxAvailable(null), false);
  assert.equal(
    pdfDocxAvailable({
      ...partial,
      pdf2docx: false,
      anymd: false,
      pandoc: false,
      libreoffice: false,
    }),
    false,
  );
});

test("A format without any backend route has no target", () => {
  assert.deepEqual(targetsForTab("data.csv", allEngines, "other"), []);
});

test("A file is deduplicated per tab, not across tabs", () => {
  const existing = [{ tab: "office" as const, path: "C:\\in\\skripsi.docx" }];

  assert.deepEqual(
    freshPaths(existing, ["c:\\IN\\skripsi.docx"], "office"),
    { accepted: [], duplicates: 1 },
  );
  assert.deepEqual(
    freshPaths(existing, ["C:\\IN\\skripsi.docx"], "markdown"),
    { accepted: ["C:\\IN\\skripsi.docx"], duplicates: 0 },
  );
});

test("Queue excludes items without target or with failed preflight", () => {
  const queue = [
    { id: "ok", path: "C:\\in\\a.pdf", target: "DOCX", status: "Siap" },
    {
      id: "no-target",
      path: "C:\\in\\b.csv",
      target: "",
      status: "Tidak ada rute/engine yang didukung",
    },
    {
      id: "failed",
      path: "C:\\in\\c.pdf",
      target: "DOCX",
      status: "Preflight gagal: rusak",
    },
    {
      id: "picker",
      path: "",
      target: "PDF",
      status: "Gunakan Pilih file untuk memilih file ini",
    },
  ];
  assert.deepEqual(
    runnableQueue(queue).map((item) => item.id),
    ["ok"],
  );
});
