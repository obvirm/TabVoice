# TabVoice

Voice-to-text typing tool ala **Whispr Flow** untuk Windows. Tahan hotkey →
ngomong → lepas → hasil transkrip otomatis ngetik di aplikasi yang sedang
fokus (browser, Notepad, IDE, dll).

Powered by [`oxiwhisper`](https://github.com/cool-japan/oxiwhisper) — pure
Rust Whisper inference engine. GUI pakai [iced](https://github.com/iced-rs/iced)
(Elm-inspired immediate mode GUI). Tidak butuh WebView2 runtime.

## ✨ Fitur

- 🎙️ **Push-to-talk** dengan global hotkey `Ctrl+Shift+Space`
- 📋 **Auto-paste** hasil transkrip ke window aktif via Win32 `SendInput(Ctrl+V)`
- 🎨 **Floating Whispr Flow-style pill**: blinking vertical caret + live audio
  waveform + real-time text
- 🪟 **System tray icon** dengan context menu (Settings, Reload Model, Quit)
- 🔒 **100% offline** — semua inference lokal, tidak ada data ke cloud
- ⚡ **Pure Rust** (no C/C++ deps untuk GUI; oxiwhisper zero C/C++ deps)
- 📦 **Single binary** ~24 MB, tidak butuh runtime tambahan

## 📦 Prasyarat

1. **OS**: Windows 10/11 (64-bit)
2. **Rust toolchain** ≥ 1.77 ([install via rustup](https://rustup.rs/))
3. **Whisper model** dalam format GGML. Download dari
   [ggerganov/whisper.cpp](https://huggingface.co/ggerganov/whisper.cpp/tree/main)
   atau sumber lain. Rekomendasi:
   - `ggml-tiny.bin` (~75 MB) — paling cepat, akurasi rendah
   - `ggml-base.bin` (~142 MB) — **default**, keseimbangan kecepatan/akurasi
   - `ggml-small.bin` (~466 MB) — lebih akurat, lebih lambat
   - `ggml-medium.bin` (~1.5 GB) — paling akurat (butuh RAM cukup)

   Letakkan model di `E:\project\TabVoice\models\ggml-base.bin` atau edit
   path di settings nanti.

## 🏗️ Build

```powershell
cd E:\project\TabVoice
cargo build --release --manifest-path src/Cargo.toml
```

Binary output: `E:\project\TabVoice\target\release\tabvoice.exe`

Atau langsung via workspace:

```powershell
cd E:\project\TabVoice
cargo build --release -p tabvoice
```

Untuk development build (lebih cepat compile):

```powershell
cargo build --manifest-path src/Cargo.toml
```

## 🚀 Jalankan

```powershell
E:\project\TabVoice\target\release\tabvoice.exe
```

Tray icon akan muncul. App **tidak menampilkan window utama** — yang ada
hanya floating pill yang muncul saat Anda tekan hotkey.

## 🎯 Cara pakai

1. **Klik window** tempat Anda ingin teks muncul (mis. address bar browser,
   Notepad, IDE, dll).
2. **Tahan `Ctrl+Shift+Space`** → floating pill muncul di tengah bawah layar
   dengan blinking caret dan live waveform.
3. **Ngomong** dengan jelas → pill menampilkan waveform yang berdenyut
   mengikuti suara Anda.
4. **Lepas hotkey** → pill ganti spinner (inference Whisper, ~1-3 detik).
5. **Hasil transkrip otomatis ngetik** di window yang tadi Anda klik.
6. Pill fade out 1.5 detik lalu hilang.

## 🪟 System tray menu

Klik kanan icon TabVoice di system tray:

- **Toggle Pill** — munculkan/hilangkan pill
- **Settings** — (planned, Phase 6 follow-up)
- **Quit** — keluar dari aplikasi

## ⚙️ Settings

Settings disimpan di `%APPDATA%\TabVoice\settings.toml`. Format:

```toml
model_path = "models/ggml-base.bin"
language = "auto"           # atau "en", "id", "ja", dll.
hotkey = "Ctrl+Shift+Space"
paste_on_release = true
```

Field `language` bisa `None` (auto-detect dari audio) atau BCP-47 code
seperti `"en"`, `"id"`, `"ja"`. Lihat
[supported languages](https://github.com/openai/whisper#available-models-and-languages).

## 🏛️ Arsitektur

```
E:\project\TabVoice\
├── Cargo.toml              # workspace manifest
├── .gitignore
├── src\                    # TabVoice Rust crate
│   ├── Cargo.toml
│   └── src\
│       ├── main.rs         # entry point + orchestration
│       ├── lib.rs          # module declarations
│       ├── app.rs          # iced state + update + view + subscription
│       ├── view.rs         # Whispr Flow pill rendering
│       ├── events.rs       # AppEvent enum + mpsc channel
│       ├── state.rs        # AppState lintas thread
│       ├── audio.rs        # MicCapture via cpal + rubato resample
│       ├── hotkey.rs       # global-hotkey (Ctrl+Shift+Space)
│       ├── transcriber.rs  # oxiwhisper wrapper + tokio worker
│       ├── paste.rs        # arboard clipboard + Win32 SendInput Ctrl+V
│       ├── settings.rs     # TOML config load/save
│       └── tray.rs         # Win32 Shell_NotifyIconW system tray
├── oxiwhisper\             # TIDAK di-include — pakai versi crates.io
└── (referensi lihat crates.io untuk oxiwhisper)
```

### Alur push-to-talk

```
[User tahan Ctrl+Shift+Space]
        ↓
GlobalHotKeyEvent → tabvoice-hotkey thread (hotkey.rs)
        ↓
   start MicCapture (audio.rs)
   ├─ cpal stream → cpal callback (real-time audio thread)
   ├─ mono mixdown + rubato resample 48k→16k mono
   └─ extend recorder.samples + emit AppEvent::Amplitude
        ↓
iced Tick 60fps → drain event_rx (app.rs)
   └─ push to amp_history → view() renders waveform
        ↓
[User lepas Ctrl+Shift+Space]
        ↓
   stop MicCapture (drop = stop stream)
   clone samples, send to release_tx
        ↓
tokio runtime (transcriber.rs run_loop)
   ├─ spawn_blocking → model.transcribe(&samples, &opts)
   ├─ paste_text(&text) (paste.rs → clipboard + SendInput Ctrl+V)
   └─ emit AppEvent::Done { text }
        ↓
iced Tick → view() renders text → fade out 1.5s
```

## 🛠️ Tech stack

| Layer | Crate | Versi |
|---|---|---|
| GUI | `iced` | 0.13 |
| Audio capture | `cpal` | 0.15 |
| Resampling | `rubato` | 0.16 |
| Whisper inference | `oxiwhisper` | 0.1.1 (crates.io) |
| Global hotkey | `global-hotkey` | 0.6 |
| Auto-paste | `arboard` + `windows` | 3 / 0.58 |
| System tray | `windows` | 0.58 |
| Async runtime | `tokio` | 1 (multi-thread) |
| Config | `toml` + `serde` | 0.8 / 1 |

## 🐛 Troubleshooting

### "Gagal load model"
Pastikan path di `settings.toml` benar. Default: `models/ggml-base.bin`
relative terhadap working directory.

### Pill tidak muncul saat hotkey ditekan
1. Cek tray icon — ada error log? (buka via Windows Event Viewer atau jalankan
   dari terminal untuk lihat stderr)
2. Pastikan mic permission diberikan ke Windows
3. Coba ganti hotkey di `settings.toml` (mungkin konflik dengan app lain)

### Auto-paste tidak jalan
1. Pastikan window tujuan punya **fokus** (klik dulu sebelum tahan hotkey)
2. Cek apakah app elevated — kalau TabVoice jalan sebagai admin tapi target
   window tidak, UIPI block. Solusi: manifest `requestedExecutionLevel="asInvoker"`
3. Beberapa app (mis. terminal tertentu) tidak terima Ctrl+V — pakai cara input
   lain atau paste manual

### Model loading lambat
Normal untuk model besar. `ggml-base.bin` Q4_0 ~80 MB butuh ~2-3 detik.
Model lebih besar bisa 10+ detik.

### Tray icon tidak hilang saat Quit
Kadang Windows delay update tray. Tunggu 5-10 detik, atau klik "Refresh"
di tray overflow.

## 📝 Lisensi

MIT untuk kode TabVoice. Lihat [LICENSE](LICENSE) untuk detail.

Library pihak ketiga mengikuti lisensi masing-masing (lihat `Cargo.lock`).
