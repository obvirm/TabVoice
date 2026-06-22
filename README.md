# TabVoice

TabVoice adalah aplikasi *Push-to-Talk* (PTT) pintar lintas platform berbasis *Global Hotkey* untuk merekam suara dan mengubahnya menjadi teks menggunakan model **Whisper** secara lokal (GPU-accelerated), lalu menempelkan (*auto-paste*) teks tersebut langsung ke jendela aplikasi yang sedang aktif.

### Video Showcase
<video src="https://github.com/obvirm/TabVoice/raw/master/media/showcase.mp4" controls="controls" width="100%"></video>

![TabVoice Showcase](media/ss.png)

*Antarmuka TabVoice yang minimalis, siap mendengarkan suara Anda.*

## Fitur Unggulan

- **Push-to-Talk (Global)**: Tekan dan tahan tombol `F4` dari aplikasi manapun untuk mulai merekam.
- **Auto-Paste**: Hasil transkripsi akan otomatis ditempel (*paste*) ke kolom input yang sedang aktif.
- **Model Mandiri (Self-Contained)**: Model AI *Whisper Base* berukuran 74MB sudah di-*embed* langsung ke dalam aplikasi. Tidak ada unduhan tambahan atau file ekstrak yang terpisah! *Plug-and-play*.
- **Ikon Profesional**: Dilengkapi ikon kustom (*branding*) untuk antarmuka pengguna, *Taskbar* Windows, serta *Dock* macOS dan Linux.
- **UI Minimalis & Modern**: Antarmuka transparan dan melayang (floating) yang tidak mengganggu pekerjaan Anda.

## Cara Instalasi

Aplikasi ini bersifat **Plug-and-Play**. Anda tidak perlu mengunduh file model secara terpisah. Cukup jalankan file *executable*.

### Untuk Pengguna (Windows)

1. Unduh `tabvoice.exe`.
2. Jalankan `tabvoice.exe`. Anda bisa langsung menggunakan tombol `F4` untuk bicara.

### Build dari Source Code (Cross-Platform)

TabVoice dapat di-*build* untuk **Windows, macOS, dan Linux**:

```bash
cargo build --release
```

*Catatan: Pastikan sistem Anda telah terpasang compiler C++ dan CMake untuk mengompilasi library backend.*

---

## Cara Penggunaan

1. **Jalankan Aplikasi:** Buka `tabvoice.exe` (atau jalankan via `cargo run --release`). 
2. **Pilih Model & Bahasa:** Saat dijalankan, aplikasi akan menampilkan antarmuka utama. Anda juga bisa mengklik kanan ikon TabVoice di *System Tray* (pojok layar), lalu pilih **Settings**. Pilih model yang diinginkan (contoh: *Base* atau *Turbo*) dan bahasa pengenalan yang Anda mau (didukung oleh pustaka Whisper asli).
3. **Mulai Berbicara:** Pindahkan fokus ke *text editor*, aplikasi *chat*, atau kolom teks apa saja. Tahan tombol `Ctrl + Shift + Space` dan mulailah berbicara.
4. **Paste Teks:** Setelah selesai bicara, lepas tombol tersebut. Teks akan segera ditranskrip dan otomatis diketikkan di tempat kursor Anda berada!

---

## Persyaratan Sistem

* **OS:** Windows 10/11, macOS, atau Linux.
* **Perangkat Keras:** GPU NVIDIA (direkomendasikan untuk akselerasi CUDA) atau Apple Silicon untuk performa maksimal.
* **Dependensi Build (Bagi Pengembang):**
  * Rust (1.77+)
  * CMake & C++ Build Tools (Visual Studio Build Tools untuk Windows, Clang untuk macOS/Linux).

## Pengembangan & Arsitektur

Aplikasi ini dikembangkan menggunakan **Rust** dengan pendekatan arsitektur berperforma tinggi (*Low-Latency* & *Zero-Bloat*):

* `egui` & `eframe` — Render antarmuka yang sangat responsif.
* `whisper-cpp` & FFI — Pemanggilan C-bindings (FFI) asli ke *whisper.cpp* untuk inferensi lokal, memastikan penggunaan memori yang efisien.
* `global-hotkey` — Pendeteksian pintasan *Push-to-Talk* global.
* `cpal` & `rubato` — Pengambilan dan _resampling_ audio mikrofon *real-time*.
* `reqwest` & `tokio` — Pengunduhan model secara asinkron (*non-blocking*).
* Ekosistem Lintas Platform — Menggunakan API asli (`windows-rs` untuk Win32) dan pustaka standar lintas platform (`enigo` dan `tray-icon`) di Linux/macOS.

---
**TabVoice** dikerjakan untuk memberikan pengalaman mendikte semulus mungkin tanpa mengorbankan privasi data Anda. Cukup tahan spasi, bicara, dan biarkan AI yang mengetik!
