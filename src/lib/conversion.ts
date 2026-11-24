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
    description: "Convert Word and Office documents to and from PDF.",
  },
  {
    id: "markdown",
    label: "Any → Markdown",
    description: "Extract document content into Markdown with AnyMD.",
  },
  {
    id: "pdf",
    label: "PDF tools",
    description: "Create searchable PDFs or extract OCR text.",
  },
  {
    id: "other",
    label: "Other formats",
    description: "Convert Pandoc-supported text and markup formats.",
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
    if (engines?.pdf2docx) choices.add("DOCX");
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
    return all.filter((target) =>
      ["Searchable PDF", "OCR Text"].includes(target),
    );
  }
  // Markdown punya tab sendiri; "Other formats" hanya menawarkan rute Pandoc.
  return all.filter((target) => target !== "Markdown");
}

export type QueueItem = { path: string; target: string; status: string };

/// Item tanpa target atau yang gagal preflight tidak dipanggil dan tidak
/// dihitung dalam total progress.
export function runnableQueue<T extends QueueItem>(items: T[]): T[] {
  return items.filter(
    (item) =>
      Boolean(item.path) &&
      Boolean(item.target) &&
      !item.status.startsWith("Preflight failed") &&
      item.status !== "No supported route / engine",
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
