//! Coverage enforcement — threshold checking for CI quality gates.

use serde::{Deserialize, Serialize};

/// Coverage thresholds (aligned with industry standard: 70% line, 55% branch).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageThresholds {
    pub line_pct: f64,
    pub function_pct: f64,
    pub branch_pct: f64,
    pub statement_pct: f64,
}

impl Default for CoverageThresholds {
    fn default() -> Self {
        Self {
            line_pct: 70.0,
            function_pct: 70.0,
            branch_pct: 55.0,
            statement_pct: 70.0,
        }
    }
}

/// Coverage report from cargo-llvm-cov or similar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageReport {
    pub lines_covered: u64,
    pub lines_total: u64,
    pub functions_covered: u64,
    pub functions_total: u64,
    pub branches_covered: u64,
    pub branches_total: u64,
}

impl CoverageReport {
    pub fn line_pct(&self) -> f64 {
        if self.lines_total == 0 { 0.0 }
        else { self.lines_covered as f64 / self.lines_total as f64 * 100.0 }
    }

    pub fn function_pct(&self) -> f64 {
        if self.functions_total == 0 { 0.0 }
        else { self.functions_covered as f64 / self.functions_total as f64 * 100.0 }
    }

    pub fn branch_pct(&self) -> f64 {
        if self.branches_total == 0 { 0.0 }
        else { self.branches_covered as f64 / self.branches_total as f64 * 100.0 }
    }
}

/// Check whether coverage meets thresholds.
#[derive(Debug, Clone)]
pub struct CoverageCheck {
    pub passed: bool,
    pub failures: Vec<String>,
}

pub fn check_coverage(report: &CoverageReport, thresholds: &CoverageThresholds) -> CoverageCheck {
    let mut failures = Vec::new();

    if report.line_pct() < thresholds.line_pct {
        failures.push(format!(
            "line coverage {:.1}% < {:.1}% threshold",
            report.line_pct(), thresholds.line_pct
        ));
    }
    if report.function_pct() < thresholds.function_pct {
        failures.push(format!(
            "function coverage {:.1}% < {:.1}% threshold",
            report.function_pct(), thresholds.function_pct
        ));
    }
    if report.branch_pct() < thresholds.branch_pct {
        failures.push(format!(
            "branch coverage {:.1}% < {:.1}% threshold",
            report.branch_pct(), thresholds.branch_pct
        ));
    }

    CoverageCheck {
        passed: failures.is_empty(),
        failures,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passing_coverage() {
        let report = CoverageReport {
            lines_covered: 800, lines_total: 1000,
            functions_covered: 90, functions_total: 100,
            branches_covered: 60, branches_total: 100,
        };
        let check = check_coverage(&report, &CoverageThresholds::default());
        assert!(check.passed);
    }

    #[test]
    fn failing_coverage() {
        let report = CoverageReport {
            lines_covered: 500, lines_total: 1000,
            functions_covered: 50, functions_total: 100,
            branches_covered: 30, branches_total: 100,
        };
        let check = check_coverage(&report, &CoverageThresholds::default());
        assert!(!check.passed);
        assert_eq!(check.failures.len(), 3);
    }
}
