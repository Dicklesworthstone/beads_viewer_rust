use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::git_history::{HistoryBeadCompat, HistoryCommitCompat, HistoryFileChangeCompat};

// ---------------------------------------------------------------------------
// Orphan detection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct OrphanCandidate {
    pub sha: String,
    pub short_sha: String,
    pub message: String,
    pub author: String,
    pub author_email: String,
    pub timestamp: String,
    pub files: Vec<String>,
    pub suspicion_score: u32,
    pub probable_beads: Vec<ProbableBead>,
    pub signals: Vec<OrphanSignalHit>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProbableBead {
    pub bead_id: String,
    pub title: String,
    pub status: String,
    pub confidence: u32,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrphanSignalHit {
    pub signal: String,
    pub weight: u32,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrphanStats {
    pub total_commits: usize,
    pub correlated_count: usize,
    pub orphan_count: usize,
    pub candidate_count: usize,
    pub orphan_ratio: f64,
    pub avg_suspicion: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrphanReport {
    pub stats: OrphanStats,
    pub candidates: Vec<OrphanCandidate>,
}

/// Detect orphan commits — commits not correlated with any bead.
///
/// Uses heuristics (message patterns, file overlap, author proximity) to
/// score orphans and suggest probable bead links.
#[must_use]
pub fn detect_orphans(
    all_commits: &[super::git_history::GitCommitRecord],
    histories: &BTreeMap<String, HistoryBeadCompat>,
    commit_index: &BTreeMap<String, Vec<String>>,
    min_score: u32,
) -> OrphanReport {
    // Build file→bead index for file overlap checks
    let file_bead_map = build_file_bead_map(histories);

    // Build author→recent-commit-timestamps for author proximity
    let author_linked: BTreeMap<String, Vec<&str>> = {
        let mut map = BTreeMap::<String, Vec<&str>>::new();
        for history in histories.values() {
            for commit in history.commits.as_deref().unwrap_or_default() {
                let key = commit.author_email.to_ascii_lowercase();
                map.entry(key).or_default().push(&commit.timestamp);
            }
        }
        map
    };

    let total_commits = all_commits.len();
    let mut orphan_commits = Vec::new();

    for commit in all_commits {
        if commit_index.contains_key(&commit.sha) {
            continue; // Already correlated
        }

        let files: Vec<String> = commit.files.iter().map(|f| f.path.clone()).collect();

        let mut signals = Vec::new();
        let mut probable_beads_map = BTreeMap::<String, (u32, Vec<String>)>::new();

        // Signal 1: Message patterns
        check_message_patterns(&commit.message, &mut signals);

        // Signal 2: File overlap with bead-touched files
        for file_path in &files {
            let normalized = normalize_path(file_path);
            if let Some(bead_ids) = file_bead_map.get(&normalized) {
                for bead_id in bead_ids {
                    let entry = probable_beads_map
                        .entry(bead_id.clone())
                        .or_insert_with(|| (0, Vec::new()));
                    entry.0 += 25;
                    entry.1.push(format!("File overlap: {}", normalized));
                }
                if signals
                    .iter()
                    .all(|s: &OrphanSignalHit| s.signal != "file_overlap")
                {
                    signals.push(OrphanSignalHit {
                        signal: "file_overlap".to_string(),
                        weight: 25,
                        detail: format!(
                            "{} file(s) overlap with bead-tracked files",
                            files.len().min(3)
                        ),
                    });
                }
            }
        }

        // Signal 3: Author proximity (author has linked commits)
        let author_key = commit.author_email.to_ascii_lowercase();
        if author_linked.contains_key(&author_key) {
            signals.push(OrphanSignalHit {
                signal: "author_proximity".to_string(),
                weight: 15,
                detail: format!("Author {} has linked commits", commit.author),
            });
        }

        let total_score: u32 = signals.iter().map(|s| s.weight).sum::<u32>().min(100);

        if total_score < min_score {
            continue;
        }

        // Build probable beads list (top 3)
        let mut probable_beads: Vec<ProbableBead> = probable_beads_map
            .into_iter()
            .filter_map(|(bead_id, (conf, reasons))| {
                histories.get(&bead_id).map(|h| ProbableBead {
                    bead_id: h.bead_id.clone(),
                    title: h.title.clone(),
                    status: h.status.clone(),
                    confidence: conf.min(100),
                    reasons,
                })
            })
            .collect();
        probable_beads.sort_by(|a, b| b.confidence.cmp(&a.confidence));
        probable_beads.truncate(3);

        orphan_commits.push(OrphanCandidate {
            sha: commit.sha.clone(),
            short_sha: commit.short_sha.clone(),
            message: commit.message.clone(),
            author: commit.author.clone(),
            author_email: commit.author_email.clone(),
            timestamp: commit.timestamp.clone(),
            files,
            suspicion_score: total_score,
            probable_beads,
            signals,
        });
    }

    orphan_commits.sort_by(|a, b| {
        b.suspicion_score
            .cmp(&a.suspicion_score)
            .then_with(|| a.sha.cmp(&b.sha))
    });

    let correlated_count = commit_index.len();
    let orphan_count = total_commits.saturating_sub(correlated_count);
    let candidate_count = orphan_commits.len();
    let avg_suspicion = if candidate_count > 0 {
        orphan_commits
            .iter()
            .map(|c| f64::from(c.suspicion_score))
            .sum::<f64>()
            / candidate_count as f64
    } else {
        0.0
    };
    let orphan_ratio = if total_commits > 0 {
        orphan_count as f64 / total_commits as f64
    } else {
        0.0
    };

    OrphanReport {
        stats: OrphanStats {
            total_commits,
            correlated_count,
            orphan_count,
            candidate_count,
            orphan_ratio,
            avg_suspicion,
        },
        candidates: orphan_commits,
    }
}

fn check_message_patterns(message: &str, signals: &mut Vec<OrphanSignalHit>) {
    let lower = message.to_ascii_lowercase();

    let word_patterns: &[(&[&str], &str, u32)] = &[
        (&["fix", "fixed", "fixes"], "fix/fixed pattern", 10),
        (&["close", "closed", "closes"], "close/closes pattern", 10),
        (&["resolve", "resolved", "resolves"], "resolve pattern", 10),
        (
            &["implement", "implemented", "implements"],
            "implement pattern",
            8,
        ),
        (&["add", "added", "adds"], "add/added pattern", 5),
    ];

    let mut total_weight = 0u32;

    for (words, detail, weight) in word_patterns {
        if words.iter().any(|w| has_word_boundary(&lower, w)) {
            total_weight += weight;
            signals.push(OrphanSignalHit {
                signal: "message_pattern".to_string(),
                weight: *weight,
                detail: detail.to_string(),
            });
        }
    }

    // Check for issue reference (#N)
    if has_issue_ref_pattern(&lower) {
        total_weight += 15;
        signals.push(OrphanSignalHit {
            signal: "message_pattern".to_string(),
            weight: 15,
            detail: "issue reference (#N)".to_string(),
        });
    }

    // Check for bead-like ID pattern (word-word-digits)
    if has_bead_id_pattern(&lower) {
        total_weight += 20;
        signals.push(OrphanSignalHit {
            signal: "message_pattern".to_string(),
            weight: 20,
            detail: "bead-like ID pattern".to_string(),
        });
    }

    // Cap total message signal weight at 35
    if total_weight > 35 {
        let excess = total_weight - 35;
        let mut remaining = excess;
        for signal in signals.iter_mut().rev() {
            if signal.signal == "message_pattern" && remaining > 0 {
                let reduction = signal.weight.min(remaining);
                signal.weight -= reduction;
                remaining -= reduction;
            }
        }
    }
}

fn has_word_boundary(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(start, matched)| {
        let left = if start > 0 {
            text.as_bytes().get(start - 1).copied()
        } else {
            None
        };
        let right = text.as_bytes().get(start + matched.len()).copied();

        let left_ok = left.is_none_or(|c| !c.is_ascii_alphanumeric());
        let right_ok = right.is_none_or(|c| !c.is_ascii_alphanumeric());
        left_ok && right_ok
    })
}

fn has_issue_ref_pattern(text: &str) -> bool {
    // Look for #N pattern
    text.match_indices('#').any(|(pos, _)| {
        text[pos + 1..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit())
    })
}

fn has_bead_id_pattern(text: &str) -> bool {
    // Look for patterns like "bd-123", "bv-abc", "feat-42"
    for (pos, _) in text.match_indices('-') {
        // Check left side has alpha chars
        let left = &text[..pos];
        let has_alpha_left = left
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_alphanumeric())
            .any(|c| c.is_ascii_alphabetic());
        // Check right side has digits
        let right = &text[pos + 1..];
        let has_digit_right = right
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .any(|c| c.is_ascii_digit());

        if has_alpha_left && has_digit_right {
            return true;
        }
    }
    false
}

fn build_file_bead_map(
    histories: &BTreeMap<String, HistoryBeadCompat>,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut map = BTreeMap::<String, BTreeSet<String>>::new();
    for history in histories.values() {
        for commit in history.commits.as_deref().unwrap_or_default() {
            for file in &commit.files {
                let normalized = normalize_path(&file.path);
                map.entry(normalized)
                    .or_default()
                    .insert(history.bead_id.clone());
            }
        }
    }
    map
}

fn normalize_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let normalized = normalized.strip_prefix("./").unwrap_or(&normalized);
    normalized.trim_end_matches('/').to_string()
}

