//! AGENTS.md blurb management for AI coding agents.
//!
//! Detects, injects, updates, and removes standardised beads workflow
//! instructions in agent configuration files (AGENTS.md, CLAUDE.md, etc.).

use std::path::{Path, PathBuf};

use crate::{BvrError, Result};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Current blurb format version. Increment on breaking changes.
const BLURB_VERSION: u32 = 1;

const BLURB_START_MARKER: &str = "<!-- bv-agent-instructions-v1 -->";
const BLURB_END_MARKER: &str = "<!-- end-bv-agent-instructions -->";

/// Ordered preference for agent file names.
const SUPPORTED_FILES: &[&str] = &["AGENTS.md", "CLAUDE.md", "agents.md", "claude.md"];

/// Beads workflow instructions blurb.
const AGENT_BLURB: &str = r#"<!-- bv-agent-instructions-v1 -->

---

## Beads Workflow Integration

This project uses [beads_rust](https://github.com/Dicklesworthstone/beads_rust) (`br`) for issue tracking. Issues are stored in `.beads/` and tracked in git.

**Important:** `br` is non-invasive—it NEVER runs git commands automatically. After `br sync --flush-only`, you must manually commit changes.

### Essential Commands

```bash
# View issues (launches TUI - avoid in automated sessions)
bv

# CLI commands for agents (use these instead)
br ready              # Show issues ready to work (no blockers)
br list --status=open # All open issues
br show <id>          # Full issue details with dependencies
br create --title="..." --type=task --priority=2
br update <id> --status=in_progress
br close <id> --reason "Completed"
br close <id1> <id2>  # Close multiple issues at once
br sync --flush-only  # Export to JSONL (NO git operations)
```

### Workflow Pattern

1. **Start**: Run `br ready` to find actionable work
2. **Claim**: Use `br update <id> --status=in_progress`
3. **Work**: Implement the task
4. **Complete**: Use `br close <id>`
5. **Sync**: Run `br sync --flush-only` then manually commit

### Key Concepts

- **Dependencies**: Issues can block other issues. `br ready` shows only unblocked work.
- **Priority**: P0=critical, P1=high, P2=medium, P3=low, P4=backlog (use numbers, not words)
- **Types**: task, bug, feature, epic, question, docs
- **Blocking**: `br dep add <issue> <depends-on>` to add dependencies

### Session Protocol

**Before ending any session, run this checklist:**

```bash
git status              # Check what changed
git add <files>         # Stage code changes
br sync --flush-only    # Export beads to JSONL
git add .beads/         # Stage beads changes
git commit -m "..."     # Commit everything together
git push                # Push to remote
```

### Best Practices

- Check `br ready` at session start to find available work
- Update status as you work (in_progress -> closed)
- Create new issues with `br create` when you discover tasks
- Use descriptive titles and set appropriate priority/type
- Always `br sync --flush-only && git add .beads/` before ending session

<!-- end-bv-agent-instructions -->"#;

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

/// Result of detecting an agent config file.
#[derive(Debug, Clone, Default)]
pub struct AgentFileDetection {
    pub file_path: Option<PathBuf>,
    pub file_type: String,
    pub has_blurb: bool,
    pub has_legacy_blurb: bool,
    pub blurb_version: u32,
    pub content: String,
}

impl AgentFileDetection {
    pub fn found(&self) -> bool {
        self.file_path.is_some()
    }

    pub fn needs_blurb(&self) -> bool {
        self.found() && !self.has_blurb
    }

    pub fn needs_upgrade(&self) -> bool {
        if self.has_legacy_blurb {
            return true;
        }
        self.has_blurb && self.blurb_version < BLURB_VERSION
    }
}

/// Check if content contains the current blurb format.
fn contains_blurb(content: &str) -> bool {
    content.contains("<!-- bv-agent-instructions-v")
}

/// Check if content contains the legacy blurb (pre-v1, no HTML markers).
fn contains_legacy_blurb(content: &str) -> bool {
    let patterns = [
        "### Using bv as an AI sidecar",
        "--robot-insights",
        "--robot-plan",
        "bv already computes the hard parts",
    ];

    // Must have the section header
    if !content.contains("Using bv as an AI sidecar") {
        return false;
    }

    // Require all patterns
    patterns.iter().all(|p| content.contains(p))
}

/// Extract blurb version from content.
fn get_blurb_version(content: &str) -> u32 {
    // Look for <!-- bv-agent-instructions-v<N> -->
    let marker = "<!-- bv-agent-instructions-v";
    if let Some(pos) = content.find(marker) {
        let after = &content[pos + marker.len()..];
        if let Some(end) = after.find(" -->") {
            if let Ok(v) = after[..end].parse::<u32>() {
                return v;
            }
        }
    }
    0
}

/// Detect an agent file in a single directory.
fn detect_agent_file(work_dir: &Path) -> AgentFileDetection {
    // Try uppercase variants first
    for &filename in SUPPORTED_FILES.iter().filter(|f| f.starts_with(|c: char| c.is_uppercase())) {
        let path = work_dir.join(filename);
        if let Some(det) = check_agent_file(&path, filename) {
            return det;
        }
    }

    // Try lowercase variants
    for &filename in SUPPORTED_FILES.iter().filter(|f| f.starts_with(|c: char| c.is_lowercase())) {
        let path = work_dir.join(filename);
        if let Some(det) = check_agent_file(&path, filename) {
            return det;
        }
    }

    AgentFileDetection::default()
}

fn check_agent_file(path: &Path, file_type: &str) -> Option<AgentFileDetection> {
    if !path.is_file() {
        return None;
    }

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => {
            return Some(AgentFileDetection {
                file_path: Some(path.to_path_buf()),
                file_type: file_type.to_string(),
                ..Default::default()
            });
        }
    };

    let has_legacy = contains_legacy_blurb(&content);
    let has_blurb = contains_blurb(&content) || has_legacy;

    Some(AgentFileDetection {
        file_path: Some(path.to_path_buf()),
        file_type: file_type.to_string(),
        has_blurb,
        has_legacy_blurb: has_legacy,
        blurb_version: get_blurb_version(&content),
        content,
    })
}

