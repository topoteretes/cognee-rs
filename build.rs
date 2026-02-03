use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let target_dir =
        PathBuf::from(env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".into()));
    let model_dir = target_dir.join("models");
    std::fs::create_dir_all(&model_dir)?;

    let bge_path = model_dir.join("BGE-Small-v1.5-model_quantized.onnx");
    if !bge_path.exists() {
        let bge_url = env::var("BGE_SMALL_ONNX_URL").unwrap_or_else(|_| {
            "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/onnx/model_quantized.onnx".to_string()
        });
        download_file(&bge_url, &bge_path)?;
    }

    let bert_path = model_dir.join("bert-tiny.onnx");
    if !bert_path.exists() {
        let bert_url = env::var("BERT_TINY_ONNX_URL").unwrap_or_else(|_| {
            "https://raw.githubusercontent.com/unit-mesh/testing-onnx-models/main/bert-tiny-onnx/model.onnx".to_string()
        });
        download_file(&bert_url, &bert_path)?;
    }

    Ok(())
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

// Conversion removed: we now download a prebuilt ONNX model.
