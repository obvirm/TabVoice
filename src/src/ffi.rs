//! Direct FFI wrapper untuk whisper.cpp.
//!
//! Mirror dari oxiwhisper::WhisperModel API supaya rewrite di tempat lain minimal.
//! Semua panggilan ke whisper.cpp lewat binding `bindgen` yang di-generate
//! di build.rs → `$OUT_DIR/whisper_bindings.rs`.

#![allow(non_camel_case_types, non_snake_case, dead_code, non_upper_case_globals)]

use std::ffi::{CStr, CString};
use std::fmt;
use std::path::Path;
use std::ptr::NonNull;

mod bindings {
    include!(concat!(env!("OUT_DIR"), "/whisper_bindings.rs"));
}

/// Error type untuk semua operasi FFI whisper.
#[derive(Debug)]
pub enum WhisperError {
    /// `whisper_init_from_file_with_params` return null (file hilang / korup).
    InitFailed,
    /// `whisper_full` return non-zero.
    InferenceFailed(i32),
    /// Segment text / language string bukan UTF-8 valid.
    Utf8,
    /// Path model mengandung interior NUL.
    PathNul,
    /// Language / initial_prompt string mengandung interior NUL.
    StringNul,
}

impl fmt::Display for WhisperError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WhisperError::InitFailed => {
                write!(f, "whisper context init returned null (file not found or invalid)")
            }
            WhisperError::InferenceFailed(rc) => {
                write!(f, "whisper_full returned code: {rc}")
            }
            WhisperError::Utf8 => write!(f, "invalid UTF-8 in segment text"),
            WhisperError::PathNul => write!(f, "path has interior NUL"),
            WhisperError::StringNul => {
                write!(f, "language/prompt string has interior NUL")
            }
        }
    }
}

impl std::error::Error for WhisperError {}

/// Opsi untuk satu kali inference. Default = greedy, auto-detect language.
#[derive(Debug, Clone)]
pub struct WhisperOptions {
    /// BCP-47 language hint (mis. `"en"`, `"id"`). `None` = auto-detect.
    pub language: Option<String>,
    /// Beam width untuk beam search decoding. `None` atau `1` = greedy.
    pub beam_width: Option<i32>,
    /// Sampling temperature. `0.0` = default whisper.cpp.
    pub temperature: f32,
    /// Emit segment timestamps. `true` = isi `Segment.start` / `end`.
    pub timestamps: bool,
    /// Initial prompt (kosongkan untuk auto).
    pub initial_prompt: Option<String>,
    /// Jumlah thread CPU untuk inference. `0` = gunakan default whisper.cpp.
    pub n_threads: i32,
}

impl Default for WhisperOptions {
    fn default() -> Self {
        let max_threads = std::thread::available_parallelism().map(|n| n.get() as i32).unwrap_or(4);
        let optimal_threads = (max_threads / 2).max(1).min(8);
        Self {
            language: None,
            beam_width: Some(1),
            temperature: 0.0,
            timestamps: false,
            initial_prompt: None,
            // Menggunakan physical core count (approx: logical / 2) lebih efisien 
            // daripada memaksa semua logical core (hyperthreading justru menurunkan performa GGML)
            n_threads: optimal_threads,
        }
    }
}

/// Satu segment hasil transkripsi (kalimat / potongan kalimat).
#[derive(Debug, Clone)]
pub struct Segment {
    /// Teks segment.
    pub text: String,
    /// Start time dalam detik.
    pub start: f32,
    /// End time dalam detik.
    pub end: f32,
    /// Rata-rata token probability (0.0 - 1.0). `0.0` kalau tidak ada token.
    pub confidence: f32,
    /// `true` kalau no-speech probability > 0.6 (kemungkinan noise/hallucination).
    pub is_hallucination: bool,
}

/// Hasil lengkap satu kali transkripsi.
#[derive(Debug, Clone)]
pub struct TranscribeResult {
    /// Gabungan semua segment text.
    pub text: String,
    pub segments: Vec<Segment>,
    /// Bahasa yang terdeteksi (kalau ada), atau sesuai hint kalau di-supply.
    pub language: Option<String>,
}

/// Owning wrapper untuk `*mut whisper_context`. `Drop` akan panggil
/// `whisper_free` agar memori model di-release.
pub struct WhisperModel {
    ctx: NonNull<bindings::whisper_context>,
}

impl WhisperModel {
    /// Load model dari file GGML. Return `Err(InitFailed)` kalau path
    /// tidak ada atau file korup.
    pub fn from_file(path: &Path) -> Result<Self, WhisperError> {
        let cpath = CString::new(path.to_str().ok_or(WhisperError::Utf8)?)
            .map_err(|_| WhisperError::PathNul)?;
        let params = unsafe { bindings::whisper_context_default_params() };
        let ctx = unsafe {
            bindings::whisper_init_from_file_with_params(cpath.as_ptr(), params)
        };
        NonNull::new(ctx)
            .map(|ctx| WhisperModel { ctx })
            .ok_or(WhisperError::InitFailed)
    }

