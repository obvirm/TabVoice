//! Wrapper di sekitar `crate::ffi::WhisperModel` (whisper.cpp FFI) + tokio worker loop.
//!
//! Worker baca dari `tokio::sync::mpsc::UnboundedReceiver<Vec<f32>>` (samples
//! 16 kHz mono f32), jalankan inference CPU-bound di `tokio::task::spawn_blocking`,
//! lalu emit `AppEvent::Done` (paste otomatis di Phase 4).
//!
//! Audio dianggap "no speech" kalau panjang < 0.3 detik (4800 samples @16kHz).

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedReceiver;

use crate::events::{AppEvent, EventSender};
use crate::ffi::{WhisperError, WhisperModel, WhisperOptions};

/// Ambang minimal durasi audio (samples @16 kHz mono). 0.3 detik = 4800 samples.
const MIN_SAMPLES: usize = 4_800;

/// Input untuk Transcriber: Parsial atau Final.
pub enum TranscriberInput {
    /// Data audio sementara saat recording (untuk visualisasi).
    Partial(Vec<f32>),
    /// Data audio final saat hotkey dilepas (untuk ditranskrip).
    Final(Vec<f32>),
}

/// Wrapper tipis di atas `WhisperModel` agar bisa di-share `Arc` ke worker thread.
pub struct Transcriber {
    /// BCP-47 language hint (mis. `"en"`, `"id"`). `None` = auto-detect.
    language: Option<String>,
    /// State aplikasi untuk setting realtime & paste
    state: Arc<crate::state::AppState>,
}

impl Transcriber {
    /// Buat `Transcriber` baru.
    pub fn new(language: Option<String>, state: Arc<crate::state::AppState>) -> Self {
        Self { language, state }
    }

    /// Jalankan inference langsung tanpa spawn ke blocking thread.
    /// Berguna untuk unit test atau pemanggilan sync.
    pub fn transcribe_blocking(&self, audio: &[f32]) -> Result<String, WhisperError> {
        let opts = WhisperOptions {
            language: self.language.clone(),
            timestamps: false,
            ..WhisperOptions::default()
        };
        let m_opt = self.state.model.lock().unwrap().clone();
        if let Some(m) = m_opt {
            m.transcribe(audio, &opts)
        } else {
            Err(WhisperError::InitFailed)
        }
    }

