//! Local LLM model database, quantization, and fit analysis.
//!
//! Embeds a curated list of popular GGUF models and scores them against
//! the detected hardware to recommend the best options.


use crate::hardware::SystemSpecs;
use serde::{Deserialize, Serialize};

/// How well a model fits the available hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FitLevel {
    Perfect,
    Good,
    Marginal,
    TooTight,
}

/// How the model will run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    Gpu,
    CpuOffload,
    CpuOnly,
}

/// Primary use case for a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UseCase {
    General,
    Coding,
    Reasoning,
    Chat,
    Multimodal,
    Embedding,
}

/// GGUF quantization level with memory and quality characteristics.
#[derive(Debug, Clone, Copy)]
pub struct QuantLevel {
    pub name: &'static str,
    pub bytes_per_param: f64,
    pub quality_penalty: f64,
    pub speed_mult: f64,
}

/// Quantization hierarchy (best quality → most compressed).
pub const QUANT_HIERARCHY: &[QuantLevel] = &[
    QuantLevel { name: "Q8_0",   bytes_per_param: 1.0,  quality_penalty: 0.02, speed_mult: 0.9 },
    QuantLevel { name: "Q6_K",   bytes_per_param: 0.75, quality_penalty: 0.04, speed_mult: 0.95 },
    QuantLevel { name: "Q5_K_M", bytes_per_param: 0.65, quality_penalty: 0.06, speed_mult: 1.0 },
    QuantLevel { name: "Q4_K_M", bytes_per_param: 0.5,  quality_penalty: 0.10, speed_mult: 1.05 },
    QuantLevel { name: "Q3_K_M", bytes_per_param: 0.4,  quality_penalty: 0.18, speed_mult: 1.1 },
    QuantLevel { name: "Q2_K",   bytes_per_param: 0.3,  quality_penalty: 0.30, speed_mult: 1.15 },
];

/// A known LLM model in the local database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalModel {
    pub name: String,
    pub provider: String,
    pub parameter_count: String,
    pub parameters_raw: u64,
    pub context_length: u32,
    pub use_case: UseCase,
    pub gguf_repo: String,
    pub gguf_filename_pattern: String,
}

impl LocalModel {
    /// Estimate memory required in GB for a given quantization and context.
    pub fn estimate_memory_gb(&self, quant: &QuantLevel, ctx: u32) -> f64 {
        let params_b = self.parameters_raw as f64 / 1_000_000_000.0;
        let model_size = params_b * quant.bytes_per_param;
        let kv_cache = 0.000008 * params_b * ctx as f64;
        let overhead = 0.5; // CUDA/Metal context
        model_size + kv_cache + overhead
    }

    /// Find the best quantization that fits within a memory budget.
    pub fn best_quant_for_budget(&self, budget_gb: f64, ctx: u32) -> Option<(&QuantLevel, f64)> {
        for q in QUANT_HIERARCHY {
            let mem = self.estimate_memory_gb(q, ctx);
            if mem <= budget_gb {
                return Some((q, mem));
            }
        }
        None
    }
}

/// Choose a practical runtime context window for a model on the current machine.
///
/// We do not want to force every model down to 8K, but we also should not
/// eagerly launch every runtime at its full advertised maximum when that would
/// waste memory on the KV cache. This picks a conservative tier based on the
/// current inference memory budget and then caps by the model's declared limit.
pub fn recommended_runtime_context(model: &LocalModel, system: &SystemSpecs) -> u32 {
    let memory_budget_gb = system.inference_memory_gb();
    let system_cap = if memory_budget_gb >= 24.0 {
        32_768
    } else if memory_budget_gb >= 12.0 {
        16_384
    } else {
        8_192
    };

    model.context_length.min(system_cap).max(8_192)
}

/// Fallback runtime context for models that are not in the built-in catalog.
pub fn fallback_runtime_context(system: &SystemSpecs) -> u32 {
    if system.inference_memory_gb() >= 12.0 {
        16_384
    } else {
        8_192
    }
}

