use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const ONNXRUNTIME_VERSION: &str = "v1.23.0";
const ONNXRUNTIME_REPO: &str = "https://github.com/microsoft/onnxruntime.git";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Get absolute path to target directory
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let target_dir = env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest_dir.join("target"));
    let model_dir = target_dir.join("models");
    std::fs::create_dir_all(&model_dir)?;

    // Download ONNX models
    download_models(&model_dir)?;

    // Build ONNX Runtime if the onnx_dynamic_library feature is enabled
    if env::var("CARGO_FEATURE_ONNX_DYNAMIC_LIBRARY").is_ok() {
        build_onnxruntime(&target_dir)?;
    }

    Ok(())
}

fn download_models(model_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    // Embedding models
    download_if_not_exists(
        model_dir,
        "BGE-Small-v1.5-model_quantized.onnx",
        "BGE_SMALL_ONNX_URL",
        "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/onnx/model_quantized.onnx",
    )?;

    download_if_not_exists(
        model_dir,
        "bert-tiny.onnx",
        "BERT_TINY_ONNX_URL",
        "https://raw.githubusercontent.com/unit-mesh/testing-onnx-models/main/bert-tiny-onnx/model.onnx",
    )?;

    // Qwen3-0.6B for entity/relation extraction (Q4 quantized for on-device use)
    download_if_not_exists(
        model_dir,
        "qwen3-0.6b-q4.onnx",
        "QWEN3_ONNX_URL",
        "https://huggingface.co/onnx-community/Qwen3-0.6B-ONNX/resolve/main/onnx/model_q4.onnx",
    )?;

    download_if_not_exists(
        model_dir,
        "qwen3-tokenizer.json",
        "QWEN3_TOKENIZER_URL",
        "https://huggingface.co/onnx-community/Qwen3-0.6B-ONNX/resolve/main/tokenizer.json",
    )?;

    Ok(())
}

fn download_if_not_exists(
    dir: &Path,
    filename: &str,
    env_var: &str,
    default_url: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = dir.join(filename);
    if !path.exists() {
        let url = env::var(env_var).unwrap_or_else(|_| default_url.to_string());
        download_file(&url, &path)?;
    }
    Ok(())
}

fn build_onnxruntime(target_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let target = env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    let host = env::var("HOST").unwrap_or_else(|_| "unknown".to_string());
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());

    let is_android = target.contains("android");
    let is_cross_compile = target != host;

    // Determine output directories
    let ort_source_dir = target_dir.join("onnxruntime-src");
    let ort_build_dir = if is_cross_compile {
        target_dir.join(&target).join("onnxruntime-build")
    } else {
        target_dir.join("onnxruntime-build")
    };

    // Determine the library output path
    let lib_name = if cfg!(target_os = "windows") {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    };

    let lib_output_dir = if is_cross_compile {
        target_dir.join(&target).join(&profile)
    } else {
        target_dir.join(&profile)
    };
    let lib_output_path = lib_output_dir.join(lib_name);

    // Skip if library already exists
    if lib_output_path.exists() {
        println!(
            "cargo:warning=ONNX Runtime library already exists at {}",
            lib_output_path.display()
        );
        println!(
            "cargo:rustc-env=ORT_DYLIB_PATH={}",
            lib_output_path.display()
        );
        return Ok(());
    }

    println!(
        "cargo:warning=Building ONNX Runtime {} for target {}",
        ONNXRUNTIME_VERSION, target
    );

    // Clone ONNX Runtime if not present
    if !ort_source_dir.join("build.sh").exists() {
        clone_onnxruntime(&ort_source_dir)?;
    }

    // Build ONNX Runtime
    let built_lib = build_ort_for_target(
        &ort_source_dir,
        &ort_build_dir,
        &target,
        is_android,
        &profile,
    )?;

    // Copy the library to output directory
    std::fs::create_dir_all(&lib_output_dir)?;
    std::fs::copy(&built_lib, &lib_output_path)?;
    println!(
        "cargo:warning=Copied ONNX Runtime to {}",
        lib_output_path.display()
    );

    // Set environment variable for ort crate
    println!(
        "cargo:rustc-env=ORT_DYLIB_PATH={}",
        lib_output_path.display()
    );

    Ok(())
}

fn clone_onnxruntime(dest: &Path) -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "cargo:warning=Cloning ONNX Runtime {}...",
        ONNXRUNTIME_VERSION
    );

    // Remove partial clone if exists
    if dest.exists() {
        std::fs::remove_dir_all(dest)?;
    }

    let status = Command::new("git")
        .args([
            "clone",
            "--depth",
            "1",
            "--branch",
            ONNXRUNTIME_VERSION,
            "--recurse-submodules",
            "--shallow-submodules",
            ONNXRUNTIME_REPO,
        ])
        .arg(dest)
        .status()?;

    if !status.success() {
        return Err(format!("Failed to clone ONNX Runtime: {}", status).into());
    }

    Ok(())
}

