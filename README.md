# documorph

Konverter dokumen desktop offline untuk Windows (Tauri 2 + React + TypeScript).
Semua file diproses lokal; tidak ada upload, telemetry, atau pembaruan otomatis.

## Prasyarat

- Windows 10/11 x64.
- Node.js **24.21.0+** dan npm **11.19.0+** (lihat field `engines` di `package.json`).
- Rust **1.98.1** (dipin di `rust-toolchain.toml`) dengan komponen `rustfmt`, `clippy`.
- WebView2 Runtime (installer mengunduh bootstrapper secara silent jika belum ada;
  butuh internet sekali saat instalasi, proses konversi tetap offline).
- Engine konversi yang sesuai rute (tabel di bawah). Engine **tidak dicari di PATH**.

## Build & test

```powershell
npm ci
npm run typecheck
npm test
npm run audit
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
npm run tauri dev      # development
npm run tauri build    # NSIS + MSI di src-tauri/target/release/bundle
```

Gunakan perintah `npm run tauri ...` (Windows), bukan `cargo tauri`.
CI menjalankan gate yang sama di `.github/workflows/ci.yml`.

## Matriks konversi

Rute hanya ditawarkan di UI jika engine yang diperlukan terdeteksi, dan tetap
divalidasi ulang di backend sebelum proses dijalankan.

| Tab | Input | Output | Engine | Tier |
| --- | --- | --- | --- | --- |
| Office ↔ PDF | DOC/DOCX/ODT/RTF | PDF | LibreOffice | A |
| Office ↔ PDF | PDF | DOCX | pdf2docx → (fallback AnyMD + Pandoc) | B |
| Office ↔ PDF | PDF | DOCX/TXT/HTML/ODT/RTF | LibreOffice (PDF import) | C |
| Any → Markdown | PDF, Office, Markdown, HTML, TXT, EPUB, TeX, dll. | Markdown | AnyMD | B |
| Alat PDF | PDF | Searchable PDF / OCR Text | OCRmyPDF + Tesseract + Ghostscript | B/C |
| Format lain | Markdown/HTML/TXT/TeX/EPUB/DOCX/ODT/RTF | DOCX/Markdown/HTML/TXT/ODT/RTF | Pandoc | B |
| Format lain | Markdown/HTML/TXT/TeX/EPUB | PDF | Pandoc + Typst | B |

Tier A/B/C adalah indikasi risiko perubahan layout; laporan per konversi
menjelaskan keterbatasan rute yang dipilih.

## Deteksi engine

Kandidat diperiksa dari lokasi eksplisit (bukan PATH): env var per engine
(`ANYMD_PATH`, `PDF2DOCX_PATH`, `PANDOC_PATH`, `LIBREOFFICE_PATH`, `TYPST_PATH`,
`OCRMYPDF_PATH`, `TESSERACT_PATH`, `GHOSTSCRIPT_PATH`), Program Files,
`%LOCALAPPDATA%`, paket WinGet, shim npm global di `%APPDATA%\npm`, dan
`Python\Python*\Scripts`.

- Install Python portable: set `PYTHON_SCRIPTS_PATH` ke folder `Scripts`.
- Shim `.cmd` npm memakai `node.exe` dari PATH pengguna (keterbatasan shim npm),
  tetapi binary engine tetap dipilih dari kandidat eksplisit dan diverifikasi
  lewat `--version`/`--help` sebelum dipakai.
- Tombol **Perbarui engine** memindai ulang tanpa restart aplikasi.
- Klik **Buka hasil** / **Tampilkan di folder** hanya menerima path hasil
  konversi yang masih tercatat di riwayat lokal dan berekstensi keluaran yang
  dikenal.
- Installer mendaftarkan asosiasi `.pdf`; membuka PDF dari Explorer lewat
  **Open with → documorph** akan memasukkan file ke antrean. Argumen dari
  Explorer difilter ke ekstensi dokumen yang dikenal (exe/dll/direktori ditolak).

## Batas aman & perilaku

- Input maksimal 500 MB (peringatan mulai 100 MB); RTF hanya dibaca 4 KB
  pertama; PDF maksimal 200 halaman / 200.000 karakter teks; total XML Office
  maksimal 64 MB dengan batas entri dan ukuran per entri.
- File keluaran dibuat di folder data aplikasi (`output/job-*`), bukan di
  sebelah file input, sehingga tidak ada penimpaan file milik user.
- Cancel per job: membatalkan satu konversi tidak membatalkan job lain; tombol
  **Batalkan antrean** menghentikan semua job aktif.
- Drag-drop difilter di backend: direktori dan ekstensi yang tidak diterima tab
  ditolak sebelum masuk antrean.
- Riwayat disimpan di SQLite WAL dengan `user_version`/migrasi skema. Warning
  preflight ikut tersimpan dan ditampilkan kembali di tab **Riwayat**.
- Seluruh UI berbahasa Indonesia; nomor versi dibaca dari metadata Tauri
  (bukan hardcode) dan disamakan dengan `package.json` / `tauri.conf.json`.
- Pesan error engine dibersihkan: satu baris, terpotong, path internal disamarkan
  sebelum ditampilkan di UI.
- Tombol **Hapus riwayat** butuh konfirmasi klik kedua; tidak ada `window.alert`.

## Catatan produksi

- Installer saat ini **unsigned**. Code signing sengaja tidak diaktifkan sampai
  ada sertifikat + timestamp URL; untuk rilis publik tambahkan
  `bundle.windows.certificateThumbprint` dan `timestampUrl` lewat secret CI.
- Updater tidak diaktifkan sampai ada keypair + endpoint HTTPS.
- WebView2 di-install via `downloadBootstrapper` (silent).
- PDFium di-pin ke SHA-256
  `71A3E63E6AB8F5746A0FC9DE9BC3D0C0DC07EB60990DFD37D9DB7FC891821943`
  (versi 159.0.8138.0). DLL dari path build-machine hanya dipakai di `debug`
  dan tetap diverifikasi hash-nya; rilis hanya memuat DLL dari resource bundle.
- Lisensi pihak ketiga ikut ter-packaging di folder `licenses/`.
- `wix.upgradeCode` dan `webviewInstallMode` sudah dikonfigurasi; target bundle
  hanya `nsis` dan `msi`.
