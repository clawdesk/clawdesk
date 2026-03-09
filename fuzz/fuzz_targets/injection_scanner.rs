//! Fuzz target: Prompt injection scanner.
//!
//! Ensures the scanner never panics on arbitrary input.

#![no_main]
use libfuzzer_sys::fuzz_target;

use clawdesk_security::injection::{InjectionScanner, InjectionScannerConfig, InputSource};

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let scanner = InjectionScanner::new(InjectionScannerConfig::default());

        // Scan as all source types
        let result_user = scanner.scan(s, InputSource::User);
        let result_tool = scanner.scan(s, InputSource::ToolOutput);
        let result_web = scanner.scan(s, InputSource::WebContent);

        // Risk score must be in [0, 1]
        assert!((0.0..=1.0).contains(&result_user.risk_score));
        assert!((0.0..=1.0).contains(&result_tool.risk_score));
        assert!((0.0..=1.0).contains(&result_web.risk_score));
    }
});