/// Search for agent files starting from `work_dir` and walking up.
pub fn detect_agent_file_in_parents(work_dir: &Path, max_levels: usize) -> AgentFileDetection {
    let mut current = work_dir.to_path_buf();
    for _ in 0..=max_levels {
        let det = detect_agent_file(&current);
        if det.found() {
            return det;
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent.to_path_buf(),
            _ => break,
        }
    }
    AgentFileDetection::default()
}

// ---------------------------------------------------------------------------
// Blurb manipulation
// ---------------------------------------------------------------------------

/// Append the blurb to content.
fn append_blurb(content: &str) -> String {
    let mut out = content.to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out.push_str(AGENT_BLURB);
    out.push('\n');
    out
}

/// Remove the current-format blurb from content.
fn remove_blurb(content: &str) -> String {
    let Some(start_byte) = content.find("<!-- bv-agent-instructions-v") else {
        return content.to_string();
    };
    let Some(end_byte) = content.find(BLURB_END_MARKER) else {
        return content.to_string();
    };

    let mut end = end_byte + BLURB_END_MARKER.len();
    // Consume trailing newlines
    while end < content.len() && matches!(content.as_bytes()[end], b'\n' | b'\r') {
        end += 1;
    }
    // Consume leading newlines
    let mut start = start_byte;
    while start > 0 && matches!(content.as_bytes()[start - 1], b'\n' | b'\r') {
        start -= 1;
    }

    let mut result = content[..start].to_string();
    result.push_str(&content[end..]);
    result
}