    /// Worker loop async: terima batch samples, transcribe, emit event.
    ///
    /// Loop berakhir ketika `release_rx` di-close (semua `Sender` di-drop).
    /// Inference berjalan di `spawn_blocking` agar tidak block runtime tokio.
    pub async fn run_loop(
        self,
        mut release_rx: UnboundedReceiver<TranscriberInput>,
        event_tx: EventSender,
    ) {
        log::info!(
            "Transcriber worker started (language={:?})",
            self.language
        );

        let idle_timeout = tokio::time::Duration::from_secs(15 * 60);

        loop {
            let memory_mode = self.state.settings.lock().unwrap().memory_mode;
            let recv_res = match memory_mode {
                crate::settings::MemoryMode::EcoMode => {
                    tokio::time::timeout(idle_timeout, release_rx.recv()).await
                }
                _ => {
                    Ok(release_rx.recv().await)
                }
            };

            let mut input = match recv_res {
                Ok(Some(i)) => i,
                Ok(None) => break, // Channel closed
                Err(_) => {
                    // Timeout (EcoMode)
                    let mut m = self.state.model.lock().unwrap();
                    if m.is_some() {
                        log::info!("EcoMode: 15 minutes idle timeout reached. Unloading model to free RAM.");
                        *m = None;
                    }
                    continue;
                }
            };

            // Kuras antrean: ambil chunk paling baru.
            // Jika ada `Final` di antrean, prioritaskan `Final` dan abaikan `Partial` sebelumnya.
            while let Ok(next_input) = release_rx.try_recv() {
                match next_input {
                    TranscriberInput::Final(s) => {
                        input = TranscriberInput::Final(s);
                        // Jangan kuras lagi kalau sudah ketemu Final
                        break;
                    }
                    TranscriberInput::Partial(s) => {
                        // Update input ke Partial yang paling baru,
                        // asalkan kita belum memegang Final.
                        if let TranscriberInput::Partial(_) = input {
                            input = TranscriberInput::Partial(s);
                        }
                    }
                }
            }

            let (samples, is_partial) = match input {
                TranscriberInput::Partial(s) => (s, true),
                TranscriberInput::Final(s) => (s, false),
            };

            // Skip audio terlalu pendek — kemungkinan noise / tidak ada speech.
            if samples.len() < MIN_SAMPLES {
                if !is_partial {
                    log::debug!(
                        "Skipping short audio chunk: {} samples (< {} = 0.3s)",
                        samples.len(),
                        MIN_SAMPLES
                    );
                    let _ = event_tx.send(AppEvent::Error {
                        message: "No speech detected".into(),
                    });
                }
                continue;
            }

            // Ambil model dari state jika ada. Jika None (lazy load atau di-unload EcoMode), load sekarang!
            let model = {
                let m_opt = self.state.model.lock().unwrap().clone();
                if let Some(current_model) = m_opt {
                    current_model
                } else {
                    let _ = event_tx.send(AppEvent::TranscriptionPartial {
                        text: "Memuat model ke RAM...".into(),
                    });
                    
                    let settings = self.state.settings.lock().unwrap().clone();
                    
                    let new_model = tokio::task::spawn_blocking(move || {
                        const DEFAULT_MODEL_BYTES: &[u8] = include_bytes!("../../models/ggml-base.bin");
                        match crate::ffi::WhisperModel::from_file(&settings.model_path) {
                            Ok(m) => Arc::new(m),
                            Err(_) => Arc::new(crate::ffi::WhisperModel::from_buffer(DEFAULT_MODEL_BYTES).unwrap())
                        }
                    }).await.expect("spawn_blocking failed");
                    
                    *self.state.model.lock().unwrap() = Some(Arc::clone(&new_model));
                    new_model
                }
            };
            let lang = self.language.clone();
            let sample_count = samples.len();

            let vad_threshold = self.state.settings.lock().unwrap().vad_threshold;

            // Sederhana: Hitung RMS (Root Mean Square) volume untuk VAD (Voice Activity Detection)
            // Untuk realtime (partial), kita hanya cek 1 detik terakhir. 
            // Jika user diam, kita tidak ingin RMS dari awal kalimat membuat VAD tetap lolos (yang memicu halusinasi spasi).
            let vad_samples = if is_partial && sample_count > 16_000 {
                &samples[sample_count - 16_000..]
            } else {
                &samples[..]
            };
            
            let sum_sq: f32 = vad_samples.iter().map(|&s| s * s).sum();
            let rms = (sum_sq / vad_samples.len() as f32).sqrt();

            // Jika sangat hening, abaikan langsung, jangan teruskan ke Whisper!
            // Ini untuk mencegah "halusinasi dari keheningan".
            if rms < vad_threshold {
                log::info!("Audio too quiet (RMS: {:.4} < {:.4}), skipping to prevent hallucination", rms, vad_threshold);
                if !is_partial {
                    let _ = event_tx.send(AppEvent::Error {
                        message: "No speech detected (Too quiet)".into(),
                    });
                }
                continue;
            }

            log::info!(
                "Transcribing {sample_count} samples (~{:.2}s) | RMS: {:.4}",
                sample_count as f32 / 16_000.0,
                rms
            );

            // Inference CPU-bound: jalankan di thread pool blocking.
            let join_result = tokio::task::spawn_blocking(move || {
                let opts = WhisperOptions {
                    language: lang,
                    timestamps: false,
                    // LAYER ANTI-HALUSINASI TAMBAHAN: Initial Prompt
                    // Memberikan konteks pada Whisper agar ia "fokus" pada transkripsi yang rapi
                    // dan tidak berhalusinasi mengucapkan frasa "subtitle YouTube"
                    initial_prompt: Some("Berikut ini adalah rekaman suara yang diucapkan dengan jelas dan ditranskripsi tanpa pengulangan kata atau kalimat acak.".to_string()),
                    ..WhisperOptions::default()
                };
                model.transcribe_full(&samples, &opts)
            })
            .await;

            match join_result {
                Ok(Ok(result)) => {
                    let cleaned = clean_non_speech_tokens(&result.text);
                    if cleaned.is_empty() {
                        log::info!("Transcription returned empty text after cleaning non-speech tokens");
                        continue;
                    }

                    if is_hallucination(&cleaned) {
                        if !is_partial {
                            log::warn!("Hallucination/repetition detected: {:?}", cleaned);
                            let _ = event_tx.send(AppEvent::Error {
                                message: "Noise/repetition detected, please try again".into(),
                            });
                        }
                        continue;
                    }

                    // LAYER ANTI-HALUSINASI TAMBAHAN: Token Confidence
                    let mut avg_confidence = 0.0;
                    let mut valid_segments = 0;
                    for seg in &result.segments {
                        if !seg.is_hallucination {
                            avg_confidence += seg.confidence;
                            valid_segments += 1;
                        }
                    }
                    if valid_segments > 0 {
                        avg_confidence /= valid_segments as f32;
                    }

                    if valid_segments > 0 && avg_confidence < 0.40 {
                        if !is_partial {
                            log::warn!("Low confidence ({:.2}), likely hallucination: {:?}", avg_confidence, cleaned);
                            let _ = event_tx.send(AppEvent::Error {
                                message: "Low confidence (Unclear speech)".into(),
                            });
                        }
                        continue;
                    }

                    let (is_realtime, paste_on_release) = {
                        let settings = self.state.settings.lock().unwrap();
                        (settings.realtime, settings.paste_on_release)
                    };

                    if is_partial {
                        log::info!(
                            "Partial OK ({} chars): {cleaned:?}",
                            cleaned.len()
                        );
                        if is_realtime && paste_on_release {
                            let text_trimmed = cleaned.trim();
                            let mut rec = self.state.recorder.lock().unwrap();
                            if !text_trimmed.is_empty() {
                                if text_trimmed.starts_with(&rec.pasted_partial_text) {
                                    let diff = &text_trimmed[rec.pasted_partial_text.len()..];
                                    if !diff.is_empty() {
                                        let _ = crate::paste::paste_text(diff);
                                    }
                                } else {
                                    let backspaces = rec.pasted_partial_text.encode_utf16().count();
                                    if backspaces > 0 {
                                        let _ = crate::paste::send_backspaces(backspaces);
                                    }
                                    let _ = crate::paste::paste_text(text_trimmed);
                                }
                                rec.pasted_partial_text = text_trimmed.to_string();
                            }
                        }
                        let _ = event_tx.send(AppEvent::TranscriptionPartial { text: cleaned });
                    } else {
                        log::info!(
                            "Transcription OK ({} chars): {cleaned:?}",
                            cleaned.len()
                        );
                        if paste_on_release {
                            let text_trimmed = cleaned.trim();
                            let mut rec = self.state.recorder.lock().unwrap();
                            if is_realtime && !rec.pasted_partial_text.is_empty() {
                                if text_trimmed.starts_with(&rec.pasted_partial_text) {
                                    let diff = &text_trimmed[rec.pasted_partial_text.len()..];
                                    if !diff.is_empty() {
                                        if let Err(e) = crate::paste::paste_text(diff) {
                                            log::warn!("paste_text failed: {e}");
                                        }
                                    }
                                } else {
                                    let backspaces = rec.pasted_partial_text.encode_utf16().count();
                                    if backspaces > 0 {
                                        let _ = crate::paste::send_backspaces(backspaces);
                                    }
                                    if !text_trimmed.is_empty() {
                                        if let Err(e) = crate::paste::paste_text(text_trimmed) {
                                            log::warn!("paste_text failed: {e}");
                                        }
                                    }
                                }
                                rec.pasted_partial_text.clear();
                            } else if !text_trimmed.is_empty() {
                                if let Err(e) = crate::paste::paste_text(text_trimmed) {
                                    log::warn!("paste_text failed: {e}");
                                }
                            }
                        }
                        let _ = event_tx.send(AppEvent::Done { text: cleaned });
                    }
                }
                Ok(Err(e)) => {
                    log::error!("Transcribe error: {e}");
                    if !is_partial {
                        let _ = event_tx.send(AppEvent::Error {
                            message: format!("Transcribe failed: {e}"),
                        });
                    }
                }
                Err(join_err) => {
                    log::error!("spawn_blocking join error: {join_err}");
                    if !is_partial {
                        let _ = event_tx.send(AppEvent::Error {
                            message: "Inference task panicked".into(),
                        });
                    }
                }
            }
        }

        log::info!("Transcriber worker stopped (channel closed)");
    }
}

