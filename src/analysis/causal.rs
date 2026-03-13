use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use serde::Serialize;

use super::git_history::HistoryBeadCompat;
use super::graph::IssueGraph;
use crate::model::Issue;

// ---------------------------------------------------------------------------
// Blocker Chain
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct BlockerChainEntry {
    pub id: String,
    pub title: String,
    pub status: String,
    pub priority: i32,
    pub depth: usize,
    pub is_root: bool,
    pub actionable: bool,
    pub blocks_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlockerChainResult {
    pub target_id: String,
    pub target_title: String,
    pub is_blocked: bool,
    pub chain_length: usize,
    pub root_blockers: Vec<BlockerChainEntry>,
    pub chain: Vec<BlockerChainEntry>,
    pub has_cycle: bool,
    pub cycle_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct RobotBlockerChainOutput {
    #[serde(flatten)]
    pub envelope: crate::robot::RobotEnvelope,
    #[serde(flatten)]
    pub result: BlockerChainResult,
}

/// BFS upward from target through blocker edges to find the full blocker chain.
pub fn get_blocker_chain(graph: &IssueGraph, target_id: &str) -> BlockerChainResult {
    let issue = graph.issue(target_id);
    let target_title = issue.map_or_else(String::new, |i| i.title.clone());

    let open_blockers = graph.open_blockers(target_id);
    if open_blockers.is_empty() {
        return BlockerChainResult {
            target_id: target_id.to_string(),
            target_title,
            is_blocked: false,
            chain_length: 0,
            root_blockers: Vec::new(),
            chain: Vec::new(),
            has_cycle: false,
            cycle_ids: Vec::new(),
        };
    }

    let mut visited = HashSet::new();
    visited.insert(target_id.to_string());

    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    for blocker_id in &open_blockers {
        queue.push_back((blocker_id.clone(), 1));
    }

    let mut chain = Vec::new();
    let mut roots = Vec::new();
    let mut cycle_ids = Vec::new();

    while let Some((id, depth)) = queue.pop_front() {
        if !visited.insert(id.clone()) {
            cycle_ids.push(id);
            continue;
        }

        let entry_issue = graph.issue(&id);
        let title = entry_issue.map_or_else(String::new, |i| i.title.clone());
        let status = entry_issue.map_or_else(|| "unknown".to_string(), |i| i.status.clone());
        let priority = entry_issue.map_or(99, |i| i.priority);

        let this_open_blockers = graph.open_blockers(&id);
        let is_root = this_open_blockers.is_empty();
        let actionable = is_root && entry_issue.is_some_and(Issue::is_open_like);

        let dependents = graph.dependents(&id);
        let blocks_count = dependents
            .iter()
            .filter(|dep_id| graph.issue(dep_id).is_some_and(Issue::is_open_like))
            .count();

        let entry = BlockerChainEntry {
            id: id.clone(),
            title,
            status,
            priority,
            depth,
            is_root,
            actionable,
            blocks_count,
        };

        if is_root {
            roots.push(entry.clone());
        }
        chain.push(entry);

        if !is_root {
            for blocker_id in &this_open_blockers {
                if !visited.contains(blocker_id) {
                    queue.push_back((blocker_id.clone(), depth + 1));
                }
            }
        }
    }

    // Sort chain by depth ascending, then by ID for determinism
    chain.sort_by(|a, b| a.depth.cmp(&b.depth).then_with(|| a.id.cmp(&b.id)));

    // Sort roots by priority (lower = higher priority), then by ID
    roots.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.id.cmp(&b.id)));

    cycle_ids.sort();
    cycle_ids.dedup();
    let has_cycle = !cycle_ids.is_empty();

    BlockerChainResult {
        target_id: target_id.to_string(),
        target_title,
        is_blocked: true,
        chain_length: chain.len(),
        root_blockers: roots,
        chain,
        has_cycle,
        cycle_ids,
    }
}

