//! Modul audio: microphone capture via cpal + resampling ke 16 kHz via rubato.
//!
//! Phase 2 — TabVoice. Berdiri sendiri, tidak depend ke state/app/iced.
//! API publik: `start_capture()` untuk [`MicCapture`], [`mix_to_mono`] & [`rms`]
//! sebagai helper, plus konstanta [`WHISPER_SAMPLE_RATE`] & [`RESAMPLER_CHUNK`].

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};

use rubato::{
    calculate_cutoff, Resampler, SincFixedIn, SincInterpolationParameters,
    SincInterpolationType, WindowFunction,
};

/// Sample rate target Whisper (16 kHz mono PCM f32).
pub const WHISPER_SAMPLE_RATE: u32 = 16_000;

/// Chunk size untuk resampler (samples per process call pada input rate).
pub const RESAMPLER_CHUNK: usize = 1024;

/// Wrapper streaming di atas [`SincFixedIn`]: accumulate input sampai chunk
/// penuh, emit output 16 kHz mono, skip leading filter delay sekali di awal
/// stream.
pub struct StreamingResampler {
    resampler: SincFixedIn<f32>,
    /// Buffer output yang di-resize ulang tiap call (1 channel mono).
    outbuf: Vec<Vec<f32>>,
    /// Sisa input yang belum cukup satu chunk penuh.
    leftover: Vec<f32>,
    /// Sisa filter delay (samples pada output rate) yang harus di-skip dari
    /// awal stream. Dikonsumsi sekali seumur hidup resampler.
    delay_samples: usize,
    chunk_size: usize,
}

impl StreamingResampler {
    /// Buat resampler baru dari `from_rate` ke `to_rate` dengan chunk size
    /// tertentu. Sinc interpolation dengan `sinc_len=128` & Blackman2 window.
    pub fn new(from_rate: u32, to_rate: u32, chunk_size: usize) -> Self {
        let ratio = to_rate as f64 / from_rate as f64;
        let params = SincInterpolationParameters {
            sinc_len: 128,
            f_cutoff: calculate_cutoff::<f32>(128, WindowFunction::Blackman2),
            oversampling_factor: 256,
            interpolation: SincInterpolationType::Cubic,
            window: WindowFunction::Blackman2,
        };
        let resampler = SincFixedIn::<f32>::new(ratio, 1.0, params, chunk_size, 1)
            .expect("SincFixedIn construction dengan params valid tidak boleh gagal");
        let out_frames = resampler.output_frames_next();
        let outbuf = vec![vec![0.0_f32; out_frames]; 1];
        let delay_samples = resampler.output_delay();
        Self {
            resampler,
            outbuf,
            leftover: Vec::with_capacity(chunk_size * 2),
            delay_samples,
            chunk_size,
        }
    }

    /// Push mono input. Accumulate ke buffer internal; setiap kali `leftover`
    /// mencapai `chunk_size`, process satu chunk lewat resampler dan emit
    /// output ke Vec yang dikembalikan. Leading filter delay di-skip otomatis
    /// sekali di awal stream.
    pub fn push(&mut self, mono_input: &[f32]) -> Result<Vec<f32>> {
        self.leftover.extend_from_slice(mono_input);
        let mut output = Vec::new();

        // Process setiap chunk penuh yang ada di leftover
        while self.leftover.len() >= self.chunk_size {
            let chunk: Vec<f32> = self.leftover.drain(..self.chunk_size).collect();
            let out_frames = self.resampler.output_frames_next();
            self.outbuf[0].resize(out_frames, 0.0);
            let (_n_in, _n_out) = self
                .resampler
                .process_into_buffer(&[&chunk], &mut self.outbuf, None)
                .context("rubato process_into_buffer gagal")?;
            output.extend_from_slice(&self.outbuf[0]);
        }

        // Consume filter delay sekali dari awal stream
        self.trim_delay(&mut output);
        Ok(output)
    }

    /// Drain sisa tail (partial chunk) + flush filter delay internal resampler.
    /// Dipanggil saat stop capture untuk memastikan semua sample ter-emit.
    pub fn flush(&mut self) -> Result<Vec<f32>> {
        let mut output = Vec::new();

        // 1. Process sisa partial chunk (kalau ada)
        if !self.leftover.is_empty() {
            let tail: Vec<f32> = std::mem::take(&mut self.leftover);
            let out_frames = self.resampler.output_frames_next();
            if out_frames > 0 {
                self.outbuf[0].resize(out_frames, 0.0);
                let (_n_in, _n_out) = self
                    .resampler
                    .process_partial_into_buffer(Some(&[&tail]), &mut self.outbuf, None)
                    .context("rubato process_partial (tail) gagal")?;
                output.extend_from_slice(&self.outbuf[0]);
            }
        }

        // 2. Flush filter delay — panggil process_partial dengan None sampai
        //    resampler tidak lagi menghasilkan output
        loop {
            let out_frames = self.resampler.output_frames_next();
            if out_frames == 0 {
                break;
            }
            self.outbuf[0].resize(out_frames, 0.0);
            match self
                .resampler
                .process_partial_into_buffer(None::<&[&[f32]]>, &mut self.outbuf, None)
            {
                Ok((_n_in, n_out)) => {
                    output.extend_from_slice(&self.outbuf[0]);
                    if n_out == 0 {
                        break;
                    }
                }
                Err(_) => break,
            }
        }

        // 3. Sisa delay yang belum ter-consume di push()
        self.trim_delay(&mut output);
        Ok(output)
    }

