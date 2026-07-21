use std::{env, fs, path::{Path, PathBuf}};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/tray_icon.ico");
    println!("cargo:rerun-if-changed=../third_party/whisper.cpp/include/whisper.h");
    println!("cargo:rerun-if-changed=../third_party/whisper.cpp/ggml/include/ggml.h");

    let whisper_src = PathBuf::from("../third_party/whisper.cpp");
    if !whisper_src.join("include/whisper.h").exists() {
        panic!("whisper.cpp not found at {}", whisper_src.display());
    }

    // Auto-detect libclang (untuk bindgen) kalau LIBCLANG_PATH belum diset.
    if env::var_os("LIBCLANG_PATH").is_none() {
        if let Some(libclang_dir) = find_libclang_dir() {
            env::set_var("LIBCLANG_PATH", &libclang_dir);
            println!(
                "cargo:warning=libclang auto-detected at {}",
                libclang_dir.display()
            );
        } else {
            println!(
                "cargo:warning=libclang tidak terdeteksi otomatis. \
                 Install LLVM (https://llvm.org/) atau set LIBCLANG_PATH."
            );
        }
    }

    // Auto-detect cmake kalau gak ada di PATH (kasus user pakai PowerShell baru).
    if which_cmake().is_none() {
        if let Some(cmake_exe) = find_cmake_in_vs() {
            let cmake_dir: PathBuf = cmake_exe.parent().unwrap().to_path_buf();
            let current = env::var_os("PATH").unwrap_or_default();
            let mut paths: Vec<PathBuf> = vec![cmake_dir];
            paths.extend(env::split_paths(&current));
            let new_path = env::join_paths(&paths).unwrap();
            env::set_var("PATH", &new_path);
            env::set_var("CMAKE", &cmake_exe);
            println!("cargo:warning=cmake auto-detected at {}", cmake_exe.display());
        } else {
            panic!(
                "cmake tidak ditemukan di PATH dan tidak ada di Visual Studio Build Tools. \
                 Install Visual Studio Build Tools atau pakai run.ps1 yang sudah setup cmake + LIBCLANG_PATH."
            );
        }
    }

    // Add ninja to PATH if present in VS
    if let Some(ninja_exe) = find_ninja_in_vs() {
        let ninja_dir = ninja_exe.parent().unwrap().to_path_buf();
        let current = env::var_os("PATH").unwrap_or_default();
        let mut paths: Vec<PathBuf> = vec![ninja_dir];
        paths.extend(env::split_paths(&current));
        let new_path = env::join_paths(&paths).unwrap();
        env::set_var("PATH", &new_path);
    }

    let use_cuda = env::var("TABVOICE_NO_CUDA").unwrap_or_else(|_| "0".to_string()) != "1";

    let mut cfg = cmake::Config::new(&whisper_src);
    cfg.generator("Ninja")
        .define("BUILD_SHARED_LIBS", "OFF")
        .define("WHISPER_BUILD_TESTS", "OFF")
        .define("WHISPER_BUILD_EXAMPLES", "OFF")
        .define("WHISPER_BUILD_SERVER", "OFF")
        .define("WHISPER_OPENVINO", "OFF")
        .define("WHISPER_COREML", "OFF");
        
    if use_cuda {
        cfg.define("GGML_CUDA", "ON"); // Aktifkan GPU / CUDA Support
    }
    cfg.profile("Release");

    let dst = cfg.build();

    println!("cargo:rustc-link-lib=static=whisper");
    println!("cargo:rustc-link-lib=static=ggml-base");
    println!("cargo:rustc-link-lib=static=ggml-cpu");
    if use_cuda {
        println!("cargo:rustc-link-lib=static=ggml-cuda");
    }
    println!("cargo:rustc-link-lib=static=ggml");
    println!("cargo:rustc-link-search=native={}/lib", dst.display());

    if cfg!(windows) && use_cuda {
        let mut cuda_path = env::var("CUDA_PATH").unwrap_or_default();
        if !Path::new(&cuda_path).join("lib").join("x64").join("cudart.lib").exists() {
            // Coba hardcode ke v13.2 kalau CUDA_PATH nyangkut di versi lama
            cuda_path = "C:\\Program Files\\NVIDIA GPU Computing Toolkit\\CUDA\\v13.2".to_string();
        }
        println!("cargo:rustc-link-search=native={}\\lib\\x64", cuda_path);
        println!("cargo:rustc-link-lib=cuda");
        println!("cargo:rustc-link-lib=cudart");
        println!("cargo:rustc-link-lib=cublas");
        println!("cargo:rustc-link-lib=cublasLt");
    }

    #[cfg(target_os = "linux")]
    {
        println!("cargo:rustc-link-lib=dylib=stdc++");
        println!("cargo:rustc-link-lib=dylib=pthread");
        println!("cargo:rustc-link-lib=dylib=m");
    }
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rustc-link-lib=dylib=c++");
    }

    // BINDGEN DI-HILANGIN: pakai pre-generated whisper_bindings.rs di src/src/
    // (copy manual biar build gak butuh libclang/bindgen)
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let pregen = manifest_dir.join("src/whisper_bindings.rs");
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("whisper_bindings.rs");
    if pregen.exists() {
        fs::copy(&pregen, &out_path).expect("copy pre-generated whisper_bindings.rs");
        println!("cargo:warning=Using pre-generated whisper_bindings.rs (skip bindgen)");
    } else {
        panic!("whisper_bindings.rs not found at {}; run build on Linux first to generate it", pregen.display());
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let icon_src = manifest_dir.join("src/tray_icon.ico");
    if icon_src.exists() {
        let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("tray_icon.ico");
        if let Err(e) = fs::copy(&icon_src, &out) {
            panic!("Failed to copy tray_icon.ico to OUT_DIR: {e}");
        }
        
        #[cfg(windows)]
        {
            let mut res = winres::WindowsResource::new();
            res.set_icon("src/tray_icon.ico");
            if let Err(e) = res.compile() {
                println!("cargo:warning=Gagal embed icon via winres: {}", e);
            }
        }
    }
}