// ---------------------------------------------------------------------------
// Impact Network
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct NetworkNode {
    pub bead_id: String,
    pub title: String,
    pub status: String,
    pub priority: i32,
    pub degree: usize,
    pub commit_count: usize,
    pub file_count: usize,
    pub cluster_id: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct NetworkEdge {
    pub from_bead: String,
    pub to_bead: String,
    pub edge_type: String,
    pub weight: usize,
    pub details: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BeadCluster {
    pub cluster_id: usize,
    pub bead_ids: Vec<String>,
    pub label: String,
    pub internal_edges: usize,
    pub central_bead: String,
    pub shared_files: Vec<String>,
    pub total_commits: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct NetworkStats {
    pub total_nodes: usize,
    pub total_edges: usize,
    pub cluster_count: usize,
    pub avg_degree: f64,
    pub max_degree: usize,
    pub density: f64,
    pub isolated_nodes: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImpactNetwork {
    pub nodes: Vec<NetworkNode>,
    pub edges: Vec<NetworkEdge>,
    pub clusters: Vec<BeadCluster>,
    pub stats: NetworkStats,
}

#[derive(Debug, Serialize)]
pub struct ImpactNetworkResult {
    pub bead_id: String,
    pub depth: usize,
    pub network: ImpactNetwork,
    pub top_connected: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct RobotImpactNetworkOutput {
    #[serde(flatten)]
    pub envelope: crate::robot::RobotEnvelope,
    #[serde(flatten)]
    pub result: ImpactNetworkResult,
}

/// Build the full impact network from issue dependencies and shared file modifications.
pub fn build_impact_network(
    graph: &IssueGraph,
    histories: &BTreeMap<String, HistoryBeadCompat>,
) -> ImpactNetwork {
    let all_ids = graph.issue_ids_sorted();

    // Build edges from three sources: dependencies, shared commits, shared files
    let mut edges = Vec::new();
    let mut degree_map: HashMap<String, usize> = HashMap::new();

    // 1. Dependency edges
    for id in &all_ids {
        for blocker_id in graph.blockers(id) {
            // Canonical ordering for dedup: smaller ID first
            let (from, to) = if *id < blocker_id {
                (id.clone(), blocker_id.clone())
            } else {
                (blocker_id.clone(), id.clone())
            };
            edges.push(NetworkEdge {
                from_bead: from,
                to_bead: to,
                edge_type: "dependency".to_string(),
                weight: 1,
                details: Vec::new(),
            });
        }
    }

    // 2. Shared-commit edges: beads that share the same commit SHA
    let mut commit_to_beads: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for history in histories.values() {
        for commit in history.commits.as_deref().unwrap_or_default() {
            commit_to_beads
                .entry(commit.sha.clone())
                .or_default()
                .insert(history.bead_id.clone());
        }
    }
    let mut shared_commit_edges: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for (sha, bead_ids) in &commit_to_beads {
        let ids: Vec<&String> = bead_ids.iter().collect();
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                let key = (ids[i].clone(), ids[j].clone());
                shared_commit_edges
                    .entry(key)
                    .or_default()
                    .push(sha.clone());
            }
        }
    }
    for ((from, to), shas) in &shared_commit_edges {
        let details: Vec<String> = shas.iter().take(5).cloned().collect();
        edges.push(NetworkEdge {
            from_bead: from.clone(),
            to_bead: to.clone(),
            edge_type: "shared_commit".to_string(),
            weight: shas.len(),
            details,
        });
    }

    // 3. Shared-file edges: beads that touched the same files
    let mut bead_files: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for history in histories.values() {
        for commit in history.commits.as_deref().unwrap_or_default() {
            for file in &commit.files {
                bead_files
                    .entry(history.bead_id.clone())
                    .or_default()
                    .insert(file.path.clone());
            }
        }
    }
    let mut shared_file_edges: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    let bead_ids: Vec<&String> = bead_files.keys().collect();
    for i in 0..bead_ids.len() {
        for j in (i + 1)..bead_ids.len() {
            let files_a = &bead_files[bead_ids[i]];
            let files_b = &bead_files[bead_ids[j]];
            let shared: Vec<String> = files_a.intersection(files_b).cloned().collect();
            if shared.len() >= 2 {
                let key = (bead_ids[i].clone(), bead_ids[j].clone());
                shared_file_edges.insert(key, shared);
            }
        }
    }
    for ((from, to), files) in &shared_file_edges {
        let details: Vec<String> = files.iter().take(5).cloned().collect();
        edges.push(NetworkEdge {
            from_bead: from.clone(),
            to_bead: to.clone(),
            edge_type: "shared_file".to_string(),
            weight: files.len(),
            details,
        });
    }

    // Deduplicate edges by (from, to, type)
    edges.sort_by(|a, b| {
        a.from_bead
            .cmp(&b.from_bead)
            .then_with(|| a.to_bead.cmp(&b.to_bead))
            .then_with(|| a.edge_type.cmp(&b.edge_type))
    });
    edges.dedup_by(|a, b| {
        a.from_bead == b.from_bead && a.to_bead == b.to_bead && a.edge_type == b.edge_type
    });

    // Compute degrees
    for edge in &edges {
        *degree_map.entry(edge.from_bead.clone()).or_default() += 1;
        *degree_map.entry(edge.to_bead.clone()).or_default() += 1;
    }

    // Build nodes
    let mut nodes: Vec<NetworkNode> = all_ids
        .iter()
        .map(|id| {
            let issue = graph.issue(id);
            let commit_count = histories
                .get(id)
                .and_then(|h| h.commits.as_ref())
                .map_or(0, Vec::len);
            let file_count = bead_files.get(id).map_or(0, BTreeSet::len);

            NetworkNode {
                bead_id: id.clone(),
                title: issue.map_or_else(String::new, |i| i.title.clone()),
                status: issue.map_or_else(|| "unknown".to_string(), |i| i.status.clone()),
                priority: issue.map_or(99, |i| i.priority),
                degree: degree_map.get(id).copied().unwrap_or(0),
                commit_count,
                file_count,
                cluster_id: -1,
            }
        })
        .collect();

    // Simple cluster detection via connected components on edge graph
    let mut clusters = detect_clusters(&edges, &mut nodes, &bead_files);
    clusters.sort_by_key(|b| std::cmp::Reverse(b.bead_ids.len()));

    let isolated_nodes = nodes.iter().filter(|n| n.degree == 0).count();
    let max_degree = nodes.iter().map(|n| n.degree).max().unwrap_or(0);
    let avg_degree = if nodes.is_empty() {
        0.0
    } else {
        nodes.iter().map(|n| n.degree).sum::<usize>() as f64 / nodes.len() as f64
    };
    let n = nodes.len();
    let density = if n > 1 {
        (2.0 * edges.len() as f64) / (n as f64 * (n as f64 - 1.0))
    } else {
        0.0
    };

    ImpactNetwork {
        stats: NetworkStats {
            total_nodes: nodes.len(),
            total_edges: edges.len(),
            cluster_count: clusters.len(),
            avg_degree,
            max_degree,
            density,
            isolated_nodes,
        },
        nodes,
        edges,
        clusters,
    }
}

fn detect_clusters(
    edges: &[NetworkEdge],
    nodes: &mut [NetworkNode],
    bead_files: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<BeadCluster> {
    // Build adjacency list
    let mut adj: HashMap<String, HashSet<String>> = HashMap::new();
    for edge in edges {
        adj.entry(edge.from_bead.clone())
            .or_default()
            .insert(edge.to_bead.clone());
        adj.entry(edge.to_bead.clone())
            .or_default()
            .insert(edge.from_bead.clone());
    }

    let mut visited = HashSet::new();
    let mut clusters = Vec::new();
    let mut cluster_id = 0usize;

    // Collect all node IDs that appear in edges
    let all_edge_nodes: BTreeSet<String> = adj.keys().cloned().collect();

    for start in &all_edge_nodes {
        if visited.contains(start) {
            continue;
        }

        // BFS to find connected component
        let mut component = Vec::new();
        let mut queue = VecDeque::new();
        queue.push_back(start.clone());
        visited.insert(start.clone());

        while let Some(current) = queue.pop_front() {
            component.push(current.clone());
            if let Some(neighbors) = adj.get(&current) {
                for neighbor in neighbors {
                    if visited.insert(neighbor.clone()) {
                        queue.push_back(neighbor.clone());
                    }
                }
            }
        }

        if component.len() < 2 {
            continue;
        }

        component.sort();

        // Compute cluster metadata
        let component_set: HashSet<&String> = component.iter().collect();
        let internal_edges = edges
            .iter()
            .filter(|e| component_set.contains(&e.from_bead) && component_set.contains(&e.to_bead))
            .count();

        // Shared files across cluster
        let mut file_counts: BTreeMap<String, usize> = BTreeMap::new();
        for bead_id in &component {
            if let Some(files) = bead_files.get(bead_id) {
                for f in files {
                    *file_counts.entry(f.clone()).or_default() += 1;
                }
            }
        }
        let mut shared_files: Vec<String> = file_counts
            .iter()
            .filter(|(_, count)| **count >= 2)
            .map(|(f, _)| f.clone())
            .collect();
        shared_files.sort();
        shared_files.truncate(10);

        // Label from most common shared file prefix
        let label = shared_files.first().map_or_else(
            || format!("cluster-{cluster_id}"),
            |f| {
                f.rsplit_once('/')
                    .map_or_else(|| f.clone(), |(dir, _)| dir.to_string())
            },
        );

        // Central bead: highest degree in cluster
        let central_bead = component
            .iter()
            .max_by_key(|id| {
                adj.get(*id).map_or(0, |n| {
                    n.iter().filter(|x| component_set.contains(x)).count()
                })
            })
            .cloned()
            .unwrap_or_default();

        let total_commits: usize = component
            .iter()
            .filter_map(|id| nodes.iter().find(|n| n.bead_id == *id))
            .map(|n| n.commit_count)
            .sum();

        let cluster_id_i32 = i32::try_from(cluster_id).unwrap_or(i32::MAX);
        // Assign cluster_id to nodes
        for node in nodes.iter_mut() {
            if component_set.contains(&node.bead_id) {
                node.cluster_id = cluster_id_i32;
            }
        }

        clusters.push(BeadCluster {
            cluster_id,
            bead_ids: component,
            label,
            internal_edges,
            central_bead,
            shared_files,
            total_commits,
        });

        cluster_id += 1;
    }

    clusters
}

/// Extract a subnetwork around a specific bead up to a given depth (capped at 3).
pub fn get_subnetwork(network: &ImpactNetwork, bead_id: &str, depth: usize) -> ImpactNetwork {
    let capped_depth = depth.clamp(1, 3);

    let mut visited = HashSet::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    queue.push_back((bead_id.to_string(), 0));
    visited.insert(bead_id.to_string());

    while let Some((current, level)) = queue.pop_front() {
        if level >= capped_depth {
            continue;
        }
        for edge in &network.edges {
            let neighbor = if edge.from_bead == current {
                &edge.to_bead
            } else if edge.to_bead == current {
                &edge.from_bead
            } else {
                continue;
            };
            if visited.insert(neighbor.clone()) {
                queue.push_back((neighbor.clone(), level + 1));
            }
        }
    }

    let sub_edges: Vec<NetworkEdge> = network
        .edges
        .iter()
        .filter(|e| visited.contains(&e.from_bead) && visited.contains(&e.to_bead))
        .cloned()
        .collect();

    let mut degree_map: HashMap<String, usize> = HashMap::new();
    for edge in &sub_edges {
        *degree_map.entry(edge.from_bead.clone()).or_default() += 1;
        *degree_map.entry(edge.to_bead.clone()).or_default() += 1;
    }

    let sub_nodes: Vec<NetworkNode> = network
        .nodes
        .iter()
        .filter(|n| visited.contains(&n.bead_id))
        .cloned()
        .map(|mut n| {
            n.degree = degree_map.get(&n.bead_id).copied().unwrap_or(0);
            n
        })
        .collect();

    let isolated = sub_nodes.iter().filter(|n| n.degree == 0).count();
    let max_degree = sub_nodes.iter().map(|n| n.degree).max().unwrap_or(0);
    let avg_degree = if sub_nodes.is_empty() {
        0.0
    } else {
        sub_nodes.iter().map(|n| n.degree).sum::<usize>() as f64 / sub_nodes.len() as f64
    };
    let n = sub_nodes.len();
    let density = if n > 1 {
        (2.0 * sub_edges.len() as f64) / (n as f64 * (n as f64 - 1.0))
    } else {
        0.0
    };

    // Recompute clusters for subnetwork
    let cluster_ids: BTreeSet<usize> = sub_nodes
        .iter()
        .filter_map(|n| usize::try_from(n.cluster_id).ok())
        .collect();
    let sub_clusters: Vec<BeadCluster> = network
        .clusters
        .iter()
        .filter(|c| cluster_ids.contains(&c.cluster_id))
        .cloned()
        .collect();

    ImpactNetwork {
        stats: NetworkStats {
            total_nodes: sub_nodes.len(),
            total_edges: sub_edges.len(),
            cluster_count: sub_clusters.len(),
            avg_degree,
            max_degree,
            density,
            isolated_nodes: isolated,
        },
        nodes: sub_nodes,
        edges: sub_edges,
        clusters: sub_clusters,
    }
}

/// Build impact network result, either full or subnetwork for a specific bead.
pub fn build_impact_network_result(
    graph: &IssueGraph,
    histories: &BTreeMap<String, HistoryBeadCompat>,
    bead_id: &str,
    depth: usize,
) -> ImpactNetworkResult {
    let full_network = build_impact_network(graph, histories);

    let (network, effective_bead_id, effective_depth) = if bead_id.is_empty() || bead_id == "all" {
        (full_network, String::new(), 0)
    } else {
        let sub = get_subnetwork(&full_network, bead_id, depth);
        (sub, bead_id.to_string(), depth.clamp(1, 3))
    };

    let mut top_connected: Vec<String> = network
        .nodes
        .iter()
        .filter(|n| n.degree > 0)
        .map(|n| n.bead_id.clone())
        .collect();
    top_connected.sort_by(|a, b| {
        let da = network
            .nodes
            .iter()
            .find(|n| n.bead_id == *a)
            .map_or(0, |n| n.degree);
        let db = network
            .nodes
            .iter()
            .find(|n| n.bead_id == *b)
            .map_or(0, |n| n.degree);
        db.cmp(&da).then_with(|| a.cmp(b))
    });
    top_connected.truncate(10);

    ImpactNetworkResult {
        bead_id: effective_bead_id,
        depth: effective_depth,
        network,
        top_connected,
    }
}

// ---------------------------------------------------------------------------
// Causality Analysis
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct CausalEvent {
    pub id: usize,
    #[serde(rename = "type")]
    pub event_type: String,
    pub timestamp: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocker_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caused_by_id: Option<usize>,
    pub enables_ids: Vec<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_next_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CausalChain {
    pub bead_id: String,
    pub title: String,
    pub status: String,
    pub events: Vec<CausalEvent>,
    pub edge_count: usize,
    pub start_time: String,
    pub end_time: String,
    pub is_complete: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlockedPeriod {
    pub start_time: String,
    pub end_time: String,
    pub duration_ms: u64,
    pub blocker_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CausalInsights {
    pub total_duration_ms: u64,
    pub blocked_duration_ms: u64,
    pub active_duration_ms: u64,
    pub blocked_percentage: f64,
    pub blocked_periods: Vec<BlockedPeriod>,
    pub commit_count: usize,
    pub avg_time_between_ms: u64,
    pub longest_gap_ms: u64,
    pub longest_gap_desc: String,
    pub summary: String,
    pub recommendations: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CausalityResult {
    pub chain: CausalChain,
    pub insights: CausalInsights,
}

#[derive(Debug, Serialize)]
pub struct RobotCausalityOutput {
    #[serde(flatten)]
    pub envelope: crate::robot::RobotEnvelope,
    #[serde(flatten)]
    pub result: CausalityResult,
}

/// Build the causal chain for a specific bead from its history events and commits.
pub fn build_causality_chain(
    bead_id: &str,
    histories: &BTreeMap<String, HistoryBeadCompat>,
    graph: &IssueGraph,
) -> CausalityResult {
    let issue = graph.issue(bead_id);
    let title = issue.map_or_else(String::new, |i| i.title.clone());
    let status = issue.map_or_else(|| "unknown".to_string(), |i| i.status.clone());

    let history = histories.get(bead_id);

    // Collect raw events
    let mut raw_events: Vec<(String, String, Option<String>, Option<String>)> = Vec::new();
    // (timestamp, type, commit_sha, blocker_id)

    if let Some(h) = history {
        // Lifecycle events
        for event in &h.events {
            let event_type = match event.event_type.as_str() {
                "created" | "claimed" | "closed" | "reopened" => event.event_type.clone(),
                "status_change" => "status_change".to_string(),
                other => other.to_string(),
            };
            raw_events.push((event.timestamp.clone(), event_type, None, None));
        }

        // Commit events
        if let Some(commits) = &h.commits {
            for commit in commits {
                raw_events.push((
                    commit.timestamp.clone(),
                    "commit".to_string(),
                    Some(commit.short_sha.clone()),
                    None,
                ));
            }
        }
    }

    // Check for blocking relationships
    let blockers = graph.blockers(bead_id);
    for blocker_id in &blockers {
        if let Some(blocker_history) = histories.get(blocker_id) {
            // Find when blocker was closed (unblocked event)
            if let Some(closed_event) = blocker_history
                .events
                .iter()
                .find(|e| e.event_type == "closed")
            {
                raw_events.push((
                    closed_event.timestamp.clone(),
                    "unblocked".to_string(),
                    None,
                    Some(blocker_id.clone()),
                ));
            }
            // Find when blocking relationship was created (blocked event)
            if let Some(created_event) = blocker_history.events.first() {
                raw_events.push((
                    created_event.timestamp.clone(),
                    "blocked".to_string(),
                    None,
                    Some(blocker_id.clone()),
                ));
            }
        }
    }

    // Sort by timestamp, then by event type for stability
    raw_events.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    raw_events.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1 && a.2 == b.2 && a.3 == b.3);

    // Build causal events
    let mut events: Vec<CausalEvent> = Vec::new();
    let mut commit_count = 0usize;

    for (idx, (timestamp, event_type, commit_sha, blocker_id)) in raw_events.iter().enumerate() {
        let description = match event_type.as_str() {
            "created" => "Issue created".to_string(),
            "claimed" => "Work started (claimed)".to_string(),
            "commit" => format!("Commit {}", commit_sha.as_deref().unwrap_or("unknown")),
            "closed" => "Issue closed".to_string(),
            "reopened" => "Issue reopened".to_string(),
            "blocked" => format!("Blocked by {}", blocker_id.as_deref().unwrap_or("unknown")),
            "unblocked" => format!(
                "Unblocked ({} closed)",
                blocker_id.as_deref().unwrap_or("unknown")
            ),
            other => other.to_string(),
        };

        if event_type == "commit" {
            commit_count += 1;
        }

        let caused_by_id = if idx > 0 { Some(idx - 1) } else { None };
        let enables_ids = if idx + 1 < raw_events.len() {
            vec![idx + 1]
        } else {
            Vec::new()
        };

        events.push(CausalEvent {
            id: idx,
            event_type: event_type.clone(),
            timestamp: timestamp.clone(),
            description,
            commit_sha: commit_sha.clone(),
            blocker_id: blocker_id.clone(),
            caused_by_id,
            enables_ids,
            duration_next_ms: None,
        });
    }

    // Calculate duration_next_ms between consecutive events
    for i in 0..events.len().saturating_sub(1) {
        let current_ts = parse_timestamp_ms(&events[i].timestamp);
        let next_ts = parse_timestamp_ms(&events[i + 1].timestamp);
        if let (Some(c), Some(n)) = (current_ts, next_ts) {
            events[i].duration_next_ms = Some(n.saturating_sub(c));
        }
    }

    let start_time = events
        .first()
        .map_or_else(String::new, |e| e.timestamp.clone());
    let end_time = events
        .last()
        .map_or_else(String::new, |e| e.timestamp.clone());
    let edge_count = events.len().saturating_sub(1);

    let is_complete = issue.is_some_and(|i| !i.is_open_like());

    // Compute insights
    let total_duration_ms = {
        let start = parse_timestamp_ms(&start_time);
        let end = parse_timestamp_ms(&end_time);
        match (start, end) {
            (Some(s), Some(e)) => e.saturating_sub(s),
            _ => 0,
        }
    };

    // Find blocked periods
    let mut blocked_periods = Vec::new();
    let mut blocked_start: Option<(String, String)> = None; // (timestamp, blocker_id)

    for event in &events {
        match event.event_type.as_str() {
            "blocked" => {
                if blocked_start.is_none() {
                    blocked_start = Some((
                        event.timestamp.clone(),
                        event.blocker_id.clone().unwrap_or_default(),
                    ));
                }
            }
            "unblocked" => {
                if let Some((start, blocker)) = blocked_start.take() {
                    let start_ms = parse_timestamp_ms(&start).unwrap_or(0);
                    let end_ms = parse_timestamp_ms(&event.timestamp).unwrap_or(0);
                    blocked_periods.push(BlockedPeriod {
                        start_time: start,
                        end_time: event.timestamp.clone(),
                        duration_ms: end_ms.saturating_sub(start_ms),
                        blocker_id: blocker,
                    });
                }
            }
            _ => {}
        }
    }

    let blocked_duration_ms: u64 = blocked_periods.iter().map(|p| p.duration_ms).sum();
    let active_duration_ms = total_duration_ms.saturating_sub(blocked_duration_ms);
    let blocked_percentage = if total_duration_ms > 0 {
        (blocked_duration_ms as f64 / total_duration_ms as f64) * 100.0
    } else {
        0.0
    };

    // Compute gaps
    let gaps: Vec<u64> = events.iter().filter_map(|e| e.duration_next_ms).collect();
    let avg_time_between_ms = if gaps.is_empty() {
        0
    } else {
        gaps.iter().sum::<u64>() / gaps.len() as u64
    };
    let longest_gap_ms = gaps.iter().copied().max().unwrap_or(0);
    let longest_gap_desc = if longest_gap_ms > 0 {
        let days = longest_gap_ms / 86_400_000;
        if days > 0 {
            format!("{days}d gap between events")
        } else {
            let hours = longest_gap_ms / 3_600_000;
            format!("{hours}h gap between events")
        }
    } else {
        String::new()
    };

    // Generate summary and recommendations
    let total_days = total_duration_ms / 86_400_000;
    let summary = if is_complete {
        format!("Completed in {total_days}d ({blocked_percentage:.0}% blocked)")
    } else {
        format!("In progress for {total_days}d ({blocked_percentage:.0}% blocked)")
    };

    let mut recommendations = Vec::new();
    if blocked_percentage > 25.0 {
        recommendations.push(format!(
            "High blocked percentage ({blocked_percentage:.0}%) - consider addressing blockers earlier"
        ));
    }
    if commit_count > 0 && total_days > 0 {
        let commits_per_day = commit_count as f64 / total_days as f64;
        if commits_per_day < 0.1 {
            recommendations.push(
                "Few commits over long period - consider more frequent incremental commits"
                    .to_string(),
            );
        }
    }
    if events.is_empty() {
        recommendations.push("No history events found - check data completeness".to_string());
    }

    CausalityResult {
        chain: CausalChain {
            bead_id: bead_id.to_string(),
            title,
            status,
            events,
            edge_count,
            start_time,
            end_time,
            is_complete,
        },
        insights: CausalInsights {
            total_duration_ms,
            blocked_duration_ms,
            active_duration_ms,
            blocked_percentage,
            blocked_periods,
            commit_count,
            avg_time_between_ms,
            longest_gap_ms,
            longest_gap_desc,
            summary,
            recommendations,
        },
    }
}

/// Parse an ISO-8601 timestamp string to milliseconds since epoch (public API).
pub fn parse_timestamp_ms_pub(ts: &str) -> Option<u64> {
    parse_timestamp_ms(ts)
}

/// Parse an ISO-8601 timestamp string to milliseconds since epoch.
fn parse_timestamp_ms(ts: &str) -> Option<u64> {
    if ts.is_empty() {
        return None;
    }
    // Parse RFC3339/ISO-8601 timestamps
    // Format: 2025-02-27T07:00:00Z or 2025-02-27T07:00:00+00:00
    let ts = ts.trim();

    // Try to extract year-month-day hour:minute:second
    let parts: Vec<&str> = ts.split('T').collect();
    if parts.len() != 2 {
        return None;
    }

    let date_parts: Vec<u64> = parts[0].split('-').filter_map(|p| p.parse().ok()).collect();
    if date_parts.len() != 3 {
        return None;
    }

    let time_str = parts[1]
        .trim_end_matches('Z')
        .split('+')
        .next()?
        .split('-')
        .next()?
        .split('.')
        .next()?;
    let time_parts: Vec<u64> = time_str.split(':').filter_map(|p| p.parse().ok()).collect();
    if time_parts.len() < 2 {
        return None;
    }

    let year = date_parts[0];
    let month = date_parts[1];
    let day = date_parts[2];
    let hour = time_parts[0];
    let minute = time_parts[1];
    let second = if time_parts.len() > 2 {
        time_parts[2]
    } else {
        0
    };

    // Simple epoch calculation (approximate, good enough for duration differences)
    let days_since_epoch = days_from_date(year, month, day)?;
    Some(((days_since_epoch * 86400) + (hour * 3600) + (minute * 60) + second) * 1000)
}

fn days_from_date(year: u64, month: u64, day: u64) -> Option<u64> {
    if year < 1970 || month == 0 || month > 12 || day == 0 || day > 31 {
        return None;
    }
    let mut days = 0u64;
    for y in 1970..year {
        days += if is_leap(y) { 366 } else { 365 };
    }
    let month_days: [u64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for m in 1..month {
        let month_index = usize::try_from(m - 1).ok()?;
        days += month_days[month_index];
        if m == 2 && is_leap(year) {
            days += 1;
        }
    }
    days += day - 1;
    Some(days)
}

const fn is_leap(year: u64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::git_history::{
        HistoryBeadCompat, HistoryCommitCompat, HistoryEventCompat, HistoryFileChangeCompat,
        HistoryMilestonesCompat,
    };
    use crate::model::{Dependency, Issue};

    fn make_issue(id: &str, title: &str, status: &str, priority: i32) -> Issue {
        Issue {
            id: id.to_string(),
            title: title.to_string(),
            status: status.to_string(),
            priority,
            ..Issue::default()
        }
    }

    fn make_issue_with_deps(
        id: &str,
        title: &str,
        status: &str,
        priority: i32,
        blockers: &[&str],
    ) -> Issue {
        let dependencies = blockers
            .iter()
            .map(|blocker_id| Dependency {
                issue_id: id.to_string(),
                depends_on_id: blocker_id.to_string(),
                dep_type: "blocks".to_string(),
                ..Dependency::default()
            })
            .collect();
        Issue {
            id: id.to_string(),
            title: title.to_string(),
            status: status.to_string(),
            priority,
            dependencies,
            ..Issue::default()
        }
    }

    fn make_history(
        bead_id: &str,
        events: Vec<(&str, &str)>,
        commits: Vec<(&str, &str, Vec<&str>)>,
    ) -> HistoryBeadCompat {
        HistoryBeadCompat {
            bead_id: bead_id.to_string(),
            title: String::new(),
            status: String::new(),
            events: events
                .into_iter()
                .map(|(ts, etype)| HistoryEventCompat {
                    bead_id: bead_id.to_string(),
                    event_type: etype.to_string(),
                    timestamp: ts.to_string(),
                    commit_sha: String::new(),
                    commit_message: String::new(),
                    author: String::new(),
                    author_email: String::new(),
                })
                .collect(),
            milestones: HistoryMilestonesCompat::default(),
            commits: Some(
                commits
                    .into_iter()
                    .enumerate()
                    .map(|(i, (sha, ts, files))| HistoryCommitCompat {
                        sha: sha.to_string(),
                        short_sha: sha[..7.min(sha.len())].to_string(),
                        message: format!("commit {i}"),
                        author: "dev".to_string(),
                        author_email: "dev@test.com".to_string(),
                        timestamp: ts.to_string(),
                        files: files
                            .into_iter()
                            .map(|p| HistoryFileChangeCompat {
                                path: p.to_string(),
                                action: "modified".to_string(),
                                insertions: 10,
                                deletions: 5,
                            })
                            .collect(),
                        method: "message".to_string(),
                        confidence: 0.9,
                        reason: "test".to_string(),
                    })
                    .collect(),
            ),
            cycle_time: None,
            last_author: String::new(),
        }
    }

    // ---- Blocker Chain Tests ----

    #[test]
    fn blocker_chain_no_blockers() {
        let issues = vec![make_issue("A", "Task A", "open", 1)];
        let graph = IssueGraph::build(&issues);

        let result = get_blocker_chain(&graph, "A");
        assert!(!result.is_blocked);
        assert_eq!(result.chain_length, 0);
        assert!(result.chain.is_empty());
        assert!(result.root_blockers.is_empty());
    }

    #[test]
    fn blocker_chain_simple() {
        let issues = vec![
            make_issue("A", "Root", "open", 1),
            make_issue_with_deps("B", "Blocked", "blocked", 2, &["A"]),
        ];
        let graph = IssueGraph::build(&issues);

        let result = get_blocker_chain(&graph, "B");
        assert!(result.is_blocked);
        assert_eq!(result.chain_length, 1);
        assert_eq!(result.chain[0].id, "A");
        assert!(result.chain[0].is_root);
        assert!(result.chain[0].actionable);
        assert_eq!(result.root_blockers.len(), 1);
        assert!(!result.has_cycle);
    }

    #[test]
    fn blocker_chain_deep() {
        let issues = vec![
            make_issue("A", "Root", "open", 1),
            make_issue_with_deps("B", "Mid", "blocked", 2, &["A"]),
            make_issue_with_deps("C", "Target", "blocked", 3, &["B"]),
        ];
        let graph = IssueGraph::build(&issues);

        let result = get_blocker_chain(&graph, "C");
        assert!(result.is_blocked);
        assert_eq!(result.chain_length, 2);
        assert_eq!(result.chain[0].depth, 1); // B at depth 1
        assert_eq!(result.chain[1].depth, 2); // A at depth 2
        assert_eq!(result.root_blockers.len(), 1);
        assert_eq!(result.root_blockers[0].id, "A");
    }

    #[test]
    fn blocker_chain_closed_blocker_not_traversed() {
        let issues = vec![
            make_issue("A", "Closed root", "closed", 1),
            make_issue_with_deps("B", "Target", "open", 2, &["A"]),
        ];
        let graph = IssueGraph::build(&issues);

        let result = get_blocker_chain(&graph, "B");
        assert!(!result.is_blocked); // A is closed, so B is not blocked
    }

    #[test]
    fn blocker_chain_deterministic_sorting() {
        let issues = vec![
            make_issue("R1", "Root 1", "open", 3),
            make_issue("R2", "Root 2", "open", 1),
            make_issue_with_deps("T", "Target", "blocked", 2, &["R1", "R2"]),
        ];
        let graph = IssueGraph::build(&issues);

        let result = get_blocker_chain(&graph, "T");
        // Root blockers sorted by priority
        assert_eq!(result.root_blockers[0].id, "R2"); // priority 1
        assert_eq!(result.root_blockers[1].id, "R1"); // priority 3
    }

    // ---- Impact Network Tests ----

    #[test]
    fn impact_network_empty() {
        let issues = vec![make_issue("A", "Solo", "open", 1)];
        let graph = IssueGraph::build(&issues);
        let histories = BTreeMap::new();

        let network = build_impact_network(&graph, &histories);
        assert_eq!(network.stats.total_nodes, 1);
        assert_eq!(network.stats.total_edges, 0);
        assert_eq!(network.stats.isolated_nodes, 1);
    }

    #[test]
    fn impact_network_dependency_edges() {
        let issues = vec![
            make_issue("A", "Root", "open", 1),
            make_issue_with_deps("B", "Blocked", "blocked", 2, &["A"]),
        ];
        let graph = IssueGraph::build(&issues);
        let histories = BTreeMap::new();

        let network = build_impact_network(&graph, &histories);
        assert_eq!(network.stats.total_edges, 1);
        let edge = &network.edges[0];
        assert_eq!(edge.edge_type, "dependency");
    }

    #[test]
    fn impact_network_shared_commits() {
        let issues = vec![
            make_issue("A", "Task A", "open", 1),
            make_issue("B", "Task B", "open", 1),
        ];
        let graph = IssueGraph::build(&issues);

        let mut histories = BTreeMap::new();
        histories.insert(
            "A".to_string(),
            make_history(
                "A",
                vec![],
                vec![("sha1", "2025-01-01T00:00:00Z", vec!["f1.rs"])],
            ),
        );
        histories.insert(
            "B".to_string(),
            make_history(
                "B",
                vec![],
                vec![("sha1", "2025-01-01T00:00:00Z", vec!["f2.rs"])],
            ),
        );

        let network = build_impact_network(&graph, &histories);
        // Should have a shared_commit edge
        assert!(network.edges.iter().any(|e| e.edge_type == "shared_commit"));
    }

    #[test]
    fn impact_network_subnetwork() {
        let issues = vec![
            make_issue("A", "A", "open", 1),
            make_issue_with_deps("B", "B", "open", 1, &["A"]),
            make_issue("C", "C", "open", 1),
            make_issue_with_deps("D", "D", "open", 1, &["C"]),
        ];
        let graph = IssueGraph::build(&issues);
        let histories = BTreeMap::new();

        let full = build_impact_network(&graph, &histories);
        assert_eq!(full.stats.total_nodes, 4);

        let sub = get_subnetwork(&full, "A", 1);
        // Should include A and B (connected by dependency), but not C or D
        assert!(sub.nodes.iter().any(|n| n.bead_id == "A"));
        assert!(sub.nodes.iter().any(|n| n.bead_id == "B"));
        assert!(!sub.nodes.iter().any(|n| n.bead_id == "C"));
    }

    // ---- Causality Tests ----

    #[test]
    fn causality_empty_history() {
        let issues = vec![make_issue("A", "Task", "open", 1)];
        let graph = IssueGraph::build(&issues);
        let histories = BTreeMap::new();

        let result = build_causality_chain("A", &histories, &graph);
        assert!(result.chain.events.is_empty());
        assert_eq!(result.insights.commit_count, 0);
        assert!(
            result
                .insights
                .recommendations
                .iter()
                .any(|r| r.contains("No history"))
        );
    }

    #[test]
    fn causality_basic_lifecycle() {
        let issues = vec![make_issue("A", "Task", "closed", 1)];
        let graph = IssueGraph::build(&issues);

        let mut histories = BTreeMap::new();
        histories.insert(
            "A".to_string(),
            make_history(
                "A",
                vec![
                    ("2025-01-01T00:00:00Z", "created"),
                    ("2025-01-02T00:00:00Z", "claimed"),
                    ("2025-01-05T00:00:00Z", "closed"),
                ],
                vec![("abc1234", "2025-01-03T00:00:00Z", vec!["src/main.rs"])],
            ),
        );

        let result = build_causality_chain("A", &histories, &graph);
        assert_eq!(result.chain.events.len(), 4); // created, claimed, commit, closed
        assert!(result.chain.is_complete);
        assert_eq!(result.insights.commit_count, 1);
        assert!(result.insights.total_duration_ms > 0);
    }

    #[test]
    fn causality_blocked_periods() {
        let issues = vec![
            make_issue("blocker", "Blocker", "closed", 1),
            make_issue_with_deps("A", "Task", "open", 2, &["blocker"]),
        ];
        let graph = IssueGraph::build(&issues);

        let mut histories = BTreeMap::new();
        histories.insert(
            "blocker".to_string(),
            make_history(
                "blocker",
                vec![
                    ("2025-01-01T00:00:00Z", "created"),
                    ("2025-01-10T00:00:00Z", "closed"),
                ],
                vec![],
            ),
        );
        histories.insert(
            "A".to_string(),
            make_history("A", vec![("2025-01-02T00:00:00Z", "created")], vec![]),
        );

        let result = build_causality_chain("A", &histories, &graph);
        // Should have blocked and unblocked events from the blocker
        let has_blocked = result
            .chain
            .events
            .iter()
            .any(|e| e.event_type == "blocked");
        let has_unblocked = result
            .chain
            .events
            .iter()
            .any(|e| e.event_type == "unblocked");
        assert!(has_blocked);
        assert!(has_unblocked);
        assert!(!result.insights.blocked_periods.is_empty());
    }

    #[test]
    fn causality_causal_links() {
        let issues = vec![make_issue("A", "Task", "open", 1)];
        let graph = IssueGraph::build(&issues);

        let mut histories = BTreeMap::new();
        histories.insert(
            "A".to_string(),
            make_history(
                "A",
                vec![
                    ("2025-01-01T00:00:00Z", "created"),
                    ("2025-01-02T00:00:00Z", "claimed"),
                ],
                vec![],
            ),
        );

        let result = build_causality_chain("A", &histories, &graph);
        assert_eq!(result.chain.events.len(), 2);
        // First event has no caused_by
        assert!(result.chain.events[0].caused_by_id.is_none());
        assert_eq!(result.chain.events[0].enables_ids, vec![1]);
        // Second event caused by first
        assert_eq!(result.chain.events[1].caused_by_id, Some(0));
        assert!(result.chain.events[1].enables_ids.is_empty());
    }

    #[test]
    fn timestamp_parsing() {
        let ms = parse_timestamp_ms("2025-01-01T00:00:00Z").unwrap();
        assert!(ms > 0);

        let ms2 = parse_timestamp_ms("2025-01-02T00:00:00Z").unwrap();
        assert_eq!(ms2 - ms, 86_400_000); // Exactly one day in ms
    }

    #[test]
    fn timestamp_parsing_empty() {
        assert!(parse_timestamp_ms("").is_none());
        assert!(parse_timestamp_ms("invalid").is_none());
    }

    // --- parse_timestamp_ms edge cases ---

    #[test]
    fn timestamp_parsing_with_timezone_offset() {
        let ms = parse_timestamp_ms("2025-01-01T00:00:00+00:00");
        assert!(ms.is_some());
    }

    #[test]
    fn timestamp_parsing_with_fractional_seconds() {
        let ms = parse_timestamp_ms("2025-01-01T12:30:45.123Z");
        assert!(ms.is_some());
    }

    #[test]
    fn timestamp_parsing_no_t_separator() {
        assert!(parse_timestamp_ms("2025-01-01 12:00:00Z").is_none());
    }

    #[test]
    fn timestamp_parsing_missing_seconds() {
        // time_parts.len() >= 2 should still work with just hour:minute
        let ms = parse_timestamp_ms("2025-01-01T12:30Z");
        assert!(ms.is_some());
    }

    // --- days_from_date tests ---

    #[test]
    fn days_from_date_pre_1970_returns_none() {
        assert!(days_from_date(1969, 12, 31).is_none());
    }

    #[test]
    fn days_from_date_invalid_month_zero() {
        assert!(days_from_date(2020, 0, 1).is_none());
    }

    #[test]
    fn days_from_date_invalid_month_13() {
        assert!(days_from_date(2020, 13, 1).is_none());
    }

    #[test]
    fn days_from_date_invalid_day_zero() {
        assert!(days_from_date(2020, 1, 0).is_none());
    }

    #[test]
    fn days_from_date_invalid_day_32() {
        assert!(days_from_date(2020, 1, 32).is_none());
    }

    #[test]
    fn days_from_date_epoch() {
        assert_eq!(days_from_date(1970, 1, 1), Some(0));
    }

    #[test]
    fn days_from_date_one_day() {
        assert_eq!(days_from_date(1970, 1, 2), Some(1));
    }

    #[test]
    fn days_from_date_leap_year_feb() {
        // 2000 is a leap year, Feb has 29 days
        let feb28 = days_from_date(2000, 2, 28).unwrap();
        let mar1 = days_from_date(2000, 3, 1).unwrap();
        assert_eq!(mar1 - feb28, 2); // Feb 28 → Feb 29 → Mar 1
    }

    // --- get_blocker_chain edge cases ---

    #[test]
    fn blocker_chain_unknown_target() {
        let issues = vec![make_issue("A", "A", "open", 1)];
        let graph = IssueGraph::build(&issues);
        let result = get_blocker_chain(&graph, "NONEXISTENT");
        assert!(!result.is_blocked);
        assert_eq!(result.target_title, "");
    }

    #[test]
    fn blocker_chain_multiple_roots_sorted_by_priority() {
        let issues = vec![
            make_issue("R1", "Root high", "open", 5),
            make_issue("R2", "Root low", "open", 1),
            make_issue("R3", "Root mid", "open", 3),
            make_issue_with_deps("T", "Target", "blocked", 2, &["R1", "R2", "R3"]),
        ];
        let graph = IssueGraph::build(&issues);
        let result = get_blocker_chain(&graph, "T");
        assert_eq!(result.root_blockers.len(), 3);
        assert_eq!(result.root_blockers[0].id, "R2"); // priority 1
        assert_eq!(result.root_blockers[1].id, "R3"); // priority 3
        assert_eq!(result.root_blockers[2].id, "R1"); // priority 5
    }

    // --- get_subnetwork tests ---

    #[test]
    fn subnetwork_depth_clamped_to_max_3() {
        let issues = vec![
            make_issue("A", "A", "open", 1),
            make_issue_with_deps("B", "B", "open", 1, &["A"]),
        ];
        let graph = IssueGraph::build(&issues);
        let histories = BTreeMap::new();
        let full = build_impact_network(&graph, &histories);

        // depth=100 should be clamped to 3
        let sub = get_subnetwork(&full, "A", 100);
        assert!(sub.stats.total_nodes <= full.stats.total_nodes);
    }

    #[test]
    fn subnetwork_unknown_bead_returns_single_node() {
        let issues = vec![
            make_issue("A", "A", "open", 1),
            make_issue_with_deps("B", "B", "open", 1, &["A"]),
        ];
        let graph = IssueGraph::build(&issues);
        let histories = BTreeMap::new();
        let full = build_impact_network(&graph, &histories);

        let sub = get_subnetwork(&full, "NONEXISTENT", 2);
        // Unknown bead won't match any nodes
        assert_eq!(sub.stats.total_nodes, 0);
    }

    #[test]
    fn subnetwork_density_zero_for_single_node() {
        let issues = vec![
            make_issue("A", "A", "open", 1),
            make_issue("B", "B", "open", 1),
        ];
        let graph = IssueGraph::build(&issues);
        let histories = BTreeMap::new();
        let full = build_impact_network(&graph, &histories);

        let sub = get_subnetwork(&full, "A", 1);
        assert_eq!(sub.stats.density, 0.0);
    }

    // --- build_impact_network_result tests ---

    #[test]
    fn impact_network_result_all_returns_full_network() {
        let issues = vec![
            make_issue("A", "A", "open", 1),
            make_issue_with_deps("B", "B", "open", 1, &["A"]),
        ];
        let graph = IssueGraph::build(&issues);
        let histories = BTreeMap::new();

        let result = build_impact_network_result(&graph, &histories, "all", 2);
        assert_eq!(result.bead_id, "");
        assert_eq!(result.depth, 0);
        assert_eq!(result.network.stats.total_nodes, 2);
    }

    #[test]
    fn impact_network_result_specific_bead() {
        let issues = vec![
            make_issue("A", "A", "open", 1),
            make_issue_with_deps("B", "B", "open", 1, &["A"]),
            make_issue("C", "C", "open", 1),
            make_issue_with_deps("D", "D", "open", 1, &["C"]),
        ];
        let graph = IssueGraph::build(&issues);
        let histories = BTreeMap::new();

        let result = build_impact_network_result(&graph, &histories, "A", 1);
        assert_eq!(result.bead_id, "A");
        assert!(result.depth >= 1);
    }

    #[test]
    fn impact_network_result_empty_bead_id_returns_full() {
        let issues = vec![make_issue("A", "A", "open", 1)];
        let graph = IssueGraph::build(&issues);
        let histories = BTreeMap::new();

        let result = build_impact_network_result(&graph, &histories, "", 1);
        assert_eq!(result.bead_id, "");
        assert_eq!(result.depth, 0);
    }

    // --- Impact network shared file edges ---

    #[test]
    fn impact_network_shared_file_edges_require_two_files() {
        let issues = vec![
            make_issue("A", "A", "open", 1),
            make_issue("B", "B", "open", 1),
        ];
        let graph = IssueGraph::build(&issues);

        let mut histories = BTreeMap::new();
        // Only 1 shared file — should NOT create a shared_file edge
        histories.insert(
            "A".to_string(),
            make_history("A", vec![], vec![("s1", "2025-01-01T00:00:00Z", vec!["shared.rs"])]),
        );
        histories.insert(
            "B".to_string(),
            make_history("B", vec![], vec![("s2", "2025-01-02T00:00:00Z", vec!["shared.rs"])]),
        );

        let network = build_impact_network(&graph, &histories);
        assert!(!network.edges.iter().any(|e| e.edge_type == "shared_file"));
    }

    #[test]
    fn impact_network_shared_file_edges_with_two_plus_files() {
        let issues = vec![
            make_issue("A", "A", "open", 1),
            make_issue("B", "B", "open", 1),
        ];
        let graph = IssueGraph::build(&issues);

        let mut histories = BTreeMap::new();
        histories.insert(
            "A".to_string(),
            make_history(
                "A",
                vec![],
                vec![("s1", "2025-01-01T00:00:00Z", vec!["f1.rs", "f2.rs"])],
            ),
        );
        histories.insert(
            "B".to_string(),
            make_history(
                "B",
                vec![],
                vec![("s2", "2025-01-02T00:00:00Z", vec!["f1.rs", "f2.rs"])],
            ),
        );

        let network = build_impact_network(&graph, &histories);
        assert!(network.edges.iter().any(|e| e.edge_type == "shared_file"));
    }

    // --- Network stats ---

    #[test]
    fn impact_network_stats_computed() {
        let issues = vec![
            make_issue("A", "A", "open", 1),
            make_issue_with_deps("B", "B", "open", 1, &["A"]),
            make_issue("C", "C", "open", 1),
        ];
        let graph = IssueGraph::build(&issues);
        let histories = BTreeMap::new();

        let network = build_impact_network(&graph, &histories);
        assert_eq!(network.stats.total_nodes, 3);
        assert_eq!(network.stats.total_edges, 1);
        assert_eq!(network.stats.isolated_nodes, 1); // C is isolated
        assert_eq!(network.stats.max_degree, 1);
    }

    // --- Causality insights ---

    #[test]
    fn causality_high_blocked_percentage_recommendation() {
        let issues = vec![
            make_issue("blocker", "Blocker", "closed", 1),
            make_issue_with_deps("A", "Task", "open", 2, &["blocker"]),
        ];
        let graph = IssueGraph::build(&issues);

        let mut histories = BTreeMap::new();
        histories.insert(
            "blocker".to_string(),
            make_history(
                "blocker",
                vec![
                    ("2025-01-01T00:00:00Z", "created"),
                    ("2025-06-01T00:00:00Z", "closed"),
                ],
                vec![],
            ),
        );
        histories.insert(
            "A".to_string(),
            make_history(
                "A",
                vec![
                    ("2025-01-02T00:00:00Z", "created"),
                    ("2025-07-01T00:00:00Z", "claimed"),
                ],
                vec![],
            ),
        );

        let result = build_causality_chain("A", &histories, &graph);
        // Blocked period should be substantial
        if result.insights.blocked_percentage > 25.0 {
            assert!(
                result
                    .insights
                    .recommendations
                    .iter()
                    .any(|r| r.contains("blocked percentage"))
            );
        }
    }

    #[test]
    fn causality_duration_next_ms_computed() {
        let issues = vec![make_issue("A", "Task", "open", 1)];
        let graph = IssueGraph::build(&issues);

        let mut histories = BTreeMap::new();
        histories.insert(
            "A".to_string(),
            make_history(
                "A",
                vec![
                    ("2025-01-01T00:00:00Z", "created"),
                    ("2025-01-02T00:00:00Z", "claimed"),
                ],
                vec![],
            ),
        );

        let result = build_causality_chain("A", &histories, &graph);
        assert_eq!(result.chain.events.len(), 2);
        assert_eq!(result.chain.events[0].duration_next_ms, Some(86_400_000));
    }

    #[test]
    fn causality_not_complete_when_open() {
        let issues = vec![make_issue("A", "Task", "open", 1)];
        let graph = IssueGraph::build(&issues);
        let mut histories = BTreeMap::new();
        histories.insert(
            "A".to_string(),
            make_history("A", vec![("2025-01-01T00:00:00Z", "created")], vec![]),
        );

        let result = build_causality_chain("A", &histories, &graph);
        assert!(!result.chain.is_complete);
    }

    #[test]
    fn causality_complete_when_closed() {
        let issues = vec![make_issue("A", "Task", "closed", 1)];
        let graph = IssueGraph::build(&issues);
        let mut histories = BTreeMap::new();
        histories.insert(
            "A".to_string(),
            make_history(
                "A",
                vec![
                    ("2025-01-01T00:00:00Z", "created"),
                    ("2025-01-05T00:00:00Z", "closed"),
                ],
                vec![],
            ),
        );

        let result = build_causality_chain("A", &histories, &graph);
        assert!(result.chain.is_complete);
    }

    #[test]
    fn causality_edge_count_is_events_minus_one() {
        let issues = vec![make_issue("A", "Task", "open", 1)];
        let graph = IssueGraph::build(&issues);
        let mut histories = BTreeMap::new();
        histories.insert(
            "A".to_string(),
            make_history(
                "A",
                vec![
                    ("2025-01-01T00:00:00Z", "created"),
                    ("2025-01-02T00:00:00Z", "claimed"),
                    ("2025-01-03T00:00:00Z", "status_change"),
                ],
                vec![],
            ),
        );

        let result = build_causality_chain("A", &histories, &graph);
        assert_eq!(result.chain.edge_count, result.chain.events.len() - 1);
    }

    #[test]
    fn causality_longest_gap_desc_days() {
        let issues = vec![make_issue("A", "Task", "open", 1)];
        let graph = IssueGraph::build(&issues);
        let mut histories = BTreeMap::new();
        histories.insert(
            "A".to_string(),
            make_history(
                "A",
                vec![
                    ("2025-01-01T00:00:00Z", "created"),
                    ("2025-02-01T00:00:00Z", "claimed"),
                ],
                vec![],
            ),
        );

        let result = build_causality_chain("A", &histories, &graph);
        assert!(result.insights.longest_gap_desc.contains("d gap"));
    }

    // --- Cluster detection ---

    #[test]
    fn cluster_requires_at_least_two_nodes() {
        let issues = vec![
            make_issue("A", "A", "open", 1),
            make_issue("B", "B", "open", 1),
        ];
        let graph = IssueGraph::build(&issues);
        let histories = BTreeMap::new();

        let network = build_impact_network(&graph, &histories);
        // No edges, so no clusters
        assert!(network.clusters.is_empty());
    }

    #[test]
    fn cluster_formed_from_connected_edges() {
        let issues = vec![
            make_issue("A", "A", "open", 1),
            make_issue_with_deps("B", "B", "open", 1, &["A"]),
            make_issue_with_deps("C", "C", "open", 1, &["A"]),
        ];
        let graph = IssueGraph::build(&issues);
        let histories = BTreeMap::new();

        let network = build_impact_network(&graph, &histories);
        assert!(!network.clusters.is_empty());
        let cluster = &network.clusters[0];
        assert!(cluster.bead_ids.len() >= 2);
    }
}
