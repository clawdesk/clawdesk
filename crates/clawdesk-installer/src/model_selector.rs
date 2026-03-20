//! Hardware-aware model selection.
//!
//! Selects the optimal model based on available memory and GPU capabilities.
//! Default to 1B model (~500 MB) as permanent starter tier for bandwidth-
//! constrained environments, with background upgrade to larger models.

use clawdesk_local_models::hardware::SystemSpecs;
use serde::{Deserialize, Serialize};

/// A recommended model with its requirements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRecommendation {
    /// Ollama model tag (e.g., "qwen2.5:0.5b")
    pub model_tag: String,
    /// Human-readable name
    pub display_name: String,
    /// Parameter count description
    pub parameters: String,
    /// Expected download size in MB
    pub download_size_mb: u64,
    /// Minimum RAM/VRAM required in GB
    pub min_memory_gb: f64,
    /// Tier for progressive download strategy
    pub tier: ModelTier,
}

/// Model tier for progressive download strategy.
///
/// Tier 0 downloads first for instant gratification (~200 MB),
/// then upgrades to Tier 1 in background (~500 MB),
/// then optionally to Tier 2 for best quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ModelTier {
    /// Instant gratification (<200 MB, 0.5B parameters)
    Instant = 0,
    /// Default starter tier (~500 MB, 1B parameters)
    Starter = 1,
    /// Standard quality (~2 GB, 3B parameters)
    Standard = 2,
    /// High quality (~4 GB, 7-8B parameters)
    Quality = 3,
    /// Maximum quality (~8 GB, 14B+ parameters)
    Premium = 4,
}

/// Available model options, from smallest to largest.
pub fn available_models() -> Vec<ModelRecommendation> {
    vec![
        ModelRecommendation {
            model_tag: "qwen2.5:0.5b".into(),
            display_name: "Qwen 2.5 Tiny".into(),
            parameters: "0.5B".into(),
            download_size_mb: 200,
            min_memory_gb: 0.5,
            tier: ModelTier::Instant,
        },
        ModelRecommendation {
            model_tag: "qwen2.5:1.5b".into(),
            display_name: "Qwen 2.5 Small".into(),
            parameters: "1.5B".into(),
            download_size_mb: 500,
            min_memory_gb: 1.5,
            tier: ModelTier::Starter,
        },
        ModelRecommendation {
            model_tag: "llama3.2:3b".into(),
            display_name: "Llama 3.2".into(),
            parameters: "3B".into(),
            download_size_mb: 2000,
            min_memory_gb: 3.0,
            tier: ModelTier::Standard,
        },
        ModelRecommendation {
            model_tag: "llama3.1:8b".into(),
            display_name: "Llama 3.1".into(),
            parameters: "8B".into(),
            download_size_mb: 4700,
            min_memory_gb: 6.0,
            tier: ModelTier::Quality,
        },
        ModelRecommendation {
            model_tag: "qwen2.5:14b".into(),
            display_name: "Qwen 2.5 Large".into(),
            parameters: "14B".into(),
            download_size_mb: 8500,
            min_memory_gb: 12.0,
            tier: ModelTier::Premium,
        },
    ]
}

/// Select the best model for the user's hardware.
///
/// Strategy:
/// 1. Returns the instant-gratification model (Tier 0) for first response.
/// 2. Returns the largest model that fits in memory as the upgrade target.
/// 3. Respects max_model_size_gb constraint if provided.
pub fn select_models(
    specs: &SystemSpecs,
    max_size_gb: Option<f64>,
) -> ModelSelection {
    let budget_gb = specs.inference_memory_gb();
    let max_gb = max_size_gb.unwrap_or(f64::MAX);
    let models = available_models();

    // Instant model (always available — fits in <512 MB)
    let instant = models.iter()
        .find(|m| m.tier == ModelTier::Instant)
        .cloned()
        .unwrap_or_else(|| models[0].clone());

    // Best model that fits in memory and size constraint
    let upgrade_target = models.iter()
        .rev() // Start from largest
        .find(|m| {
            m.min_memory_gb <= budget_gb
                && (m.download_size_mb as f64 / 1024.0) <= max_gb
        })
        .cloned()
        .unwrap_or_else(|| instant.clone());

    ModelSelection {
        instant_model: instant,
        upgrade_target,
        available_memory_gb: budget_gb,
        has_gpu: specs.has_gpu,
    }
}

/// Result of model selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSelection {
    /// Small model for immediate first response
    pub instant_model: ModelRecommendation,
    /// Largest model that fits — download in background
    pub upgrade_target: ModelRecommendation,
    /// Available memory budget
    pub available_memory_gb: f64,
    /// Whether GPU acceleration is available
    pub has_gpu: bool,
}

impl ModelSelection {
    /// Whether an upgrade from instant to target is worth doing.
    pub fn should_upgrade(&self) -> bool {
        self.upgrade_target.tier > self.instant_model.tier
    }

    /// Estimated download time for instant model at given bandwidth (Mbps).
    pub fn instant_download_secs(&self, bandwidth_mbps: f64) -> f64 {
        let size_megabits = self.instant_model.download_size_mb as f64 * 8.0;
        size_megabits / bandwidth_mbps
    }

    /// Estimated download time for upgrade model at given bandwidth (Mbps).
    pub fn upgrade_download_secs(&self, bandwidth_mbps: f64) -> f64 {
        let size_megabits = self.upgrade_target.download_size_mb as f64 * 8.0;
        size_megabits / bandwidth_mbps
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clawdesk_local_models::hardware::{GpuBackend, GpuInfo, SystemSpecs};

    fn mock_specs(ram_gb: f64, has_gpu: bool, vram_gb: f64) -> SystemSpecs {
        SystemSpecs {
            total_ram_gb: ram_gb,
            available_ram_gb: ram_gb * 0.8,
            total_cpu_cores: 8,
            cpu_name: "Test CPU".into(),
            has_gpu,
            gpu_vram_gb: if has_gpu { Some(vram_gb) } else { None },
            total_gpu_vram_gb: if has_gpu { Some(vram_gb) } else { None },
            gpu_name: if has_gpu { Some("Test GPU".into()) } else { None },
            gpu_count: if has_gpu { 1 } else { 0 },
            unified_memory: false,
            backend: GpuBackend::CpuX86,
            gpus: if has_gpu {
                vec![GpuInfo { name: "Test".into(), vram_gb, index: 0 }]
            } else {
                vec![]
            },
        }
    }

    #[test]
    fn selects_instant_for_low_memory() {
        let specs = mock_specs(2.0, false, 0.0);
        let selection = select_models(&specs, None);
        assert_eq!(selection.instant_model.tier, ModelTier::Instant);
    }

    #[test]
    fn selects_quality_for_high_memory() {
        let specs = mock_specs(16.0, true, 8.0);
        let selection = select_models(&specs, None);
        assert!(selection.upgrade_target.tier >= ModelTier::Quality);
    }

    #[test]
    fn respects_max_size_constraint() {
        let specs = mock_specs(32.0, true, 24.0);
        let selection = select_models(&specs, Some(1.0)); // Max 1 GB
        assert!(selection.upgrade_target.download_size_mb <= 1024);
    }

    #[test]
    fn should_upgrade_when_target_differs() {
        let specs = mock_specs(8.0, false, 0.0);
        let selection = select_models(&specs, None);
        assert!(selection.should_upgrade());
    }
}
