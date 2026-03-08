//! Hardware detection for local LLM inference.
//!
//! Detects system RAM, CPU, GPU (NVIDIA/AMD/Apple Silicon), and determines
//! the best inference backend. Inspired by llmfit-core's hardware detection.

use serde::{Deserialize, Serialize};
use std::process::Command;
use tracing::{debug, info};

/// GPU compute backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuBackend {
    Cuda,
    Metal,
    Rocm,
    Vulkan,
    CpuArm,
    CpuX86,
}

/// Info about a single GPU.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuInfo {
    pub name: String,
    pub vram_gb: f64,
    pub index: u32,
}

/// Detected system hardware specifications.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemSpecs {
    pub total_ram_gb: f64,
    pub available_ram_gb: f64,
    pub total_cpu_cores: usize,
    pub cpu_name: String,
    pub has_gpu: bool,
    pub gpu_vram_gb: Option<f64>,
    pub total_gpu_vram_gb: Option<f64>,
    pub gpu_name: Option<String>,
    pub gpu_count: u32,
    pub unified_memory: bool,
    pub backend: GpuBackend,
    pub gpus: Vec<GpuInfo>,
}

impl SystemSpecs {
    /// Detect the current system's hardware capabilities.
    pub fn detect() -> Self {
        let mut sys = sysinfo::System::new_all();
        sys.refresh_all();

        let total_ram_gb = sys.total_memory() as f64 / (1024.0 * 1024.0 * 1024.0);
        let available_ram_gb = sys.available_memory() as f64 / (1024.0 * 1024.0 * 1024.0);
        let total_cpu_cores = sys.cpus().len();
        let cpu_name = sys
            .cpus()
            .first()
            .map(|c| c.brand().to_string())
            .unwrap_or_else(|| "Unknown CPU".to_string());

        let mut specs = SystemSpecs {
            total_ram_gb,
            available_ram_gb,
            total_cpu_cores,
            cpu_name: cpu_name.clone(),
            has_gpu: false,
            gpu_vram_gb: None,
            total_gpu_vram_gb: None,
            gpu_name: None,
            gpu_count: 0,
            unified_memory: false,
            backend: if cfg!(target_arch = "aarch64") {
                GpuBackend::CpuArm
            } else {
                GpuBackend::CpuX86
            },
            gpus: vec![],
        };

        // Try detecting GPUs in order of preference
        if detect_apple_silicon(&mut specs) {
            info!(gpu = ?specs.gpu_name, vram = ?specs.gpu_vram_gb, "detected Apple Silicon");
        } else if detect_nvidia(&mut specs) {
            info!(gpu = ?specs.gpu_name, vram = ?specs.gpu_vram_gb, "detected NVIDIA GPU");
        } else if detect_amd(&mut specs) {
            info!(gpu = ?specs.gpu_name, vram = ?specs.gpu_vram_gb, "detected AMD GPU");
        } else {
            debug!("no discrete GPU detected, will use CPU inference");
        }

        specs
    }

    /// Effective memory budget for model loading (GPU VRAM or unified memory).
    pub fn inference_memory_gb(&self) -> f64 {
        if self.unified_memory {
            // Apple Silicon: use ~75% of total RAM for model
            self.total_ram_gb * 0.75
        } else if let Some(vram) = self.total_gpu_vram_gb {
            vram
        } else {
            // CPU-only: use ~60% of RAM
            self.total_ram_gb * 0.60
        }
    }
}

/// Detect Apple Silicon unified memory GPU.
fn detect_apple_silicon(specs: &mut SystemSpecs) -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }

    let output = Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output();

    let is_apple = match &output {
        Ok(o) => {
            let brand = String::from_utf8_lossy(&o.stdout);
            brand.contains("Apple")
        }
        Err(_) => false,
    };

    if !is_apple {
        return false;
    }

    // On Apple Silicon, GPU shares unified memory
    specs.has_gpu = true;
    specs.unified_memory = true;
    specs.backend = GpuBackend::Metal;
    specs.gpu_count = 1;

    // Use system_profiler to get chip name
    if let Ok(output) = Command::new("system_profiler")
        .args(["SPHardwareDataType"])
        .output()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("Chip:") || trimmed.starts_with("Chip Name:") {
                let chip = trimmed.split(':').nth(1).unwrap_or("Apple Silicon").trim();
                specs.gpu_name = Some(chip.to_string());
            }
        }
    }

    // Unified memory = total system RAM is available to GPU
    specs.gpu_vram_gb = Some(specs.total_ram_gb);
    specs.total_gpu_vram_gb = Some(specs.total_ram_gb);
    specs.gpus.push(GpuInfo {
        name: specs.gpu_name.clone().unwrap_or("Apple Silicon".into()),
        vram_gb: specs.total_ram_gb,
        index: 0,
    });

    true
}

/// Detect NVIDIA GPUs via nvidia-smi.
fn detect_nvidia(specs: &mut SystemSpecs) -> bool {
    let output = Command::new("nvidia-smi")
        .args(["--query-gpu=name,memory.total", "--format=csv,noheader,nounits"])
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return false,
    };

    let text = String::from_utf8_lossy(&output.stdout);
    let mut total_vram = 0.0_f64;
    let mut gpus = Vec::new();

    for (idx, line) in text.lines().enumerate() {
        let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if parts.len() >= 2 {
            let name = parts[0].to_string();
            let vram_mb: f64 = parts[1].parse().unwrap_or(0.0);
            let vram_gb = vram_mb / 1024.0;
            total_vram += vram_gb;
            gpus.push(GpuInfo {
                name: name.clone(),
                vram_gb,
                index: idx as u32,
            });
        }
    }

    if gpus.is_empty() {
        return false;
    }

    specs.has_gpu = true;
    specs.backend = GpuBackend::Cuda;
    specs.gpu_count = gpus.len() as u32;
    specs.gpu_name = Some(gpus[0].name.clone());
    specs.gpu_vram_gb = Some(gpus[0].vram_gb);
    specs.total_gpu_vram_gb = Some(total_vram);
    specs.gpus = gpus;

    true
}

/// Detect AMD GPUs via rocm-smi.
fn detect_amd(specs: &mut SystemSpecs) -> bool {
    let output = Command::new("rocm-smi")
        .args(["--showmeminfo", "vram", "--json"])
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return false,
    };

    let text = String::from_utf8_lossy(&output.stdout);
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
        // Parse rocm-smi JSON output
        if let Some(obj) = json.as_object() {
            let mut total_vram = 0.0;
            let mut gpus = Vec::new();

            for (key, val) in obj {
                if key.starts_with("card") {
                    if let Some(total) = val.get("VRAM Total Memory (B)") {
                        let bytes: f64 = total
                            .as_str()
                            .and_then(|s| s.parse().ok())
                            .or_else(|| total.as_f64())
                            .unwrap_or(0.0);
                        let gb = bytes / (1024.0 * 1024.0 * 1024.0);
                        total_vram += gb;
                        gpus.push(GpuInfo {
                            name: format!("AMD GPU {}", key),
                            vram_gb: gb,
                            index: gpus.len() as u32,
                        });
                    }
                }
            }

            if !gpus.is_empty() {
                specs.has_gpu = true;
                specs.backend = GpuBackend::Rocm;
                specs.gpu_count = gpus.len() as u32;
                specs.gpu_name = Some(gpus[0].name.clone());
                specs.gpu_vram_gb = Some(gpus[0].vram_gb);
                specs.total_gpu_vram_gb = Some(total_vram);
                specs.gpus = gpus;
                return true;
            }
        }
    }

    false
}
