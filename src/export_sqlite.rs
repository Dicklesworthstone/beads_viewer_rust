use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::analysis::Analyzer;
use crate::analysis::triage::TriageComputation;
use crate::model::Issue;
use crate::{BvrError, Result};

pub const SQLITE_EXPORT_FILENAME: &str = "beads.sqlite3";
pub const SQLITE_EXPORT_CONFIG_FILENAME: &str = "beads.sqlite3.config.json";
pub const SQLITE_EXPORT_SCHEMA_VERSION: i32 = 1;
pub const SQLITE_WRITER_CONTRACT_VERSION: &str = "1";
pub const DEFAULT_SQLITE_PAGE_SIZE: u32 = 1_024;
pub const DEFAULT_SQLITE_CHUNK_THRESHOLD_BYTES: u64 = 5 * 1024 * 1024;
pub const DEFAULT_SQLITE_CHUNK_SIZE_BYTES: u64 = 1_048_576;

const EXPORT_META_KEYS: &[(&str, &str)] = &[
    ("schema_version", "1"),
    ("writer_contract_version", SQLITE_WRITER_CONTRACT_VERSION),
    ("layout", "single-file"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteBootstrapOptions {
    pub page_size: u32,
}

impl Default for SqliteBootstrapOptions {
    fn default() -> Self {
        Self {
            page_size: DEFAULT_SQLITE_PAGE_SIZE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteBootstrapSummary {
    pub database_path: PathBuf,
    pub schema_version: i32,
    pub writer_contract_version: &'static str,
    pub page_size: u32,
    pub journal_mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteBundleOptions {
    pub chunk_threshold_bytes: u64,
    pub chunk_size_bytes: u64,
}

impl Default for SqliteBundleOptions {
    fn default() -> Self {
        Self {
            chunk_threshold_bytes: DEFAULT_SQLITE_CHUNK_THRESHOLD_BYTES,
            chunk_size_bytes: DEFAULT_SQLITE_CHUNK_SIZE_BYTES,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SqliteChunkInfo {
    pub path: String,
    pub hash: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SqliteBootstrapConfig {
    pub chunked: bool,
    pub chunk_count: usize,
    pub chunk_size: u64,
    pub total_size: u64,
    pub hash: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chunks: Vec<SqliteChunkInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExportDependencyRow {
    issue_id: String,
    depends_on_id: String,
    dep_type: String,
    created_by: String,
    created_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExportCommentRow {
    id: String,
    issue_id: String,
    author: String,
    text: String,
    created_at: Option<String>,
}

#[must_use]
pub fn export_database_path(output_dir: &Path) -> PathBuf {
    output_dir.join(SQLITE_EXPORT_FILENAME)
}

#[must_use]
pub fn export_config_path(output_dir: &Path) -> PathBuf {
    output_dir.join(SQLITE_EXPORT_CONFIG_FILENAME)
}

pub fn bootstrap_export_database(
    output_dir: &Path,
    options: &SqliteBootstrapOptions,
) -> Result<SqliteBootstrapSummary> {
    fs::create_dir_all(output_dir)?;

    let database_path = export_database_path(output_dir);
    let connection = Connection::open(&database_path)?;

    configure_connection(&connection, options.page_size)?;
    verify_existing_contract(&connection)?;
    create_schema(&connection)?;
    rebuild_issue_search_index(&connection)?;
    rebuild_issue_overview_mv(&connection)?;
    upsert_contract_meta(&connection, options.page_size)?;

    let actual_page_size = query_pragma_i64(&connection, "page_size")?;
    let journal_mode = query_pragma_string(&connection, "journal_mode")?;

    Ok(SqliteBootstrapSummary {
        database_path,
        schema_version: SQLITE_EXPORT_SCHEMA_VERSION,
        writer_contract_version: SQLITE_WRITER_CONTRACT_VERSION,
        page_size: u32::try_from(actual_page_size).map_err(|_| {
            BvrError::InvalidArgument(format!(
                "sqlite returned an invalid page_size value: {actual_page_size}"
            ))
        })?,
        journal_mode,
    })
}

pub fn emit_bootstrap_config(
    output_dir: &Path,
    options: &SqliteBundleOptions,
) -> Result<SqliteBootstrapConfig> {
    if options.chunk_size_bytes == 0 {
        return Err(BvrError::InvalidArgument(
            "sqlite export chunk_size_bytes must be greater than zero".to_string(),
        ));
    }

    let database_path = export_database_path(output_dir);
    let total_size = fs::metadata(&database_path)?.len();
    let hash = hash_file(&database_path)?;

    let mut config = SqliteBootstrapConfig {
        chunked: false,
        chunk_count: 0,
        chunk_size: options.chunk_size_bytes,
        total_size,
        hash,
        chunks: Vec::new(),
    };

    if total_size > options.chunk_threshold_bytes {
        config.chunked = true;
        config.chunks =
            write_database_chunks(output_dir, &database_path, options.chunk_size_bytes)?;
        config.chunk_count = config.chunks.len();
    }

    write_json_pretty(&export_config_path(output_dir), &config)?;
    Ok(config)
}

pub fn populate_export_database(
    output_dir: &Path,
    title: Option<&str>,
    issues: &[Issue],
    analyzer: &Analyzer,
    triage: &TriageComputation,
) -> Result<()> {
    let database_path = export_database_path(output_dir);
    let mut connection = Connection::open(&database_path)?;

    let dependency_rows = collect_dependency_rows(issues);
    let comment_rows = collect_comment_rows(issues);

    {
        let tx = connection.transaction()?;
        clear_export_rows(&tx)?;
        insert_issues(&tx, issues)?;
        insert_dependencies(&tx, &dependency_rows)?;
        insert_comments(&tx, &comment_rows)?;
        insert_metrics(&tx, issues, analyzer, triage)?;
        insert_triage_recommendations(&tx, triage, analyzer)?;
        upsert_export_content_meta(
            &tx,
            title,
            issues.len(),
            dependency_rows.len(),
            comment_rows.len(),
        )?;
        tx.commit()?;
    }

    rebuild_issue_search_index(&connection)?;
    rebuild_issue_overview_mv(&connection)?;
    Ok(())
}

fn clear_export_rows(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "
        DELETE FROM triage_recommendations;
        DELETE FROM issue_metrics;
        DELETE FROM comments;
        DELETE FROM dependencies;
        DELETE FROM issues;
        DELETE FROM issue_overview_mv;
        ",
    )?;
    tx.execute(
        "DELETE FROM export_meta WHERE key IN ('title', 'issue_count', 'dependency_count', 'comment_count')",
        [],
    )?;
    Ok(())
}

fn insert_issues(tx: &Transaction<'_>, issues: &[Issue]) -> Result<()> {
    let mut sorted = issues.iter().collect::<Vec<_>>();
    sorted.sort_by(|left, right| left.id.cmp(&right.id));

    let mut stmt = tx.prepare(
        "
        INSERT INTO issues (
            id, title, description, design, acceptance_criteria, notes, status,
            priority, issue_type, assignee, estimated_minutes, labels, created_at,
            updated_at, due_date, closed_at, source_repo
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ",
    )?;

    for issue in sorted {
        let labels = serde_json::to_string(&issue.labels)?;
        let source_repo = normalized_source_repo(issue);
        let created_at = issue.created_at.map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
        let updated_at = issue.updated_at.map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
        let due_date = issue.due_date.map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
        let closed_at = issue.closed_at.map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
        stmt.execute(params![
            issue.id.as_str(),
            issue.title.as_str(),
            issue.description.as_str(),
            issue.design.as_str(),
            issue.acceptance_criteria.as_str(),
            issue.notes.as_str(),
            issue.status.as_str(),
            issue.priority,
            issue.issue_type.as_str(),
            issue.assignee.as_str(),
            issue.estimated_minutes,
            labels,
            created_at.as_deref(),
            updated_at.as_deref(),
            due_date.as_deref(),
            closed_at.as_deref(),
            source_repo,
        ])?;
    }

    Ok(())
}

fn insert_dependencies(
    tx: &Transaction<'_>,
    dependency_rows: &[ExportDependencyRow],
) -> Result<()> {
    let mut stmt = tx.prepare(
        "
        INSERT INTO dependencies (issue_id, depends_on_id, type, created_by, created_at)
        VALUES (?, ?, ?, ?, ?)
        ",
    )?;

    for dep in dependency_rows {
        stmt.execute(params![
            dep.issue_id.as_str(),
            dep.depends_on_id.as_str(),
            dep.dep_type.as_str(),
            dep.created_by.as_str(),
            dep.created_at.as_deref(),
        ])?;
    }

    Ok(())
}

fn insert_comments(tx: &Transaction<'_>, comment_rows: &[ExportCommentRow]) -> Result<()> {
    let mut stmt = tx.prepare(
        "
        INSERT INTO comments (id, issue_id, author, text, created_at)
        VALUES (?, ?, ?, ?, ?)
        ",
    )?;

    for comment in comment_rows {
        stmt.execute(params![
            comment.id.as_str(),
            comment.issue_id.as_str(),
            comment.author.as_str(),
            comment.text.as_str(),
            comment.created_at.as_deref(),
        ])?;
    }

    Ok(())
}

fn insert_metrics(
    tx: &Transaction<'_>,
    issues: &[Issue],
    analyzer: &Analyzer,
    triage: &TriageComputation,
) -> Result<()> {
    let mut sorted = issues.iter().collect::<Vec<_>>();
    sorted.sort_by(|left, right| left.id.cmp(&right.id));

    let mut stmt = tx.prepare(
        "
        INSERT INTO issue_metrics (
            issue_id, pagerank, betweenness, critical_path_depth,
            triage_score, blocks_count, blocked_by_count
        )
        VALUES (?, ?, ?, ?, ?, ?, ?)
        ",
    )?;

    for issue in sorted {
        stmt.execute(params![
            issue.id.as_str(),
            analyzer
                .metrics
                .pagerank
                .get(&issue.id)
                .copied()
                .unwrap_or_default(),
            analyzer
                .metrics
                .betweenness
                .get(&issue.id)
                .copied()
                .unwrap_or_default(),
            i64::try_from(
                analyzer
                    .metrics
                    .critical_depth
                    .get(&issue.id)
                    .copied()
                    .unwrap_or_default(),
            )
            .map_err(|_| {
                BvrError::InvalidArgument(format!(
                    "critical depth does not fit in sqlite integer for {}",
                    issue.id
                ))
            })?,
            triage
                .score_by_id
                .get(&issue.id)
                .copied()
                .unwrap_or_default(),
            i64::try_from(
                analyzer
                    .metrics
                    .blocks_count
                    .get(&issue.id)
                    .copied()
                    .unwrap_or_default(),
            )
            .map_err(|_| {
                BvrError::InvalidArgument(format!(
                    "blocks_count does not fit in sqlite integer for {}",
                    issue.id
                ))
            })?,
            i64::try_from(
                analyzer
                    .metrics
                    .blocked_by_count
                    .get(&issue.id)
                    .copied()
                    .unwrap_or_default(),
            )
            .map_err(|_| {
                BvrError::InvalidArgument(format!(
                    "blocked_by_count does not fit in sqlite integer for {}",
                    issue.id
                ))
            })?,
        ])?;
    }

    Ok(())
}

fn insert_triage_recommendations(
    tx: &Transaction<'_>,
    triage: &TriageComputation,
    analyzer: &Analyzer,
) -> Result<()> {
    let mut recommendations = triage.result.recommendations.iter().collect::<Vec<_>>();
    recommendations.sort_by(|left, right| left.id.cmp(&right.id));

    let mut stmt = tx.prepare(
        "
        INSERT INTO triage_recommendations (issue_id, score, action, reasons, unblocks_ids, blocked_by_ids)
        VALUES (?, ?, ?, ?, ?, ?)
        ",
    )?;

    for recommendation in recommendations {
        let reasons = serde_json::to_string(&recommendation.reasons)?;
        let unblocks_ids = serde_json::to_string(&analyzer.graph.dependents(&recommendation.id))?;
        let blocked_by_ids = serde_json::to_string(&analyzer.graph.blockers(&recommendation.id))?;
        stmt.execute(params![
            recommendation.id.as_str(),
            recommendation.score,
            recommendation.claim_command.as_str(),
            reasons,
            unblocks_ids,
            blocked_by_ids,
        ])?;
    }

    Ok(())
}

fn upsert_export_content_meta(
    tx: &Transaction<'_>,
    title: Option<&str>,
    issue_count: usize,
    dependency_count: usize,
    comment_count: usize,
) -> Result<()> {
    upsert_meta_value(tx, "issue_count", &issue_count.to_string())?;
    upsert_meta_value(tx, "dependency_count", &dependency_count.to_string())?;
    upsert_meta_value(tx, "comment_count", &comment_count.to_string())?;

    if let Some(title) = title.map(str::trim).filter(|value| !value.is_empty()) {
        upsert_meta_value(tx, "title", title)?;
    }

    Ok(())
}

fn upsert_meta_value(tx: &Transaction<'_>, key: &str, value: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO export_meta (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [key, value],
    )?;
    Ok(())
}

fn collect_dependency_rows(issues: &[Issue]) -> Vec<ExportDependencyRow> {
    let exported_ids = issues
        .iter()
        .map(|issue| issue.id.clone())
        .collect::<BTreeSet<_>>();
    let mut rows = Vec::new();

    for issue in issues {
        if !exported_ids.contains(&issue.id) {
            continue;
        }

        for dep in &issue.dependencies {
            if dep.depends_on_id.trim().is_empty() || !exported_ids.contains(&dep.depends_on_id) {
                continue;
            }

            rows.push(ExportDependencyRow {
                issue_id: issue.id.clone(),
                depends_on_id: dep.depends_on_id.clone(),
                dep_type: dep.dep_type.clone(),
                created_by: dep.created_by.clone(),
                created_at: dep.created_at.map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
            });
        }
    }

    rows.sort_by(|left, right| {
        left.issue_id
            .cmp(&right.issue_id)
            .then_with(|| left.depends_on_id.cmp(&right.depends_on_id))
            .then_with(|| left.dep_type.cmp(&right.dep_type))
            .then_with(|| left.created_by.cmp(&right.created_by))
            .then_with(|| left.created_at.cmp(&right.created_at))
    });
    rows.dedup();
    rows
}

fn collect_comment_rows(issues: &[Issue]) -> Vec<ExportCommentRow> {
    let mut rows = Vec::new();

    for issue in issues {
        for comment in &issue.comments {
            rows.push(ExportCommentRow {
                id: format!("{}:{}", issue.id, comment.id),
                issue_id: issue.id.clone(),
                author: comment.author.clone(),
                text: comment.text.clone(),
                created_at: comment.created_at.map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
            });
        }
    }

    rows.sort_by(|left, right| {
        left.issue_id
            .cmp(&right.issue_id)
            .then_with(|| left.id.cmp(&right.id))
    });
    rows
}

fn normalized_source_repo(issue: &Issue) -> &str {
    let source_repo = issue.source_repo.trim();
    if source_repo.is_empty() {
        "."
    } else {
        source_repo
    }
}

pub fn rebuild_issue_search_index(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "
        CREATE VIRTUAL TABLE IF NOT EXISTS issues_fts USING fts5(
            id UNINDEXED,
            title,
            description,
            design,
            acceptance_criteria,
            notes,
            labels,
            assignee,
            source_repo,
            content='issues',
            content_rowid='rowid',
            tokenize='porter unicode61'
        );

        INSERT INTO issues_fts(issues_fts) VALUES('rebuild');
        ",
    )?;

    Ok(())
}

pub fn rebuild_issue_overview_mv(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "
        DELETE FROM issue_overview_mv;

        INSERT INTO issue_overview_mv (
            id,
            title,
            description,
            design,
            acceptance_criteria,
            notes,
            status,
            priority,
            issue_type,
            assignee,
            estimated_minutes,
            labels,
            created_at,
            updated_at,
            due_date,
            closed_at,
            source_repo,
            pagerank,
            betweenness,
            critical_path_depth,
            triage_score,
            blocks_count,
            blocked_by_count,
            blocker_count,
            dependent_count,
            critical_depth,
            in_cycle,
            comment_count,
            blocks_ids,
            blocked_by_ids
        )
        SELECT
            i.id,
            i.title,
            i.description,
            i.design,
            i.acceptance_criteria,
            i.notes,
            i.status,
            i.priority,
            i.issue_type,
            i.assignee,
            i.estimated_minutes,
            i.labels,
            i.created_at,
            i.updated_at,
            i.due_date,
            i.closed_at,
            i.source_repo,
            COALESCE(m.pagerank, 0),
            COALESCE(m.betweenness, 0),
            COALESCE(m.critical_path_depth, 0),
            COALESCE(m.triage_score, 0),
            COALESCE(m.blocks_count, 0),
            COALESCE(m.blocked_by_count, 0),
            COALESCE(m.blocked_by_count, 0),
            COALESCE(m.blocks_count, 0),
            COALESCE(m.critical_path_depth, 0),
            0,
            (
                SELECT COUNT(*)
                FROM comments c
                WHERE c.issue_id = i.id
            ),
            (
                SELECT GROUP_CONCAT(issue_id)
                FROM (
                    SELECT issue_id
                    FROM dependencies
                    WHERE depends_on_id = i.id
                        AND (type = 'blocks' OR type = '')
                    ORDER BY issue_id
                )
            ),
            (
                SELECT GROUP_CONCAT(depends_on_id)
                FROM (
                    SELECT depends_on_id
                    FROM dependencies
                    WHERE issue_id = i.id
                        AND (type = 'blocks' OR type = '')
                    ORDER BY depends_on_id
                )
            )
        FROM issues i
        LEFT JOIN issue_metrics m
            ON i.id = m.issue_id
        ORDER BY i.id;
        ",
    )?;
    Ok(())
}

fn configure_connection(connection: &Connection, page_size: u32) -> Result<()> {
    connection.execute_batch(
        "
        PRAGMA foreign_keys = ON;
        PRAGMA journal_mode = DELETE;
        PRAGMA synchronous = NORMAL;
        ",
    )?;
    connection.pragma_update(None, "page_size", page_size)?;
    Ok(())
}

fn verify_existing_contract(connection: &Connection) -> Result<()> {
    let existing_schema_version = query_pragma_i64(connection, "user_version")?;
    if existing_schema_version != 0
        && existing_schema_version != i64::from(SQLITE_EXPORT_SCHEMA_VERSION)
    {
        return Err(BvrError::InvalidArgument(format!(
            "existing export database uses schema version {existing_schema_version}, expected {SQLITE_EXPORT_SCHEMA_VERSION}"
        )));
    }
    Ok(())
}

fn create_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS issues (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            design TEXT NOT NULL DEFAULT '',
            acceptance_criteria TEXT NOT NULL DEFAULT '',
            notes TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL,
            priority INTEGER NOT NULL,
            issue_type TEXT NOT NULL,
            assignee TEXT NOT NULL DEFAULT '',
            estimated_minutes INTEGER,
            labels TEXT NOT NULL DEFAULT '[]',
            created_at TEXT,
            updated_at TEXT,
            due_date TEXT,
            closed_at TEXT,
            source_repo TEXT NOT NULL DEFAULT '.'
        );

        CREATE TABLE IF NOT EXISTS dependencies (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            issue_id TEXT NOT NULL,
            depends_on_id TEXT NOT NULL,
            type TEXT NOT NULL DEFAULT 'blocks',
            created_by TEXT NOT NULL DEFAULT '',
            created_at TEXT,
            FOREIGN KEY (issue_id) REFERENCES issues(id),
            FOREIGN KEY (depends_on_id) REFERENCES issues(id)
        );

        CREATE TABLE IF NOT EXISTS comments (
            id TEXT PRIMARY KEY,
            issue_id TEXT NOT NULL,
            author TEXT NOT NULL DEFAULT '',
            text TEXT NOT NULL,
            created_at TEXT,
            FOREIGN KEY (issue_id) REFERENCES issues(id)
        );

        CREATE TABLE IF NOT EXISTS issue_metrics (
            issue_id TEXT PRIMARY KEY,
            pagerank REAL NOT NULL DEFAULT 0,
            betweenness REAL NOT NULL DEFAULT 0,
            critical_path_depth INTEGER NOT NULL DEFAULT 0,
            triage_score REAL NOT NULL DEFAULT 0,
            blocks_count INTEGER NOT NULL DEFAULT 0,
            blocked_by_count INTEGER NOT NULL DEFAULT 0,
            FOREIGN KEY (issue_id) REFERENCES issues(id)
        );

        CREATE TABLE IF NOT EXISTS triage_recommendations (
            issue_id TEXT PRIMARY KEY,
            score REAL NOT NULL,
            action TEXT NOT NULL,
            reasons TEXT NOT NULL DEFAULT '[]',
            unblocks_ids TEXT NOT NULL DEFAULT '[]',
            blocked_by_ids TEXT NOT NULL DEFAULT '[]',
            FOREIGN KEY (issue_id) REFERENCES issues(id)
        );

        CREATE TABLE IF NOT EXISTS export_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS issue_overview_mv (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            design TEXT NOT NULL DEFAULT '',
            acceptance_criteria TEXT NOT NULL DEFAULT '',
            notes TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL,
            priority INTEGER NOT NULL,
            issue_type TEXT NOT NULL,
            assignee TEXT NOT NULL DEFAULT '',
            estimated_minutes INTEGER,
            labels TEXT NOT NULL DEFAULT '[]',
            created_at TEXT,
            updated_at TEXT,
            due_date TEXT,
            closed_at TEXT,
            source_repo TEXT NOT NULL DEFAULT '.',
            pagerank REAL NOT NULL DEFAULT 0,
            betweenness REAL NOT NULL DEFAULT 0,
            critical_path_depth INTEGER NOT NULL DEFAULT 0,
            triage_score REAL NOT NULL DEFAULT 0,
            blocks_count INTEGER NOT NULL DEFAULT 0,
            blocked_by_count INTEGER NOT NULL DEFAULT 0,
            blocker_count INTEGER NOT NULL DEFAULT 0,
            dependent_count INTEGER NOT NULL DEFAULT 0,
            critical_depth INTEGER NOT NULL DEFAULT 0,
            in_cycle INTEGER NOT NULL DEFAULT 0,
            comment_count INTEGER NOT NULL DEFAULT 0,
            blocks_ids TEXT,
            blocked_by_ids TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_issues_status
            ON issues(status);
        CREATE INDEX IF NOT EXISTS idx_issues_priority
            ON issues(priority, status);
        CREATE INDEX IF NOT EXISTS idx_issues_updated
            ON issues(updated_at DESC);
        CREATE INDEX IF NOT EXISTS idx_issues_type_status
            ON issues(issue_type, status);
        CREATE INDEX IF NOT EXISTS idx_issues_source_repo
            ON issues(source_repo, status);

        CREATE INDEX IF NOT EXISTS idx_deps_issue
            ON dependencies(issue_id);
        CREATE INDEX IF NOT EXISTS idx_deps_depends
            ON dependencies(depends_on_id);
        CREATE INDEX IF NOT EXISTS idx_deps_type
            ON dependencies(type);

        CREATE INDEX IF NOT EXISTS idx_comments_issue
            ON comments(issue_id);
        CREATE INDEX IF NOT EXISTS idx_comments_created
            ON comments(created_at DESC);

        CREATE INDEX IF NOT EXISTS idx_metrics_score
            ON issue_metrics(triage_score DESC);
        CREATE INDEX IF NOT EXISTS idx_metrics_pagerank
            ON issue_metrics(pagerank DESC);

        CREATE INDEX IF NOT EXISTS idx_mv_status
            ON issue_overview_mv(status);
        CREATE INDEX IF NOT EXISTS idx_mv_priority
            ON issue_overview_mv(priority);
        CREATE INDEX IF NOT EXISTS idx_mv_score
            ON issue_overview_mv(triage_score DESC);
        ",
    )?;
    connection.pragma_update(None, "user_version", SQLITE_EXPORT_SCHEMA_VERSION)?;
    Ok(())
}

fn upsert_contract_meta(connection: &Connection, page_size: u32) -> Result<()> {
    for (key, value) in EXPORT_META_KEYS {
        connection.execute(
            "INSERT INTO export_meta (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [*key, *value],
        )?;
    }

    let actual_page_size = query_pragma_i64(connection, "page_size")?;
    let page_size_value = if actual_page_size > 0 {
        actual_page_size.to_string()
    } else {
        page_size.to_string()
    };
    connection.execute(
        "INSERT INTO export_meta (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        ["page_size", page_size_value.as_str()],
    )?;

    let journal_mode = query_pragma_string(connection, "journal_mode")?;
    connection.execute(
        "INSERT INTO export_meta (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        ["journal_mode", journal_mode.as_str()],
    )?;
    Ok(())
}

fn query_pragma_i64(connection: &Connection, pragma_name: &str) -> Result<i64> {
    let sql = format!("PRAGMA {pragma_name}");
    Ok(connection.query_row(&sql, [], |row| row.get(0))?)
}

fn query_pragma_string(connection: &Connection, pragma_name: &str) -> Result<String> {
    let sql = format!("PRAGMA {pragma_name}");
    Ok(connection.query_row(&sql, [], |row| row.get(0))?)
}

fn hash_file(path: &Path) -> Result<String> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];

    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

fn write_database_chunks(
    output_dir: &Path,
    database_path: &Path,
    chunk_size_bytes: u64,
) -> Result<Vec<SqliteChunkInfo>> {
    let chunk_size = usize::try_from(chunk_size_bytes).map_err(|_| {
        BvrError::InvalidArgument(format!(
            "sqlite export chunk_size_bytes is too large for this platform: {chunk_size_bytes}"
        ))
    })?;

    let chunks_dir = output_dir.join("chunks");
    fs::create_dir_all(&chunks_dir)?;

    let file = File::open(database_path)?;
    let mut reader = BufReader::new(file);
    let mut buffer = vec![0_u8; chunk_size];
    let mut chunks = Vec::new();

    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }

        let file_name = format!("{:05}.bin", chunks.len());
        let relative_path = format!("chunks/{file_name}");
        let chunk_path = chunks_dir.join(&file_name);
        let bytes = &buffer[..read];
        fs::write(&chunk_path, bytes)?;

        let mut hasher = Sha256::new();
        hasher.update(bytes);

        chunks.push(SqliteChunkInfo {
            path: relative_path,
            hash: format!("{:x}", hasher.finalize()),
            size: u64::try_from(read).map_err(|_| {
                BvrError::InvalidArgument(format!(
                    "sqlite export chunk size does not fit in u64: {read}"
                ))
            })?,
        });
    }

    Ok(chunks)
}

fn write_json_pretty<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let file = File::create(path)?;
    serde_json::to_writer_pretty(file, value)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use rusqlite::params;
    use tempfile::tempdir;

    use crate::analysis::Analyzer;
    use crate::analysis::triage::TriageOptions;
    use crate::model::{Comment, Dependency, Issue, ts};

    use super::*;

    #[test]
    fn bootstrap_export_database_creates_expected_schema_inventory() {
        let temp = tempdir().expect("tempdir");
        let summary = bootstrap_export_database(temp.path(), &SqliteBootstrapOptions::default())
            .expect("bootstrap sqlite export database");

        assert_eq!(
            summary.database_path,
            temp.path().join(SQLITE_EXPORT_FILENAME)
        );
        assert_eq!(summary.schema_version, SQLITE_EXPORT_SCHEMA_VERSION);
        assert_eq!(
            summary.writer_contract_version,
            SQLITE_WRITER_CONTRACT_VERSION
        );
        assert_eq!(summary.page_size, DEFAULT_SQLITE_PAGE_SIZE);
        assert_eq!(summary.journal_mode, "delete");

        let connection =
            Connection::open(&summary.database_path).expect("open bootstrapped sqlite database");

        assert_eq!(
            query_pragma_i64(&connection, "user_version").expect("pragma user_version"),
            i64::from(SQLITE_EXPORT_SCHEMA_VERSION)
        );
        assert_eq!(
            query_pragma_i64(&connection, "page_size").expect("pragma page_size"),
            i64::from(DEFAULT_SQLITE_PAGE_SIZE)
        );

        let object_names = sqlite_object_inventory(&connection);

        for table in [
            "issues",
            "dependencies",
            "comments",
            "issue_metrics",
            "triage_recommendations",
            "export_meta",
            "issue_overview_mv",
            "issues_fts",
        ] {
            assert!(
                object_names.contains(table),
                "missing sqlite object {table}; inventory: {object_names:?}"
            );
        }

        for index in [
            "idx_issues_status",
            "idx_issues_priority",
            "idx_issues_updated",
            "idx_issues_type_status",
            "idx_issues_source_repo",
            "idx_deps_issue",
            "idx_deps_depends",
            "idx_deps_type",
            "idx_comments_issue",
            "idx_comments_created",
            "idx_metrics_score",
            "idx_metrics_pagerank",
            "idx_mv_status",
            "idx_mv_priority",
            "idx_mv_score",
        ] {
            assert!(
                object_names.contains(index),
                "missing sqlite object {index}; inventory: {object_names:?}"
            );
        }

        let meta = export_meta(&connection);
        assert_eq!(meta.get("schema_version"), Some(&"1".to_string()));
        assert_eq!(
            meta.get("writer_contract_version"),
            Some(&SQLITE_WRITER_CONTRACT_VERSION.to_string())
        );
        assert_eq!(
            meta.get("page_size"),
            Some(&DEFAULT_SQLITE_PAGE_SIZE.to_string())
        );
        assert_eq!(meta.get("journal_mode"), Some(&"delete".to_string()));
        assert_eq!(meta.get("layout"), Some(&"single-file".to_string()));
    }

    #[test]
    fn bootstrap_export_database_is_idempotent() {
        let temp = tempdir().expect("tempdir");
        bootstrap_export_database(temp.path(), &SqliteBootstrapOptions::default())
            .expect("first bootstrap");
        let second = bootstrap_export_database(temp.path(), &SqliteBootstrapOptions::default())
            .expect("second bootstrap");

        let connection =
            Connection::open(&second.database_path).expect("open bootstrapped sqlite database");

        let issue_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM issue_overview_mv", [], |row| {
                row.get(0)
            })
            .expect("issue_overview_mv count");
        let fts_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM issues_fts", [], |row| row.get(0))
            .expect("issues_fts count");

        assert_eq!(issue_count, 0);
        assert_eq!(fts_count, 0);
        assert_eq!(export_meta(&connection).len(), 5);
    }

    #[test]
    fn bootstrap_export_database_rejects_conflicting_schema_version() {
        let temp = tempdir().expect("tempdir");
        let db_path = export_database_path(temp.path());
        let connection = Connection::open(&db_path).expect("open sqlite database");
        connection
            .pragma_update(None, "user_version", 99_i32)
            .expect("set conflicting schema version");
        drop(connection);

        let err = bootstrap_export_database(temp.path(), &SqliteBootstrapOptions::default())
            .expect_err("conflicting schema version should fail");
        assert!(
            err.to_string().contains("schema version 99"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rebuild_issue_overview_mv_derives_counts_and_aliases() {
        let temp = tempdir().expect("tempdir");
        let summary = bootstrap_export_database(temp.path(), &SqliteBootstrapOptions::default())
            .expect("bootstrap sqlite export database");
        let connection =
            Connection::open(&summary.database_path).expect("open bootstrapped sqlite database");

        connection
            .execute(
                "
                INSERT INTO issues (
                    id, title, description, design, acceptance_criteria, notes, status,
                    priority, issue_type, assignee, estimated_minutes, labels, created_at,
                    updated_at, due_date, closed_at, source_repo
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ",
                params![
                    "ISSUE-1",
                    "Export SQLite",
                    "Ship the export schema",
                    "Use a dedicated module",
                    "Schema is explicit",
                    "Keep bootstrap deterministic",
                    "open",
                    1_i32,
                    "task",
                    "alex",
                    45_i32,
                    "[\"export\",\"sqlite\"]",
                    "2026-03-08T18:00:00Z",
                    "2026-03-08T18:30:00Z",
                    "2026-03-10T00:00:00Z",
                    Option::<String>::None,
                    "core"
                ],
            )
            .expect("insert issue");
        connection
            .execute(
                "
                INSERT INTO issues (
                    id, title, description, design, acceptance_criteria, notes, status,
                    priority, issue_type, assignee, estimated_minutes, labels, created_at,
                    updated_at, due_date, closed_at, source_repo
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ",
                params![
                    "ISSUE-2",
                    "Downstream consumer",
                    "",
                    "",
                    "",
                    "",
                    "blocked",
                    2_i32,
                    "task",
                    "",
                    Option::<i32>::None,
                    "[]",
                    Option::<String>::None,
                    Option::<String>::None,
                    Option::<String>::None,
                    Option::<String>::None,
                    "core"
                ],
            )
            .expect("insert second issue");
        connection
            .execute(
                "
                INSERT INTO dependencies (issue_id, depends_on_id, type, created_by, created_at)
                VALUES (?, ?, ?, ?, ?)
                ",
                params![
                    "ISSUE-2",
                    "ISSUE-1",
                    "blocks",
                    "tester",
                    "2026-03-08T18:31:00Z"
                ],
            )
            .expect("insert dependency");
        connection
            .execute(
                "
                INSERT INTO comments (id, issue_id, author, text, created_at)
                VALUES (?, ?, ?, ?, ?)
                ",
                params![
                    "ISSUE-1:1",
                    "ISSUE-1",
                    "alex",
                    "Need the schema locked down before population.",
                    "2026-03-08T18:40:00Z"
                ],
            )
            .expect("insert comment");
        connection
            .execute(
                "
                INSERT INTO issue_metrics (
                    issue_id, pagerank, betweenness, critical_path_depth,
                    triage_score, blocks_count, blocked_by_count
                )
                VALUES (?, ?, ?, ?, ?, ?, ?)
                ",
                params!["ISSUE-1", 0.25_f64, 0.1_f64, 4_i32, 0.8_f64, 1_i32, 0_i32],
            )
            .expect("insert metrics");

        rebuild_issue_overview_mv(&connection).expect("rebuild issue_overview_mv");

        let derived = connection
            .query_row(
                "
                SELECT
                    blocker_count,
                    dependent_count,
                    critical_depth,
                    comment_count,
                    blocks_ids,
                    blocked_by_ids,
                    source_repo,
                    design,
                    acceptance_criteria,
                    notes
                FROM issue_overview_mv
                WHERE id = ?
                ",
                ["ISSUE-1"],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                    ))
                },
            )
            .expect("query issue_overview_mv");

        assert_eq!(derived.0, 0);
        assert_eq!(derived.1, 1);
        assert_eq!(derived.2, 4);
        assert_eq!(derived.3, 1);
        assert_eq!(derived.4.as_deref(), Some("ISSUE-2"));
        assert_eq!(derived.5, None);
        assert_eq!(derived.6, "core");
        assert_eq!(derived.7, "Use a dedicated module");
        assert_eq!(derived.8, "Schema is explicit");
        assert_eq!(derived.9, "Keep bootstrap deterministic");
    }

    #[test]
    fn rebuild_issue_search_index_supports_prefix_queries() {
        let temp = tempdir().expect("tempdir");
        let summary = bootstrap_export_database(temp.path(), &SqliteBootstrapOptions::default())
            .expect("bootstrap sqlite export database");
        let connection =
            Connection::open(&summary.database_path).expect("open bootstrapped sqlite database");

        connection
            .execute(
                "
                INSERT INTO issues (
                    id, title, description, design, acceptance_criteria, notes, status,
                    priority, issue_type, assignee, estimated_minutes, labels, created_at,
                    updated_at, due_date, closed_at, source_repo
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ",
                params![
                    "SEARCH-1",
                    "Authentication hardening",
                    "Tighten export viewer auth checks",
                    "Keep search parity with legacy",
                    "",
                    "",
                    "open",
                    2_i32,
                    "feature",
                    "sam",
                    Option::<i32>::None,
                    "[\"auth\",\"export\"]",
                    Option::<String>::None,
                    Option::<String>::None,
                    Option::<String>::None,
                    Option::<String>::None,
                    "security"
                ],
            )
            .expect("insert searchable issue");

        rebuild_issue_search_index(&connection).expect("rebuild issues_fts");

        let exact_matches: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM issues_fts WHERE issues_fts MATCH 'authentication'",
                [],
                |row| row.get(0),
            )
            .expect("exact fts query");
        let prefix_matches: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM issues_fts WHERE issues_fts MATCH 'hard*'",
                [],
                |row| row.get(0),
            )
            .expect("prefix fts query");

        assert_eq!(exact_matches, 1);
        assert_eq!(prefix_matches, 1);
    }

    #[test]
    fn populate_export_database_writes_core_rows_and_meta() {
        let temp = tempdir().expect("tempdir");
        bootstrap_export_database(temp.path(), &SqliteBootstrapOptions::default())
            .expect("bootstrap sqlite export database");

        let issues = vec![
            Issue {
                id: "ISSUE-1".to_string(),
                title: "Export SQLite".to_string(),
                description: "Ship the populated export database".to_string(),
                design: "Reuse analyzer output for deterministic rows".to_string(),
                acceptance_criteria: "Core issue records are queryable".to_string(),
                notes: "Keep row order deterministic".to_string(),
                status: "open".to_string(),
                priority: 1,
                issue_type: "task".to_string(),
                assignee: "alex".to_string(),
                estimated_minutes: Some(45),
                created_at: ts("2026-03-08T18:00:00Z"),
                updated_at: ts("2026-03-08T18:30:00Z"),
                due_date: ts("2026-03-10T00:00:00Z"),
                labels: vec!["export".to_string(), "sqlite".to_string()],
                comments: vec![Comment {
                    id: 7,
                    issue_id: "ISSUE-1".to_string(),
                    author: "alex".to_string(),
                    text: "Need populated export rows".to_string(),
                    created_at: ts("2026-03-08T18:40:00Z"),
                }],
                source_repo: "services/api".to_string(),
                ..Issue::default()
            },
            Issue {
                id: "ISSUE-2".to_string(),
                title: "Downstream consumer".to_string(),
                status: "blocked".to_string(),
                priority: 2,
                issue_type: "task".to_string(),
                dependencies: vec![Dependency {
                    issue_id: "ISSUE-2".to_string(),
                    depends_on_id: "ISSUE-1".to_string(),
                    dep_type: "blocks".to_string(),
                    created_by: "tester".to_string(),
                    created_at: ts("2026-03-08T18:31:00Z"),
                }],
                source_repo: "services/web".to_string(),
                ..Issue::default()
            },
        ];

        let analyzer = Analyzer::new(issues.clone());
        let triage = analyzer.triage(TriageOptions {
            group_by_track: false,
            group_by_label: false,
            max_recommendations: 50,
            ..TriageOptions::default()
        });

        populate_export_database(
            temp.path(),
            Some("SQLite Export Fixture"),
            &issues,
            &analyzer,
            &triage,
        )
        .expect("populate sqlite export database");

        let connection =
            Connection::open(export_database_path(temp.path())).expect("open populated database");

        let issue_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM issues", [], |row| row.get(0))
            .expect("query issues count");
        let dependency_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM dependencies", [], |row| row.get(0))
            .expect("query dependency count");
        let comment_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM comments", [], |row| row.get(0))
            .expect("query comment count");
        let metrics_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM issue_metrics", [], |row| row.get(0))
            .expect("query metrics count");
        let recommendation_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM triage_recommendations", [], |row| {
                row.get(0)
            })
            .expect("query recommendation count");
        let overview_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM issue_overview_mv", [], |row| {
                row.get(0)
            })
            .expect("query overview count");

        assert_eq!(issue_count, 2);
        assert_eq!(dependency_count, 1);
        assert_eq!(comment_count, 1);
        assert_eq!(metrics_count, 2);
        assert_eq!(overview_count, 2);
        assert_eq!(recommendation_count, 1);

        let exported_issue = connection
            .query_row(
                "
                SELECT title, design, acceptance_criteria, notes, labels, source_repo
                FROM issues
                WHERE id = ?
                ",
                ["ISSUE-1"],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .expect("query exported issue");

        assert_eq!(exported_issue.0, "Export SQLite");
        assert_eq!(
            exported_issue.1,
            "Reuse analyzer output for deterministic rows"
        );
        assert_eq!(exported_issue.2, "Core issue records are queryable");
        assert_eq!(exported_issue.3, "Keep row order deterministic");
        assert_eq!(exported_issue.4, "[\"export\",\"sqlite\"]");
        assert_eq!(exported_issue.5, "services/api");

        let overview = connection
            .query_row(
                "
                SELECT dependent_count, blocker_count, comment_count, blocks_ids, blocked_by_ids
                FROM issue_overview_mv
                WHERE id = ?
                ",
                ["ISSUE-1"],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .expect("query overview row");

        assert_eq!(overview.0, 1);
        assert_eq!(overview.1, 0);
        assert_eq!(overview.2, 1);
        assert_eq!(overview.3.as_deref(), Some("ISSUE-2"));
        assert_eq!(overview.4, None);

        let triage_score: f64 = connection
            .query_row(
                "SELECT triage_score FROM issue_metrics WHERE issue_id = ?",
                ["ISSUE-1"],
                |row| row.get(0),
            )
            .expect("query triage score");
        assert!(triage_score > 0.0);

        let meta = export_meta(&connection);
        assert_eq!(
            meta.get("title"),
            Some(&"SQLite Export Fixture".to_string())
        );
        assert_eq!(meta.get("issue_count"), Some(&"2".to_string()));
        assert_eq!(meta.get("dependency_count"), Some(&"1".to_string()));
        assert_eq!(meta.get("comment_count"), Some(&"1".to_string()));
    }

    #[test]
    fn emit_bootstrap_config_writes_hash_and_size_for_single_file_bundle() {
        let temp = tempdir().expect("tempdir");
        let summary = bootstrap_export_database(temp.path(), &SqliteBootstrapOptions::default())
            .expect("bootstrap sqlite export database");

        let config = emit_bootstrap_config(temp.path(), &SqliteBundleOptions::default())
            .expect("emit bootstrap config");

        assert!(summary.database_path.is_file());
        assert!(export_config_path(temp.path()).is_file());
        assert!(!config.chunked);
        assert_eq!(config.chunk_count, 0);
        assert_eq!(config.chunk_size, DEFAULT_SQLITE_CHUNK_SIZE_BYTES);
        assert!(config.total_size > 0);
        assert_eq!(config.hash.len(), 64);
        assert!(config.chunks.is_empty());

        let persisted: SqliteBootstrapConfig = serde_json::from_str(
            &fs::read_to_string(export_config_path(temp.path())).expect("read config json"),
        )
        .expect("parse config json");
        assert_eq!(persisted, config);
    }

    #[test]
    fn emit_bootstrap_config_writes_chunk_inventory_when_threshold_is_exceeded() {
        let temp = tempdir().expect("tempdir");
        bootstrap_export_database(temp.path(), &SqliteBootstrapOptions::default())
            .expect("bootstrap sqlite export database");

        let config = emit_bootstrap_config(
            temp.path(),
            &SqliteBundleOptions {
                chunk_threshold_bytes: 1,
                chunk_size_bytes: 256,
            },
        )
        .expect("emit chunked bootstrap config");

        assert!(config.chunked);
        assert!(!config.chunks.is_empty());
        assert_eq!(config.chunk_count, config.chunks.len());
        assert!(config.chunks.iter().all(|chunk| chunk.hash.len() == 64));
        assert!(
            config
                .chunks
                .iter()
                .all(|chunk| temp.path().join(&chunk.path).is_file())
        );

        let total_chunk_size = config.chunks.iter().map(|chunk| chunk.size).sum::<u64>();
        assert_eq!(total_chunk_size, config.total_size);
    }

    fn sqlite_object_inventory(connection: &Connection) -> BTreeSet<String> {
        let mut stmt = connection
            .prepare(
                "
                SELECT name
                FROM sqlite_master
                WHERE type IN ('table', 'index')
                    AND name NOT LIKE 'sqlite_%'
                ORDER BY name
                ",
            )
            .expect("prepare sqlite inventory query");
        let names = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query sqlite inventory");
        names
            .collect::<std::result::Result<BTreeSet<_>, _>>()
            .expect("collect sqlite inventory")
    }

    fn export_meta(connection: &Connection) -> BTreeMap<String, String> {
        let mut stmt = connection
            .prepare("SELECT key, value FROM export_meta ORDER BY key")
            .expect("prepare export_meta query");
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .expect("query export_meta");
        rows.collect::<std::result::Result<BTreeMap<_, _>, _>>()
            .expect("collect export_meta")
    }
}
