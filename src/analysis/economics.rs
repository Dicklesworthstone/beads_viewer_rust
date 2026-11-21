//! `--robot-economics` — pure projection of operating cost over the existing
//! analyzer state. No new graph traversal; no new data collection.
//!
//! See GH#12 for the design rationale. Key contract properties pinned here:
//!
//! - **Guards are explicit fields**, not silent omissions. Consumers must be
//!   able to distinguish "absent" from "suppressed".
//! - **No threshold labels** (no `status: "critical"` etc.). Downstream
//!   decides thresholds — a CI gate, a portfolio roll-up, and a Slack digest
//!   will reasonably set different thresholds on the same numbers.
//! - **Non-graph inputs are fully reflected in provenance**: `overlay_hash`,
//!   `estimate_coverage_pct`, `project_age_days`, `throughput_window_days`.
//!   Without these, `cost_to_complete` on a young project with 10% estimate
//!   coverage would be silently misleading.
//! - **`rate_per_day` equals `burn_rate_per_day` for every `cost_of_delay`
//!   entry by construction** — the cost of delaying a blocker one day is
//!   the team's burn rate (the whole team is held up by the block); the
//!   `dependents_count` field conveys *scope* of the block, not rate.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

use crate::model::Issue;

/// Default trailing window for throughput computation.
///
/// Short enough to reflect current team velocity; long enough to average
/// across the single-PR noise of a ~3-engineer team.
pub const DEFAULT_THROUGHPUT_WINDOW_DAYS: u32 = 30;

/// Minimum fraction of open issues that must have `estimated_minutes` set.
///
/// Below this, the estimate-based `cost_to_complete` is considered
/// unreliable and the `estimate_coverage_below_threshold` guard trips.
/// The threshold is exposed as a constant rather than a flag because it
/// influences guard output shape; downstream can still decide what to do
/// with the boolean.
pub const ESTIMATE_COVERAGE_GUARD_THRESHOLD: f64 = 0.50;

/// Minimum project age (days) before throughput-based metrics are considered
/// meaningful. A 3-day-old project has no signal in its throughput number.
pub const PROJECT_AGE_GUARD_THRESHOLD_DAYS: i64 = 30;

/// Opt-in overlay for monetary inputs.
///
/// The overlay is the only caller-provided input that matters for economics
/// output (project age, estimate coverage, throughput are all derived from
/// the beads data itself). Loaded from JSON or TOML via
/// `EconomicsOverlay::from_json_str` / `EconomicsOverlay::from_toml_str`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct EconomicsOverlay {
    /// Per-engineer effective hourly rate in the project's currency.
    pub hourly_rate: f64,
    /// Hours of engineer-time the project consumes per calendar day.
    pub hours_per_day: f64,
    /// Optional total budget envelope; when present, drives
    /// `budget_utilization_pct`.
    #[serde(default)]
    pub budget_envelope: Option<f64>,
    /// Override for the trailing throughput window. Defaults to
    /// [`DEFAULT_THROUGHPUT_WINDOW_DAYS`] when unset.
    #[serde(default)]
    pub throughput_window_days: Option<u32>,
    /// Informational currency label (e.g. "USD"). Not interpreted.
    #[serde(default)]
    pub currency: Option<String>,
}

impl EconomicsOverlay {
    pub fn from_json_str(raw: &str) -> Result<Self, String> {
        serde_json::from_str::<Self>(raw)
            .map_err(|error| format!("failed to parse economics overlay JSON: {error}"))
            .and_then(Self::validated)
    }

