//! Build metadata for `bvr --version`, using only the standard library.
//!
//! Emits:
//! - `BVR_BUILD_TARGET`: the Cargo target triple.
//! - `BVR_BUILD_RUSTC`: the rustc semver used for the build.
//! - `BVR_BUILD_TIMESTAMP`: RFC 3339 UTC build time (honours `SOURCE_DATE_EPOCH`).

use std::env;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    println!("cargo:rerun-if-env-changed=RUSTC");

    let target = env::var("TARGET").unwrap_or_else(|_| "unknown-target".to_string());
    println!("cargo:rustc-env=BVR_BUILD_TARGET={target}");

    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let rustc_semver = Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| parse_rustc_semver(&text))
        .unwrap_or_else(|| "unknown-rustc".to_string());
    println!("cargo:rustc-env=BVR_BUILD_RUSTC={rustc_semver}");

    let epoch_secs = env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(0))
        });
    println!(
        "cargo:rustc-env=BVR_BUILD_TIMESTAMP={}",
        format_rfc3339_utc(epoch_secs)
    );
}

/// Extract `1.2.3[-pre]` from `rustc 1.2.3[-pre] (hash date)`.
fn parse_rustc_semver(version_line: &str) -> Option<String> {
    let mut parts = version_line.split_whitespace();
    if parts.next()? != "rustc" {
        return None;
    }
    parts.next().map(str::to_string)
}

/// Format Unix seconds as `YYYY-MM-DDTHH:MM:SSZ` (proleptic Gregorian, UTC).
fn format_rfc3339_utc(epoch_secs: i64) -> String {
    let days = epoch_secs.div_euclid(86_400);
    let secs_of_day = epoch_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60,
        secs_of_day % 60
    )
}

/// Howard Hinnant's days-from-civil inverse.
fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}