/// Remove the legacy blurb (pre-v1) from content.
fn remove_legacy_blurb(content: &str) -> String {
    if !contains_legacy_blurb(content) {
        return content.to_string();
    }

    // Find start: "## Using bv as an AI sidecar" or "### Using bv as an AI sidecar"
    let Some(start_byte) = content.find("Using bv as an AI sidecar") else {
        return content.to_string();
    };
    // Back up to the heading marker
    let start = content[..start_byte]
        .rfind('#')
        .unwrap_or(start_byte);

    // Find end: "bv already computes the hard parts"
    let end = if let Some(pos) = content[start..].find("bv already computes the hard parts") {
        let mut e = start + pos;
        // Skip to end of line
        if let Some(nl) = content[e..].find('\n') {
            e += nl + 1;
        } else {
            e = content.len();
        }
        // Skip trailing code fence if present
        let remaining = &content[e..];
        if remaining.starts_with("```") {
            if let Some(nl) = remaining.find('\n') {
                e += nl + 1;
            }
        }
        // Consume trailing newlines
        while e < content.len() && matches!(content.as_bytes()[e], b'\n' | b'\r') {
            e += 1;
        }
        e
    } else {
        content.len()
    };

    // Consume leading newlines before start
    let mut adj_start = start;
    while adj_start > 0 && matches!(content.as_bytes()[adj_start - 1], b'\n' | b'\r') {
        adj_start -= 1;
    }
    if adj_start > 0 {
        adj_start += 1; // Keep one newline separator
    }

    let mut result = content[..adj_start].to_string();
    result.push_str(&content[end..]);
    result
}

/// Replace existing blurb (current or legacy) with the current version.
fn update_blurb(content: &str) -> String {
    let content = remove_legacy_blurb(content);
    let content = remove_blurb(&content);
    append_blurb(&content)
}

// ---------------------------------------------------------------------------
// File operations
// ---------------------------------------------------------------------------

/// Atomic file write via temp-file + rename.
fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    use std::io::Write;

    let dir = path
        .parent()
        .ok_or_else(|| BvrError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "no parent dir")))?;

    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .map_err(BvrError::Io)?;

    tmp.write_all(content).map_err(BvrError::Io)?;
    tmp.as_file().sync_all().map_err(BvrError::Io)?;

    tmp.persist(path)
        .map_err(|e| BvrError::Io(e.error))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Public CLI actions
// ---------------------------------------------------------------------------

/// Result returned to the CLI dispatcher with a user-facing message.
pub struct AgentsResult {
    pub message: String,
    pub changed: bool,
}

/// `--agents-check`: report blurb status.
pub fn agents_check(work_dir: &Path) -> AgentsResult {
    let det = detect_agent_file_in_parents(work_dir, 3);

    if !det.found() {
        return AgentsResult {
            message: format!(
                "No agent file found (searched up to 3 parent directories from {}).\n\
                 Run 'bvr --agents-add' to create AGENTS.md with beads workflow instructions.",
                work_dir.display()
            ),
            changed: false,
        };
    }

    let path = det.file_path.as_ref().unwrap().display();

    if det.needs_upgrade() {
        let current_ver = if det.has_legacy_blurb {
            "legacy".to_string()
        } else {
            format!("v{}", det.blurb_version)
        };
        return AgentsResult {
            message: format!(
                "Found {file_type} at {path} (blurb {current_ver}, current v{BLURB_VERSION} — needs update)\n\
                 Run 'bvr --agents-update' to update to the latest version.",
                file_type = det.file_type,
            ),
            changed: false,
        };
    }

    if det.needs_blurb() {
        return AgentsResult {
            message: format!(
                "Found {file_type} at {path} (no blurb)\n\
                 Run 'bvr --agents-add' to add beads workflow instructions.",
                file_type = det.file_type,
            ),
            changed: false,
        };
    }

    AgentsResult {
        message: format!(
            "Found {file_type} at {path} (blurb v{BLURB_VERSION} — up to date)",
            file_type = det.file_type,
        ),
        changed: false,
    }
}