    fn validated(self) -> Result<Self, String> {
        if !self.hourly_rate.is_finite() || self.hourly_rate < 0.0 {
            return Err(format!(
                "economics overlay: hourly_rate must be finite and non-negative (got {})",
                self.hourly_rate
            ));
        }
        if !self.hours_per_day.is_finite() || self.hours_per_day < 0.0 || self.hours_per_day > 24.0
        {
            return Err(format!(
                "economics overlay: hours_per_day must be finite and within [0, 24] (got {})",
                self.hours_per_day
            ));
        }
        if let Some(envelope) = self.budget_envelope
            && (!envelope.is_finite() || envelope < 0.0)
        {
            return Err(format!(
                "economics overlay: budget_envelope must be finite and non-negative (got {envelope})",
            ));
        }
        if let Some(window) = self.throughput_window_days
            && window == 0
        {
            return Err("economics overlay: throughput_window_days must be > 0".to_string());
        }
        Ok(self)
    }

    /// Canonical SHA256 hash of the overlay content. The hash lets consumers
    /// spot drift when the same beads state is evaluated against a different
    /// overlay across runs. 16 hex chars matches `RobotEnvelope::data_hash`.
    pub fn canonical_hash(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.hourly_rate.to_bits().to_le_bytes());
        hasher.update(b"\x1f");
        hasher.update(self.hours_per_day.to_bits().to_le_bytes());
        hasher.update(b"\x1f");
        if let Some(envelope) = self.budget_envelope {
            hasher.update(b"E");
            hasher.update(envelope.to_bits().to_le_bytes());
        } else {
            hasher.update(b"e");
        }
        hasher.update(b"\x1f");
        hasher.update(
            self.throughput_window_days
                .unwrap_or(DEFAULT_THROUGHPUT_WINDOW_DAYS)
                .to_le_bytes(),
        );
        hasher.update(b"\x1f");
        hasher.update(self.currency.as_deref().unwrap_or("").as_bytes());
        let digest = hasher.finalize();
        crate::robot::hex_lower(&digest[..8])
    }

    pub fn throughput_window_days(&self) -> u32 {
        self.throughput_window_days
            .unwrap_or(DEFAULT_THROUGHPUT_WINDOW_DAYS)
    }
}

/// Structured inputs echoed back in the output for provenance. All numeric
/// fields are plain values; no formatting or rounding is applied.
#[derive(Debug, Clone, Serialize)]
pub struct EconomicsInputs {
    pub hourly_rate: f64,
    pub hours_per_day: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_envelope: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    pub throughput_window_days: u32,
    pub project_age_days: i64,
    pub estimate_coverage_pct: f64,
    pub open_issues: usize,
    pub closed_in_window: usize,
}

/// One entry per top open blocker, sized by `dependents_count`.
///
/// Ordering is blocks_count desc — the same signal `--robot-overview`'s
/// `top_blocker` and `--robot-insights`' `Bottlenecks` use, so consumers
/// can cross-check.
#[derive(Debug, Clone, Serialize)]
pub struct CostOfDelayEntry {
    pub id: String,
    pub title: String,
    pub dependents_count: usize,
    /// Equal to `burn_rate_per_day` for every entry by construction — see the
    /// module doc. The per-entry field is retained so consumers that drop or
    /// reorder entries do not lose rate context.
    pub rate_per_day: f64,
}

/// Pure-arithmetic projections. Every field is either derivable from the
/// beads state + overlay or explicitly flagged in `guards`.
#[derive(Debug, Clone, Serialize)]
pub struct EconomicsProjections {
    pub burn_rate_per_day: f64,
    pub throughput_issues_per_day: f64,
    /// Projected remaining cost using estimate-based math when estimate
    /// coverage is above threshold, otherwise throughput-based. `None` when
    /// neither method has enough signal (both guards tripped).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_to_complete: Option<f64>,
    /// `None` when no budget_envelope was provided (not a data gap — an
    /// input gap). Distinguished from `0.0` (fully spent) by the option.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_utilization_pct: Option<f64>,
    pub cost_of_delay: Vec<CostOfDelayEntry>,
}

/// Booleans that tell downstream why a projection might be unreliable.
/// Explicit, not omitted — so a consumer can tell "absent" from "suppressed".
#[derive(Debug, Clone, Serialize)]
pub struct EconomicsGuards {
    pub estimate_coverage_below_threshold: bool,
    pub project_too_young_for_throughput: bool,
    pub zero_throughput: bool,
    pub no_budget_envelope: bool,
}