    /// Load model dari memory buffer. Return `Err(InitFailed)` kalau
    /// buffer tidak valid.
    pub fn from_buffer(buffer: &[u8]) -> Result<Self, WhisperError> {
        let params = unsafe { bindings::whisper_context_default_params() };
        let ctx = unsafe {
            bindings::whisper_init_from_buffer_with_params(
                buffer.as_ptr() as *mut std::ffi::c_void,
                buffer.len(),
                params,
            )
        };
        NonNull::new(ctx)
            .map(|ctx| WhisperModel { ctx })
            .ok_or(WhisperError::InitFailed)
    }

    /// Quick helper: cuma ambil text hasil (tanpa segment detail).
    pub fn transcribe(
        &self,
        audio: &[f32],
        opts: &WhisperOptions,
    ) -> Result<String, WhisperError> {
        let result = self.transcribe_full(audio, opts)?;
        Ok(result.text)
    }

    /// Full inference: jalankan whisper_full, baca segment, hitung confidence.
    pub fn transcribe_full(
        &self,
        audio: &[f32],
        opts: &WhisperOptions,
    ) -> Result<TranscribeResult, WhisperError> {
        let strategy = if opts.beam_width.unwrap_or(1) > 1 {
            bindings::whisper_sampling_strategy_WHISPER_SAMPLING_BEAM_SEARCH
        } else {
            bindings::whisper_sampling_strategy_WHISPER_SAMPLING_GREEDY
        };

        // whisper_full_params di-pass by value — bindgen menghasilkan struct
        // dengan layout yang cocok dengan C. Field-field callback kita biarkan
        // null (default whisper_full_default_params()).
        let mut wparams = unsafe { bindings::whisper_full_default_params(strategy) };
        wparams.n_threads = if opts.n_threads > 0 {
            opts.n_threads
        } else {
            wparams.n_threads
        };
        wparams.translate = false;
        wparams.no_context = true;
        wparams.no_timestamps = !opts.timestamps;
        wparams.print_realtime = false;
        wparams.print_progress = false;
        wparams.print_timestamps = false;
        wparams.print_special = false;
        wparams.temperature = opts.temperature;
        wparams.entropy_thold = 2.4;
        wparams.suppress_blank = true;
        wparams.suppress_nst = true;
        wparams.no_speech_thold = 0.6;

        if let Some(beam) = opts.beam_width {
            wparams.beam_search.beam_size = beam;
        }

        let lang_c = match opts.language.as_deref() {
            Some(l) => Some(CString::new(l).map_err(|_| WhisperError::StringNul)?),
            None => None,
        };
        wparams.language = lang_c
            .as_ref()
            .map(|c| c.as_ptr())
            .unwrap_or(std::ptr::null());

        let prompt_c = match opts.initial_prompt.as_deref() {
            Some(p) => Some(CString::new(p).map_err(|_| WhisperError::StringNul)?),
            None => None,
        };
        wparams.initial_prompt = prompt_c
            .as_ref()
            .map(|c| c.as_ptr())
            .unwrap_or(std::ptr::null());

        let rc = unsafe {
            bindings::whisper_full(
                self.ctx.as_ptr(),
                wparams,
                audio.as_ptr(),
                audio.len() as std::os::raw::c_int,
            )
        };
        if rc != 0 {
            return Err(WhisperError::InferenceFailed(rc));
        }

        let n = unsafe { bindings::whisper_full_n_segments(self.ctx.as_ptr()) };
        let n_usize = n.max(0) as usize;
        let mut segments = Vec::with_capacity(n_usize);
        let mut full_text = String::new();

        for i in 0..n {
            let c_text =
                unsafe { bindings::whisper_full_get_segment_text(self.ctx.as_ptr(), i) };
            let text = unsafe { cstr_to_string(c_text)? };

            let t0_cs =
                unsafe { bindings::whisper_full_get_segment_t0(self.ctx.as_ptr(), i) };
            let t1_cs =
                unsafe { bindings::whisper_full_get_segment_t1(self.ctx.as_ptr(), i) };
            let no_sp = unsafe {
                bindings::whisper_full_get_segment_no_speech_prob(self.ctx.as_ptr(), i)
            };

            let n_tokens =
                unsafe { bindings::whisper_full_n_tokens(self.ctx.as_ptr(), i) };
            let mut sum_p = 0.0_f32;
            for j in 0..n_tokens {
                sum_p += unsafe {
                    bindings::whisper_full_get_token_p(self.ctx.as_ptr(), i, j)
                };
            }
            let confidence = if n_tokens > 0 {
                sum_p / n_tokens as f32
            } else {
                0.0
            };

            // Centiseconds -> detik
            let start = t0_cs as f32 * 0.01;
            let end = t1_cs as f32 * 0.01;
            let is_hallu = no_sp > 0.6;

            segments.push(Segment {
                text: text.clone(),
                start,
                end,
                confidence,
                is_hallucination: is_hallu,
            });
            if !is_hallu {
                full_text.push_str(&text);
            }
        }

        let detected_lang = unsafe {
            let id = bindings::whisper_full_lang_id(self.ctx.as_ptr());
            if id >= 0 {
                let s = bindings::whisper_lang_str(id);
                cstr_to_string(s).ok()
            } else {
                None
            }
        };
        let language = opts.language.clone().or(detected_lang);

        Ok(TranscribeResult {
            text: full_text,
            segments,
            language,
        })
    }