fn build_ort_for_target(
    source_dir: &Path,
    build_dir: &Path,
    target: &str,
    is_android: bool,
    profile: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    std::fs::create_dir_all(build_dir)?;

    let config = match profile {
        "release" => "MinSizeRel",
        _ => "Debug",
    };

    let mut args = vec![
        "--build_dir".to_string(),
        build_dir.to_string_lossy().to_string(),
        "--config".to_string(),
        config.to_string(),
        "--build_shared_lib".to_string(),
        "--parallel".to_string(),
        "--skip_tests".to_string(),
        "--skip_submodule_sync".to_string(),
        "--compile_no_warning_as_error".to_string(),
        // Disable unnecessary components
        "--disable_ml_ops".to_string(),
    ];

    if is_android {
        // Android-specific configuration
        let ndk_path = env::var("ANDROID_NDK_HOME")
            .or_else(|_| env::var("NDK_HOME"))
            .map_err(|_| "ANDROID_NDK_HOME or NDK_HOME must be set for Android builds")?;

        let sdk_path = env::var("ANDROID_SDK_ROOT")
            .or_else(|_| env::var("ANDROID_HOME"))
            .map_err(|_| "ANDROID_SDK_ROOT or ANDROID_HOME must be set for Android builds")?;

        // Determine Android ABI from target
        let android_abi = if target.starts_with("aarch64") {
            "arm64-v8a"
        } else if target.starts_with("armv7") || target.starts_with("arm-") {
            "armeabi-v7a"
        } else if target.starts_with("x86_64") {
            "x86_64"
        } else if target.starts_with("i686") {
            "x86"
        } else {
            return Err(format!("Unsupported Android target: {}", target).into());
        };

        // Android API level (27 is minimum for NNAPI)
        let api_level = env::var("ANDROID_API_LEVEL").unwrap_or_else(|_| "27".to_string());

        args.extend([
            "--android".to_string(),
            "--android_sdk_path".to_string(),
            sdk_path,
            "--android_ndk_path".to_string(),
            ndk_path,
            "--android_abi".to_string(),
            android_abi.to_string(),
            "--android_api".to_string(),
            api_level,
            // Enable NNAPI for hardware acceleration
            "--use_nnapi".to_string(),
        ]);

        println!(
            "cargo:warning=Building for Android {} with NNAPI support",
            android_abi
        );
    } else {
        // Native build - can use XNNPACK for optimized CPU inference
        if target.contains("aarch64") || target.contains("arm") {
            args.push("--use_xnnpack".to_string());
            println!("cargo:warning=Building with XNNPACK for ARM optimization");
        }
    }

    // Run the build script
    let build_script = if cfg!(windows) {
        source_dir.join("build.bat")
    } else {
        source_dir.join("build.sh")
    };

    println!("cargo:warning=Running ONNX Runtime build (this may take a while)...");
    println!("cargo:warning=Build args: {:?}", args);

    let status = Command::new(&build_script)
        .args(&args)
        .current_dir(source_dir)
        .status()?;

    if !status.success() {
        return Err(format!("ONNX Runtime build failed: {}", status).into());
    }

    // Find the built library
    let lib_name = if cfg!(windows) {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    };

    // The library is typically in build_dir/<config>/
    let lib_path = build_dir.join(config).join(lib_name);
    if lib_path.exists() {
        return Ok(lib_path);
    }

    // Alternative location for some build configurations
    let alt_lib_path = build_dir.join(config).join("lib").join(lib_name);
    if alt_lib_path.exists() {
        return Ok(alt_lib_path);
    }

    // Search recursively as fallback
    find_library_recursive(build_dir, lib_name)
        .ok_or_else(|| format!("Could not find {} in {}", lib_name, build_dir.display()).into())
}

fn find_library_recursive(dir: &Path, lib_name: &str) -> Option<PathBuf> {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && path
                    .file_name()
                    .map(|n| n.to_string_lossy().contains(lib_name))
                    .unwrap_or(false)
            {
                return Some(path);
            }
            if path.is_dir()
                && let Some(found) = find_library_recursive(&path, lib_name)
            {
                return Some(found);
            }
        }
    }
    None
}

fn download_file(url: &str, dest: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::new("curl");
    cmd.args(["-L", "--fail", "--retry", "3", "-o"])
        .arg(dest)
        .arg(url);

    if let Ok(token) = env::var("HF_TOKEN").or_else(|_| env::var("HUGGINGFACE_TOKEN")) {
        cmd.args(["-H", &format!("Authorization: Bearer {}", token)]);
    }

    let status = cmd.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("curl failed with status {status}")).map_err(Into::into)
    }
}