/// Inputs to [`compute_economics`]. Split from the overlay so callers can
/// substitute bottleneck data already computed for the overview surface
/// instead of re-running the analyzer.
pub struct EconomicsComputation<'a> {
    pub issues: &'a [Issue],
    pub overlay: &'a EconomicsOverlay,
    /// Top bottlenecks sorted by descending `dependents_count`. Passed in so
    /// this module does not depend on the full `Analyzer` surface — keeps
    /// unit tests small and makes it obvious what graph state drives the
    /// projection.
    pub bottlenecks: &'a [BottleneckRef],
    pub now: DateTime<Utc>,
    /// Cap for `cost_of_delay` entries. Matches the existing `insight_limit`
    /// CLI flag so operators can pass the same ceiling.
    pub cost_of_delay_limit: usize,
}

/// Minimal projection of an analyzer bottleneck that this module needs.
///
/// Keeping this local to the module means `src/analysis/economics.rs` has no
/// back-reference into analyzer internals beyond what the caller supplies.
#[derive(Debug, Clone)]
pub struct BottleneckRef {
    pub id: String,
    pub title: String,
    pub dependents_count: usize,
}

/// Top-level output. Flattens [`EconomicsInputs`], [`EconomicsProjections`],
/// and [`EconomicsGuards`] under `inputs` / `projections` / `guards` exactly
/// as the GH#12 strawman specifies.
///
/// The `RobotEnvelope` is flattened at top level (matching every other
/// `--robot-*` output) and `schema_version` is exposed here as a payload
/// field so downstream consumers can pin against refactors without relying
/// on `--robot-schema` round-trips.
#[derive(Debug, Clone, Serialize)]
pub struct RobotEconomicsOutput {
    #[serde(flatten)]
    pub envelope: crate::robot::RobotEnvelope,
    pub schema_version: &'static str,
    pub overlay_hash: String,
    pub inputs: EconomicsInputs,
    pub projections: EconomicsProjections,
    pub guards: EconomicsGuards,
}

/// Schema version for the robot-economics payload. Bump when any field is
/// renamed or its semantics shift. Adding new optional fields does not
/// require a bump.
pub const ECONOMICS_SCHEMA_VERSION: &str = "1";