    /// Consume `delay_samples` dari awal output (cukup sekali seumur hidup).
    fn trim_delay(&mut self, output: &mut Vec<f32>) {
        if self.delay_samples > 0 && !output.is_empty() {
            let skip = self.delay_samples.min(output.len());
            output.drain(..skip);
            self.delay_samples -= skip;
        }
    }
}

/// Mix samples interleaved multi-channel ke mono.
/// - `channels == 1`: copy.
/// - `channels == 2`: rata-rata L+R.
/// - `channels >  2`: ambil channel 0 saja.
pub fn mix_to_mono(interleaved: &[f32], channels: u16) -> Vec<f32> {
    let ch = channels as usize;
    if ch <= 1 {
        return interleaved.to_vec();
    }
    let n_frames = interleaved.len() / ch;
    let mut out = Vec::with_capacity(n_frames);
    if ch == 2 {
        for i in 0..n_frames {
            let l = interleaved[i * 2];
            let r = interleaved[i * 2 + 1];
            out.push((l + r) * 0.5);
        }
    } else {
        for i in 0..n_frames {
            out.push(interleaved[i * ch]);
        }
    }
    out
}

/// Hitung RMS (root mean square) dari samples. Empty → 0.0, NaN → 0.0.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples.iter().map(|&s| (s as f64).powi(2)).sum();
    let mean_sq = sum_sq / samples.len() as f64;
    let r = mean_sq.sqrt() as f32;
    if r.is_nan() {
        0.0
    } else {
        r
    }
}

/// Convert i16 PCM ke f32 dalam range ~[-1, 1]. Pakai 1/32768 supaya
/// i16::MIN map ke -1.0 dan i16::MAX map ke ~0.99997.
fn i16_to_f32(samples: &[i16]) -> Vec<f32> {
    const SCALE: f32 = 1.0 / 32_768.0;
    samples.iter().map(|&s| s as f32 * SCALE).collect()
}

/// Handle ke microphone capture yang sedang aktif. Drop = stop capture.
///
/// Saat di-drop, [`Stream`] di-intern release → cpal otomatis stop callback.
/// Resampler di-drop setelahnya; `flush()` harus dipanggil manual sebelum drop
/// kalau mau drain tail.
pub struct MicCapture {
    /// Stream cpal — drop = stop capture.
    _stream: Stream,
    /// Resampler shared dengan callback (Arc<Mutex<>>).
    resampler: Arc<Mutex<StreamingResampler>>,
}

// SAFETY: `cpal::Stream` di-marked `!Send` oleh upstream (lihat `NotSendSyncAcrossAllPlatforms`
// di cpal 0.15.3) sebagai konservatif cross-platform default. Untuk use-case kita:
// - `MicCapture` selalu dibuat DAN di-drop dari thread yang sama (yaitu thread
//   `tabvoice-hotkey`), bukan dipindah-pindah thread.
// - `Stream` diakses hanya lewat `Drop` (`cpal` handle callback stop di sini)
//   dan lewat callback audio callback (yang jalan di audio thread internal cpal).
// - Akses dari hotkey thread ke field `mic: Mutex<Option<MicCapture>>` exclusive:
//   tidak ada thread lain yang boleh drop MicCapture.
// - `resampler` field-nya adalah `Arc<Mutex<_>>` yang `Send + Sync`.
// Maka `Send` aman untuk pola penggunaan kita — override marker upstream.
unsafe impl Send for MicCapture {}

impl MicCapture {
    /// Akses resampler untuk operasi tambahan (mis. `flush()` saat stop).
    pub fn resampler(&self) -> Arc<Mutex<StreamingResampler>> {
        Arc::clone(&self.resampler)
    }
}

