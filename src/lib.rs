#![forbid(unsafe_code)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::implicit_hasher)]
#![allow(clippy::missing_const_for_fn)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::suboptimal_flops)]
#![allow(clippy::too_many_lines)]

pub mod agents;
pub mod analysis;
pub mod cli;
pub mod error;
pub mod export_md;
pub mod export_pages;
pub mod export_sqlite;
pub mod loader;
pub mod model;
pub mod pages_wizard;
pub mod report_template;
pub mod robot;
pub mod tui;
pub mod tutorial;
pub mod viewer_assets;

pub use error::{BvrError, Result};

#[cfg(test)]
mod version_guard {
    //! Catches hard-coded version strings that should use `env!("CARGO_PKG_VERSION")`.
    //!
    //! When adding a new struct with a `version` or `schema_version` field, use
    //! `env!("CARGO_PKG_VERSION")` instead of a string literal. This test will
    //! fail if a literal version string sneaks in.

    use std::path::Path;

    /// Walk `src/` looking for version string literals in non-test production code.
    #[test]
    fn no_hardcoded_version_strings_in_production_code() {
        let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let pkg_version = env!("CARGO_PKG_VERSION");

        // Patterns that indicate a hard-coded version where env!() should be used.
        let suspicious = [
            format!("\"{}\"", pkg_version), // e.g. "0.1.0"
            "\"1.0.0\"".to_string(),
            "\"1.0\"".to_string(),
            "\"2.0.0\"".to_string(),
            "\"2.0\"".to_string(),
        ];

        // Allowlisted files/patterns (test code, HTML markers, HTTP headers, etc.)
        let allowlist: &[&str] = &[
            "agents.rs", // AGENT_BLURB HTML markers — guarded by agent_blurb_version_matches_constant
            "viewer_assets.rs", // Embedded asset content
        ];

        let mut violations = Vec::new();

        for entry in walkdir(src_dir) {
            let path = entry.as_path();
            if !path.extension().is_some_and(|ext| ext == "rs") {
                continue;
            }

            let filename = path.file_name().unwrap_or_default().to_string_lossy();
            if allowlist.iter().any(|a| filename.as_ref() == *a) {
                continue;
            }

            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            // Split into production vs test code at #[cfg(test)]
            let prod_code = content.split("#[cfg(test)]").next().unwrap_or(&content);

            for pattern in &suspicious {
                for (line_no, line) in prod_code.lines().enumerate() {
                    let trimmed = line.trim();
                    // Skip comments
                    if trimmed.starts_with("//") || trimmed.starts_with("///") {
                        continue;
                    }
                    if line.contains(pattern.as_str()) {
                        violations.push(format!(
                            "{}:{}: contains {} — use env!(\"CARGO_PKG_VERSION\") instead",
                            path.display(),
                            line_no + 1,
                            pattern,
                        ));
                    }
                }
            }
        }

        assert!(
            violations.is_empty(),
            "Found hard-coded version strings in production code:\n{}",
            violations.join("\n")
        );
    }

    /// Simple recursive directory walker (no external dependency needed).
    fn walkdir(dir: std::path::PathBuf) -> Vec<std::path::PathBuf> {
        let mut files = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    files.extend(walkdir(path));
                } else {
                    files.push(path);
                }
            }
        }
        files
    }
}