/// Check apakah `cmake` ada di PATH.
fn which_cmake() -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let candidate = dir.join(if cfg!(windows) { "cmake.exe" } else { "cmake" });
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Cari direktori berisi `libclang.dll` di lokasi umum Windows.
#[cfg(windows)]
fn find_libclang_dir() -> Option<PathBuf> {
    let program_files =
        env::var_os("ProgramFiles").unwrap_or_else(|| "C:\\Program Files".into());
    let program_files_x86 =
        env::var_os("ProgramFiles(x86)").unwrap_or_else(|| "C:\\Program Files (x86)".into());
    let username = env::var_os("USERNAME")
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    let libclang_name = if cfg!(windows) { "libclang.dll" } else { "libclang.so" };
    let candidates: Vec<PathBuf> = vec![
        PathBuf::from(&program_files).join("LLVM").join("bin"),
        PathBuf::from("C:\\Users")
            .join(&username)
            .join("scoop")
            .join("apps")
            .join("llvm")
            .join("current")
            .join("bin"),
        PathBuf::from("D:\\llvm-extract\\bin"),
        PathBuf::from("D:\\llvm\\bin"),
        PathBuf::from(&program_files_x86).join("LLVM").join("bin"),
        PathBuf::from(&program_files)
            .join("Microsoft Visual Studio")
            .join("2022")
            .join("Community")
            .join("VC")
            .join("Tools")
            .join("Llvm")
            .join("x64")
            .join("bin"),
        PathBuf::from(&program_files)
            .join("Microsoft Visual Studio")
            .join("2022")
            .join("BuildTools")
            .join("VC")
            .join("Tools")
            .join("Llvm")
            .join("x64")
            .join("bin"),
        PathBuf::from(&program_files)
            .join("Microsoft Visual Studio")
            .join("18")
            .join("BuildTools")
            .join("VC")
            .join("Tools")
            .join("Llvm")
            .join("x64")
            .join("bin"),
    ];
    candidates
        .into_iter()
        .find(|p| p.join(libclang_name).is_file())
}

#[cfg(not(windows))]
fn find_libclang_dir() -> Option<PathBuf> {
    None
}

/// Cari `cmake.exe` di instalasi Visual Studio Build Tools Windows.
fn find_cmake_in_vs() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let program_files_x86 =
        env::var_os("ProgramFiles(x86)").unwrap_or_else(|| "C:\\Program Files (x86)".into());
    let program_files = env::var_os("ProgramFiles").unwrap_or_else(|| "C:\\Program Files".into());

    for root in &[program_files_x86.clone(), program_files.clone()] {
        let vs_root = Path::new(root).join("Microsoft Visual Studio");
        if !vs_root.exists() {
            continue;
        }
        if let Ok(entries) = fs::read_dir(&vs_root) {
            for entry in entries.flatten() {
                let cmake_path = entry
                    .path()
                    .join("BuildTools")
                    .join("Common7")
                    .join("IDE")
                    .join("CommonExtensions")
                    .join("Microsoft")
                    .join("CMake")
                    .join("CMake")
                    .join("bin")
                    .join("cmake.exe");
                if cmake_path.is_file() {
                    return Some(cmake_path);
                }
            }
        }
    }
    None
}

/// Cari `ninja.exe` di instalasi Visual Studio Build Tools Windows.
fn find_ninja_in_vs() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let program_files_x86 =
        env::var_os("ProgramFiles(x86)").unwrap_or_else(|| "C:\\Program Files (x86)".into());
    let program_files = env::var_os("ProgramFiles").unwrap_or_else(|| "C:\\Program Files".into());

    for root in &[program_files_x86.clone(), program_files.clone()] {
        let vs_root = Path::new(root).join("Microsoft Visual Studio");
        if !vs_root.exists() {
            continue;
        }
        if let Ok(entries) = fs::read_dir(&vs_root) {
            for entry in entries.flatten() {
                let ninja_path = entry
                    .path()
                    .join("BuildTools")
                    .join("Common7")
                    .join("IDE")
                    .join("CommonExtensions")
                    .join("Microsoft")
                    .join("CMake")
                    .join("Ninja")
                    .join("ninja.exe");
                if ninja_path.is_file() {
                    return Some(ninja_path);
                }
            }
        }
    }
    None
}