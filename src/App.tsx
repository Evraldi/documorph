import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { conversionTabs, extensionsForTab, runnableQueue, targetsForTab } from "./lib/conversion";
import type { ConversionTab, EngineStatus } from "./lib/conversion";
import "./App.css";

type FileItem = { id: string; tab: ConversionTab; file: { name: string; size: number }; path: string; target: string; status: string; warnings?: string[] };
type PreflightReport = { size_bytes: number; warnings: string[] };
type ConversionReport = { output: string; engine: string; tier: string; duration_ms: number; warning: string | null };
type HistoryItem = { name: string; output: string; engine: string; tier: string; at: number };

function App() {
  const [view, setView] = useState<"convert" | "history">("convert");
  const [activeTab, setActiveTab] = useState<ConversionTab>("office");
  const activeTabRef = useRef<ConversionTab>("office");
  const [history, setHistory] = useState<HistoryItem[]>([]);
  const [items, setItems] = useState<FileItem[]>([]);
  const [dragging, setDragging] = useState(false);
  const [engines, setEngines] = useState<EngineStatus | null>(null);
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState({ current: 0, total: 0, id: "", name: "" });
  const stopRequested = useRef(false);
  const enginesRef = useRef<EngineStatus | null>(null);
  const enginesPromise = useRef<Promise<EngineStatus> | null>(null);
  const busyRef = useRef(false);
  const unavailable: EngineStatus = { pandoc: false, anymd: false, pdf2docx: false, libreoffice: false, typst: false, ocrmypdf: false, tesseract: false, ghostscript: false };
  const applyEngines = (status: EngineStatus) => {
    enginesRef.current = status;
    setEngines(status);
  };
  const loadEngines = (command: "engine_status" | "refresh_dependencies") => {
    const pending = invoke<EngineStatus>(command).then((status) => { applyEngines(status); return status; }).catch(() => { applyEngines(unavailable); return unavailable; });
    enginesPromise.current = pending;
    return pending;
  };
  useEffect(() => { void loadEngines("engine_status"); }, []);
  const refreshEngines = async () => {
    await loadEngines("refresh_dependencies");
    setItems((old) => old.map((item) => !item.target && item.status === "No supported route / engine" ? { ...item, status: "Ready", target: targetsForTab(item.file.name, enginesRef.current, item.tab)[0] || "" } : item));
    setNotice("Engine status refreshed.");
  };
  const addPaths = async (paths: string[]) => {
    if (busyRef.current) return;
    // Drop bisa terjadi sebelum engine_status selesai; tunggu dulu supaya
    // target tidak kosong dan status tidak perlu di-refresh manual.
    if (enginesPromise.current) await enginesPromise.current;
    let acceptedPaths: string[];
    try {
      acceptedPaths = await invoke<string[]>("filter_inputs", { paths, extensions: extensionsForTab(activeTabRef.current) });
    } catch (error) {
      setNotice(`Input filter failed: ${String(error)}`);
      return;
    }
    if (!acceptedPaths.length) return;
    setNotice(acceptedPaths.length < paths.length ? `${paths.length - acceptedPaths.length} file diabaikan (bukan file / ekstensi tidak didukung).` : "");
    setItems((old) => {
      const existing = new Set(old.map((item) => item.path.toLowerCase()));
      const additions = acceptedPaths.filter((path) => {
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
        if (event.payload.type === "drop") void addPaths(event.payload.paths);
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
      await addPaths(paths);
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
    // Item tanpa target tidak pernah dipanggil dan tidak masuk total progress.
    const work = runnableQueue(queue);
    setProgress({ current: 0, total: work.length, id: "", name: "" });
    try {
      for (const item of queue) {
        if (stopRequested.current) break;
        if (!item.path) {
          setItems((old) => old.map((entry) => entry.id === item.id ? { ...entry, status: "Use Browse files to select this file" } : entry));
          continue;
        }
        if (!item.target || item.status.startsWith("Preflight failed") || item.status === "No supported route / engine") continue;
        const current = work.findIndex((queued) => queued.id === item.id) + 1;
        setProgress({ current, total: work.length, id: item.id, name: item.file.name });
        setItems((old) => old.map((entry) => entry.id === item.id ? { ...entry, status: "Starting conversion…" } : entry));
        try {
          const result = await invoke<ConversionReport>("convert_file", { input: item.path, target: item.target, jobId: item.id });
          setItems((old) => old.map((entry) => entry.id === item.id ? { ...entry, status: `Done · Tier ${result.tier} · ${result.engine} · ${(result.duration_ms / 1000).toFixed(1)}s · ${result.output}${result.warning ? ` · ${result.warning}` : ""}` } : entry));
          // Riwayat ditulis Rust saat konversi berhasil; frontend hanya memuat ulang.
          try {
            setHistory(await invoke<HistoryItem[]>("history_list"));
          } catch (error) { console.error("Failed to reload conversion history:", error); }
        } catch (error) {
          setItems((old) => old.map((entry) => entry.id === item.id ? { ...entry, status: stopRequested.current ? "Cancelled" : `Failed · ${String(error)}` } : entry));
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
  const cancelJob = async (id: string) => {
    try { await invoke("cancel_conversion", { jobId: id }); } catch (error) { console.error("Failed to cancel job:", error); }
  };

  return <main className="app-shell">
    <aside className="sidebar"><div className="brand"><span className="brand-mark">D</span><span>documorph<small>OFFLINE DOCUMENT TOOLS</small></span></div>
      <nav><button className={`nav-item ${view === "convert" ? "active" : ""}`} onClick={() => setView("convert")}><span>⇄</span> Convert files</button><button className={`nav-item ${view === "history" ? "active" : ""}`} onClick={() => setView("history")}><span>◷</span> History</button></nav>
      <div className="side-bottom"><div className="offline"><i /> Offline &amp; private</div><div className="version">DOCUMORPH <span>v0.1.0</span></div></div>
    </aside>
    <section className="workspace"><header><div><div className="eyebrow">WORKSPACE / CONVERT</div><h1>Convert documents</h1><p>Transform files between formats, right on your device.</p>{notice && <p className="stage-note" role="status">{notice}</p>}</div><div className="privacy-pill">⌑ &nbsp; Files stay on this device</div></header>
      {view === "history" ? <><div className="section-heading"><div><h2>Conversion history <span className="count">{history.length}</span></h2><p>Saved locally on this device.</p></div>{history.length > 0 && <button className="clear" onClick={() => { void invoke("history_clear").then(() => setHistory([])).catch((error) => console.error("Failed to clear history:", error)); }}>Clear history</button>}</div>{history.length ? <div className="queue">{history.map((record, i) => <article className="file-row" key={`${record.output}-${record.at}-${i}`}><div className="file-icon">{record.tier}</div><div className="file-info"><strong>Original: {record.name}</strong><span>Output: {record.output.split(/[\\/]/).pop()} · {record.engine} · Tier {record.tier} · {new Date(record.at).toLocaleString()}</span></div><div className="history-actions"><button className="browse" onClick={() => void tryOpenOutput(record.output)}>Open output</button><button className="browse" onClick={() => void invoke("reveal_output", { path: record.output }).catch((error) => window.alert(`Could not show output in folder.\\n${String(error)}`))}>Show in folder</button></div></article>)}</div> : <div className="empty"><span>◷</span><strong>No conversions yet</strong><p>Successful conversions will appear here.</p></div>}</> : <><div className="conversion-tabs" role="tablist" aria-label="Conversion types">{conversionTabs.map((tab) => <button key={tab.id} role="tab" aria-selected={activeTab === tab.id} className={`conversion-tab ${activeTab === tab.id ? "active" : ""}`} disabled={busy} onClick={() => setTab(tab.id)}>{tab.label}</button>)}</div>
      <section className={`dropzone ${dragging ? "dragging" : ""}`} onClick={() => void browseFiles()}>
        <div className="engine-status">{engines ? <>PDF→DOCX {engines.pdf2docx ? "✓" : "not found"} <b>·</b> AnyMD {engines.anymd ? "✓" : "not found"} <b>·</b> Pandoc {engines.pandoc ? "✓" : "not found"} <b>·</b> LibreOffice {engines.libreoffice ? "✓" : "not found"} <b>·</b> Typst {engines.typst ? "✓" : "not found"} <b>·</b> OCR {engines.ocrmypdf && engines.tesseract && engines.ghostscript ? "ready" : "not found"}</> : "Checking conversion engines…"} <button className="browse" disabled={busy} onClick={() => void refreshEngines()}>Refresh engines</button></div>
        <div className="upload-icon">↑</div><h2>{conversionTabs.find((tab) => tab.id === activeTab)?.label}</h2><p>{conversionTabs.find((tab) => tab.id === activeTab)?.description}</p><p>or <button className="browse" onClick={(e) => { e.stopPropagation(); void browseFiles(); }}>browse files</button> from your computer</p><div className="formats">Accepted files: {extensionsForTab(activeTab).map((extension, index) => <span key={extension}>{index > 0 && <> <b>·</b> </>}{extension.toUpperCase()}</span>)}</div>
      </section>
      <div className="section-heading"><div><h2>{conversionTabs.find((tab) => tab.id === activeTab)?.label} queue <span className="count">{visibleItems.length}</span></h2><p>Choose an output format for each file.</p></div>{busy ? <button className="clear" onClick={() => void cancelQueue()}>Cancel queue</button> : visibleItems.length > 0 && <button className="clear" onClick={() => setItems((old) => old.filter((item) => item.tab !== activeTab))}>Clear tab</button>}</div>
      {busy && <div className="progress-panel"><div className="progress-copy"><strong>{progress.name ? `Converting file ${progress.current} of ${progress.total}` : "Preparing conversion queue…"}</strong><span>{progress.name ? `${progress.name} → ${items.find((item) => item.id === progress.id)?.target || "output"}` : "Checking queued files"}</span></div><div className="progress-track indeterminate" role="progressbar" aria-label="Conversion in progress"><i /></div><p>Conversion is running. Time depends on file size and the selected format.</p><p>Cancel stops the active engine and skips remaining files.</p></div>}
      {visibleItems.length ? <div className="queue">{visibleItems.map((item) => <article className="file-row" key={item.id}><div className="file-icon">{item.file.name.split(".").pop()?.toUpperCase().slice(0, 4)}</div><div className="file-info"><strong>{item.file.name}</strong><span>{item.file.size ? `${(item.file.size / 1024).toFixed(0)} KB` : "Local file"} <b>·</b> {item.status}{busy && progress.id === item.id && item.status.startsWith("Starting") && <span className="stage-note">Conversion in progress…</span>}{item.warnings?.map((warning) => <span className="preflight-warning" key={warning}>⚠ {warning}</span>)}</span></div><span className="arrow">→</span><select value={targetsForTab(item.file.name, engines, activeTab).includes(item.target) ? item.target : (targetsForTab(item.file.name, engines, activeTab)[0] || "")} disabled={!targetsForTab(item.file.name, engines, activeTab).length || busy} onChange={(e) => update(item.id, { target: e.target.value })}>{targetsForTab(item.file.name, engines, activeTab).map((format) => <option key={format}>{format}</option>)}</select>{busy && progress.id === item.id && <button className="clear" onClick={() => void cancelJob(item.id)}>Cancel</button>}<button className="remove" aria-label="Remove file" disabled={busy} onClick={() => setItems((old) => old.filter((entry) => entry.id !== item.id))}>×</button></article>)}</div> : <div className="empty"><span>▤</span><strong>Your queue is empty</strong><p>Add a {conversionTabs.find((tab) => tab.id === activeTab)?.label.toLowerCase()} file to get started.</p></div>}
      <footer><div className="quality"><span>✳</span><div><strong>Quality comes first</strong><p>Every conversion includes a transparent quality report.</p></div></div><button className="convert" disabled={!visibleItems.length || busy || visibleItems.some((item) => item.status === "Checking file…") || visibleItems.every((item) => !item.target)} onClick={convert}>{busy ? `Converting ${progress.current}/${progress.total}` : "Convert files"} <span>{busy ? "…" : "→"}</span></button></footer></>}
    </section>
  </main>;
}
export default App;
