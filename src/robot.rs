use std::collections::BTreeMap;

use chrono::Utc;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::Result;
use crate::cli::OutputFormat;
use crate::model::Issue;

#[derive(Debug, Clone, Serialize)]
pub struct RobotEnvelope {
    pub generated_at: String,
    pub data_hash: String,
}

#[must_use]
pub fn envelope(issues: &[Issue]) -> RobotEnvelope {
    RobotEnvelope {
        generated_at: Utc::now().to_rfc3339(),
        data_hash: compute_data_hash(issues),
    }
}

#[must_use]
pub fn compute_data_hash(issues: &[Issue]) -> String {
    let mut stable = issues
        .iter()
        .map(|issue| {
            (
                issue.id.clone(),
                issue.status.clone(),
                issue.priority,
                issue.updated_at.clone().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();

    stable.sort_by(|left, right| left.0.cmp(&right.0));

    let mut hasher = Sha256::new();
    for row in stable {
        hasher.update(row.0);
        hasher.update(b"\x1f");
        hasher.update(row.1);
        hasher.update(b"\x1f");
        hasher.update(row.2.to_string());
        hasher.update(b"\x1f");
        hasher.update(row.3);
        hasher.update("\n");
    }

    let digest = hasher.finalize();
    format!("{digest:x}")[..16].to_string()
}

pub fn emit<T: Serialize>(format: OutputFormat, payload: &T) -> Result<()> {
    match format {
        // TODO(port-parity): replace this compatibility behavior with true TOON output.
        OutputFormat::Json | OutputFormat::Toon => {
            let line = serde_json::to_string(payload)?;
            println!("{line}");
        }
    }

    Ok(())
}

#[must_use]
pub fn default_field_descriptions() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("score", "Composite impact score (0..1)"),
        (
            "confidence",
            "Heuristic confidence for recommendation quality (0..1)",
        ),
        (
            "unblocks",
            "Count of downstream issues immediately unblocked",
        ),
        (
            "claim_command",
            "Suggested br command to claim/start the issue",
        ),
    ])
}
