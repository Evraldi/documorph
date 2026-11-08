import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import "./App.css";

type FileItem = { file: { name: string; size: number }; path: string; target: string; status: string; warnings?: string[] };
type PreflightReport = { size_bytes: number; warnings: string[] };
type EngineStatus = { pandoc: boolean; anymd: boolean; libreoffice: boolean; typst: boolean; ocrmypdf: boolean; tesseract: boolean; ghostscript: boolean };
type ConversionReport = { output: string; engine: string; tier: string; duration_ms: number; warning: string | null };
type HistoryItem = { name: string; output: string; engine: string; tier: string; at: number };
const pandocInputs = ["md", "markdown", "html", "htm", "txt", "tex", "epub", "docx", "odt", "rtf"];
const loInputs = ["docx", "doc", "odt", "rtf", "pdf"];
function formatsFor(fileName: string, engines: EngineStatus | null) {
  const ext = fileName.split(".").pop()?.toLowerCase() || "";
  const choices = new Set<string>();
  if (engines?.anymd) choices.add("Markdown");
  if (ext === "pdf") {
    if (engines?.ocrmypdf && engines.tesseract && engines.ghostscript) choices.add("Searchable PDF").add("OCR Text");
    if (engines?.libreoffice) ["DOCX", "TXT", "HTML", "ODT", "RTF"].forEach((format) => choices.add(format));
  } else {
    if (engines?.pandoc && pandocInputs.includes(ext)) {
      ["DOCX", "Markdown", "HTML", "TXT", "ODT", "RTF"].forEach((format) => choices.add(format));
      if (engines.typst) choices.add("PDF");
    }
    if (engines?.libreoffice && loInputs.includes(ext)) ["PDF", "DOCX", "ODT", "RTF"].forEach((format) => choices.add(format));
  }
  const sameFormat: Record<string, string> = { md: "Markdown", markdown: "Markdown", html: "HTML", htm: "HTML", txt: "TXT", docx: "DOCX", odt: "ODT", rtf: "RTF", pdf: "PDF" };
  return [...choices].filter((format) => format !== sameFormat[ext]);
}

