export type ConversionTab = "office" | "markdown" | "pdf" | "other";

export type EngineStatus = {
  pandoc: boolean;
  anymd: boolean;
  pdf2docx: boolean;
  libreoffice: boolean;
  typst: boolean;
  ocrmypdf: boolean;
  tesseract: boolean;
  ghostscript: boolean;
};

export const pandocInputs = [
  "md",
  "markdown",
  "html",
  "htm",
  "txt",
  "tex",
  "epub",
  "docx",
  "odt",
  "rtf",
];

export const loInputs = ["docx", "doc", "odt", "rtf", "pdf"];

export const conversionTabs: {
  id: ConversionTab;
  label: string;
  description: string;
}[] = [
  {
    id: "office",
    label: "Office ↔ PDF",
    description: "Konversi dokumen Word/Office ke dan dari PDF.",
  },
  {
    id: "markdown",
    label: "Any → Markdown",
    description: "Ekstrak isi dokumen ke Markdown dengan AnyMD.",
  },
  {
    id: "pdf",
    label: "Alat PDF",
    description: "PDF → DOCX, buat PDF yang dapat dicari, atau ekstrak teks OCR.",
  },
  {
    id: "other",
    label: "Format lain",
    description: "Konversi format teks/markup yang didukung Pandoc.",
  },
];

export function formatsFor(
  fileName: string,
  engines: EngineStatus | null,
): string[] {
  const ext = fileName.split(".").pop()?.toLowerCase() || "";
  const choices = new Set<string>();
  if (engines?.anymd) choices.add("Markdown");
  if (ext === "pdf") {
    if (
      engines?.pdf2docx ||
      (engines?.anymd && engines.pandoc) ||
      engines?.libreoffice
    ) {
      choices.add("DOCX");
    }
    if (engines?.ocrmypdf && engines.tesseract && engines.ghostscript) {
      choices.add("Searchable PDF").add("OCR Text");
    }
    if (engines?.libreoffice) {
      ["DOCX", "TXT", "HTML", "ODT", "RTF"].forEach((format) =>
        choices.add(format),
      );
    }
  } else {
    if (engines?.pandoc && pandocInputs.includes(ext)) {
      ["DOCX", "Markdown", "HTML", "TXT", "ODT", "RTF"].forEach((format) =>
        choices.add(format),
      );
      if (engines.typst) choices.add("PDF");
    }
    if (engines?.libreoffice && loInputs.includes(ext)) {
      ["PDF", "DOCX", "ODT", "RTF"].forEach((format) => choices.add(format));
    }
  }
  const sameFormat: Record<string, string> = {
    md: "Markdown",
    markdown: "Markdown",
    html: "HTML",
    htm: "HTML",
    txt: "TXT",
    docx: "DOCX",
    odt: "ODT",
    rtf: "RTF",
    pdf: "PDF",
  };
  return [...choices].filter((format) => format !== sameFormat[ext]);
}

export function targetsForTab(
  fileName: string,
  engines: EngineStatus | null,
  tab: ConversionTab,
): string[] {
  const all = formatsFor(fileName, engines);
  const ext = fileName.split(".").pop()?.toLowerCase() || "";
  if (tab === "office") {
    const officeInputs = ["doc", "docx", "odt", "rtf", "pdf"];
    return officeInputs.includes(ext)
      ? all.filter(
          (target) =>
            target === "PDF" || (ext === "pdf" && target === "DOCX"),
        )
      : [];
  }
  if (tab === "markdown") return all.filter((target) => target === "Markdown");
  if (tab === "pdf") {
    // PDF → DOCX juga alat PDF, bukan hanya Office. OCR butuh tiga engine.
    return all.filter((target) =>
      ["DOCX", "Searchable PDF", "OCR Text"].includes(target),
    );
  }
  // Markdown punya tab sendiri; "Format lain" hanya menawarkan rute Pandoc.
  return all.filter((target) => target !== "Markdown");
}

export type QueueItem = { path: string; target: string; status: string };
export type QueueEntry = { tab: ConversionTab; path: string };

export function queueKey(tab: ConversionTab, path: string): string {
  return `${tab}\u0000${path.toLowerCase()}`;
}

/// Antrean dipisah per tab: file yang sama boleh diproses di tab berbeda,
/// tetapi tidak boleh dobel di tab yang sama.
export function freshPaths<T extends QueueEntry>(
  items: T[],
  paths: string[],
  tab: ConversionTab,
): { accepted: string[]; duplicates: number } {
  const seen = new Set(items.map((item) => queueKey(item.tab, item.path)));
  const accepted: string[] = [];
  let duplicates = 0;
  for (const path of paths) {
    const key = queueKey(tab, path);
    if (seen.has(key)) {
      duplicates += 1;
      continue;
    }
    seen.add(key);
    accepted.push(path);
  }
  return { accepted, duplicates };
}

/// Item tanpa target atau yang gagal preflight tidak dipanggil dan tidak
/// dihitung dalam total progress.
export function runnableQueue<T extends QueueItem>(items: T[]): T[] {
  return items.filter(
    (item) =>
      Boolean(item.path) &&
      Boolean(item.target) &&
      !item.status.startsWith("Preflight gagal") &&
      item.status !== "Tidak ada rute/engine yang didukung",
  );
}

export function extensionsForTab(tab: ConversionTab): string[] {
  if (tab === "office") return ["doc", "docx", "odt", "rtf", "pdf"];
  if (tab === "markdown") {
    return [
      "pdf",
      "doc",
      "docx",
      "odt",
      "rtf",
      "md",
      "markdown",
      "html",
      "htm",
      "txt",
      "tex",
      "epub",
      "eml",
      "msg",
      "ipynb",
      "png",
      "jpg",
      "jpeg",
      "zip",
    ];
  }
  if (tab === "pdf") return ["pdf"];
  return ["md", "markdown", "html", "htm", "txt", "tex", "epub"];
}

/// Engine yang harus ada supaya opsi OCR di tab Alat PDF muncul.
export function missingOcrEngines(engines: EngineStatus | null): string[] {
  const missing: string[] = [];
  if (!engines?.ocrmypdf) missing.push("OCRmyPDF");
  if (!engines?.tesseract) missing.push("Tesseract");
  if (!engines?.ghostscript) missing.push("Ghostscript");
  return missing;
}