/// Result of fitting a model to the system hardware.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelFit {
    pub model: LocalModel,
    pub fit_level: FitLevel,
    pub run_mode: RunMode,
    pub memory_required_gb: f64,
    pub memory_available_gb: f64,
    pub utilization_pct: f64,
    pub best_quant: String,
    pub estimated_tps: f64,
    pub score: f64,
    pub use_case: UseCase,
    pub gguf_download_url: String,
    pub installed: bool,
}

impl ModelFit {
    /// Analyze how well a model fits on the given system.
    pub fn analyze(model: &LocalModel, system: &SystemSpecs, installed: bool) -> Self {
        let memory_available = system.inference_memory_gb();
        let context = recommended_runtime_context(model, system);

        // Find best quantization that fits
        let (quant, mem_required) = model
            .best_quant_for_budget(memory_available, context)
            .unwrap_or((&QUANT_HIERARCHY[QUANT_HIERARCHY.len() - 1], memory_available * 1.5));

        let utilization = (mem_required / memory_available * 100.0).min(200.0);

        let (fit_level, run_mode) = if system.has_gpu && mem_required <= memory_available * 0.8 {
            (FitLevel::Perfect, RunMode::Gpu)
        } else if mem_required <= memory_available {
            if system.has_gpu {
                (FitLevel::Good, RunMode::Gpu)
            } else {
                (FitLevel::Good, RunMode::CpuOnly)
            }
        } else if mem_required <= system.total_ram_gb * 0.7 {
            (FitLevel::Marginal, RunMode::CpuOffload)
        } else {
            (FitLevel::TooTight, RunMode::CpuOnly)
        };

        let estimated_tps = estimate_speed(model, system, quant, &run_mode);

        // Composite score (0-100)
        let quality_score = quality_score(model, quant);
        let speed_score = (estimated_tps / 50.0 * 100.0).min(100.0);
        let fit_score = match fit_level {
            FitLevel::Perfect => 95.0,
            FitLevel::Good => 75.0,
            FitLevel::Marginal => 40.0,
            FitLevel::TooTight => 10.0,
        };
        let composite = quality_score * 0.35 + speed_score * 0.30 + fit_score * 0.35;

        let gguf_download_url = format!(
            "https://huggingface.co/{}/resolve/main/{}",
            model.gguf_repo,
            model.gguf_filename_pattern.replace("{Q}", quant.name)
        );

        ModelFit {
            model: model.clone(),
            fit_level,
            run_mode,
            memory_required_gb: mem_required,
            memory_available_gb: memory_available,
            utilization_pct: utilization,
            best_quant: quant.name.to_string(),
            estimated_tps,
            score: composite,
            use_case: model.use_case,
            gguf_download_url,
            installed,
        }
    }
}

fn quality_score(model: &LocalModel, quant: &QuantLevel) -> f64 {
    let params_b = model.parameters_raw as f64 / 1_000_000_000.0;
    let base = match params_b as u64 {
        0..=3 => 50.0,
        4..=8 => 70.0,
        9..=14 => 80.0,
        15..=34 => 88.0,
        35..=72 => 93.0,
        _ => 95.0,
    };
    (base * (1.0 - quant.quality_penalty)).max(0.0)
}

fn estimate_speed(
    model: &LocalModel,
    system: &SystemSpecs,
    quant: &QuantLevel,
    run_mode: &RunMode,
) -> f64 {
    let params_b = model.parameters_raw as f64 / 1_000_000_000.0;
    let base_tps = 40.0 / params_b * 7.0 * quant.speed_mult;

    let mode_mult = match run_mode {
        RunMode::Gpu => 1.0,
        RunMode::CpuOffload => 0.5,
        RunMode::CpuOnly => 0.3,
    };

    let gpu_mult = if system.has_gpu { 1.5 } else { 1.0 };

    (base_tps * mode_mult * gpu_mult).max(1.0)
}

