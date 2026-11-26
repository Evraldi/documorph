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
| Any → Markdown | PDF, Office, Markdown, HTML, TXT, EPUB, TeX, dll. | Markdown | AnyMD | B |
| Alat PDF | PDF | DOCX | PDF scan: Tesseract/OCRmyPDF + Pandoc; digital: pdf2docx → AnyMD + Pandoc → LibreOffice Draw+Writer | B/C |
| Alat PDF | PDF | Searchable PDF / OCR Text | OCRmyPDF + Tesseract + Ghostscript | B/C |
| Format lain | Markdown/HTML/TXT/TeX/EPUB/DOCX/ODT/RTF | DOCX/Markdown/HTML/TXT/ODT/RTF | Pandoc | B |
| Format lain | Markdown/HTML/TXT/TeX/EPUB | PDF | Pandoc + Typst | B |

Tier A/B/C adalah indikasi risiko perubahan layout; laporan per konversi
menjelaskan keterbatasan rute yang dipilih. LibreOffice tidak bisa mengekspor
PDF langsung ke DOCX/ODT/TXT/RTF, jadi rute cadangannya adalah LibreOffice Draw → Writer:
PDF diimpor sebagai HTML lewat Draw lalu dibuka di Writer dengan filter eksplisit.

### Searchable PDF

Rute **Alat PDF → Searchable PDF** mempertahankan piksel scan asli: halaman
dirender pada 300 DPI **apa adanya** untuk dibaca Tesseract, lalu kata hasil
koreksi ditulis sebagai text layer invisible. OCRmyPDF baru dijalankan setelahnya
dengan `--skip-text` (tanpa `--deskew`/`--rotate-pages`) sehingga hanya mengubah
struktur/PDF-A dan tidak mengulang OCR full-page. Urutan ini menghemat satu kali
OCR per halaman dibanding menjalankan OCRmyPDF lebih dulu lalu membuang layer-nya.
Binarisasi, denoise, dan upscale tidak dipakai untuk body text.
Sebelum OCR, orientasi halaman dicek memakai Tesseract OSD (`--psm 0`) pada thumbnail
kecil. Bila terdeteksi 90°/180°/270° dengan confidence memadai, gambar diputar
sebelum OCR dan bbox kata dikembalikan ke koordinat halaman asli saat text layer
ditulis. Ini tidak mengubah piksel PDF dan tidak memakai `--rotate-pages` OCRmyPDF.
karena ketiganya tidak memperbaiki CER pada sample. Upscale hanya dipakai pada
crop judul/font besar outlier, dengan padding supaya huruf di tepi halaman tidak
terpotong. Kandidat OCR ulang judul juga ditolak bila lebih pendek lebih dari satu
karakter (mis. `LinnSequencer` → `e` adalah potongan crop, bukan koreksi).
Setiap kata ditulis kembali sebagai teks render mode 3 (invisible) pada
bbox-nya. Pemisah di dalam text object memakai spasi, bukan `\n`: font WinAnsi
tidak punya glyph newline sehingga PDFium menulis byte `0xFF` yang terbaca
sebagai `ÿ`. Lapisan memakai font TrueType/CID Unicode agar `″` benar-benar
ter-encode sebagai U+2033. Baseline dan ukuran font disamakan per baris supaya
ekstraktor tidak menyisipkan newline di tengah kalimat. Rute **OCR Text** tidak
berubah (`--deskew --rotate-pages --force-ocr --sidecar`).

Koreksi sengaja konservatif supaya body text tidak rusak: kamus hanya menyentuh
token yang tidak dikenal, confidence rendah, edit distance tepat 1, dan
kandidatnya muncul minimal 3 kali sebagai kata benar di dokumen yang sama.
Kandidat frekuensi juga harus berupa kata alpha minimal 3 karakter; token pendek
dan token bertanda baca terbukti salah mengoreksi kata lain pada scan noisy.
Pengecualian eksplisit: `1s` → `is` hanya bila `is` dominan, dan `pedai,` →
`pedal,`. Infleksi/plural tidak diubah. `32` tidak pernah dianggap pecahan;
normalisasi `312”`/`3½ÿ` → `3½″` hanya di konteks "disk drive". Hyphen akhir baris
tetap dua fragmen (`func-`, `tion.`) agar tidak muncul `function tion`. Baris
dibentuk dari `line_num` Tesseract (bukan posisi-x awal); urutan asli dipertahankan
karena pengurutan geometris penuh menaikkan CER body dan CER full (0,0025 →
0,0117). `Additional Features` sudah berada di posisi yang diharapkan ground truth
pada urutan Tesseract asli, jadi tidak dipindah. Helper XY-cut/gap whitespace tetap
dipakai untuk analisis blok dan regresi. Bullet yang
tidak terdeteksi Tesseract disisipkan bila kata pertamanya confidence 0 dan
berada di kolom bullet yang sama dengan item sebelumnya; baris lanjutannya tetap
menempel pada bullet induknya.

