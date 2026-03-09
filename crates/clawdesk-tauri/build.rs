use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("missing CARGO_MANIFEST_DIR"));
    let script_path = manifest_dir.join("../../scripts/download-llama-server.sh");
    let sidecar_path = manifest_dir.join(target_sidecar_name());

    println!("cargo:rerun-if-changed={}", script_path.display());
    println!("cargo:rerun-if-changed={}", manifest_dir.join("tauri.conf.json").display());
    println!("cargo:rerun-if-env-changed=LLAMA_CPP_VERSION");
    println!("cargo:rerun-if-env-changed=CLAWDESK_SKIP_LLAMA_SERVER_DOWNLOAD");

    ensure_llama_server_sidecar(&script_path, &sidecar_path);
    tauri_build::build()
}

fn ensure_llama_server_sidecar(script_path: &Path, sidecar_path: &Path) {
    if sidecar_path.exists() {
        return;
    }

    if env::var_os("CLAWDESK_SKIP_LLAMA_SERVER_DOWNLOAD").is_some() {
        panic!(
            "Bundled llama-server sidecar is missing at {} and CLAWDESK_SKIP_LLAMA_SERVER_DOWNLOAD is set. Either provide the sidecar manually or unset the skip flag.",
            sidecar_path.display()
        );
    }

    if !script_path.exists() {
        panic!(
            "Cannot prepare bundled llama-server sidecar because {} does not exist.",
            script_path.display()
        );
    }

    let status = Command::new("bash")
        .arg(script_path)
        .status()
        .unwrap_or_else(|error| {
            panic!(
                "Failed to launch {} via bash: {}. Install bash or provide the llama-server sidecar manually.",
                script_path.display(),
                error
            )
        });

    if !status.success() {
        panic!(
            "Automatic llama-server sidecar preparation failed with status {}.",
            status
        );
    }

    if !sidecar_path.exists() {
        panic!(
            "llama-server download completed but the expected sidecar was not found at {}.",
            sidecar_path.display()
        );
    }
}

fn target_sidecar_name() -> PathBuf {
    let target_os = env::var("CARGO_CFG_TARGET_OS").expect("missing CARGO_CFG_TARGET_OS");
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").expect("missing CARGO_CFG_TARGET_ARCH");

    let file_name = match (target_os.as_str(), target_arch.as_str()) {
        ("macos", "aarch64") => "binaries/llama-server-aarch64-apple-darwin",
        ("macos", "x86_64") => "binaries/llama-server-x86_64-apple-darwin",
        ("linux", "x86_64") => "binaries/llama-server-x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "binaries/llama-server-aarch64-unknown-linux-gnu",
        ("windows", "x86_64") => "binaries/llama-server-x86_64-pc-windows-msvc.exe",
        ("windows", "aarch64") => "binaries/llama-server-aarch64-pc-windows-msvc.exe",
        _ => panic!("Unsupported target for bundled llama-server: {target_os}-{target_arch}"),
    };

    PathBuf::from(file_name)
}
