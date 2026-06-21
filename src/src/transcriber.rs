//! Wrapper di sekitar `oxiwhisper::WhisperModel` + tokio worker loop.
//!
//! Worker baca dari `tokio::sync::mpsc::UnboundedReceiver<Vec<f32>>` (samples
//! 16 kHz mono f32), jalankan inference CPU-bound di `tokio::task::spawn_blocking`,
//! lalu emit `AppEvent::Done` (paste otomatis di Phase 4).
//!
//! Audio dianggap "no speech" kalau panjang < 0.3 detik (4800 samples @16kHz).

use std::sync::Arc;

use oxiwhisper::{OxiWhisperError, TranscribeOptions, WhisperModel};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::events::{AppEvent, EventSender};

/// Ambang minimal durasi audio (samples @16 kHz mono). 0.3 detik = 4800 samples.
const MIN_SAMPLES: usize = 4_800;

/// Wrapper tipis di atas `WhisperModel` agar bisa di-share `Arc` ke worker thread.
pub struct Transcriber {
    /// Model Whisper yang sudah di-load dari file GGML.
    model: Arc<WhisperModel>,
    /// BCP-47 language hint (mis. `"en"`, `"id"`). `None` = auto-detect.
    language: Option<String>,
}

impl Transcriber {
    /// Buat `Transcriber` baru. `model` biasanya di-load sekali di startup dan
    /// di-share `Arc` ke worker + UI (untuk `info()`).
    pub fn new(model: Arc<WhisperModel>, language: Option<String>) -> Self {
        Self { model, language }
    }

    /// Jalankan inference langsung tanpa spawn ke blocking thread.
    /// Berguna untuk unit test atau pemanggilan sync.
    pub fn transcribe_blocking(&self, audio: &[f32]) -> Result<String, OxiWhisperError> {
        let opts = TranscribeOptions {
            language: self.language.as_deref(),
            no_repeat_ngram_size: 4,
            ..TranscribeOptions::default()
        };
        self.model.transcribe(audio, &opts)
    }

    /// Worker loop async: terima batch samples, transcribe, emit event.
    ///
    /// Loop berakhir ketika `release_rx` di-close (semua `Sender` di-drop).
    /// Inference berjalan di `spawn_blocking` agar tidak block runtime tokio.
    pub async fn run_loop(
        self,
        mut release_rx: UnboundedReceiver<Vec<f32>>,
        event_tx: EventSender,
    ) {
        log::info!(
            "Transcriber worker started (language={:?})",
            self.language
        );

        while let Some(samples) = release_rx.recv().await {
            // Skip audio terlalu pendek — kemungkinan noise / tidak ada speech.
            if samples.len() < MIN_SAMPLES {
                log::debug!(
                    "Skipping short audio chunk: {} samples (< {} = 0.3s)",
                    samples.len(),
                    MIN_SAMPLES
                );
                let _ = event_tx.send(AppEvent::Error {
                    message: "No speech detected".into(),
                });
                continue;
            }

            // Clone handle ke model & language untuk dipindahkan ke blocking task.
            let model = Arc::clone(&self.model);
            let lang = self.language.clone();
            let sample_count = samples.len();

            log::info!("Transcribing {sample_count} samples (~{:.2}s)", sample_count as f32 / 16_000.0);

            // Inference CPU-bound: jalankan di thread pool blocking.
            let join_result = tokio::task::spawn_blocking(move || {
                let opts = TranscribeOptions {
                    language: lang.as_deref(),
                    no_repeat_ngram_size: 4,
                    ..TranscribeOptions::default()
                };
                model.transcribe(&samples, &opts)
            })
            .await;

             match join_result {
                Ok(Ok(text)) => {
                    let cleaned = clean_non_speech_tokens(&text);
                    if cleaned.is_empty() {
                        log::info!("Transcription returned empty text after cleaning non-speech tokens");
                        continue;
                    }

                    if is_hallucination(&cleaned) {
                        log::warn!("Hallucination/repetition detected: {:?}", cleaned);
                        let _ = event_tx.send(AppEvent::Error {
                            message: "Noise/repetition detected, please try again".into(),
                        });
                        continue;
                    }

                    log::info!("Transcription OK ({} chars): {cleaned:?}", cleaned.len());
                    // Paste ke active window (clipboard + SendInput Ctrl+V).
                    // Error di-log tapi tidak menggagalkan emit Done — teks
                    // minimal sudah masuk clipboard.
                    if let Err(e) = crate::paste::paste_text(&cleaned) {
                        log::warn!("paste_text failed (text tetap di clipboard): {e}");
                    }
                    let _ = event_tx.send(AppEvent::Done { text: cleaned });
                }
                Ok(Err(e)) => {
                    log::error!("Transcribe error: {e}");
                    let _ = event_tx.send(AppEvent::Error {
                        message: format!("Transcribe failed: {e}"),
                    });
                }
                Err(join_err) => {
                    log::error!("spawn_blocking join error: {join_err}");
                    let _ = event_tx.send(AppEvent::Error {
                        message: "Inference task panicked".into(),
                    });
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