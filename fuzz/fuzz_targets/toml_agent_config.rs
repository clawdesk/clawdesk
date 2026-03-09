//! Fuzz target: TOML agent configuration parsing.
//!
//! Ensures that parsing arbitrary TOML never panics or causes UB.

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // Attempt to parse as a generic TOML Value — should never panic
        let _ = toml::from_str::<toml::Value>(s);

        // Attempt to parse as TOML table and check specific fields
        if let Ok(table) = toml::from_str::<toml::map::Map<String, toml::Value>>(s) {
            // Exercise field access patterns used by agent config loader
            let _ = table.get("agent");
            let _ = table.get("name");
            let _ = table.get("model");
            let _ = table.get("system_prompt");
            let _ = table.get("tools");
        }
    }
});