/// Mulai microphone capture dari default input device.
/// Audio di-mix ke mono, di-resample ke [`WHISPER_SAMPLE_RATE`] (16 kHz f32),
/// lalu di-push ke callback `on_samples(samples, rms)` per chunk output.
///
/// Parameter `device_sample_rate` & `device_channels` cuma dipakai untuk
/// logging — nilai aktual diambil dari [`pick_input_config`] (device bisa
/// negotiate beda dari hint).
pub fn start_capture<F>(
    device_sample_rate: u32,
    device_channels: u16,
    device_name: Option<&str>,
    on_samples: F,
) -> Result<MicCapture>
where
    F: Fn(Vec<f32>, f32) + Send + Sync + 'static,
{
    let host = cpal::default_host();
    log::info!("Audio host backend: {:?}", host.id());

    let device = if let Some(name) = device_name {
        host.input_devices()
            .context("Gagal enumerasi input devices")?
            .find(|d| d.name().unwrap_or_default() == name)
            .or_else(|| {
                log::warn!("Device '{}' tidak ditemukan, fallback ke default", name);
                host.default_input_device()
            })
            .ok_or_else(|| anyhow::anyhow!("Tidak ada input device yang tersedia"))?
    } else {
        host.default_input_device()
            .ok_or_else(|| anyhow::anyhow!("Tidak ada default input device (microphone)"))?
    };
    log::info!(
        "Input device: {}",
        device.name().unwrap_or_else(|_| "<unknown>".into())
    );

    let sup_config = pick_input_config(&device)?;
    let fmt = sup_config.sample_format();
    let channels = sup_config.channels();
    let sample_rate = sup_config.sample_rate();
    log::info!(
        "Input config: rate={} ch={} fmt={:?}",
        sample_rate.0,
        channels,
        fmt
    );
    log::info!(
        "Caller hint: rate={} ch={} (informational only)",
        device_sample_rate,
        device_channels
    );

    let stream_config: StreamConfig = sup_config.into();

    // Resampler di-share dengan callback via Arc<Mutex<>>.
    let resampler = Arc::new(Mutex::new(StreamingResampler::new(
        sample_rate.0,
        WHISPER_SAMPLE_RATE,
        RESAMPLER_CHUNK,
    )));
    // Wrap callback di Arc agar bisa di-clone ke kedua arm (F32 & I16).
    let on_samples = Arc::new(on_samples);

    let stream = match fmt {
        SampleFormat::F32 => {
            let res = Arc::clone(&resampler);
            let cb = Arc::clone(&on_samples);
            device
                .build_input_stream(
                    &stream_config,
                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                        let mono = mix_to_mono(data, channels);
                        let Ok(mut guard) = res.lock() else {
                            log::error!("resampler mutex poisoned, skipping audio chunk");
                            return;
                        };
                        if let Ok(out) = guard.push(&mono) {
                            if !out.is_empty() {
                                let r = rms(&out);
                                drop(guard);
                                cb(out, r);
                            }
                        }
                    },
                    stream_err_fn,
                    None,
                )
                .context("gagal build_input_stream (F32)")?
        }
        SampleFormat::I16 => {
            let res = Arc::clone(&resampler);
            let cb = Arc::clone(&on_samples);
            device
                .build_input_stream(
                    &stream_config,
                    move |data: &[i16], _: &cpal::InputCallbackInfo| {
                        let mono = mix_to_mono(&i16_to_f32(data), channels);
                        let Ok(mut guard) = res.lock() else {
                            log::error!("resampler mutex poisoned, skipping audio chunk");
                            return;
                        };
                        if let Ok(out) = guard.push(&mono) {
                            if !out.is_empty() {
                                let r = rms(&out);
                                drop(guard);
                                cb(out, r);
                            }
                        }
                    },
                    stream_err_fn,
                    None,
                )
                .context("gagal build_input_stream (I16)")?
        }
        other => {
            // SampleFormat non_exhaustive — wildcard arm untuk future-proof.
            return Err(anyhow::anyhow!(
                "Unsupported sample format {:?} (cuma F32 & I16)",
                other
            ));
        }
    };

    stream.play().context("gagal start stream playback")?;
    log::info!("Microphone capture started");

    Ok(MicCapture {
        _stream: stream,
        resampler,
    })
}

/// Pilih supported input config: prefer F32, fallback ke format pertama.
fn pick_input_config(device: &cpal::Device) -> Result<cpal::SupportedStreamConfig> {
    let configs: Vec<_> = device
        .supported_input_configs()
        .context("gagal enumerasi supported input configs")?
        .collect();

    let range = configs
        .iter()
        .find(|r| r.sample_format() == SampleFormat::F32)
        .or_else(|| configs.first())
        .ok_or_else(|| anyhow::anyhow!("Device tidak punya supported input config"))?;

    Ok(range.with_max_sample_rate())
}

/// Error callback untuk cpal stream — log aja, tidak propagate.
fn stream_err_fn(err: cpal::StreamError) {
    log::error!("cpal stream error: {}", err);
}

/// Ambil daftar nama microphone yang tersedia di system
pub fn get_available_microphones() -> Vec<String> {
    let host = cpal::default_host();
    let mut names = Vec::new();
    if let Ok(devices) = host.input_devices() {
        for device in devices {
            if let Ok(name) = device.name() {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    names
}