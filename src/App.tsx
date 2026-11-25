import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { open } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import {
  conversionTabs,
  extensionsForTab,
  freshPaths,
  runnableQueue,
  targetsForTab,
} from "./lib/conversion";
import type { ConversionTab, EngineStatus } from "./lib/conversion";
import "./App.css";

type FileItem = {
  id: string;
  tab: ConversionTab;
  file: { name: string; size: number };
  path: string;
  target: string;
  status: string;
  warnings?: string[];
};
type PreflightReport = { size_bytes: number; warnings: string[] };
type ConversionReport = {
  output: string;
  engine: string;
  tier: string;
  duration_ms: number;
  warning: string | null;
};
type HistoryItem = {
  name: string;
  output: string;
  engine: string;
  tier: string;
  at: number;
  warnings: string[];
};

const NO_ROUTE = "Tidak ada rute/engine yang didukung";
const CHECKING = "Memeriksa file…";

function App() {
  const [view, setView] = useState<"convert" | "history">("convert");
  const [activeTab, setActiveTab] = useState<ConversionTab>("office");
  const activeTabRef = useRef<ConversionTab>("office");
  const [history, setHistory] = useState<HistoryItem[]>([]);
  const [items, setItemsState] = useState<FileItem[]>([]);
  const itemsRef = useRef<FileItem[]>([]);
  const [dragging, setDragging] = useState(false);
  const [engines, setEngines] = useState<EngineStatus | null>(null);
  const [notice, setNotice] = useState("");
  const [version, setVersion] = useState("");
  const [confirmClear, setConfirmClear] = useState(false);
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState({ current: 0, total: 0, id: "", name: "" });
  const stopRequested = useRef(false);
  const enginesRef = useRef<EngineStatus | null>(null);
  const enginesPromise = useRef<Promise<EngineStatus> | null>(null);
  const busyRef = useRef(false);
  const setItems = (updater: (old: FileItem[]) => FileItem[]) => {
    setItemsState((old) => {
      const next = updater(old);
      itemsRef.current = next;
      return next;
    });
  };
  const unavailable: EngineStatus = {
    pandoc: false,
    anymd: false,
    pdf2docx: false,
    libreoffice: false,
    typst: false,
    ocrmypdf: false,
    tesseract: false,
    ghostscript: false,
  };

  const applyEngines = (status: EngineStatus) => {
    enginesRef.current = status;
    setEngines(status);
  };
  const loadEngines = (command: "engine_status" | "refresh_dependencies") => {
    const pending = invoke<EngineStatus>(command)
      .then((status) => {
        applyEngines(status);
        return status;
      })
      .catch(() => {
        applyEngines(unavailable);
        return unavailable;
      });
    enginesPromise.current = pending;
    return pending;
  };

  useEffect(() => {
    void loadEngines("engine_status");
    void getVersion().then(setVersion).catch(() => setVersion(""));
  }, []);

  const refreshEngines = async () => {
    await loadEngines("refresh_dependencies");
    setItems((old) =>
      old.map((item) =>
        !item.target && item.status === NO_ROUTE
          ? {
              ...item,
              status: "Siap",
              target: targetsForTab(item.file.name, enginesRef.current, item.tab)[0] || "",
            }
          : item,
      ),
    );
    setNotice("Status engine diperbarui.");
  };

  const addPaths = async (paths: string[]) => {
    // Drop bisa terjadi sebelum engine_status selesai; tunggu dulu supaya
    // target tidak kosong dan status tidak perlu di-refresh manual.
    const tab = activeTabRef.current;
    if (enginesPromise.current) await enginesPromise.current;
    let acceptedPaths: string[];
    try {
      acceptedPaths = await invoke<string[]>("filter_inputs", {
        paths,
        extensions: extensionsForTab(tab),
      });
    } catch (error) {
      setNotice(`Filter input gagal: ${String(error)}`);
      return;
    }
    const { accepted, duplicates } = freshPaths(itemsRef.current, acceptedPaths, tab);
    const ignored = paths.length - acceptedPaths.length;
    const notes = [
      ignored ? `${ignored} file diabaikan (bukan file / ekstensi tidak didukung).` : "",
      duplicates ? `${duplicates} file sudah ada di antrean tab ini.` : "",
    ].filter(Boolean);
    setNotice(notes.join(" "));
    if (!accepted.length) return;
    setItems((old) => {
      const additions = freshPaths(old, accepted, tab).accepted.map((path) => {
        const name = path.split(/[\\/]/).pop() || path;
        const targets = targetsForTab(name, enginesRef.current, tab);
        return {
          id: crypto.randomUUID(),
          tab,
          file: { name, size: 0 },
          path,
          target: targets.includes("Markdown")
            ? "Markdown"
            : targets.includes("PDF")
              ? "PDF"
              : targets[0] || "",
          status: targets.length ? CHECKING : NO_ROUTE,
          warnings: [] as string[],
        };
      });
      return additions.length ? [...old, ...additions] : old;
    });
    for (const path of accepted) {
      const matches = (item: FileItem) => item.tab === tab && item.path.toLowerCase() === path.toLowerCase();
      void invoke<PreflightReport>("preflight_file", { input: path })
        .then((report) => {
          setItems((old) =>
            old.map((item) =>
              matches(item)
                ? {
                    ...item,
                    file: { ...item.file, size: report.size_bytes },
                    status: item.target ? "Siap" : item.status,
                    warnings: report.warnings,
                  }
                : item,
            ),
          );
        })
        .catch((error) => {
          setItems((old) =>
            old.map((item) =>
              matches(item) ? { ...item, status: `Preflight gagal: ${String(error)}` } : item,
            ),
          );
        });
    }
  };

  useEffect(() => {
    void invoke<string[]>("take_open_files")
      .then((paths) => {
        if (paths.length) void addPaths(paths);
      })
      .catch((error) => console.error("Gagal mengambil file dari Explorer:", error));
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void getCurrentWebview()
      .onDragDropEvent((event) => {
        if (event.payload.type === "over") setDragging(true);
        else {
          setDragging(false);
          if (event.payload.type === "drop") void addPaths(event.payload.paths);
        }
      })
      .then((dispose) => {
        if (cancelled) dispose();
        else unlisten = dispose;
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const reloadHistory = async () => {
    try {
      setHistory(await invoke<HistoryItem[]>("history_list"));
    } catch (error) {
      setNotice(`Gagal memuat riwayat: ${String(error)}`);
    }
  };
  useEffect(() => {
    void reloadHistory();
  }, []);

  const browseFiles = async () => {
    try {
      const tab = activeTabRef.current;
      const label = conversionTabs.find((entry) => entry.id === tab)?.label || "dokumen";
      const selected = await open({
        multiple: true,
        title: `Pilih ${label}`,
        filters: [{ name: `Berkas ${label}`, extensions: extensionsForTab(tab) }],
      });
      if (!selected) return;
      await addPaths(Array.isArray(selected) ? selected : [selected]);
    } catch {
      if (!busyRef.current) {
        setNotice("Pemilih file tidak tersedia. Jalankan aplikasi desktop Tauri.");
      }
    }
  };

  const visibleItems = items.filter((item) => item.tab === activeTab);
  const setTab = (tab: ConversionTab) => {
    activeTabRef.current = tab;
    setActiveTab(tab);
  };
  const update = (id: string, patch: Partial<FileItem>) =>
    setItems((old) => old.map((item) => (item.id === id ? { ...item, ...patch } : item)));

  const convert = async () => {
    const queue = items
      .filter((item) => item.tab === activeTab)
      .map((item) => ({
        ...item,
        file: { ...item.file },
        warnings: item.warnings ? [...item.warnings] : undefined,
      }));
    stopRequested.current = false;
    busyRef.current = true;
    setBusy(true);
    const work = runnableQueue(queue);
    setProgress({ current: 0, total: work.length, id: "", name: "" });
    try {
      for (const item of queue) {
        if (stopRequested.current) break;
        if (!item.path) {
          setItems((old) =>
            old.map((entry) =>
              entry.id === item.id
                ? { ...entry, status: "Gunakan Pilih file untuk memilih file ini" }
                : entry,
            ),
          );
          continue;
        }
        if (!item.target || item.status.startsWith("Preflight gagal") || item.status === NO_ROUTE) {
          continue;
        }
        const current = work.findIndex((queued) => queued.id === item.id) + 1;
        setProgress({ current, total: work.length, id: item.id, name: item.file.name });
        setItems((old) =>
          old.map((entry) =>
            entry.id === item.id ? { ...entry, status: "Memulai konversi…" } : entry,
          ),
        );
        try {
          const result = await invoke<ConversionReport>("convert_file", {
            input: item.path,
            target: item.target,
            jobId: item.id,
          });
          setItems((old) =>
            old.map((entry) =>
              entry.id === item.id
                ? {
                    ...entry,
                    status: `Selesai · Tier ${result.tier} · ${result.engine} · ${(result.duration_ms / 1000).toFixed(1)}s · ${result.output}${result.warning ? ` · ${result.warning}` : ""}`,
                  }
                : entry,
            ),
          );
          await reloadHistory();
        } catch (error) {
          setItems((old) =>
            old.map((entry) =>
              entry.id === item.id
                ? {
                    ...entry,
                    status: stopRequested.current ? "Dibatalkan" : `Gagal · ${String(error)}`,
                  }
                : entry,
            ),
          );
          if (stopRequested.current) break;
        }
      }
      if (stopRequested.current) {
        setItems((old) =>
          old.map((entry) =>
            queue.some((queued) => queued.id === entry.id) && entry.status === "Siap"
              ? { ...entry, status: "Dilewati · antrean dibatalkan" }
              : entry,
          ),
        );
      }
    } finally {
      busyRef.current = false;
      setBusy(false);
      setProgress({ current: 0, total: 0, id: "", name: "" });
    }
  };

  const tryOpenOutput = async (output: string) => {
    try {
      await invoke("open_output", { path: output });
    } catch (error) {
      setNotice(`Gagal membuka keluaran: ${String(error)}`);
    }
  };
  const tryRevealOutput = async (output: string) => {
    try {
      await invoke("reveal_output", { path: output });
    } catch (error) {
      setNotice(`Gagal menampilkan folder keluaran: ${String(error)}`);
    }
  };
  const clearHistory = async () => {
    if (!confirmClear) {
      setConfirmClear(true);
      setNotice("Klik sekali lagi untuk menghapus seluruh riwayat.");
      return;
    }
    try {
      await invoke("history_clear");
      setHistory([]);
      setConfirmClear(false);
      setNotice("Riwayat dihapus.");
    } catch (error) {
      setNotice(`Gagal menghapus riwayat: ${String(error)}`);
    }
  };
  const cancelQueue = async () => {
    stopRequested.current = true;
    try {
      await invoke("cancel_conversion");
    } catch (error) {
      setNotice(`Gagal membatalkan antrean: ${String(error)}`);
    }
  };
  const cancelJob = async (id: string) => {
    try {
      await invoke("cancel_conversion", { jobId: id });
    } catch (error) {
      setNotice(`Gagal membatalkan pekerjaan: ${String(error)}`);
    }
  };

  const activeLabel = conversionTabs.find((tab) => tab.id === activeTab);

  return (
    <main className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <span className="brand-mark">D</span>
          <span>documorph<small>ALAT DOKUMEN OFFLINE</small></span>
        </div>
        <nav>
          <button
            className={`nav-item ${view === "convert" ? "active" : ""}`}
            onClick={() => setView("convert")}
          >
            <span>⇄</span> Konversi file
          </button>
          <button
            className={`nav-item ${view === "history" ? "active" : ""}`}
            onClick={() => setView("history")}
          >
            <span>◷</span> Riwayat
          </button>
        </nav>
        <div className="side-bottom">
          <div className="offline">
            <i /> Offline &amp; privat
          </div>
          <div className="version">
            DOCUMORPH <span>{version ? `v${version}` : ""}</span>
          </div>
        </div>
      </aside>
      <section className="workspace">
        <header>
          <div>
            <div className="eyebrow">RUANG KERJA / {view === "history" ? "RIWAYAT" : "KONVERSI"}</div>
            <h1>{view === "history" ? "Riwayat konversi" : "Konversi dokumen"}</h1>
            <p>
              {view === "history"
                ? "Semua konversi tersimpan lokal di perangkat ini."
                : "Ubah format dokumen langsung di perangkat kamu."}
            </p>
            {notice && (
              <p className="stage-note" role="status">
                {notice}
              </p>
            )}
          </div>
          <div className="privacy-pill">⌑ &nbsp; File tetap di perangkat ini</div>
        </header>
        {view === "history" ? (
          <>
            <div className="section-heading">
              <div>
                <h2>
                  Riwayat konversi <span className="count">{history.length}</span>
                </h2>
                <p>Tersimpan lokal di perangkat ini.</p>
              </div>
              {history.length > 0 && (
                <button className="clear" onClick={() => void clearHistory()}>
                  {confirmClear ? "Yakin hapus?" : "Hapus riwayat"}
                </button>
              )}
            </div>
            {history.length ? (
              <div className="queue">
                {history.map((record, i) => (
                  <article className="file-row" key={`${record.output}-${record.at}-${i}`}>
                    <div className="file-icon">{record.tier}</div>
                    <div className="file-info">
                      <strong>Asli: {record.name}</strong>
                      <span>
                        Keluaran: {record.output.split(/[\\/]/).pop()} · {record.engine} · Tier{" "}
                        {record.tier} · {new Date(record.at).toLocaleString()}
                      </span>
                      {record.warnings?.map((warning) => (
                        <span className="preflight-warning" key={warning}>
                          ⚠ {warning}
                        </span>
                      ))}
                    </div>
                    <div className="history-actions">
                      <button className="browse" onClick={() => void tryOpenOutput(record.output)}>
                        Buka hasil
                      </button>
                      <button className="browse" onClick={() => void tryRevealOutput(record.output)}>
                        Tampilkan di folder
                      </button>
                    </div>
                  </article>
                ))}
              </div>
            ) : (
              <div className="empty">
                <span>◷</span>
                <strong>Belum ada konversi</strong>
                <p>Konversi yang berhasil akan muncul di sini.</p>
              </div>
            )}
          </>
        ) : (
          <>
            <div className="conversion-tabs" role="tablist" aria-label="Jenis konversi">
              {conversionTabs.map((tab) => (
                <button
                  key={tab.id}
                  role="tab"
                  aria-selected={activeTab === tab.id}
                  className={`conversion-tab ${activeTab === tab.id ? "active" : ""}`}
                  disabled={busy}
                  onClick={() => setTab(tab.id)}
                >
                  {tab.label}
                </button>
              ))}
            </div>
            <section
              className={`dropzone ${dragging ? "dragging" : ""}`}
              onClick={() => void browseFiles()}
            >
              <div className="engine-status">
                {engines ? (
                  <>
                    PDF→DOCX {engines.pdf2docx ? "✓" : "tidak ada"} <b>·</b> AnyMD{" "}
                    {engines.anymd ? "✓" : "tidak ada"} <b>·</b> Pandoc{" "}
                    {engines.pandoc ? "✓" : "tidak ada"} <b>·</b> LibreOffice{" "}
                    {engines.libreoffice ? "✓" : "tidak ada"} <b>·</b> Typst{" "}
                    {engines.typst ? "✓" : "tidak ada"} <b>·</b> OCR{" "}
                    {engines.ocrmypdf && engines.tesseract && engines.ghostscript
                      ? "siap"
                      : "tidak ada"}
                  </>
                ) : (
                  "Memeriksa engine konversi…"
                )}{" "}
                <button className="browse" disabled={busy} onClick={() => void refreshEngines()}>
                  Perbarui engine
                </button>
              </div>
              <div className="upload-icon">↑</div>
              <h2>{activeLabel?.label}</h2>
              <p>{activeLabel?.description}</p>
              <p>
                atau{" "}
                <button
                  className="browse"
                  onClick={(event) => {
                    event.stopPropagation();
                    void browseFiles();
                  }}
                >
                  pilih file
                </button>{" "}
                dari komputer kamu
              </p>
              <div className="formats">
                File diterima:{" "}
                {extensionsForTab(activeTab).map((extension, index) => (
                  <span key={extension}>
                    {index > 0 && <> <b>·</b> </>}
                    {extension.toUpperCase()}
                  </span>
                ))}
              </div>
            </section>
            <div className="section-heading">
              <div>
                <h2>
                  Antrean {activeLabel?.label} <span className="count">{visibleItems.length}</span>
                </h2>
                <p>Pilih format keluaran untuk setiap file.</p>
              </div>
              {busy ? (
                <button className="clear" onClick={() => void cancelQueue()}>
                  Batalkan antrean
                </button>
              ) : (
                visibleItems.length > 0 && (
                  <button
                    className="clear"
                    onClick={() => setItems((old) => old.filter((item) => item.tab !== activeTab))}
                  >
                    Kosongkan tab
                  </button>
                )
              )}
            </div>
            {busy && (
              <div className="progress-panel">
                <div className="progress-copy">
                  <strong>
                    {progress.name
                      ? `Mengonversi file ${progress.current} dari ${progress.total}`
                      : "Menyiapkan antrean…"}
                  </strong>
                  <span>
                    {progress.name
                      ? `${progress.name} → ${items.find((item) => item.id === progress.id)?.target || "keluaran"}`
                      : "Memeriksa file dalam antrean"}
                  </span>
                </div>
                <div
                  className="progress-track indeterminate"
                  role="progressbar"
                  aria-label="Konversi sedang berjalan"
                >
                  <i />
                </div>
                <p>Waktu konversi bergantung pada ukuran file dan format yang dipilih.</p>
                <p>Batal menghentikan engine aktif dan melewati sisa file.</p>
              </div>
            )}
            {visibleItems.length ? (
              <div className="queue">
                {visibleItems.map((item) => (
                  <article className="file-row" key={item.id}>
                    <div className="file-icon">
                      {item.file.name.split(".").pop()?.toUpperCase().slice(0, 4)}
                    </div>
                    <div className="file-info">
                      <strong>{item.file.name}</strong>
                      <span>
                        {item.file.size ? `${(item.file.size / 1024).toFixed(0)} KB` : "File lokal"}{" "}
                        <b>·</b> {item.status}
                        {busy && progress.id === item.id && item.status.startsWith("Memulai") && (
                          <span className="stage-note">Konversi berjalan…</span>
                        )}
                        {item.warnings?.map((warning) => (
                          <span className="preflight-warning" key={warning}>
                            ⚠ {warning}
                          </span>
                        ))}
                      </span>
                    </div>
                    <span className="arrow">→</span>
                    <select
                      value={
                        targetsForTab(item.file.name, engines, activeTab).includes(item.target)
                          ? item.target
                          : targetsForTab(item.file.name, engines, activeTab)[0] || ""
                      }
                      disabled={!targetsForTab(item.file.name, engines, activeTab).length || busy}
                      onChange={(event) => update(item.id, { target: event.target.value })}
                    >
                      {targetsForTab(item.file.name, engines, activeTab).map((format) => (
                        <option key={format}>{format}</option>
                      ))}
                    </select>
                    {busy && progress.id === item.id && (
                      <button className="clear" onClick={() => void cancelJob(item.id)}>
                        Batal
                      </button>
                    )}
                    <button
                      className="remove"
                      aria-label="Hapus file"
                      disabled={busy}
                      onClick={() => setItems((old) => old.filter((entry) => entry.id !== item.id))}
                    >
                      ×
                    </button>
                  </article>
                ))}
              </div>
            ) : (
              <div className="empty">
                <span>▤</span>
                <strong>Antrean kamu kosong</strong>
                <p>Tambahkan file {activeLabel?.label.toLowerCase()} untuk mulai.</p>
              </div>
            )}
            <footer>
              <div className="quality">
                <span>✳</span>
                <div>
                  <strong>Kualitas diutamakan</strong>
                  <p>Setiap konversi menyertakan laporan kualitas yang transparan.</p>
                </div>
              </div>
              <button
                className="convert"
                disabled={
                  !visibleItems.length ||
                  busy ||
                  visibleItems.some((item) => item.status === CHECKING) ||
                  visibleItems.every((item) => !item.target)
                }
                onClick={convert}
              >
                {busy ? `Mengonversi ${progress.current}/${progress.total}` : "Konversi file"}{" "}
                <span>{busy ? "…" : "→"}</span>
              </button>
            </footer>
          </>
        )}
      </section>
    </main>
  );
}
export default App;