pub fn compute_economics(computation: EconomicsComputation<'_>) -> RobotEconomicsOutput {
    let EconomicsComputation {
        issues,
        overlay,
        bottlenecks,
        now,
        cost_of_delay_limit,
    } = computation;

    let open_issues: Vec<&Issue> = issues.iter().filter(|issue| issue.is_open_like()).collect();
    let open_count = open_issues.len();
    let window_days = overlay.throughput_window_days();

    // Project age: max(days since earliest created_at, 0). Defaults to 0 when
    // no issue has a created_at — the guard below will fire.
    let project_age_days = project_age_days(issues, now);

    // Estimate coverage: fraction of open issues with estimated_minutes set.
    // `estimated_minutes` of 0 is treated as "no estimate" because the beads
    // spec allows callers to populate it with zero without meaning "zero
    // effort"; treating zero as signal would overstate coverage.
    let open_with_estimates: Vec<&Issue> = open_issues
        .iter()
        .copied()
        .filter(|issue| issue.estimated_minutes.unwrap_or(0) > 0)
        .collect();
    let estimate_coverage_pct = if open_count == 0 {
        0.0
    } else {
        open_with_estimates.len() as f64 / open_count as f64
    };

    // Throughput: issues closed in trailing window_days. Uses the same
    // is_closed_like predicate as the rest of the codebase so categorization
    // is consistent with --robot-overview counts.
    let window_start = now - Duration::days(window_days as i64);
    let closed_in_window = issues
        .iter()
        .filter(|issue| {
            issue.is_closed_like()
                && issue
                    .closed_at
                    .is_some_and(|closed_at| closed_at >= window_start && closed_at <= now)
        })
        .count();
    let throughput_issues_per_day = closed_in_window as f64 / window_days as f64;

    // Burn rate: pure arithmetic, independent of graph state.
    let burn_rate_per_day = overlay.hourly_rate * overlay.hours_per_day;

    // Guards gate the cost-to-complete calculation. Both tripped → None.
    let estimate_coverage_below_threshold =
        estimate_coverage_pct < ESTIMATE_COVERAGE_GUARD_THRESHOLD;
    let project_too_young_for_throughput = project_age_days < PROJECT_AGE_GUARD_THRESHOLD_DAYS;
    let zero_throughput = closed_in_window == 0;

    let cost_to_complete = if !estimate_coverage_below_threshold {
        // Estimate-based: sum known + average-imputed for the rest.
        let known_minutes: i64 = open_with_estimates
            .iter()
            .filter_map(|issue| issue.estimated_minutes)
            .map(i64::from)
            .sum();
        let avg_minutes = if open_with_estimates.is_empty() {
            0.0
        } else {
            known_minutes as f64 / open_with_estimates.len() as f64
        };
        let uncovered_count = open_count.saturating_sub(open_with_estimates.len());
        let total_minutes = known_minutes as f64 + (avg_minutes * uncovered_count as f64);
        Some((total_minutes / 60.0) * overlay.hourly_rate)
    } else if !zero_throughput && !project_too_young_for_throughput {
        // Throughput-based fallback: how many days to burn through open work
        // at the observed rate, times the burn rate.
        let days_remaining = open_count as f64 / throughput_issues_per_day;
        Some(days_remaining * burn_rate_per_day)
    } else {
        None
    };

    let budget_utilization_pct = overlay.budget_envelope.and_then(|envelope| {
        if envelope <= 0.0 {
            return None;
        }
        cost_to_complete.map(|remaining| (remaining / envelope).clamp(0.0, f64::INFINITY))
    });

    // `cost_of_delay`: one entry per top blocker (by dependents_count), each
    // carrying the team burn rate. The rate is the *team's* per-day cost,
    // not scaled by dependents, because:
    //   1) each blocker independently stalls its dependents at the full
    //      team rate (the team isn't split proportionally), and
    //   2) the strawman shape in GH#12 assigns identical `rate_per_day`
    //      to every entry — explicitly so downstream can aggregate by
    //      summing rate * day-held without needing to know the graph.
    // `dependents_count` is retained so consumers can still order or
    // threshold by scope-of-block separately from $/day.
    let cost_of_delay = bottlenecks
        .iter()
        .filter(|b| b.dependents_count > 0)
        .take(cost_of_delay_limit)
        .map(|b| CostOfDelayEntry {
            id: b.id.clone(),
            title: b.title.clone(),
            dependents_count: b.dependents_count,
            rate_per_day: burn_rate_per_day,
        })
        .collect::<Vec<_>>();

    let inputs = EconomicsInputs {
        hourly_rate: overlay.hourly_rate,
        hours_per_day: overlay.hours_per_day,
        budget_envelope: overlay.budget_envelope,
        currency: overlay.currency.clone(),
        throughput_window_days: window_days,
        project_age_days,
        estimate_coverage_pct,
        open_issues: open_count,
        closed_in_window,
    };

    let projections = EconomicsProjections {
        burn_rate_per_day,
        throughput_issues_per_day,
        cost_to_complete,
        budget_utilization_pct,
        cost_of_delay,
    };

    let guards = EconomicsGuards {
        estimate_coverage_below_threshold,
        project_too_young_for_throughput,
        zero_throughput,
        no_budget_envelope: overlay.budget_envelope.is_none(),
    };

    RobotEconomicsOutput {
        envelope: crate::robot::envelope(issues),
        schema_version: ECONOMICS_SCHEMA_VERSION,
        overlay_hash: overlay.canonical_hash(),
        inputs,
        projections,
        guards,
    }
}