    /// String representasi tipe model (mis. `"tiny"`, `"base"`, `"small.en"`).
    pub fn model_type_readable(&self) -> String {
        unsafe {
            let p = bindings::whisper_model_type_readable(self.ctx.as_ptr());
            cstr_to_string(p).unwrap_or_default()
        }
    }

    /// `true` kalau model multilingual (bukan `.en`).
    pub fn is_multilingual(&self) -> bool {
        unsafe { bindings::whisper_is_multilingual(self.ctx.as_ptr()) != 0 }
    }

    /// Bundle info model untuk logging / UI.
    pub fn info(&self) -> ModelInfo {
        ModelInfo {
            model_type: self.model_type_readable(),
            multilingual: self.is_multilingual(),
        }
    }
}

impl Drop for WhisperModel {
    fn drop(&mut self) {
        unsafe { bindings::whisper_free(self.ctx.as_ptr()) };
    }
}

/// Bundle info model untuk logging / UI.
#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub model_type: String,
    pub multilingual: bool,
}

// Send + Sync: whisper.cpp context TIDAK thread-safe per whisper.h docs
// ("thread-safe as long as the sample whisper_context is not used by
// multiple threads concurrently"). Kita jamin ini dengan semua akses
// ke `WhisperModel` selalu di dalam `Mutex` di `AppState`, dan worker
// transcriber spawn_blocking yang join kembali ke single thread owner.
// Karena itu, kita nyatakan `Sync` secara manual — SAFETY: caller
// wajib serialisasi akses lewat Mutex, JANGAN panggil `transcribe_full`
// dari dua thread secara paralel.
unsafe impl Send for WhisperModel {}
unsafe impl Sync for WhisperModel {}

/// Konversi `*const c_char` dari whisper.cpp ke `String` Rust. Null pointer
/// diperlakukan sebagai string kosong (whisper_lang_str bisa return null).
pub(crate) fn c_char_to_string(c: *const std::ffi::c_char) -> String {
    if c.is_null() {
        return String::new();
    }
    let cstr = unsafe { std::ffi::CStr::from_ptr(c) };
    cstr.to_string_lossy().into_owned()
}

/// Mendapatkan daftar semua bahasa yang didukung oleh whisper.cpp.
/// Mengembalikan daftar tupel: (kode_bahasa, nama_bahasa).
pub fn get_supported_languages() -> Vec<(String, String)> {
    let mut langs = Vec::new();
    unsafe {
        let max_id = bindings::whisper_lang_max_id();
        for id in 0..=max_id {
            let str_ptr = bindings::whisper_lang_str(id);
            let full_ptr = bindings::whisper_lang_str_full(id);
            if !str_ptr.is_null() && !full_ptr.is_null() {
                let code = std::ffi::CStr::from_ptr(str_ptr).to_string_lossy().to_string();
                let name = std::ffi::CStr::from_ptr(full_ptr).to_string_lossy().to_string();
                
                // Jangan masukkan "auto" di loop, kita taruh di atas nanti
                if code != "auto" {
                    // Capitalize the first letter
                    let name = if name.is_empty() {
                        name
                    } else {
                        let mut c = name.chars();
                        match c.next() {
                            None => String::new(),
                            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                        }
                    };
                    langs.push((code, name));
                }
            }
        }
    }
    
    // Sort alphabetically by name
    langs.sort_by(|a, b| a.1.cmp(&b.1));
    
    // Taruh auto detect di paling atas
    langs.insert(0, ("auto".to_string(), "Auto Detect".to_string()));
    
    langs
}

unsafe fn cstr_to_string(p: *const std::os::raw::c_char) -> Result<String, WhisperError> {
    if p.is_null() {
        return Ok(String::new());
    }
    unsafe {
        CStr::from_ptr(p)
            .to_str()
            .map(str::to_owned)
            .map_err(|_| WhisperError::Utf8)
    }
}