Setiap koreksi dicatat di riwayat dengan format `Halaman n: sebelum → sesudah
(rule)`, maksimal 10 entri pertama plus ringkasan sisanya. Semua pass bisa
dimatikan dengan nilai `0`, `false`, `no`, atau `off`:

| Variable | Fungsi |
| --- | --- |
| `DOCUMORPH_OCR_POSTPROCESS` | Matikan/aktifkan seluruh koreksi + penulisan ulang layer |
| `DOCUMORPH_OCR_ROTATION` | Deteksi orientasi Tesseract OSD (`--psm 0`) dan putar gambar sebelum OCR |
| `DOCUMORPH_OCR_DICTIONARY` | Koreksi ejaan sangat konservatif + `1s`→`is` dan `pedai,`→`pedal,` |
| `DOCUMORPH_OCR_FONT_RERUN` | OCR ulang kata judul/font besar pada 600 DPI (default aktif) |
| `DOCUMORPH_OCR_FONT_RERUN_SKIP_SAFE` | Lewati OCR ulang untuk kata umum/merek/angka yang sudah confidence ≥ 90 (default aktif) |
| `DOCUMORPH_OCR_PARALLEL_PAGES` | OCR halaman paralel, maximal 4 worker; otomatis sequential bila ada halaman > 40 MP (default aktif) |
| `DOCUMORPH_OCR_PARALLEL_FONT_RERUN` | OCR ulang judul paralel, maximal 4 worker; hasil di-apply urut supaya deterministik (default aktif) |
| `DOCUMORPH_OCR_LAYOUT` | Pertahankan urutan Tesseract; analisis blok XY-cut + bullet satu blok |
| `DOCUMORPH_OCR_BULLET` | Normalisasi bullet margin kiri + sisipkan bullet yang gagal terdeteksi |
| `DOCUMORPH_OCR_HYPHEN` | Pertahankan dua fragmen hyphen tanpa duplikasi kata |
| `DOCUMORPH_OCR_FRACTION` | `312”`/`3½ÿ` → `3½″` hanya di konteks "disk drive"; `32` tidak disentuh |
| `DOCUMORPH_OCR_RENDER_DPI` | DPI render OCR (minimum 300) |
| `DOCUMORPH_OCR_RAW_BGR` | Render langsung RGB 3-channel tanpa konversi RGBA→RGB (default aktif) |
| `DOCUMORPH_OCR_RERENDER_DPI` | DPI crop untuk OCR ulang font besar outlier |
| `DOCUMORPH_OCR_DICTIONARY_EXTRA` | Kata tambahan yang dikecualikan, dipisah `,` atau `;` |

### PDF → DOCX

PDF scan tanpa text layer tidak lagi dikirim ke pdf2docx/AnyMD yang hanya
menghasilkan gambar. Dua kandidat teks dijalankan: render langsung 300 DPI dengan
Tesseract + koreksi kata yang sama seperti Searchable PDF, dan sidecar OCRmyPDF
(`--deskew --rotate-pages --force-ocr`) yang lebih tahan pada tabel serta halaman
terputar. Confidence rata-rata Tesseract memilih kandidat; pemilih ini cocok dengan
ground truth 11/11 sample. PDF yang sudah punya text layer tetap memakai rute
pdf2docx/AnyMD seperti sebelumnya.

Teks yang dikirim ke Pandoc memakai bbox kata untuk memisahkan paragraf: celah
vertikal besar menjadi `\n\n`, sementara baris dalam satu paragraf digabung
dengan spasi. Kata-katanya tidak diubah, jadi CER/WER tetap sama; yang membaik
adalah DOCX tidak lagi keluar sebagai satu paragraf panjang.

Setiap koreksi kata dicatat dengan format `Halaman n: sebelum → sesudah (rule)`.
Rute OCR bisa dikembalikan ke perilaku lama dengan:

| Variable | Fungsi |
| --- | --- |
| `DOCUMORPH_PDF_DOCX_OCR` | `0` mematikan rute OCR untuk PDF scan (default aktif) |
| `DOCUMORPH_PDF_DOCX_SIDECAR_POSTPROCESS` | `0` mematikan koreksi pada teks sidecar OCRmyPDF (default aktif) |

Teks sidecar juga dilewati pass koreksi yang sama. Karena sidecar tidak punya
confidence dan bbox per kata, word disintesis dengan confidence `95` supaya kamus
hanya memakai koreksi eksplisit; token `0`/`2` dikecualikan dari normalisasi
bullet agar angka di awal baris tabel tidak berubah. Varian aman ini tidak
menurunkan CER ke-11 sample dan memperbaiki `id-linn-noise` dari `0,0099` menjadi
`0,0062`. Opsi baca Pandoc lain (`--wrap=none`, escape Markdown,
`markdown_strict`, `commonmark`) sudah diuji tetapi tidak dipakai karena tidak
memberi perbaikan terukur.

