# Tauri + React + Typescript

This template should help get you started developing with Tauri, React and Typescript in Vite.

## Recommended IDE Setup

- [VS Code](https://code.visualstudio.com/) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)

## Production notes

- Installer yang dihasilkan saat ini **unsigned**. Code signing sengaja tidak
  diaktifkan sampai ada sertifikat + timestamp URL. Untuk rilis publik,
  tambahkan `bundle.windows.certificateThumbprint` dan `timestampUrl` lewat
  secret CI. Updater juga tidak diaktifkan sampai ada keypair + endpoint HTTPS.
- WebView2 di-install via `downloadBootstrapper` (silent), jadi butuh internet
  sekali saat instalasi jika runtime belum ada. Proses konversi tetap offline.
- PDFium di-**pin** ke SHA-256 `71A3E63E6AB8F5746A0FC9DE9BC3D0C0DC07EB60990DFD37D9DB7FC891821943`
  (versi 159.0.8138.0). DLL dari path build-machine hanya dipakai di `debug`
  dan tetap diverifikasi hash-nya; rilis hanya memuat DLL dari resource bundle.
- Lisensi pihak ketiga ikut ter-packaging di folder `licenses/`.
- Engine eksternal **tidak dicari di PATH**. Tentukan eksplisit lewat env var
  opsional `ANYMD_PATH`, `PDF2DOCX_PATH`, `PANDOC_PATH`, `LIBREOFFICE_PATH`,
  `TYPST_PATH`, `OCRMYPDF_PATH`, `TESSERACT_PATH`, `GHOSTSCRIPT_PATH`, atau
  install di lokasi Program Files / `%LOCALAPPDATA%` yang dikenal. Setiap
  binary diverifikasi lewat `--version`/`--help` sebelum dipakai.
- Batas aman preflight: input maksimal 500 MB, peringatan mulai 100 MB,
  RTF hanya dibaca 4 KB pertama, PDF maksimal 200 halaman / 200.000 karakter
  teks, dan total XML Office maksimal 64 MB.
- `open_output` / `reveal_output` hanya menerima path hasil konversi yang masih
  tercatat di riwayat lokal dan berekstensi keluaran yang dikenal.