fn project_age_days(issues: &[Issue], now: DateTime<Utc>) -> i64 {
    let earliest = issues.iter().filter_map(|issue| issue.created_at).min();
    earliest
        .map(|created_at| (now - created_at).num_days().max(0))
        .unwrap_or(0)
}

/// Facade hiding the construction of [`BottleneckRef`] so callers in
/// `main.rs` do not have to reach into analyzer internals.
pub fn bottlenecks_from_blocks_count(
    blocks_count: &std::collections::HashMap<String, usize>,
    title_by_id: &BTreeMap<&str, &str>,
    limit: usize,
) -> Vec<BottleneckRef> {
    let mut entries: Vec<(&str, &usize)> = blocks_count
        .iter()
        .filter(|(_, count)| **count > 0)
        .map(|(id, count)| (id.as_str(), count))
        .collect();
    entries.sort_by(|left, right| right.1.cmp(left.1).then_with(|| left.0.cmp(right.0)));
    entries
        .into_iter()
        .take(limit)
        .map(|(id, count)| BottleneckRef {
            id: id.to_string(),
            title: title_by_id.get(id).copied().unwrap_or("").to_string(),
            dependents_count: *count,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn issue(id: &str, status: &str, priority: i32, created: DateTime<Utc>) -> Issue {
        Issue {
            id: id.to_string(),
            title: format!("title of {id}"),
            status: status.to_string(),
            priority,
            issue_type: "task".to_string(),
            created_at: Some(created),
            ..Issue::default()
        }
    }

    fn overlay_basic() -> EconomicsOverlay {
        EconomicsOverlay {
            hourly_rate: 100.0,
            hours_per_day: 6.0,
            budget_envelope: Some(10_000.0),
            throughput_window_days: Some(30),
            currency: Some("USD".into()),
        }
    }

    #[test]
    fn burn_rate_is_pure_arithmetic_over_overlay() {
        let now = Utc.with_ymd_and_hms(2026, 4, 20, 0, 0, 0).unwrap();
        let issues = vec![issue("A-1", "open", 1, now - Duration::days(60))];
        let output = compute_economics(EconomicsComputation {
            issues: &issues,
            overlay: &overlay_basic(),
            bottlenecks: &[],
            now,
            cost_of_delay_limit: 20,
        });
        assert_eq!(output.projections.burn_rate_per_day, 600.0);
    }

    #[test]
    fn overlay_rejects_negative_rate() {
        let err = EconomicsOverlay::from_json_str(r#"{"hourly_rate": -1, "hours_per_day": 6}"#)
            .unwrap_err();
        assert!(err.contains("hourly_rate"));
    }

    #[test]
    fn overlay_rejects_hours_over_24() {
        let err = EconomicsOverlay::from_json_str(r#"{"hourly_rate": 50, "hours_per_day": 25}"#)
            .unwrap_err();
        assert!(err.contains("hours_per_day"));
    }

    #[test]
    fn overlay_hash_is_stable_across_runs() {
        let overlay = overlay_basic();
        let a = overlay.canonical_hash();
        let b = overlay.canonical_hash();
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn overlay_hash_changes_when_any_field_changes() {
        let base = overlay_basic().canonical_hash();
        let mut v = overlay_basic();
        v.hourly_rate = 101.0;
        assert_ne!(base, v.canonical_hash());
        let mut v = overlay_basic();
        v.hours_per_day = 7.0;
        assert_ne!(base, v.canonical_hash());
        let mut v = overlay_basic();
        v.budget_envelope = Some(10_001.0);
        assert_ne!(base, v.canonical_hash());
        let mut v = overlay_basic();
        v.budget_envelope = None;
        assert_ne!(base, v.canonical_hash());
        let mut v = overlay_basic();
        v.currency = Some("EUR".into());
        assert_ne!(base, v.canonical_hash());
    }

    #[test]
    fn zero_throughput_guard_trips_on_no_closures() {
        let now = Utc.with_ymd_and_hms(2026, 4, 20, 0, 0, 0).unwrap();
        let issues = vec![issue("A-1", "open", 1, now - Duration::days(60))];
        let output = compute_economics(EconomicsComputation {
            issues: &issues,
            overlay: &overlay_basic(),
            bottlenecks: &[],
            now,
            cost_of_delay_limit: 20,
        });
        assert!(output.guards.zero_throughput);
        assert_eq!(output.projections.throughput_issues_per_day, 0.0);
    }

    #[test]
    fn project_age_guard_trips_on_young_project() {
        let now = Utc.with_ymd_and_hms(2026, 4, 20, 0, 0, 0).unwrap();
        let issues = vec![issue("A-1", "open", 1, now - Duration::days(3))];
        let output = compute_economics(EconomicsComputation {
            issues: &issues,
            overlay: &overlay_basic(),
            bottlenecks: &[],
            now,
            cost_of_delay_limit: 20,
        });
        assert!(output.guards.project_too_young_for_throughput);
        assert_eq!(output.inputs.project_age_days, 3);
    }

    #[test]
    fn estimate_coverage_guard_trips_when_most_open_issues_lack_estimates() {
        let now = Utc.with_ymd_and_hms(2026, 4, 20, 0, 0, 0).unwrap();
        let mut issues = vec![
            issue("A-1", "open", 1, now - Duration::days(120)),
            issue("A-2", "open", 1, now - Duration::days(120)),
            issue("A-3", "open", 1, now - Duration::days(120)),
        ];
        issues[0].estimated_minutes = Some(120);
        // Only 1/3 have estimates → below 0.50 threshold.
        let output = compute_economics(EconomicsComputation {
            issues: &issues,
            overlay: &overlay_basic(),
            bottlenecks: &[],
            now,
            cost_of_delay_limit: 20,
        });
        assert!(output.guards.estimate_coverage_below_threshold);
        assert!(
            (output.inputs.estimate_coverage_pct - (1.0 / 3.0)).abs() < 1e-9,
            "got {}",
            output.inputs.estimate_coverage_pct
        );
    }

    #[test]
    fn cost_to_complete_uses_estimates_when_coverage_meets_threshold() {
        let now = Utc.with_ymd_and_hms(2026, 4, 20, 0, 0, 0).unwrap();
        let mut issues = vec![
            issue("A-1", "open", 1, now - Duration::days(120)),
            issue("A-2", "open", 1, now - Duration::days(120)),
        ];
        issues[0].estimated_minutes = Some(60);
        issues[1].estimated_minutes = Some(60);
        // 2 open, both estimated 60 min → 120 total minutes → 2 hours →
        // 2 * $100/hr = $200.
        let output = compute_economics(EconomicsComputation {
            issues: &issues,
            overlay: &overlay_basic(),
            bottlenecks: &[],
            now,
            cost_of_delay_limit: 20,
        });
        assert_eq!(output.projections.cost_to_complete, Some(200.0));
        assert!(!output.guards.estimate_coverage_below_threshold);
    }

    #[test]
    fn cost_to_complete_falls_back_to_throughput_when_coverage_insufficient() {
        let now = Utc.with_ymd_and_hms(2026, 4, 20, 0, 0, 0).unwrap();
        let mut issues = Vec::new();
        // 2 open, 0 estimates → coverage 0 → fall back to throughput.
        for i in 0..2 {
            issues.push(issue(
                &format!("A-{i}"),
                "open",
                1,
                now - Duration::days(200),
            ));
        }
        // 6 closed in window (6/30 ≈ 0.2 issues/day). Project is old enough.
        for i in 0..6 {
            let mut closed = issue(&format!("C-{i}"), "closed", 1, now - Duration::days(200));
            closed.closed_at = Some(now - Duration::days(i as i64 + 1));
            issues.push(closed);
        }
        let output = compute_economics(EconomicsComputation {
            issues: &issues,
            overlay: &overlay_basic(),
            bottlenecks: &[],
            now,
            cost_of_delay_limit: 20,
        });
        // Throughput fallback: open / rate * burn
        // = 2 / 0.2 * 600 = 6000
        let cost = output.projections.cost_to_complete.expect("cost");
        assert!((cost - 6000.0).abs() < 1e-6, "got {cost}");
        assert!(output.guards.estimate_coverage_below_threshold);
    }

    #[test]
    fn cost_to_complete_is_none_when_both_methods_unusable() {
        let now = Utc.with_ymd_and_hms(2026, 4, 20, 0, 0, 0).unwrap();
        // Young project, zero estimates, zero throughput.
        let issues = vec![issue("A-1", "open", 1, now - Duration::days(3))];
        let output = compute_economics(EconomicsComputation {
            issues: &issues,
            overlay: &overlay_basic(),
            bottlenecks: &[],
            now,
            cost_of_delay_limit: 20,
        });
        assert!(output.projections.cost_to_complete.is_none());
        assert!(output.guards.estimate_coverage_below_threshold);
        assert!(output.guards.project_too_young_for_throughput);
        assert!(output.guards.zero_throughput);
    }

    #[test]
    fn cost_of_delay_rate_equals_burn_rate_for_every_entry() {
        let now = Utc.with_ymd_and_hms(2026, 4, 20, 0, 0, 0).unwrap();
        let bottlenecks = vec![
            BottleneckRef {
                id: "B-1".into(),
                title: "big block".into(),
                dependents_count: 9,
            },
            BottleneckRef {
                id: "B-2".into(),
                title: "small block".into(),
                dependents_count: 2,
            },
        ];
        let overlay = overlay_basic();
        let expected_burn = overlay.hourly_rate * overlay.hours_per_day;
        let output = compute_economics(EconomicsComputation {
            issues: &[],
            overlay: &overlay,
            bottlenecks: &bottlenecks,
            now,
            cost_of_delay_limit: 20,
        });
        assert_eq!(output.projections.cost_of_delay.len(), 2);
        for entry in &output.projections.cost_of_delay {
            assert_eq!(
                entry.rate_per_day, expected_burn,
                "rate_per_day must equal burn_rate_per_day for every cost_of_delay entry — see GH#12"
            );
        }
    }

    #[test]
    fn cost_of_delay_ordering_matches_bottleneck_input_order() {
        let now = Utc.with_ymd_and_hms(2026, 4, 20, 0, 0, 0).unwrap();
        let bottlenecks = vec![
            BottleneckRef {
                id: "B-hi".into(),
                title: "hi".into(),
                dependents_count: 9,
            },
            BottleneckRef {
                id: "B-med".into(),
                title: "med".into(),
                dependents_count: 4,
            },
            BottleneckRef {
                id: "B-lo".into(),
                title: "lo".into(),
                dependents_count: 1,
            },
        ];
        let output = compute_economics(EconomicsComputation {
            issues: &[],
            overlay: &overlay_basic(),
            bottlenecks: &bottlenecks,
            now,
            cost_of_delay_limit: 20,
        });
        let ids: Vec<&str> = output
            .projections
            .cost_of_delay
            .iter()
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(ids, vec!["B-hi", "B-med", "B-lo"]);
    }

    #[test]
    fn cost_of_delay_skips_zero_dependents_entries() {
        let now = Utc.with_ymd_and_hms(2026, 4, 20, 0, 0, 0).unwrap();
        let bottlenecks = vec![
            BottleneckRef {
                id: "B-hi".into(),
                title: "hi".into(),
                dependents_count: 5,
            },
            BottleneckRef {
                id: "B-zero".into(),
                title: "zero".into(),
                dependents_count: 0,
            },
        ];
        let output = compute_economics(EconomicsComputation {
            issues: &[],
            overlay: &overlay_basic(),
            bottlenecks: &bottlenecks,
            now,
            cost_of_delay_limit: 20,
        });
        assert_eq!(output.projections.cost_of_delay.len(), 1);
        assert_eq!(output.projections.cost_of_delay[0].id, "B-hi");
    }

    #[test]
    fn cost_of_delay_respects_limit() {
        let now = Utc.with_ymd_and_hms(2026, 4, 20, 0, 0, 0).unwrap();
        let bottlenecks: Vec<BottleneckRef> = (0..5)
            .map(|i| BottleneckRef {
                id: format!("B-{i}"),
                title: "".into(),
                dependents_count: 10 - i,
            })
            .collect();
        let output = compute_economics(EconomicsComputation {
            issues: &[],
            overlay: &overlay_basic(),
            bottlenecks: &bottlenecks,
            now,
            cost_of_delay_limit: 3,
        });
        assert_eq!(output.projections.cost_of_delay.len(), 3);
    }

    #[test]
    fn budget_utilization_none_when_no_envelope_provided() {
        let now = Utc.with_ymd_and_hms(2026, 4, 20, 0, 0, 0).unwrap();
        let mut overlay = overlay_basic();
        overlay.budget_envelope = None;
        let mut issues = vec![issue("A-1", "open", 1, now - Duration::days(200))];
        issues[0].estimated_minutes = Some(60);
        let output = compute_economics(EconomicsComputation {
            issues: &issues,
            overlay: &overlay,
            bottlenecks: &[],
            now,
            cost_of_delay_limit: 20,
        });
        assert!(output.projections.budget_utilization_pct.is_none());
        assert!(output.guards.no_budget_envelope);
    }

    #[test]
    fn output_is_structurally_deterministic_for_fixed_inputs() {
        // Same beads state + same overlay + same `now` → structurally
        // identical output modulo `envelope.generated_at`. Matches the
        // contract other --robot-* commands already offer.
        let now = Utc.with_ymd_and_hms(2026, 4, 20, 0, 0, 0).unwrap();
        let issues = vec![issue("A-1", "open", 1, now - Duration::days(120))];
        let a = compute_economics(EconomicsComputation {
            issues: &issues,
            overlay: &overlay_basic(),
            bottlenecks: &[],
            now,
            cost_of_delay_limit: 20,
        });
        let b = compute_economics(EconomicsComputation {
            issues: &issues,
            overlay: &overlay_basic(),
            bottlenecks: &[],
            now,
            cost_of_delay_limit: 20,
        });
        assert_eq!(a.overlay_hash, b.overlay_hash);
        assert_eq!(a.inputs.project_age_days, b.inputs.project_age_days);
        assert_eq!(
            a.projections.burn_rate_per_day,
            b.projections.burn_rate_per_day
        );
        assert_eq!(
            a.projections.cost_to_complete,
            b.projections.cost_to_complete
        );
    }

    #[test]
    fn cost_of_delay_ids_match_top_bottlenecks_for_cross_surface_coherence() {
        // cost_of_delay ordering should derive from the same blocks_count
        // signal --robot-overview uses for top_blocker and --robot-insights
        // uses for Bottlenecks; this test pins the invariant the GH#12
        // regression-surface discussion called out.
        let now = Utc.with_ymd_and_hms(2026, 4, 20, 0, 0, 0).unwrap();
        let blocks_count: std::collections::HashMap<String, usize> = [
            ("TOP".to_string(), 7),
            ("MID".to_string(), 3),
            ("LOW".to_string(), 1),
        ]
        .into();
        let title_by_id: BTreeMap<&str, &str> =
            [("TOP", "top"), ("MID", "mid"), ("LOW", "low")].into();
        let top = bottlenecks_from_blocks_count(&blocks_count, &title_by_id, 20);

        let output = compute_economics(EconomicsComputation {
            issues: &[],
            overlay: &overlay_basic(),
            bottlenecks: &top,
            now,
            cost_of_delay_limit: 20,
        });
        let ids: Vec<&str> = output
            .projections
            .cost_of_delay
            .iter()
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(ids, vec!["TOP", "MID", "LOW"]);
    }
}
