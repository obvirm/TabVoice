//! Test transcription pipeline dengan file WAV.
//!
//! Usage:
//!   cargo run --example transcribe_test -- <model.bin> <audio.wav>
//!
//! Bukti end-to-end bahwa oxiwhisper inference benar-benar bekerja
//! dengan model dan audio yang sebenarnya.

use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: {} <model.bin> <audio.wav>", args[0]);
        std::process::exit(1);
    }

    let model_path = Path::new(&args[1]);
    let wav_path = Path::new(&args[2]);

    println!("Loading model: {:?}", model_path);
    let model = match oxiwhisper::WhisperModel::from_file(model_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("ERROR loading model: {e}");
            std::process::exit(1);
        }
    };
    println!("Model loaded: {:?}", model.info());

    println!("Loading audio: {:?}", wav_path);
    let audio = match oxiwhisper::audio::load_wav(wav_path) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("ERROR loading audio: {e}");
            std::process::exit(1);
        }
    };
    let duration = audio.len() as f32 / 16000.0;
    println!(
        "Audio: {:.1}s ({} samples @16kHz)",
        duration,
        audio.len()
    );

    let opts = oxiwhisper::TranscribeOptions {
        timestamps: true,
        ..Default::default()
    };

    println!("\nTranscribing...");
    let start = std::time::Instant::now();

    let result = match model.transcribe_segmented(&audio, &opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ERROR transcribing: {e}");
            std::process::exit(1);
        }
    };

    let elapsed = start.elapsed();
    println!("\n=== RESULT ===");
    println!("Full text: {}", result.text);
    println!("Language: {:?}", result.language);
    println!("Segments: {}", result.segments.len());
    println!("\nSegment details:");
    for (i, seg) in result.segments.iter().enumerate() {
        println!(
            "  [{:>2}] {:.2}s - {:.2}s: {} (conf: {:.3}{})",
            i + 1,
            seg.start,
            seg.end,
            seg.text,
            seg.confidence,
            if seg.is_hallucination {
                " ⚠ HALLUCINATION"
            } else {
                ""
            }
        );
    }
    println!(
        "\nInference time: {:.2}s (RTF: {:.3})",
        elapsed.as_secs_f32(),
        elapsed.as_secs_f32() / duration
    );
}