Pass koreksi mengikuti flag `DOCUMORPH_OCR_*` di atas. Engine harus terdeteksi
(Tesseract, Ghostscript, OCRmyPDF, Pandoc); bila auto-discovery gagal, path bisa
diisi lewat `TESSERACT_PATH`, `GHOSTSCRIPT_PATH`, `OCRMYPDF_PATH`, dan
`PANDOC_PATH`.

Tes runtime PDF→DOCX tidak berhenti di teks OCR: hasilnya tetap disusun menjadi
DOCX dengan Pandoc, lalu `word/document.xml` dibaca untuk memeriksa CER, `U+00FF`,
sentinel koreksi, serta kata kunci `Sequencer`, `function`, `is`, dan `3½`.
Batas regresi lama memakai CER dengan denominator hasil; `CER-ref` dengan
denominator ground truth juga di-assert dengan batas terpisah, karena angkanya
lebih tinggi untuk sample tabel.

### Diagnosa akurasi OCR

Ground truth sample disimpan di `tests/fixtures/linn-sequencer.ground-truth.txt`
(diambil dari resource upstream OCRmyPDF, bukan dari hasil OCR aplikasi) dan
`tests/fixtures/linn-sequencer.body-ground-truth.txt` untuk metrik body saja.
Jalankan:

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --lib -- --ignored --nocapture ground_truth_metrics
```

Perintah ini mencetak CER/WER untuk varian preprocessing (`raw`, `gray`,
`binary`, `denoise`, `upscale`) dan untuk setiap langkah koreksi. Tes gagal bila
ada langkah yang menaikkan CER body text lebih dari 0,005, jadi perbaikan yang
merusak body tidak boleh di-merge.

Regresi lintas dokumen menjalankan 11 scan (bersih, noisy, miring, rot90, tabel,
tabel miring, lowres, blur, dua kolom, linn asli, linn+noise) dengan ground truth
dari teks sumber:

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --lib -- --ignored --nocapture cross_document_metrics
```

Setiap tahap juga tidak boleh menaikkan CER body lebih dari 0,005. Tambahkan
`$env:DOCUMORPH_CROSS_FILTER = 'id-noisy'` untuk menjalankan satu dokumen dan
mencetak daftar koreksinya.

Benchmark 5 scan asli (linn, skew, rotated skew, grafik, Prancis) mencetak
jumlah kata, koreksi, dan detik per halaman untuk tahap rewrite layer:

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --lib -- --ignored --nocapture real_scan_benchmark
```

Daftar PDF bisa diganti lewat `DOCUMORPH_REAL_SCANS` (path dipisah `;`).
Tambahkan `$env:DOCUMORPH_TIMING = '1'` untuk memecah waktu rewrite menjadi
render, konversi RGBA, OSD, OCR full-page, OCR ulang judul, penulisan objek
teks, regenerate, dan save. Tambahkan `$env:DOCUMORPH_BENCH_OCRMYPDF = '1'` untuk
menjalankan OCRmyPDF `--skip-text` setelah rewrite dan mencetak total pipeline;
angka waktu per halaman = rewrite + OCRmyPDF.

Penulisan layer sengaja memakai
`PdfPageContentRegenerationStrategy::Manual`: default `pdfium-render`
meng-regenerate content stream scan 300 DPI setelah setiap objek kata, yang
membuat biaya tumbuh linier dengan jumlah kata. Semua objek ditulis dulu, lalu
content diregenerate sekali per halaman.

Halaman yang terdeteksi terbalik diverifikasi terpisah:

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --lib -- --ignored rotated_pdf_text_layer
```

Tes ini memastikan text layer mengikuti piksel scan asli (bukan gambar hasil
rotasi): kata-kata judul pada halaman 90° punya `left` yang sama dan `top`
berurutan.

Tes runtime ignored juga memeriksa hasil ekstraksi `pdftotext` dan PyMuPDF: tidak
boleh ada `U+00FF`, tanda inci harus terbaca `U+2033`, dan pencarian PDFium untuk
`Sequencer`, `3½`, `function`, serta `is` harus tetap menemukan hasil. Setel
`DOCUMORPH_PDFTOTEXT` / `DOCUMORPH_PYTHON` bila letak keduanya berbeda.

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
  Penghapusan juga menghapus semua file hasil yang masih tercatat di riwayat,
  setelah pathnya divalidasi lewat `output_guard` (file asli di luar riwayat
  tidak disentuh).
- **PDF → DOCX** hanya ditawarkan di tab **Alat PDF**, bukan dobel di
  **Office ↔ PDF**.

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
- `cargo audit` dijalankan di CI. Lockfile menjaga `zip` ≥ 9, `cookie` ≥ 0.18.3,
  `url` ≥ 2.5.9, dan `rusqlite` ≥ 0.41 (`libsqlite3-sys` ≥ 0.39 / SQLite 3.54.1)
  sesuai advisory 2026; test config menolak regresi ke versi lama.
