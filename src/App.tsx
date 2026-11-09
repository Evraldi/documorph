import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import "./App.css";

type ConversionTab = "office" | "markdown" | "pdf" | "other";
type FileItem = { id: string; tab: ConversionTab; file: { name: string; size: number }; path: string; target: string; status: string; warnings?: string[] };
type PreflightReport = { size_bytes: number; warnings: string[] };
type EngineStatus = { pandoc: boolean; anymd: boolean; pdf2docx: boolean; libreoffice: boolean; typst: boolean; ocrmypdf: boolean; tesseract: boolean; ghostscript: boolean };
type ConversionReport = { output: string; engine: string; tier: string; duration_ms: number; warning: string | null };
type HistoryItem = { name: string; output: string; engine: string; tier: string; at: number };
const pandocInputs = ["md", "markdown", "html", "htm", "txt", "tex", "epub", "docx", "odt", "rtf"];
const loInputs = ["docx", "doc", "odt", "rtf", "pdf"];
const conversionTabs: { id: ConversionTab; label: string; description: string }[] = [
  { id: "office", label: "Office ↔ PDF", description: "Convert Word and Office documents to and from PDF." },
  { id: "markdown", label: "Any → Markdown", description: "Extract document content into Markdown with AnyMD." },
  { id: "pdf", label: "PDF tools", description: "Create searchable PDFs or extract OCR text." },
  { id: "other", label: "Other formats", description: "Convert HTML, text, and other supported formats." },
];
function targetsForTab(fileName: string, engines: EngineStatus | null, tab: ConversionTab) {
  const all = formatsFor(fileName, engines);
  const ext = fileName.split(".").pop()?.toLowerCase() || "";
  if (tab === "office") {
    const officeInputs = ["doc", "docx", "odt", "rtf", "pdf"];
    return officeInputs.includes(ext) ? all.filter((target) => target === "PDF" || (ext === "pdf" && target === "DOCX")) : [];
  }
  if (tab === "markdown") return all.filter((target) => target === "Markdown");
  if (tab === "pdf") return all.filter((target) => ["Searchable PDF", "OCR Text"].includes(target));
  return all.filter((target) => !["PDF", "DOCX", "Markdown", "Searchable PDF", "OCR Text"].includes(target));
}
function extensionsForTab(tab: ConversionTab) {
  if (tab === "office") return ["doc", "docx", "odt", "rtf", "pdf"];
  if (tab === "markdown") return ["pdf", "doc", "docx", "odt", "rtf", "md", "markdown", "html", "htm", "txt", "tex", "epub", "eml", "msg", "ipynb", "png", "jpg", "jpeg", "zip"];
  if (tab === "pdf") return ["pdf"];
  return ["md", "markdown", "html", "htm", "txt", "tex", "epub", "csv", "tsv", "json", "yaml", "yml", "toml", "xml", "xlsx", "xls", "pptx", "ods", "odp"];
}
function formatsFor(fileName: string, engines: EngineStatus | null) {
  const ext = fileName.split(".").pop()?.toLowerCase() || "";
  const choices = new Set<string>();
  if (engines?.anymd) choices.add("Markdown");
  if (ext === "pdf") {
    if (engines?.pdf2docx) choices.add("DOCX");
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
  const [activeTab, setActiveTab] = useState<ConversionTab>("office");
  const activeTabRef = useRef<ConversionTab>("office");
  const [history, setHistory] = useState<HistoryItem[]>([]);
  const [items, setItems] = useState<FileItem[]>([]);
  const [dragging, setDragging] = useState(false);
  const [engines, setEngines] = useState<EngineStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState({ current: 0, total: 0, id: "", name: "" });
  const stopRequested = useRef(false);
  const enginesRef = useRef<EngineStatus | null>(null);
  const busyRef = useRef(false);
  useEffect(() => { void invoke<EngineStatus>("engine_status").then((status) => { enginesRef.current = status; setEngines(status); }).catch(() => { const unavailable = { pandoc: false, anymd: false, pdf2docx: false, libreoffice: false, typst: false, ocrmypdf: false, tesseract: false, ghostscript: false }; enginesRef.current = unavailable; setEngines(unavailable); }); }, []);
  const addPaths = (paths: string[]) => {
    if (busyRef.current) return;
    setItems((old) => {
      const existing = new Set(old.map((item) => item.path.toLowerCase()));
      const additions = paths.filter((path) => {
        const normalized = path.toLowerCase();
        if (existing.has(normalized)) return false;
        existing.add(normalized);
        return true;
      }).map((path) => {
        const name = path.split(/[\\/]/).pop() || path;
        const tab = activeTabRef.current;
        const targets = targetsForTab(name, enginesRef.current, tab);
        return { id: crypto.randomUUID(), tab, file: { name, size: 0 }, path, target: targets.includes("Markdown") ? "Markdown" : targets.includes("PDF") ? "PDF" : targets[0] || "", status: targets.length ? "Checking file…" : "No supported route / engine", warnings: [] as string[] };
      });
      return additions.length ? [...old, ...additions] : old;
    });
    const acceptedPaths = paths.filter((path, index) => paths.findIndex((candidate) => candidate.toLowerCase() === path.toLowerCase()) === index);
    for (const path of acceptedPaths) {
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
      const tab = activeTabRef.current;
      const selected = await open({ multiple: true, title: `Choose ${conversionTabs.find((entry) => entry.id === tab)?.label || "documents"}`, filters: [{ name: `${conversionTabs.find((entry) => entry.id === tab)?.label || "Documents"} files`, extensions: extensionsForTab(tab) }] });
      if (!selected) return;
      const paths = Array.isArray(selected) ? selected : [selected];
      addPaths(paths);
    } catch { if (!busyRef.current) setItems((old) => [...old, { id: crypto.randomUUID(), tab: activeTabRef.current, file: { name: "File picker unavailable", size: 0 }, path: "", target: "PDF", status: "Run in the Tauri desktop app" }]); }
  };
  const visibleItems = items.filter((item) => item.tab === activeTab);
  const setTab = (tab: ConversionTab) => { activeTabRef.current = tab; setActiveTab(tab); };
  const update = (id: string, patch: Partial<FileItem>) => setItems((old) => old.map((item) => item.id === id ? { ...item, ...patch } : item));
  const convert = async () => {
    const queue = items.filter((item) => item.tab === activeTab).map((item) => ({ ...item, file: { ...item.file }, warnings: item.warnings ? [...item.warnings] : undefined }));
    stopRequested.current = false;
    busyRef.current = true;
    setBusy(true);
    const work = queue.filter((item) => item.path && !item.status.startsWith("Preflight failed"));
    setProgress({ current: 0, total: work.length, id: "", name: "" });
    try {
      for (const item of queue) {
        if (stopRequested.current) break;
        if (!item.path) {
          setItems((old) => old.map((entry) => entry.id === item.id ? { ...entry, status: "Use Browse files to select this file" } : entry));
          continue;
        }
        if (item.status.startsWith("Preflight failed")) continue;
        const current = work.findIndex((queued) => queued.id === item.id) + 1;
        setProgress({ current, total: work.length, id: item.id, name: item.file.name });
        setItems((old) => old.map((entry) => entry.id === item.id ? { ...entry, status: "Starting conversion…" } : entry));
        try {
          const result = await invoke<ConversionReport>("convert_file", { input: item.path, target: item.target });
          setItems((old) => old.map((entry) => entry.id === item.id ? { ...entry, status: `Done · Tier ${result.tier} · ${result.engine} · ${(result.duration_ms / 1000).toFixed(1)}s · ${result.output}${result.warning ? ` · ${result.warning}` : ""}` } : entry));
          const record = { name: item.file.name, output: result.output, engine: result.engine, tier: result.tier, at: Date.now() };
          try {
            await invoke("history_add", { item: record });
            setHistory((old) => [record, ...old].slice(0, 100));
          } catch (error) { console.error("Failed to save conversion history:", error); }
        } catch (error) {
          setItems((old) => old.map((entry) => entry.id === item.id ? { ...entry, status: stopRequested.current ? "Cancelled" : String(error) } : entry));
          if (stopRequested.current) break;
        }
      }
      if (stopRequested.current) {
        setItems((old) => old.map((entry) => queue.some((queued) => queued.id === entry.id) && entry.status === "Ready" ? { ...entry, status: "Skipped · queue cancelled" } : entry));
      }
    } finally { busyRef.current = false; setBusy(false); setProgress({ current: 0, total: 0, id: "", name: "" }); }
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
      {view === "history" ? <><div className="section-heading"><div><h2>Conversion history <span className="count">{history.length}</span></h2><p>Saved locally on this device.</p></div>{history.length > 0 && <button className="clear" onClick={() => { void invoke("history_clear").then(() => setHistory([])).catch((error) => console.error("Failed to clear history:", error)); }}>Clear history</button>}</div>{history.length ? <div className="queue">{history.map((record, i) => <article className="file-row" key={`${record.output}-${record.at}-${i}`}><div className="file-icon">{record.tier}</div><div className="file-info"><strong>Original: {record.name}</strong><span>Output: {record.output.split(/[\\/]/).pop()} · {record.engine} · Tier {record.tier} · {new Date(record.at).toLocaleString()}</span></div><div className="history-actions"><button className="browse" onClick={() => void tryOpenOutput(record.output)}>Open output</button><button className="browse" onClick={() => void invoke("reveal_output", { path: record.output }).catch((error) => window.alert(`Could not show output in folder.\\n${String(error)}`))}>Show in folder</button></div></article>)}</div> : <div className="empty"><span>◷</span><strong>No conversions yet</strong><p>Successful conversions will appear here.</p></div>}</> : <><div className="conversion-tabs" role="tablist" aria-label="Conversion types">{conversionTabs.map((tab) => <button key={tab.id} role="tab" aria-selected={activeTab === tab.id} className={`conversion-tab ${activeTab === tab.id ? "active" : ""}`} disabled={busy} onClick={() => setTab(tab.id)}>{tab.label}</button>)}</div>
      <section className={`dropzone ${dragging ? "dragging" : ""}`} onClick={() => void browseFiles()}>
        <div className="engine-status">{engines ? <>PDF→DOCX {engines.pdf2docx ? "✓" : "not found"} <b>·</b> AnyMD {engines.anymd ? "✓" : "not found"} <b>·</b> Pandoc {engines.pandoc ? "✓" : "not found"} <b>·</b> LibreOffice {engines.libreoffice ? "✓" : "not found"} <b>·</b> Typst {engines.typst ? "✓" : "not found"} <b>·</b> OCR {engines.ocrmypdf && engines.tesseract && engines.ghostscript ? "ready" : "not found"}</> : "Checking conversion engines…"}</div>
        <div className="upload-icon">↑</div><h2>{conversionTabs.find((tab) => tab.id === activeTab)?.label}</h2><p>{conversionTabs.find((tab) => tab.id === activeTab)?.description}</p><p>or <button className="browse" onClick={(e) => { e.stopPropagation(); void browseFiles(); }}>browse files</button> from your computer</p><div className="formats">Accepted files: {extensionsForTab(activeTab).map((extension, index) => <span key={extension}>{index > 0 && <> <b>·</b> </>}{extension.toUpperCase()}</span>)}</div>
      </section>
      <div className="section-heading"><div><h2>{conversionTabs.find((tab) => tab.id === activeTab)?.label} queue <span className="count">{visibleItems.length}</span></h2><p>Choose an output format for each file.</p></div>{busy ? <button className="clear" onClick={() => void cancelQueue()}>Cancel queue</button> : visibleItems.length > 0 && <button className="clear" onClick={() => setItems((old) => old.filter((item) => item.tab !== activeTab))}>Clear tab</button>}</div>
      {busy && <div className="progress-panel"><div className="progress-copy"><strong>{progress.name ? `Converting file ${progress.current} of ${progress.total}` : "Preparing conversion queue…"}</strong><span>{progress.name ? `${progress.name} → ${items.find((item) => item.id === progress.id)?.target || "output"}` : "Checking queued files"}</span></div><div className="progress-track indeterminate" role="progressbar" aria-label="Conversion in progress"><i /></div><p>Conversion is running. Time depends on file size and the selected format.</p><p>Cancel stops the active engine and skips remaining files.</p></div>}
      {visibleItems.length ? <div className="queue">{visibleItems.map((item) => <article className="file-row" key={item.id}><div className="file-icon">{item.file.name.split(".").pop()?.toUpperCase().slice(0, 4)}</div><div className="file-info"><strong>{item.file.name}</strong><span>{item.file.size ? `${(item.file.size / 1024).toFixed(0)} KB` : "Local file"} <b>·</b> {item.status}{busy && progress.id === item.id && item.status.startsWith("Starting") && <span className="stage-note">Conversion in progress…</span>}{item.warnings?.map((warning) => <span className="preflight-warning" key={warning}>⚠ {warning}</span>)}</span></div><span className="arrow">→</span><select value={targetsForTab(item.file.name, engines, activeTab).includes(item.target) ? item.target : (targetsForTab(item.file.name, engines, activeTab)[0] || "")} disabled={!targetsForTab(item.file.name, engines, activeTab).length || busy} onChange={(e) => update(item.id, { target: e.target.value })}>{targetsForTab(item.file.name, engines, activeTab).map((format) => <option key={format}>{format}</option>)}</select><button className="remove" aria-label="Remove file" disabled={busy} onClick={() => setItems((old) => old.filter((entry) => entry.id !== item.id))}>×</button></article>)}</div> : <div className="empty"><span>▤</span><strong>Your queue is empty</strong><p>Add a {conversionTabs.find((tab) => tab.id === activeTab)?.label.toLowerCase()} file to get started.</p></div>}
      <footer><div className="quality"><span>✳</span><div><strong>Quality comes first</strong><p>Every conversion includes a transparent quality report.</p></div></div><button className="convert" disabled={!visibleItems.length || busy || visibleItems.some((item) => item.status === "Checking file…") || visibleItems.every((item) => !item.target)} onClick={convert}>{busy ? `Converting ${progress.current}/${progress.total}` : "Convert files"} <span>{busy ? "…" : "→"}</span></button></footer></>}
    </section>
  </main>;
}
export default App;