/// `--agents-add`: add blurb to agent file (create if needed).
pub fn agents_add(work_dir: &Path, dry_run: bool) -> Result<AgentsResult> {
    let det = detect_agent_file_in_parents(work_dir, 3);

    if det.found() {
        let path = det.file_path.as_ref().unwrap();

        if det.has_blurb && !det.needs_upgrade() {
            return Ok(AgentsResult {
                message: format!(
                    "{} already has blurb v{BLURB_VERSION} — nothing to do.",
                    det.file_type
                ),
                changed: false,
            });
        }

        if det.needs_upgrade() {
            return Err(BvrError::InvalidArgument(format!(
                "{} has outdated blurb. Run 'bvr --agents-update' instead.",
                det.file_type
            )));
        }

        // File exists but no blurb — append
        if dry_run {
            return Ok(AgentsResult {
                message: format!(
                    "[dry-run] Would append blurb to {}.",
                    path.display()
                ),
                changed: false,
            });
        }

        let new_content = append_blurb(&det.content);
        atomic_write(path, new_content.as_bytes())?;

        return Ok(AgentsResult {
            message: format!("Added blurb to {}.", path.display()),
            changed: true,
        });
    }

    // No agent file — create AGENTS.md
    let path = work_dir.join("AGENTS.md");
    if dry_run {
        return Ok(AgentsResult {
            message: format!("[dry-run] Would create {}.", path.display()),
            changed: false,
        });
    }

    let content = format!("# AI Agent Instructions\n\n{AGENT_BLURB}\n");
    atomic_write(&path, content.as_bytes())?;

    Ok(AgentsResult {
        message: format!("Created {} with beads workflow instructions.", path.display()),
        changed: true,
    })
}

/// `--agents-update`: upgrade blurb to current version.
pub fn agents_update(work_dir: &Path, dry_run: bool) -> Result<AgentsResult> {
    let det = detect_agent_file_in_parents(work_dir, 3);

    if !det.found() {
        return Err(BvrError::InvalidArgument(
            "No agent file found. Run 'bvr --agents-add' first.".to_string(),
        ));
    }

    let path = det.file_path.as_ref().unwrap();

    if !det.has_blurb {
        return Err(BvrError::InvalidArgument(format!(
            "{} has no blurb to update. Run 'bvr --agents-add' instead.",
            det.file_type,
        )));
    }

    if !det.needs_upgrade() {
        return Ok(AgentsResult {
            message: format!(
                "{} blurb is already v{BLURB_VERSION} — nothing to do.",
                det.file_type,
            ),
            changed: false,
        });
    }

    let label = if det.has_legacy_blurb {
        "legacy blurb"
    } else {
        "outdated blurb"
    };

    if dry_run {
        return Ok(AgentsResult {
            message: format!(
                "[dry-run] Would upgrade {label} to v{BLURB_VERSION} in {}.",
                path.display()
            ),
            changed: false,
        });
    }

    let new_content = update_blurb(&det.content);
    atomic_write(path, new_content.as_bytes())?;

    Ok(AgentsResult {
        message: format!(
            "Updated blurb to v{BLURB_VERSION} in {}.",
            path.display()
        ),
        changed: true,
    })
}