/// Mendeteksi pola repetisi halusinasi Whisper yang sering terjadi saat hening atau berderau (noise).
fn is_hallucination(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < 3 {
        return false;
    }

    // 1. Karakter tunggal yang diulang berturut-turut (misal: ffffffff...)
    let mut max_consecutive = 1;
    let mut current_consecutive = 1;
    let mut prev_char = chars[0];

    for &c in &chars[1..] {
        if c == prev_char && !c.is_whitespace() {
            current_consecutive += 1;
            max_consecutive = max_consecutive.max(current_consecutive);
        } else {
            current_consecutive = 1;
            prev_char = c;
        }
    }

    if max_consecutive > 6 {
        return true;
    }

    // 2. Pola pendek yang berulang berturut-turut (misal: "r-r-r-r-" atau "abcabcabc")
    let len = chars.len();
    for pattern_len in 2..=8 {
        let min_repeats = if pattern_len <= 3 { 6 } else { 4 };
        if len < pattern_len * min_repeats {
            continue;
        }
        for i in 0..=(len - pattern_len * min_repeats) {
            let pattern = &chars[i..i + pattern_len];
            let mut repeat_count = 1;
            for r in 1..min_repeats {
                let next_idx = i + r * pattern_len;
                if &chars[next_idx..next_idx + pattern_len] == pattern {
                    repeat_count += 1;
                } else {
                    break;
                }
            }
            if repeat_count >= min_repeats {
                // Kecuali jika polanya hanya berisi spasi atau tanda baca standar
                if pattern.iter().all(|&c| c.is_whitespace() || c.is_ascii_punctuation()) {
                    continue;
                }
                return true;
            }
        }
    }

    // 3. Kata tunggal yang diulang berturut-turut (misal: "anjing anjing anjing anjing anjing")
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() >= 5 {
        for i in 0..=(words.len() - 5) {
            let word = words[i];
            let mut repeat_count = 1;
            for r in 1..5 {
                if words[i + r].eq_ignore_ascii_case(word) {
                    repeat_count += 1;
                } else {
                    break;
                }
            }
            if repeat_count >= 5 {
                return true;
            }
        }
    }

    // 4. Frasa berulang (mis. "terima kasih banyak lagi bersama saya" berulang)
    // Cek panjang frasa dari 2 kata sampai 10 kata.
    for phrase_len in 2..=10 {
        if words.len() >= phrase_len * 2 {
            for i in 0..=(words.len() - phrase_len * 2) {
                let phrase = &words[i..i + phrase_len];
                let mut repeat_count = 1;
                // Cek berapa kali frasa ini diulang berturut-turut
                let mut next_idx = i + phrase_len;
                while next_idx + phrase_len <= words.len() {
                    let next_phrase = &words[next_idx..next_idx + phrase_len];
                    if phrase.iter().zip(next_phrase.iter()).all(|(a, b)| a.eq_ignore_ascii_case(b)) {
                        repeat_count += 1;
                        next_idx += phrase_len;
                    } else {
                        break;
                    }
                }
                
                // Jika frasa panjang (>=4 kata) diulang 2x saja sudah aneh (halusinasi).
                // Jika frasa pendek (2-3 kata), butuh minimal 3x ulang.
                let required_repeats = if phrase_len >= 4 { 2 } else { 3 };
                if repeat_count >= required_repeats {
                    return true;
                }
            }
        }
    }

    // 5. Hardcoded hallucinations yang sangat umum pada bahasa Indonesia
    let lower_text = text.to_lowercase();
    if lower_text.contains("terima kasih banyak") && lower_text.matches("terima kasih").count() >= 2 {
        return true;
    }
    if lower_text.contains("jangan lupa subscribe") || lower_text.contains("sampai jumpa di video selanjutnya") {
        return true;
    }

    false
}

/// Menghapus tanda penanda suara non-verbal dalam tanda kurung siku `[...]` or kurung biasa `(...)`.
/// Contoh: "[sigh]", "[cough]", "[laughter]", "[h]", dll.
fn clean_non_speech_tokens(text: &str) -> String {
    let mut result = String::new();
    let mut in_bracket = 0;
    let mut in_paren = 0;

    let chars: Vec<char> = text.chars().collect();
    for &c in &chars {
        match c {
            '[' => in_bracket += 1,
            ']' => {
                if in_bracket > 0 {
                    in_bracket -= 1;
                }
            }
            '(' => in_paren += 1,
            ')' => {
                if in_paren > 0 {
                    in_paren -= 1;
                }
            }
            _ => {
                if in_bracket == 0 && in_paren == 0 {
                    result.push(c);
                }
            }
        }
    }

    result
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ")
}