// ---------------------------------------------------------------------------
// File-to-bead mapping
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct BeadReference {
    pub bead_id: String,
    pub title: String,
    pub status: String,
    pub commit_count: usize,
    pub last_touch: String,
    pub total_changes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileBeadLookupResult {
    pub file_path: String,
    pub open_beads: Vec<BeadReference>,
    pub closed_beads: Vec<BeadReference>,
    pub total_beads: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileIndexStats {
    pub total_files: usize,
    pub total_bead_links: usize,
    pub files_with_multiple_beads: usize,
}

/// Look up which beads have touched a given file path.
///
/// Supports exact match and prefix (directory) matching.
#[must_use]
pub fn lookup_file_beads(
    path: &str,
    histories: &BTreeMap<String, HistoryBeadCompat>,
    closed_limit: usize,
) -> FileBeadLookupResult {
    let target = normalize_path(path);
    let mut bead_refs = BTreeMap::<String, (Vec<String>, String, i64)>::new();

    for history in histories.values() {
        for commit in history.commits.as_deref().unwrap_or_default() {
            let matches = commit.files.iter().any(|f| {
                let norm = normalize_path(&f.path);
                norm == target || norm.starts_with(&format!("{target}/"))
            });

            if matches {
                let entry = bead_refs
                    .entry(history.bead_id.clone())
                    .or_insert_with(|| (Vec::new(), String::new(), 0));
                entry.0.push(commit.sha.clone());
                if entry.1.is_empty() || commit.timestamp > entry.1 {
                    entry.1 = commit.timestamp.clone();
                }
                for f in &commit.files {
                    let norm = normalize_path(&f.path);
                    if norm == target || norm.starts_with(&format!("{target}/")) {
                        entry.2 += f.insertions + f.deletions;
                    }
                }
            }
        }
    }

    let mut open_beads = Vec::new();
    let mut closed_beads = Vec::new();

    for (bead_id, (shas, last_touch, total_changes)) in &bead_refs {
        let Some(history) = histories.get(bead_id) else {
            continue;
        };

        let reference = BeadReference {
            bead_id: bead_id.clone(),
            title: history.title.clone(),
            status: history.status.clone(),
            commit_count: shas.len(),
            last_touch: last_touch.clone(),
            total_changes: *total_changes,
        };

        if is_open_status(&history.status) {
            open_beads.push(reference);
        } else {
            closed_beads.push(reference);
        }
    }

    // Sort by commit_count descending, then by bead_id
    open_beads.sort_by(|a, b| {
        b.commit_count
            .cmp(&a.commit_count)
            .then_with(|| a.bead_id.cmp(&b.bead_id))
    });
    closed_beads.sort_by(|a, b| {
        b.commit_count
            .cmp(&a.commit_count)
            .then_with(|| a.bead_id.cmp(&b.bead_id))
    });

    if closed_limit > 0 {
        closed_beads.truncate(closed_limit);
    }

    let total_beads = open_beads.len() + closed_beads.len();

    FileBeadLookupResult {
        file_path: target,
        open_beads,
        closed_beads,
        total_beads,
    }
}

fn is_open_status(status: &str) -> bool {
    matches!(
        status.to_ascii_lowercase().as_str(),
        "open" | "in_progress" | "blocked" | "ready"
    )
}

// ---------------------------------------------------------------------------
// File hotspots
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct FileHotspot {
    pub file_path: String,
    pub total_beads: usize,
    pub open_beads: usize,
    pub closed_beads: usize,
}

/// Find files touched by the most beads ("hotspots").
#[must_use]
pub fn compute_hotspots(
    histories: &BTreeMap<String, HistoryBeadCompat>,
    limit: usize,
) -> Vec<FileHotspot> {
    // file → set of (bead_id, is_open)
    let mut file_beads = BTreeMap::<String, BTreeMap<String, bool>>::new();

    for history in histories.values() {
        let is_open = is_open_status(&history.status);
        for commit in history.commits.as_deref().unwrap_or_default() {
            for file in &commit.files {
                let normalized = normalize_path(&file.path);
                file_beads
                    .entry(normalized)
                    .or_default()
                    .insert(history.bead_id.clone(), is_open);
            }
        }
    }

    let mut hotspots: Vec<FileHotspot> = file_beads
        .into_iter()
        .map(|(path, beads)| {
            let open = beads.values().filter(|&&is_open| is_open).count();
            let closed = beads.len() - open;
            FileHotspot {
                file_path: path,
                total_beads: beads.len(),
                open_beads: open,
                closed_beads: closed,
            }
        })
        .collect();

    hotspots.sort_by(|a, b| {
        b.total_beads
            .cmp(&a.total_beads)
            .then_with(|| a.file_path.cmp(&b.file_path))
    });

    if limit > 0 {
        hotspots.truncate(limit);
    }

    hotspots
}

/// Compute file index statistics.
#[must_use]
pub fn compute_file_index_stats(histories: &BTreeMap<String, HistoryBeadCompat>) -> FileIndexStats {
    let mut file_beads = BTreeMap::<String, BTreeSet<String>>::new();

    for history in histories.values() {
        for commit in history.commits.as_deref().unwrap_or_default() {
            for file in &commit.files {
                let normalized = normalize_path(&file.path);
                file_beads
                    .entry(normalized)
                    .or_default()
                    .insert(history.bead_id.clone());
            }
        }
    }

    let total_files = file_beads.len();
    let total_bead_links: usize = file_beads.values().map(|s| s.len()).sum();
    let files_with_multiple_beads = file_beads.values().filter(|s| s.len() > 1).count();

    FileIndexStats {
        total_files,
        total_bead_links,
        files_with_multiple_beads,
    }
}

// ---------------------------------------------------------------------------
// Robot output structs
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct RobotOrphansOutput {
    pub generated_at: String,
    pub data_hash: String,
    pub output_format: String,
    pub version: String,
    #[serde(flatten)]
    pub report: OrphanReport,
}

#[derive(Debug, Serialize)]
pub struct RobotFileBeadsOutput {
    pub generated_at: String,
    pub data_hash: String,
    pub output_format: String,
    pub version: String,
    #[serde(flatten)]
    pub result: FileBeadLookupResult,
}

#[derive(Debug, Serialize)]
pub struct RobotFileHotspotsOutput {
    pub generated_at: String,
    pub data_hash: String,
    pub output_format: String,
    pub version: String,
    pub hotspots: Vec<FileHotspot>,
    pub stats: FileIndexStats,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::git_history::{
        GitCommitRecord, HistoryBeadCompat, HistoryCommitCompat, HistoryFileChangeCompat,
        HistoryMilestonesCompat,
    };

    fn make_history(bead_id: &str, status: &str, files: &[&str]) -> HistoryBeadCompat {
        let commits = files
            .iter()
            .enumerate()
            .map(|(i, path)| HistoryCommitCompat {
                sha: format!("commit-{bead_id}-{i}"),
                short_sha: format!("c{i}"),
                message: format!("work on {bead_id}"),
                author: "TestUser".to_string(),
                author_email: "test@example.com".to_string(),
                timestamp: format!("2026-01-{:02}T10:00:00Z", i + 1),
                files: vec![HistoryFileChangeCompat {
                    path: path.to_string(),
                    action: "M".to_string(),
                    insertions: 10,
                    deletions: 2,
                }],
                method: "explicit_id".to_string(),
                confidence: 0.85,
                reason: "test".to_string(),
            })
            .collect();

        HistoryBeadCompat {
            bead_id: bead_id.to_string(),
            title: format!("Bead {bead_id}"),
            status: status.to_string(),
            events: vec![],
            milestones: HistoryMilestonesCompat::default(),
            commits: Some(commits),
            cycle_time: None,
            last_author: "TestUser".to_string(),
        }
    }

    fn make_git_commit(sha: &str, message: &str, files: &[&str]) -> GitCommitRecord {
        GitCommitRecord {
            sha: sha.to_string(),
            short_sha: sha[..7.min(sha.len())].to_string(),
            timestamp: "2026-01-15T10:00:00Z".to_string(),
            author: "TestUser".to_string(),
            author_email: "test@example.com".to_string(),
            message: message.to_string(),
            files: files
                .iter()
                .map(|p| HistoryFileChangeCompat {
                    path: p.to_string(),
                    action: "M".to_string(),
                    insertions: 5,
                    deletions: 1,
                })
                .collect(),
            changed_beads: false,
            changed_non_beads: true,
        }
    }

    #[test]
    fn orphan_detection_basic() {
        let mut histories = BTreeMap::new();
        histories.insert(
            "bd-1".to_string(),
            make_history("bd-1", "open", &["src/main.rs"]),
        );

        let mut commit_index = BTreeMap::new();
        commit_index.insert("commit-bd-1-0".to_string(), vec!["bd-1".to_string()]);

        let all_commits = vec![
            make_git_commit("commit-bd-1-0", "work on bd-1", &["src/main.rs"]),
            make_git_commit("orphan-sha-001", "fix bug in main", &["src/main.rs"]),
        ];

        let report = detect_orphans(&all_commits, &histories, &commit_index, 0);

        assert_eq!(report.stats.total_commits, 2);
        assert_eq!(report.stats.correlated_count, 1);
        assert_eq!(report.stats.orphan_count, 1);
        assert!(report.candidates.len() >= 1);
        assert_eq!(report.candidates[0].sha, "orphan-sha-001");
    }

    #[test]
    fn orphan_min_score_filter() {
        let histories = BTreeMap::new();
        let commit_index = BTreeMap::new();
        let all_commits = vec![make_git_commit("sha-1", "update docs", &["README.md"])];

        let report_low = detect_orphans(&all_commits, &histories, &commit_index, 0);
        let report_high = detect_orphans(&all_commits, &histories, &commit_index, 90);

        assert!(report_low.candidates.len() >= report_high.candidates.len());
    }

    #[test]
    fn file_beads_lookup() {
        let mut histories = BTreeMap::new();
        histories.insert(
            "bd-1".to_string(),
            make_history("bd-1", "open", &["src/lib.rs", "src/main.rs"]),
        );
        histories.insert(
            "bd-2".to_string(),
            make_history("bd-2", "closed", &["src/lib.rs"]),
        );

        let result = lookup_file_beads("src/lib.rs", &histories, 20);

        assert_eq!(result.file_path, "src/lib.rs");
        assert_eq!(result.open_beads.len(), 1);
        assert_eq!(result.closed_beads.len(), 1);
        assert_eq!(result.total_beads, 2);
    }

    #[test]
    fn file_beads_closed_limit() {
        let mut histories = BTreeMap::new();
        for i in 0..5 {
            histories.insert(
                format!("bd-c{i}"),
                make_history(&format!("bd-c{i}"), "closed", &["shared.rs"]),
            );
        }

        let result = lookup_file_beads("shared.rs", &histories, 2);
        assert_eq!(result.closed_beads.len(), 2);
    }

    #[test]
    fn hotspots_ranking() {
        let mut histories = BTreeMap::new();
        histories.insert(
            "bd-1".to_string(),
            make_history("bd-1", "open", &["src/hot.rs", "src/cold.rs"]),
        );
        histories.insert(
            "bd-2".to_string(),
            make_history("bd-2", "open", &["src/hot.rs"]),
        );
        histories.insert(
            "bd-3".to_string(),
            make_history("bd-3", "closed", &["src/hot.rs"]),
        );

        let hotspots = compute_hotspots(&histories, 10);

        assert!(!hotspots.is_empty());
        assert_eq!(hotspots[0].file_path, "src/hot.rs");
        assert_eq!(hotspots[0].total_beads, 3);
        assert_eq!(hotspots[0].open_beads, 2);
        assert_eq!(hotspots[0].closed_beads, 1);
    }

    #[test]
    fn hotspots_limit() {
        let mut histories = BTreeMap::new();
        for i in 0..10 {
            histories.insert(
                format!("bd-{i}"),
                make_history(&format!("bd-{i}"), "open", &[&format!("file{i}.rs")]),
            );
        }

        let hotspots = compute_hotspots(&histories, 3);
        assert!(hotspots.len() <= 3);
    }

    #[test]
    fn file_index_stats() {
        let mut histories = BTreeMap::new();
        histories.insert(
            "bd-1".to_string(),
            make_history("bd-1", "open", &["a.rs", "b.rs"]),
        );
        histories.insert(
            "bd-2".to_string(),
            make_history("bd-2", "open", &["b.rs", "c.rs"]),
        );

        let stats = compute_file_index_stats(&histories);
        assert_eq!(stats.total_files, 3); // a.rs, b.rs, c.rs
        assert_eq!(stats.total_bead_links, 4); // bd-1:a, bd-1:b, bd-2:b, bd-2:c
        assert_eq!(stats.files_with_multiple_beads, 1); // b.rs
    }

    #[test]
    fn normalize_path_consistency() {
        assert_eq!(normalize_path("src\\main.rs"), "src/main.rs");
        assert_eq!(normalize_path("./src/main.rs"), "src/main.rs");
        assert_eq!(normalize_path("src/dir/"), "src/dir");
        assert_eq!(normalize_path("src/main.rs"), "src/main.rs");
    }

    #[test]
    fn empty_histories_produce_empty_results() {
        let histories = BTreeMap::new();
        let hotspots = compute_hotspots(&histories, 10);
        assert!(hotspots.is_empty());

        let result = lookup_file_beads("any.rs", &histories, 20);
        assert_eq!(result.total_beads, 0);

        let stats = compute_file_index_stats(&histories);
        assert_eq!(stats.total_files, 0);
    }
}