/// `--agents-remove`: remove blurb from agent file.
pub fn agents_remove(work_dir: &Path, dry_run: bool) -> Result<AgentsResult> {
    let det = detect_agent_file_in_parents(work_dir, 3);

    if !det.found() {
        return Ok(AgentsResult {
            message: "No agent file found — nothing to remove.".to_string(),
            changed: false,
        });
    }

    let path = det.file_path.as_ref().unwrap();

    if !det.has_blurb {
        return Ok(AgentsResult {
            message: format!("{} has no blurb — nothing to remove.", det.file_type),
            changed: false,
        });
    }

    if dry_run {
        return Ok(AgentsResult {
            message: format!("[dry-run] Would remove blurb from {}.", path.display()),
            changed: false,
        });
    }

    let new_content = if det.has_legacy_blurb {
        remove_legacy_blurb(&det.content)
    } else {
        remove_blurb(&det.content)
    };
    atomic_write(path, new_content.as_bytes())?;

    Ok(AgentsResult {
        message: format!("Removed blurb from {}.", path.display()),
        changed: true,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_blurb_detects_current_marker() {
        assert!(contains_blurb("before\n<!-- bv-agent-instructions-v1 -->\nstuff\n<!-- end-bv-agent-instructions -->\nafter"));
        assert!(!contains_blurb("no marker here"));
    }

    #[test]
    fn get_blurb_version_extracts_version() {
        assert_eq!(get_blurb_version("<!-- bv-agent-instructions-v1 -->"), 1);
        assert_eq!(get_blurb_version("<!-- bv-agent-instructions-v42 -->"), 42);
        assert_eq!(get_blurb_version("no marker"), 0);
    }

    #[test]
    fn append_blurb_adds_to_content() {
        let result = append_blurb("# Existing\n");
        assert!(result.starts_with("# Existing\n"));
        assert!(result.contains(BLURB_START_MARKER));
        assert!(result.contains(BLURB_END_MARKER));
    }

    #[test]
    fn remove_blurb_strips_current() {
        let content = format!("before\n\n{AGENT_BLURB}\n\nafter");
        let result = remove_blurb(&content);
        assert!(result.contains("before"));
        assert!(result.contains("after"));
        assert!(!result.contains(BLURB_START_MARKER));
    }

    #[test]
    fn update_blurb_replaces_existing() {
        let old = "# File\n\n<!-- bv-agent-instructions-v0 -->\nold stuff\n<!-- end-bv-agent-instructions -->\n";
        let result = update_blurb(old);
        assert!(result.contains(BLURB_START_MARKER));
        assert!(!result.contains("old stuff"));
        assert!(result.contains("br ready"));
    }

    #[test]
    fn detection_defaults() {
        let det = AgentFileDetection::default();
        assert!(!det.found());
        assert!(!det.needs_blurb());
        assert!(!det.needs_upgrade());
    }

    #[test]
    fn detection_needs_upgrade_for_legacy() {
        let det = AgentFileDetection {
            file_path: Some(PathBuf::from("/test/AGENTS.md")),
            has_blurb: true,
            has_legacy_blurb: true,
            ..Default::default()
        };
        assert!(det.needs_upgrade());
    }

    #[test]
    fn detection_up_to_date() {
        let det = AgentFileDetection {
            file_path: Some(PathBuf::from("/test/AGENTS.md")),
            has_blurb: true,
            blurb_version: BLURB_VERSION,
            ..Default::default()
        };
        assert!(!det.needs_upgrade());
        assert!(!det.needs_blurb());
    }

    #[test]
    fn agents_check_no_file() {
        let tmp = tempfile::tempdir().unwrap();
        let result = agents_check(tmp.path());
        assert!(result.message.contains("No agent file found"));
        assert!(!result.changed);
    }

    #[test]
    fn agents_add_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let result = agents_add(tmp.path(), false).unwrap();
        assert!(result.changed);
        assert!(result.message.contains("Created"));

        let content = std::fs::read_to_string(tmp.path().join("AGENTS.md")).unwrap();
        assert!(content.contains(BLURB_START_MARKER));
    }

    #[test]
    fn agents_add_dry_run_no_write() {
        let tmp = tempfile::tempdir().unwrap();
        let result = agents_add(tmp.path(), true).unwrap();
        assert!(!result.changed);
        assert!(result.message.contains("[dry-run]"));
        assert!(!tmp.path().join("AGENTS.md").exists());
    }

    #[test]
    fn agents_remove_strips_blurb() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("AGENTS.md");
        let content = format!("# Header\n\n{AGENT_BLURB}\n\n## Other\n");
        std::fs::write(&path, &content).unwrap();

        let result = agents_remove(tmp.path(), false).unwrap();
        assert!(result.changed);

        let updated = std::fs::read_to_string(&path).unwrap();
        assert!(!updated.contains(BLURB_START_MARKER));
        assert!(updated.contains("# Header"));
        assert!(updated.contains("## Other"));
    }

    #[test]
    fn agents_update_upgrades_old_version() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("AGENTS.md");
        let content = "# Header\n\n<!-- bv-agent-instructions-v0 -->\nold\n<!-- end-bv-agent-instructions -->\n";
        std::fs::write(&path, content).unwrap();

        let result = agents_update(tmp.path(), false).unwrap();
        assert!(result.changed);

        let updated = std::fs::read_to_string(&path).unwrap();
        assert!(updated.contains("bv-agent-instructions-v1"));
        assert!(updated.contains("br ready"));
    }

    #[test]
    fn roundtrip_add_check_remove() {
        let tmp = tempfile::tempdir().unwrap();

        // Add
        let r = agents_add(tmp.path(), false).unwrap();
        assert!(r.changed);

        // Check
        let r = agents_check(tmp.path());
        assert!(r.message.contains("up to date"));

        // Remove
        let r = agents_remove(tmp.path(), false).unwrap();
        assert!(r.changed);

        // Check again
        let r = agents_check(tmp.path());
        assert!(r.message.contains("no blurb"));
    }
}