/// Built-in curated model database for popular GGUF models.
pub fn builtin_models() -> Vec<LocalModel> {
    vec![
        // ── Llama 3.2 ──
        LocalModel {
            name: "Llama-3.2-1B-Instruct".into(),
            provider: "Meta".into(),
            parameter_count: "1B".into(),
            parameters_raw: 1_000_000_000,
            context_length: 131072,
            use_case: UseCase::Chat,
            gguf_repo: "bartowski/Llama-3.2-1B-Instruct-GGUF".into(),
            gguf_filename_pattern: "Llama-3.2-1B-Instruct-{Q}.gguf".into(),
        },
        LocalModel {
            name: "Llama-3.2-3B-Instruct".into(),
            provider: "Meta".into(),
            parameter_count: "3B".into(),
            parameters_raw: 3_000_000_000,
            context_length: 131072,
            use_case: UseCase::Chat,
            gguf_repo: "bartowski/Llama-3.2-3B-Instruct-GGUF".into(),
            gguf_filename_pattern: "Llama-3.2-3B-Instruct-{Q}.gguf".into(),
        },
        // ── Llama 3.1/3.3 ──
        LocalModel {
            name: "Llama-3.1-8B-Instruct".into(),
            provider: "Meta".into(),
            parameter_count: "8B".into(),
            parameters_raw: 8_000_000_000,
            context_length: 131072,
            use_case: UseCase::General,
            gguf_repo: "bartowski/Meta-Llama-3.1-8B-Instruct-GGUF".into(),
            gguf_filename_pattern: "Meta-Llama-3.1-8B-Instruct-{Q}.gguf".into(),
        },
        LocalModel {
            name: "Llama-3.3-70B-Instruct".into(),
            provider: "Meta".into(),
            parameter_count: "70B".into(),
            parameters_raw: 70_000_000_000,
            context_length: 131072,
            use_case: UseCase::General,
            gguf_repo: "bartowski/Llama-3.3-70B-Instruct-GGUF".into(),
            gguf_filename_pattern: "Llama-3.3-70B-Instruct-{Q}.gguf".into(),
        },
        // ── Qwen 3 ──
        LocalModel {
            name: "Qwen3-4B".into(),
            provider: "Qwen".into(),
            parameter_count: "4B".into(),
            parameters_raw: 4_000_000_000,
            context_length: 32768,
            use_case: UseCase::General,
            gguf_repo: "unsloth/Qwen3-4B-GGUF".into(),
            gguf_filename_pattern: "Qwen3-4B-{Q}.gguf".into(),
        },
        LocalModel {
            name: "Qwen3-8B".into(),
            provider: "Qwen".into(),
            parameter_count: "8B".into(),
            parameters_raw: 8_000_000_000,
            context_length: 32768,
            use_case: UseCase::General,
            gguf_repo: "unsloth/Qwen3-8B-GGUF".into(),
            gguf_filename_pattern: "Qwen3-8B-{Q}.gguf".into(),
        },
        LocalModel {
            name: "Qwen2.5-Coder-7B-Instruct".into(),
            provider: "Qwen".into(),
            parameter_count: "7B".into(),
            parameters_raw: 7_000_000_000,
            context_length: 32768,
            use_case: UseCase::Coding,
            gguf_repo: "bartowski/Qwen2.5-Coder-7B-Instruct-GGUF".into(),
            gguf_filename_pattern: "Qwen2.5-Coder-7B-Instruct-{Q}.gguf".into(),
        },
        LocalModel {
            name: "Qwen2.5-Coder-32B-Instruct".into(),
            provider: "Qwen".into(),
            parameter_count: "32B".into(),
            parameters_raw: 32_000_000_000,
            context_length: 32768,
            use_case: UseCase::Coding,
            gguf_repo: "bartowski/Qwen2.5-Coder-32B-Instruct-GGUF".into(),
            gguf_filename_pattern: "Qwen2.5-Coder-32B-Instruct-{Q}.gguf".into(),
        },
        // ── Mistral ──
        LocalModel {
            name: "Mistral-7B-Instruct-v0.3".into(),
            provider: "Mistral".into(),
            parameter_count: "7B".into(),
            parameters_raw: 7_000_000_000,
            context_length: 32768,
            use_case: UseCase::Chat,
            gguf_repo: "bartowski/Mistral-7B-Instruct-v0.3-GGUF".into(),
            gguf_filename_pattern: "Mistral-7B-Instruct-v0.3-{Q}.gguf".into(),
        },
        LocalModel {
            name: "Mistral-Small-24B-Instruct-2501".into(),
            provider: "Mistral".into(),
            parameter_count: "24B".into(),
            parameters_raw: 24_000_000_000,
            context_length: 32768,
            use_case: UseCase::General,
            gguf_repo: "bartowski/Mistral-Small-24B-Instruct-2501-GGUF".into(),
            gguf_filename_pattern: "Mistral-Small-24B-Instruct-2501-{Q}.gguf".into(),
        },
        // ── Gemma ──
        LocalModel {
            name: "gemma-2-2b-it".into(),
            provider: "Google".into(),
            parameter_count: "2B".into(),
            parameters_raw: 2_000_000_000,
            context_length: 8192,
            use_case: UseCase::Chat,
            gguf_repo: "bartowski/gemma-2-2b-it-GGUF".into(),
            gguf_filename_pattern: "gemma-2-2b-it-{Q}.gguf".into(),
        },
        LocalModel {
            name: "gemma-2-9b-it".into(),
            provider: "Google".into(),
            parameter_count: "9B".into(),
            parameters_raw: 9_000_000_000,
            context_length: 8192,
            use_case: UseCase::General,
            gguf_repo: "bartowski/gemma-2-9b-it-GGUF".into(),
            gguf_filename_pattern: "gemma-2-9b-it-{Q}.gguf".into(),
        },
        // ── Phi ──
        LocalModel {
            name: "Phi-3.5-mini-instruct".into(),
            provider: "Microsoft".into(),
            parameter_count: "3.8B".into(),
            parameters_raw: 3_800_000_000,
            context_length: 131072,
            use_case: UseCase::General,
            gguf_repo: "bartowski/Phi-3.5-mini-instruct-GGUF".into(),
            gguf_filename_pattern: "Phi-3.5-mini-instruct-{Q}.gguf".into(),
        },
        LocalModel {
            name: "Phi-4".into(),
            provider: "Microsoft".into(),
            parameter_count: "14B".into(),
            parameters_raw: 14_000_000_000,
            context_length: 16384,
            use_case: UseCase::Reasoning,
            gguf_repo: "bartowski/phi-4-GGUF".into(),
            gguf_filename_pattern: "phi-4-{Q}.gguf".into(),
        },
        // ── DeepSeek ──
        LocalModel {
            name: "DeepSeek-R1-Distill-Qwen-7B".into(),
            provider: "DeepSeek".into(),
            parameter_count: "7B".into(),
            parameters_raw: 7_000_000_000,
            context_length: 32768,
            use_case: UseCase::Reasoning,
            gguf_repo: "bartowski/DeepSeek-R1-Distill-Qwen-7B-GGUF".into(),
            gguf_filename_pattern: "DeepSeek-R1-Distill-Qwen-7B-{Q}.gguf".into(),
        },
        LocalModel {
            name: "DeepSeek-R1-Distill-Qwen-14B".into(),
            provider: "DeepSeek".into(),
            parameter_count: "14B".into(),
            parameters_raw: 14_000_000_000,
            context_length: 32768,
            use_case: UseCase::Reasoning,
            gguf_repo: "bartowski/DeepSeek-R1-Distill-Qwen-14B-GGUF".into(),
            gguf_filename_pattern: "DeepSeek-R1-Distill-Qwen-14B-{Q}.gguf".into(),
        },
        // ── StarCoder ──
        LocalModel {
            name: "starcoder2-7b".into(),
            provider: "BigCode".into(),
            parameter_count: "7B".into(),
            parameters_raw: 7_000_000_000,
            context_length: 16384,
            use_case: UseCase::Coding,
            gguf_repo: "bartowski/starcoder2-7b-GGUF".into(),
            gguf_filename_pattern: "starcoder2-7b-{Q}.gguf".into(),
        },
        // ── Nomic Embed ──
        LocalModel {
            name: "nomic-embed-text-v1.5".into(),
            provider: "Nomic".into(),
            parameter_count: "137M".into(),
            parameters_raw: 137_000_000,
            context_length: 8192,
            use_case: UseCase::Embedding,
            gguf_repo: "nomic-ai/nomic-embed-text-v1.5-GGUF".into(),
            gguf_filename_pattern: "nomic-embed-text-v1.5.Q8_0.gguf".into(),
        },
    ]
}