function App() {
  const [view, setView] = useState<"convert" | "history">("convert");
  const [history, setHistory] = useState<HistoryItem[]>([]);
  const [items, setItems] = useState<FileItem[]>([]);
  const [dragging, setDragging] = useState(false);
  const [engines, setEngines] = useState<EngineStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState({ current: 0, total: 0, name: "" });
  const stopRequested = useRef(false);
  useEffect(() => { void invoke<EngineStatus>("engine_status").then(setEngines).catch(() => setEngines({ pandoc: false, anymd: false, libreoffice: false, typst: false, ocrmypdf: false, tesseract: false, ghostscript: false })); }, []);
  const addPaths = (paths: string[]) => {
    setItems((old) => {
      const existing = new Set(old.map((item) => item.path.toLowerCase()));
      const additions = paths.filter((path) => !existing.has(path.toLowerCase())).map((path) => {
        const name = path.split(/[\\/]/).pop() || path;
        const targets = formatsFor(name, engines);
        return { file: { name, size: 0 }, path, target: targets.includes("Markdown") ? "Markdown" : targets.includes("PDF") ? "PDF" : targets[0] || "", status: targets.length ? "Checking file…" : "No supported route / engine", warnings: [] as string[] };
      });
      return additions.length ? [...old, ...additions] : old;
    });
    for (const path of paths) {
      void invoke<PreflightReport>("preflight_file", { input: path }).then((report) => {
        setItems((old) => old.map((item) => item.path.toLowerCase() === path.toLowerCase() ? { ...item, file: { ...item.file, size: report.size_bytes }, status: item.target ? "Ready" : item.status, warnings: report.warnings } : item));
      }).catch((error) => {
        setItems((old) => old.map((item) => item.path.toLowerCase() === path.toLowerCase() ? { ...item, status: `Preflight failed: ${String(error)}` } : item));
      });
    }
  };
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void getCurrentWebview().onDragDropEvent((event) => {
      if (event.payload.type === "over") setDragging(true);
      else {
        setDragging(false);
        if (event.payload.type === "drop") addPaths(event.payload.paths);
      }
    }).then((dispose) => { if (cancelled) dispose(); else unlisten = dispose; });
    return () => { cancelled = true; unlisten?.(); };
  }, []);
  useEffect(() => { void invoke<HistoryItem[]>("history_list").then(setHistory).catch((error) => console.error("Failed to load conversion history:", error)); }, []);
  const browseFiles = async () => {
    try {
      const selected = await open({ multiple: true, title: "Choose documents", filters: [{ name: "Documents", extensions: ["docx", "doc", "odt", "ods", "odp", "xlsx", "xls", "pptx", "rtf", "pdf", "md", "markdown", "html", "htm", "xhtml", "txt", "tex", "epub", "csv", "tsv", "json", "yaml", "yml", "toml", "xml", "eml", "msg", "ipynb", "png", "jpg", "jpeg", "zip"] }] });
      if (!selected) return;
      const paths = Array.isArray(selected) ? selected : [selected];
      addPaths(paths);
    } catch { setItems((old) => [...old, { file: { name: "File picker unavailable", size: 0 }, path: "", target: "PDF", status: "Run in the Tauri desktop app" }]); }
  };
  const update = (index: number, patch: Partial<FileItem>) => setItems((old) => old.map((item, i) => i === index ? { ...item, ...patch } : item));
  const convert = async () => {
    stopRequested.current = false;
    setBusy(true);
    const work = items.filter((item) => item.path && !item.status.startsWith("Preflight failed"));
    setProgress({ current: 0, total: work.length, name: "" });
    try {
      for (let i = 0; i < items.length; i++) {
        if (stopRequested.current) break;
        const item = items[i];
        if (!item.path) { update(i, { status: "Use Browse files to select this file" }); continue; }
        if (item.status.startsWith("Preflight failed")) continue;
        setProgress({ current: work.findIndex((queued) => queued.path === item.path) + 1, total: work.length, name: item.file.name });
        update(i, { status: "Starting conversion…" });
        try {
          const result = await invoke<ConversionReport>("convert_file", { input: item.path, target: item.target });
          update(i, { status: `Done · Tier ${result.tier} · ${result.engine} · ${(result.duration_ms / 1000).toFixed(1)}s · ${result.output}${result.warning ? ` · ${result.warning}` : ""}` });
          const record = { name: item.file.name, output: result.output, engine: result.engine, tier: result.tier, at: Date.now() };
          try {
            await invoke("history_add", { item: record });
            setHistory((old) => [record, ...old].slice(0, 100));
          } catch (error) { console.error("Failed to save conversion history:", error); }
        } catch (error) {
          update(i, { status: stopRequested.current ? "Cancelled" : String(error) });
          if (stopRequested.current) break;
        }
      }
      if (stopRequested.current) {
        setItems((old) => old.map((entry) => entry.status === "Ready" ? { ...entry, status: "Skipped · queue cancelled" } : entry));
      }
    } finally { setBusy(false); setProgress({ current: 0, total: 0, name: "" }); }
  };
  const tryOpenOutput = async (output: string) => {
    try {
      await invoke("open_output", { path: output });
    } catch (error) {
      window.alert(`Could not open output.\\n${String(error)}`);
    }
  };
  const cancelQueue = async () => {
    stopRequested.current = true;
    try { await invoke("cancel_conversion"); } catch (error) { console.error("Failed to cancel conversion:", error); }
  };

  return <main className="app-shell">
    <aside className="sidebar"><div className="brand"><span className="brand-mark">D</span><span>documorph<small>OFFLINE DOCUMENT TOOLS</small></span></div>
      <nav><button className={`nav-item ${view === "convert" ? "active" : ""}`} onClick={() => setView("convert")}><span>⇄</span> Convert files</button><button className={`nav-item ${view === "history" ? "active" : ""}`} onClick={() => setView("history")}><span>◷</span> History</button></nav>
      <div className="side-bottom"><div className="offline"><i /> Offline &amp; private</div><div className="version">DOCUMORPH <span>v0.1.0</span></div></div>
    </aside>
    <section className="workspace"><header><div><div className="eyebrow">WORKSPACE / CONVERT</div><h1>Convert documents</h1><p>Transform files between formats, right on your device.</p></div><div className="privacy-pill">⌑ &nbsp; Files stay on this device</div></header>
      {view === "history" ? <><div className="section-heading"><div><h2>Conversion history <span className="count">{history.length}</span></h2><p>Saved locally on this device.</p></div>{history.length > 0 && <button className="clear" onClick={() => { void invoke("history_clear").then(() => setHistory([])).catch((error) => console.error("Failed to clear history:", error)); }}>Clear history</button>}</div>{history.length ? <div className="queue">{history.map((record, i) => <article className="file-row" key={`${record.output}-${record.at}-${i}`}><div className="file-icon">{record.tier}</div><div className="file-info"><strong>Original: {record.name}</strong><span>Output: {record.output.split(/[\\/]/).pop()} · {record.engine} · Tier {record.tier} · {new Date(record.at).toLocaleString()}</span></div><div className="history-actions"><button className="browse" onClick={() => void tryOpenOutput(record.output)}>Open output</button><button className="browse" onClick={() => void invoke("reveal_output", { path: record.output }).catch((error) => window.alert(`Could not show output in folder.\\n${String(error)}`))}>Show in folder</button></div></article>)}</div> : <div className="empty"><span>◷</span><strong>No conversions yet</strong><p>Successful conversions will appear here.</p></div>}</> : <><section className={`dropzone ${dragging ? "dragging" : ""}`} onClick={() => void browseFiles()}>
        <div className="engine-status">{engines ? <>AnyMD {engines.anymd ? "✓" : "not found"} <b>·</b> Pandoc {engines.pandoc ? "✓" : "not found"} <b>·</b> LibreOffice {engines.libreoffice ? "✓" : "not found"} <b>·</b> Typst {engines.typst ? "✓" : "not found"} <b>·</b> OCR {engines.ocrmypdf && engines.tesseract && engines.ghostscript ? "ready" : "not found"}</> : "Checking conversion engines…"}</div>
        <div className="upload-icon">↑</div><h2>Select documents to convert</h2><p>or <button className="browse" onClick={(e) => { e.stopPropagation(); void browseFiles(); }}>browse files</button> from your computer</p><div className="formats">DOCX <b>·</b> PDF <b>·</b> MD <b>·</b> HTML <b>·</b> TXT</div>
      </section>
      <div className="section-heading"><div><h2>Conversion queue <span className="count">{items.length}</span></h2><p>Choose an output format for each file.</p></div>{busy ? <button className="clear" onClick={() => void cancelQueue()}>Cancel queue</button> : items.length > 0 && <button className="clear" onClick={() => setItems([])}>Clear all</button>}</div>
      {busy && <div className="progress-panel"><div className="progress-copy"><strong>{progress.name ? `Converting file ${progress.current} of ${progress.total}` : "Preparing conversion queue…"}</strong><span>{progress.name ? `${progress.name} → ${items.find((item) => item.file.name === progress.name)?.target || "output"}` : "Checking queued files"}</span></div><div className="progress-track indeterminate" role="progressbar" aria-label="Conversion in progress"><i /></div><p>Conversion is running. Time depends on file size and the selected format.</p><p>Cancel stops the active engine and skips remaining files.</p></div>}
      {items.length ? <div className="queue">{items.map((item, i) => <article className="file-row" key={`${item.file.name}-${i}`}><div className="file-icon">{item.file.name.split(".").pop()?.toUpperCase().slice(0, 4)}</div><div className="file-info"><strong>{item.file.name}</strong><span>{item.file.size ? `${(item.file.size / 1024).toFixed(0)} KB` : "Local file"} <b>·</b> {item.status}{busy && progress.name === item.file.name && item.status.startsWith("Starting") && <span className="stage-note">Conversion in progress…</span>}{item.warnings?.map((warning) => <span className="preflight-warning" key={warning}>⚠ {warning}</span>)}</span></div><span className="arrow">→</span><select value={formatsFor(item.file.name, engines).includes(item.target) ? item.target : (formatsFor(item.file.name, engines)[0] || "")} disabled={!formatsFor(item.file.name, engines).length || busy} onChange={(e) => update(i, { target: e.target.value })}>{formatsFor(item.file.name, engines).map((format) => <option key={format}>{format}</option>)}</select><button className="remove" aria-label="Remove file" onClick={() => setItems((old) => old.filter((_, j) => i !== j))}>×</button></article>)}</div> : <div className="empty"><span>▤</span><strong>Your queue is empty</strong><p>Add a document to get started.</p></div>}
      <footer><div className="quality"><span>✳</span><div><strong>Quality comes first</strong><p>Every conversion includes a transparent quality report.</p></div></div><button className="convert" disabled={!items.length || busy || items.some((item) => item.status === "Checking file…")} onClick={convert}>{busy ? `Converting ${progress.current}/${progress.total}` : "Convert files"} <span>{busy ? "…" : "→"}</span></button></footer></>}
    </section>
  </main>;
}
export default App;
