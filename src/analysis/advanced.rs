//! Advanced graph insights: `TopKSet`, `CoverageSet`, `KPaths`, `CycleBreak`, `ParallelCut`, `ParallelGain`.
//!
//! Implements six advanced analysis algorithms that build on the core graph metrics.

use std::collections::{HashMap, HashSet, VecDeque};

use serde::Serialize;

use crate::analysis::graph::{GraphMetrics, IssueGraph};
use crate::model::Issue;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Aggregates all six advanced insight types.
#[derive(Debug, Clone, Serialize)]
pub struct AdvancedInsights {
    pub top_k_set: TopKSetResult,
    pub coverage_set: CoverageSetResult,
    pub k_paths: KPathsResult,
    pub cycle_break: CycleBreakResult,
    pub parallel_cut: ParallelCutResult,
    pub parallel_gain: ParallelGainResult,
    pub config: AdvancedInsightsConfig,
    pub usage_hints: Vec<String>,
}

/// Configuration used to produce the advanced insights.
#[derive(Debug, Clone, Serialize)]
pub struct AdvancedInsightsConfig {
    pub top_k: usize,
    pub k_paths_k: usize,
    pub max_cycle_break: usize,
    pub max_parallel_cut: usize,
}

impl Default for AdvancedInsightsConfig {
    fn default() -> Self {
        Self {
            top_k: 10,
            k_paths_k: 5,
            max_cycle_break: 10,
            max_parallel_cut: 10,
        }
    }
}

// ---------------------------------------------------------------------------
// 1. TopKSet – greedy submodular set of issues that maximize downstream unlocks
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct TopKSetResult {
    pub items: Vec<TopKItem>,
    pub total_unlocked: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct TopKItem {
    pub id: String,
    pub marginal_unlocks: usize,
    pub cumulative_unlocks: usize,
}

/// Greedy selection of up to `k` issues whose completion maximally unlocks downstream work.
///
/// At each step we pick the open issue whose completion would unblock the most new downstream
/// issues (that are not already unblocked by previously selected items).
///
/// This is exposed `pub` so callers that only need the unlock ranking can
/// skip the full `compute_advanced_insights` which also computes coverage,
/// k-paths, cycle-break, parallel-cut, and parallel-gain.
#[must_use]
pub fn compute_top_k_set(graph: &IssueGraph, _metrics: &GraphMetrics, k: usize) -> TopKSetResult {
    let open_ids: Vec<String> = graph
        .issue_ids_sorted()
        .into_iter()
        .filter(|id| graph.issue(id).is_some_and(Issue::is_open_like))
        .collect();

    // Pre-compute transitive downstream set for each open issue (BFS through dependents).
    let downstream_map: HashMap<String, HashSet<String>> = open_ids
        .iter()
        .map(|id| {
            let downstream = bfs_downstream(graph, id);
            (id.clone(), downstream)
        })
        .collect();

    let mut selected = Vec::new();
    let mut already_unlocked = HashSet::<String>::new();
    let mut remaining: HashSet<String> = open_ids.into_iter().collect();

    for _ in 0..k {
        if remaining.is_empty() {
            break;
        }

        // Find the issue whose downstream set gains the most new unlocks.
        let best = remaining
            .iter()
            .map(|id| {
                let downstream = downstream_map.get(id).map_or(0, |set| {
                    set.iter()
                        .filter(|d| !already_unlocked.contains(*d) && *d != id)
                        .count()
                });
                (id.clone(), downstream)
            })
            .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)));

        let Some((best_id, marginal)) = best else {
            break;
        };

        if marginal == 0 && !selected.is_empty() {
            break;
        }

        // Add newly unlocked downstream issues, but do not count the selected
        // issue itself as an "unlocked" downstream item.
        if let Some(ds) = downstream_map.get(&best_id) {
            for d in ds {
                already_unlocked.insert(d.clone());
            }
        }
        remaining.remove(&best_id);

        selected.push(TopKItem {
            id: best_id,
            marginal_unlocks: marginal,
            cumulative_unlocks: already_unlocked.len(),
        });
    }

    TopKSetResult {
        total_unlocked: already_unlocked.len(),
        items: selected,
    }
}

/// BFS downstream through dependents (issues that depend on `start_id`).
fn bfs_downstream(graph: &IssueGraph, start_id: &str) -> HashSet<String> {
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    queue.push_back(start_id.to_string());

    while let Some(current) = queue.pop_front() {
        for dep in graph.dependents(&current) {
            if visited.insert(dep.clone()) {
                queue.push_back(dep);
            }
        }
    }
    visited
}

// ---------------------------------------------------------------------------
// 2. CoverageSet – greedy vertex cover of the critical path DAG
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct CoverageSetResult {
    pub items: Vec<String>,
    pub paths_covered: usize,
    pub total_paths: usize,
}

/// Greedy minimum vertex cover: select issues that appear on the most critical paths.
///
/// We approximate by collecting all edges on the critical sub-DAG (issues with non-zero
/// critical depth) and greedily covering them.
fn compute_coverage_set(graph: &IssueGraph, metrics: &GraphMetrics) -> CoverageSetResult {
    // Collect edges where both endpoints have critical_depth > 0.
    let mut edges: Vec<(String, String)> = Vec::new();
    let critical_ids: HashSet<&String> = metrics
        .critical_depth
        .iter()
        .filter(|(_, d)| **d > 0)
        .map(|(id, _)| id)
        .collect();

    for id in &critical_ids {
        for dep in graph.dependents(id) {
            if critical_ids.contains(&dep) {
                edges.push(((*id).clone(), dep));
            }
        }
    }

    let total_paths = edges.len();
    let mut uncovered = edges;
    let mut selected = Vec::new();

    while !uncovered.is_empty() {
        // Count how many uncovered edges each node touches.
        let mut freq: HashMap<String, usize> = HashMap::new();
        for (a, b) in &uncovered {
            *freq.entry(a.clone()).or_default() += 1;
            *freq.entry(b.clone()).or_default() += 1;
        }

        // Pick the node with highest frequency (ties broken by ID).
        let best = freq
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)));

        let Some((best_id, _)) = best else {
            break;
        };

        // Remove all edges touching best_id.
        uncovered.retain(|(a, b)| a != &best_id && b != &best_id);
        selected.push(best_id);
    }

    selected.sort();

    CoverageSetResult {
        paths_covered: total_paths,
        total_paths,
        items: selected,
    }
}

// ---------------------------------------------------------------------------
// 3. KPaths – K shortest critical paths (simplified Yen's algorithm)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct KPathsResult {
    pub paths: Vec<CriticalPath>,
    pub k: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CriticalPath {
    pub path: Vec<String>,
    pub length: usize,
    pub depth_sum: usize,
}

/// Find up to `k` longest (most critical) paths through the dependency graph.
///
/// Uses iterative DFS from source nodes (those with no open blockers) to sink nodes
/// (those with no open dependents), collecting the longest paths first.
fn compute_k_paths(graph: &IssueGraph, metrics: &GraphMetrics, k: usize) -> KPathsResult {
    let open_ids: HashSet<String> = graph
        .issue_ids_sorted()
        .into_iter()
        .filter(|id| graph.issue(id).is_some_and(Issue::is_open_like))
        .collect();

    if open_ids.is_empty() {
        return KPathsResult {
            paths: Vec::new(),
            k,
        };
    }

    // Source nodes: open issues with no open blockers.
    let sources: Vec<String> = open_ids
        .iter()
        .filter(|id| graph.open_blockers(id).is_empty())
        .cloned()
        .collect();

    // DFS from each source to find all maximal paths.
    let mut all_paths: Vec<Vec<String>> = Vec::new();
    let path_cap = (10 * k).min(10_000);

    'outer: for source in &sources {
        let mut stack: Vec<(String, Vec<String>)> = vec![(source.clone(), vec![source.clone()])];
        let mut visited_from_source = HashSet::new();

        while let Some((current, path)) = stack.pop() {
            let deps: Vec<String> = graph
                .dependents(&current)
                .into_iter()
                .filter(|d| open_ids.contains(d) && !path.contains(d))
                .collect();

            if deps.is_empty() {
                // Leaf path — only keep non-trivial paths.
                if path.len() > 1 {
                    all_paths.push(path);
                    if all_paths.len() >= path_cap {
                        break 'outer;
                    }
                }
            } else {
                for dep in deps {
                    if visited_from_source.insert((current.clone(), dep.clone())) {
                        let mut new_path = path.clone();
                        new_path.push(dep.clone());
                        stack.push((dep, new_path));
                    }
                }
            }
        }
    }

    // Score by length then depth_sum, take top k.
    let mut scored: Vec<CriticalPath> = all_paths
        .into_iter()
        .map(|path| {
            let depth_sum: usize = path
                .iter()
                .filter_map(|id| metrics.critical_depth.get(id))
                .sum();
            let length = path.len();
            CriticalPath {
                path,
                length,
                depth_sum,
            }
        })
        .collect();

    scored.sort_by(|a, b| {
        b.length
            .cmp(&a.length)
            .then_with(|| b.depth_sum.cmp(&a.depth_sum))
    });
    scored.truncate(k);

    KPathsResult { paths: scored, k }
}

// ---------------------------------------------------------------------------
// 4. CycleBreak – greedy feedback arc removal with residual cycle checks
// ---------------------------------------------------------------------------

/// A stable topology for edge-removal experiments. Adjacency stores edge IDs so
/// simulations toggle a bit instead of rebuilding issue records or string maps.
struct ResidualGraph {
    ids: Vec<String>,
    edges: Vec<(usize, usize)>,
    outgoing: Vec<Vec<usize>>,
    incoming: Vec<Vec<usize>>,
    removed: Vec<bool>,
}

struct CyclicComponents {
    membership: Vec<usize>,
    cyclic: Vec<bool>,
    count: usize,
}

struct DagDepths {
    prefix: Vec<usize>,
    suffix: Vec<usize>,
    max_depth: usize,
}

#[derive(Default)]
struct DepthScratch {
    pending: Vec<usize>,
    prefix: Vec<usize>,
    order: Vec<usize>,
}

impl ResidualGraph {
    fn from_issue_graph(graph: &IssueGraph, open_only: bool) -> Self {
        let mut ids = graph.issue_ids_sorted();
        ids.dedup();
        if open_only {
            ids.retain(|id| graph.issue(id).is_some_and(Issue::is_open_like));
        }
        let indices = ids
            .iter()
            .enumerate()
            .map(|(index, id)| (id.as_str(), index))
            .collect::<HashMap<_, _>>();
        let mut edges = Vec::new();
        for (dependent, id) in ids.iter().enumerate() {
            for blocker in graph.blockers(id) {
                if let Some(&prerequisite) = indices.get(blocker.as_str()) {
                    edges.push((prerequisite, dependent));
                }
            }
        }
        edges.sort_unstable();
        edges.dedup();
        let mut outgoing = vec![Vec::new(); ids.len()];
        let mut incoming = vec![Vec::new(); ids.len()];
        for (edge, &(from, to)) in edges.iter().enumerate() {
            outgoing[from].push(edge);
            incoming[to].push(edge);
        }
        let removed = vec![false; edges.len()];
        Self {
            ids,
            edges,
            outgoing,
            incoming,
            removed,
        }
    }

    /// Iterative Kosaraju traversal: cyclic components include singleton loops.
    fn cyclic_components(&self) -> CyclicComponents {
        let mut visited = vec![false; self.ids.len()];
        let mut finish_order = Vec::with_capacity(self.ids.len());
        let mut frames = Vec::new();
        for root in 0..self.ids.len() {
            if visited[root] {
                continue;
            }
            visited[root] = true;
            frames.push((root, 0));
            while let Some((node, cursor)) = frames.last_mut() {
                let node = *node;
                if *cursor == self.outgoing[node].len() {
                    finish_order.push(node);
                    frames.pop();
                    continue;
                }
                let edge = self.outgoing[node][*cursor];
                *cursor += 1;
                let next = self.edges[edge].1;
                if !self.removed[edge] && !visited[next] {
                    visited[next] = true;
                    frames.push((next, 0));
                }
            }
        }

        let mut membership = vec![usize::MAX; self.ids.len()];
        let mut cyclic = Vec::new();
        let mut stack = Vec::new();
        for &root in finish_order.iter().rev() {
            if membership[root] != usize::MAX {
                continue;
            }
            let component = cyclic.len();
            let mut size = 0;
            membership[root] = component;
            stack.push(root);
            while let Some(node) = stack.pop() {
                size += 1;
                for &edge in &self.incoming[node] {
                    let next = self.edges[edge].0;
                    if !self.removed[edge] && membership[next] == usize::MAX {
                        membership[next] = component;
                        stack.push(next);
                    }
                }
            }
            cyclic.push(
                size > 1
                    || self.outgoing[root]
                        .iter()
                        .any(|&edge| !self.removed[edge] && self.edges[edge].1 == root),
            );
        }
        let count = cyclic.iter().filter(|&&is_cyclic| is_cyclic).count();
        CyclicComponents {
            membership,
            cyclic,
            count,
        }
    }

    /// Longest paths count nodes, so an isolated issue has depth one. A cyclic
    /// graph has no topological depth; never report a partially traversed DAG.
    fn dag_depths(&self) -> Option<DagDepths> {
        let mut scratch = DepthScratch::default();
        let max_depth = self.dag_max_depth(&mut scratch)?;
        let mut suffix = vec![1usize; self.ids.len()];
        for &node in scratch.order.iter().rev() {
            for &edge in &self.outgoing[node] {
                if !self.removed[edge] {
                    suffix[node] = suffix[node].max(suffix[self.edges[edge].1] + 1);
                }
            }
        }
        Some(DagDepths {
            prefix: scratch.prefix,
            suffix,
            max_depth,
        })
    }

    /// Scalar simulation reuses its buffers across candidates. The order vector
    /// also serves as Kahn's queue, avoiding a second allocation per experiment.
    fn dag_max_depth(&self, scratch: &mut DepthScratch) -> Option<usize> {
        scratch.pending.clear();
        scratch.pending.extend(
            self.incoming
                .iter()
                .map(|edges| edges.iter().filter(|&&edge| !self.removed[edge]).count()),
        );
        scratch.prefix.resize(self.ids.len(), 1);
        scratch.prefix.fill(1);
        scratch.order.clear();
        scratch.order.extend(
            scratch
                .pending
                .iter()
                .enumerate()
                .filter_map(|(node, &count)| (count == 0).then_some(node)),
        );
        let mut cursor = 0;
        while cursor < scratch.order.len() {
            let node = scratch.order[cursor];
            cursor += 1;
            for &edge in &self.outgoing[node] {
                if self.removed[edge] {
                    continue;
                }
                let next = self.edges[edge].1;
                scratch.prefix[next] = scratch.prefix[next].max(scratch.prefix[node] + 1);
                scratch.pending[next] -= 1;
                if scratch.pending[next] == 0 {
                    scratch.order.push(next);
                }
            }
        }
        (scratch.order.len() == self.ids.len())
            .then(|| scratch.prefix.iter().copied().max().unwrap_or(0))
    }

    fn critical_edges(&self, depths: &DagDepths) -> Vec<usize> {
        self.edges
            .iter()
            .enumerate()
            .filter_map(|(edge, &(from, to))| {
                (!self.removed[edge] && depths.prefix[from] + depths.suffix[to] == depths.max_depth)
                    .then_some(edge)
            })
            .collect()
    }

    /// Every longest path crosses each prefix layer once. If another critical
    /// edge exists at the same layer, its longest path avoids this edge, proving
    /// that this single deletion cannot shorten the maximum chain. Only edges
    /// alone at their layer require a depth simulation.
    fn critical_edge_candidates(&self, depths: &DagDepths) -> Vec<(usize, bool)> {
        let critical = self.critical_edges(depths);
        let mut layer_counts = vec![0usize; depths.max_depth];
        for &edge in &critical {
            layer_counts[depths.prefix[self.edges[edge].0]] += 1;
        }
        critical
            .into_iter()
            .map(|edge| {
                let needs_simulation = layer_counts[depths.prefix[self.edges[edge].0]] == 1;
                (edge, needs_simulation)
            })
            .collect()
    }

    fn component_count(&self) -> usize {
        let mut visited = vec![false; self.ids.len()];
        let mut stack = Vec::new();
        let mut count = 0;
        for root in 0..self.ids.len() {
            if visited[root] {
                continue;
            }
            count += 1;
            visited[root] = true;
            stack.push(root);
            while let Some(node) = stack.pop() {
                for &edge in self.outgoing[node].iter().chain(&self.incoming[node]) {
                    if self.removed[edge] {
                        continue;
                    }
                    let (from, to) = self.edges[edge];
                    let next = if from == node { to } else { from };
                    if !visited[next] {
                        visited[next] = true;
                        stack.push(next);
                    }
                }
            }
        }
        count
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CycleBreakResult {
    pub suggestions: Vec<CycleBreakSuggestion>,
    /// Number of cyclic strongly connected components, including self-loops.
    pub cycles_before: usize,
    /// Actual cyclic component count after applying all suggestions together.
    pub estimated_cycles_after: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CycleBreakSuggestion {
    pub from: String,
    pub to: String,
    /// Nonnegative decrease in cyclic SCC count at this step. A cut can leave
    /// subcycles or split one SCC into several cyclic SCCs, yielding zero here.
    pub cycles_broken: usize,
    pub collateral_score: f64,
}

/// Suggest cumulative edge removals that break dependency cycles.
///
/// Each step removes the lowest-PageRank-collateral edge inside a remaining
/// cyclic SCC, then recomputes SCCs. This greedy plan does not guarantee a
/// minimum feedback arc set. Its counters describe SCCs, not simple cycles.
fn compute_cycle_break(
    graph: &IssueGraph,
    metrics: &GraphMetrics,
    max_suggestions: usize,
) -> CycleBreakResult {
    let mut residual = ResidualGraph::from_issue_graph(graph, false);
    let mut components = residual.cyclic_components();
    let cycles_before = components.count;
    let collateral = residual
        .ids
        .iter()
        .map(|id| metrics.pagerank.get(id).copied().unwrap_or_default())
        .collect::<Vec<_>>();
    let mut suggestions = Vec::new();

    for _ in 0..max_suggestions {
        if components.count == 0 {
            break;
        }
        let best = residual
            .edges
            .iter()
            .enumerate()
            .filter(|&(edge, &(from, to))| {
                !residual.removed[edge]
                    && components.cyclic[components.membership[from]]
                    && components.membership[from] == components.membership[to]
            })
            .map(|(edge, &(from, to))| (edge, collateral[from] + collateral[to]))
            .min_by(|left, right| {
                left.1
                    .total_cmp(&right.1)
                    .then_with(|| left.0.cmp(&right.0))
            });
        let Some((edge, collateral_score)) = best else {
            break;
        };
        residual.removed[edge] = true;
        let next_components = residual.cyclic_components();
        let (from, to) = residual.edges[edge];
        suggestions.push(CycleBreakSuggestion {
            from: residual.ids[from].clone(),
            to: residual.ids[to].clone(),
            cycles_broken: components.count.saturating_sub(next_components.count),
            collateral_score,
        });
        components = next_components;
    }

    CycleBreakResult {
        suggestions,
        cycles_before,
        estimated_cycles_after: components.count,
    }
}

// ---------------------------------------------------------------------------
// 5. ParallelCut – cumulative greedy longest-path reductions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ParallelCutResult {
    /// Cumulative plan in application order; later cuts include earlier ones.
    pub cuts: Vec<ParallelCutEdge>,
    /// Longest open-issue chain in nodes, or zero when the open graph is cyclic.
    pub current_serial_depth: usize,
    /// Actual depth after applying the cumulative plan.
    pub estimated_depth_after: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ParallelCutEdge {
    pub from: String,
    pub to: String,
    /// Reduction from this step alone. Zero can prepare a later reduction when
    /// several equally long paths must be cut.
    pub depth_reduction: usize,
}

/// Greedily cut current longest paths using measured residual chain lengths.
///
/// Choose the best immediate depth, breaking ties by edge IDs. A zero-reduction
/// step may be needed to cut tied longest paths. This is a cumulative heuristic,
/// not a guarantee of the globally best set for the requested budget.
fn compute_parallel_cut(
    graph: &IssueGraph,
    _metrics: &GraphMetrics,
    max_cuts: usize,
) -> ParallelCutResult {
    let mut residual = ResidualGraph::from_issue_graph(graph, true);
    let Some(mut depths) = residual.dag_depths() else {
        return ParallelCutResult {
            cuts: Vec::new(),
            current_serial_depth: 0,
            estimated_depth_after: 0,
        };
    };
    let current_serial_depth = depths.max_depth;
    let mut cuts = Vec::new();
    let mut scratch = DepthScratch::default();
    for _ in 0..max_cuts {
        let mut best = None;
        for (edge, needs_simulation) in residual.critical_edge_candidates(&depths) {
            let candidate_depth = if needs_simulation {
                residual.removed[edge] = true;
                // Deleting an edge from a DAG cannot introduce a cycle.
                let depth = residual
                    .dag_max_depth(&mut scratch)
                    .expect("edge deletion preserves DAG");
                residual.removed[edge] = false;
                depth
            } else {
                depths.max_depth
            };
            if best.is_none_or(|previous| (candidate_depth, edge) < previous) {
                best = Some((candidate_depth, edge));
            }
        }
        let Some((next_depth, edge)) = best else {
            break;
        };
        residual.removed[edge] = true;
        let (from, to) = residual.edges[edge];
        cuts.push(ParallelCutEdge {
            from: residual.ids[from].clone(),
            to: residual.ids[to].clone(),
            depth_reduction: depths.max_depth - next_depth,
        });
        depths = residual.dag_depths().expect("edge deletion preserves DAG");
    }

    ParallelCutResult {
        cuts,
        current_serial_depth,
        estimated_depth_after: depths.max_depth,
    }
}

// ---------------------------------------------------------------------------
// 6. ParallelGain – estimate throughput gain from removing dependencies
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ParallelGainResult {
    pub current_components: usize,
    /// Longest open-issue chain in nodes, or zero when the open graph is cyclic.
    pub current_max_chain: usize,
    /// Independent single-edge experiments, each starting from the baseline.
    pub gains: Vec<ParallelGainItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ParallelGainItem {
    pub edge_from: String,
    pub edge_to: String,
    pub new_components: usize,
    pub new_max_chain: usize,
    pub parallelism_delta: f64,
}

/// Estimate how much parallelism each edge removal would provide.
///
/// For each critical-path edge, we simulate removal and measure:
/// - Change in number of connected components (more = more parallel tracks).
/// - Change in longest chain length (shorter = faster completion with parallel agents).
fn compute_parallel_gain(
    graph: &IssueGraph,
    _metrics: &GraphMetrics,
    max_items: usize,
) -> ParallelGainResult {
    let mut residual = ResidualGraph::from_issue_graph(graph, true);
    let current_components = residual.component_count();
    let Some(depths) = residual.dag_depths() else {
        return ParallelGainResult {
            current_components,
            current_max_chain: 0,
            gains: Vec::new(),
        };
    };
    let current_max = depths.max_depth;
    let mut ranked = Vec::new();
    let mut scratch = DepthScratch::default();
    if max_items > 0 {
        for (edge, needs_simulation) in residual.critical_edge_candidates(&depths) {
            let new_depth = if needs_simulation {
                residual.removed[edge] = true;
                let depth = residual
                    .dag_max_depth(&mut scratch)
                    .expect("edge deletion preserves DAG");
                residual.removed[edge] = false;
                depth
            } else {
                current_max
            };
            ranked.push((new_depth, edge));
        }
    }
    // A smaller residual depth is a larger gain. Edge indices follow complete
    // (from, to) lexical order, so ties are independent of hash iteration.
    ranked.sort_unstable();
    ranked.truncate(max_items);
    let gains = ranked
        .into_iter()
        .map(|(new_max_chain, edge)| {
            residual.removed[edge] = true;
            let new_components = residual.component_count();
            residual.removed[edge] = false;
            let (from, to) = residual.edges[edge];
            ParallelGainItem {
                edge_from: residual.ids[from].clone(),
                edge_to: residual.ids[to].clone(),
                new_components,
                new_max_chain,
                parallelism_delta: (current_max - new_max_chain) as f64 / current_max as f64,
            }
        })
        .collect();
    ParallelGainResult {
        current_components,
        current_max_chain: current_max,
        gains,
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Compute all advanced insights with default configuration.
#[must_use]
pub fn compute_advanced_insights(graph: &IssueGraph, metrics: &GraphMetrics) -> AdvancedInsights {
    compute_advanced_insights_with_config(graph, metrics, &AdvancedInsightsConfig::default())
}

/// Compute all advanced insights with custom configuration.
#[must_use]
pub fn compute_advanced_insights_with_config(
    graph: &IssueGraph,
    metrics: &GraphMetrics,
    config: &AdvancedInsightsConfig,
) -> AdvancedInsights {
    let top_k_set = compute_top_k_set(graph, metrics, config.top_k);
    let coverage_set = compute_coverage_set(graph, metrics);
    let k_paths = compute_k_paths(graph, metrics, config.k_paths_k);
    let cycle_break = compute_cycle_break(graph, metrics, config.max_cycle_break);
    let parallel_cut = compute_parallel_cut(graph, metrics, config.max_parallel_cut);
    let parallel_gain = compute_parallel_gain(graph, metrics, config.max_parallel_cut);

    AdvancedInsights {
        top_k_set,
        coverage_set,
        k_paths,
        cycle_break,
        parallel_cut,
        parallel_gain,
        config: config.clone(),
        usage_hints: vec![
            "jq '.top_k_set.items[:5]' — Top 5 issues to unlock the most downstream work"
                .to_string(),
            "jq '.coverage_set.items' — Minimal set covering all critical paths".to_string(),
            "jq '.k_paths.paths[0].path' — Longest critical path through the graph".to_string(),
            "jq '.cycle_break' — Apply suggestions in order; estimated_cycles_after counts remaining cyclic components"
                .to_string(),
            "jq '.parallel_cut.cuts' — Apply in order; depth_reduction measures that step after prior cuts".to_string(),
            "jq '.parallel_gain.gains[:3]' — Independent removals; zero gain means the longest chain is unchanged"
                .to_string(),
        ],
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Dependency, Issue};

    fn make_issue(id: &str, deps: &[&str]) -> Issue {
        Issue {
            id: id.to_string(),
            title: format!("Issue {id}"),
            status: "open".to_string(),
            priority: 2,
            dependencies: deps
                .iter()
                .map(|d| Dependency {
                    issue_id: id.to_string(),
                    depends_on_id: d.to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    fn make_closed_issue(id: &str) -> Issue {
        Issue {
            id: id.to_string(),
            title: format!("Issue {id}"),
            status: "closed".to_string(),
            priority: 2,
            ..Default::default()
        }
    }

    // -- Empty graph --

    #[test]
    fn empty_graph_returns_empty_results() {
        let graph = IssueGraph::build(&[]);
        let metrics = graph.compute_metrics();
        let result = compute_advanced_insights(&graph, &metrics);

        assert!(result.top_k_set.items.is_empty());
        assert_eq!(result.top_k_set.total_unlocked, 0);
        assert!(result.coverage_set.items.is_empty());
        assert!(result.k_paths.paths.is_empty());
        assert!(result.cycle_break.suggestions.is_empty());
        assert!(result.parallel_cut.cuts.is_empty());
        assert!(result.parallel_gain.gains.is_empty());
    }

    // -- Single node --

    #[test]
    fn single_node_graph() {
        let issues = vec![make_issue("A", &[])];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let result = compute_advanced_insights(&graph, &metrics);

        // Single node: no downstream to unlock, no paths, no cycles.
        assert!(result.top_k_set.items.len() <= 1);
        assert!(result.k_paths.paths.is_empty());
        assert!(result.cycle_break.suggestions.is_empty());
        assert!(result.parallel_cut.cuts.is_empty());
    }

    // -- Linear chain: A -> B -> C -> D --

    #[test]
    fn linear_chain_top_k_identifies_root_blocker() {
        let issues = vec![
            make_issue("A", &[]),
            make_issue("B", &["A"]),
            make_issue("C", &["B"]),
            make_issue("D", &["C"]),
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let result = compute_top_k_set(&graph, &metrics, 3);

        // A blocks everything downstream, so it should be picked first.
        assert!(!result.items.is_empty());
        assert_eq!(result.items[0].id, "A");
        assert!(result.items[0].marginal_unlocks >= 3);
    }

    #[test]
    fn linear_chain_coverage_set() {
        let issues = vec![
            make_issue("A", &[]),
            make_issue("B", &["A"]),
            make_issue("C", &["B"]),
            make_issue("D", &["C"]),
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let result = compute_coverage_set(&graph, &metrics);

        // Should cover all critical-path edges with a small vertex set.
        assert!(!result.items.is_empty());
        assert!(result.items.len() <= 3); // Greedy cover of 3 edges needs at most 2-3 vertices.
    }

    #[test]
    fn linear_chain_k_paths_returns_full_chain() {
        let issues = vec![
            make_issue("A", &[]),
            make_issue("B", &["A"]),
            make_issue("C", &["B"]),
            make_issue("D", &["C"]),
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let result = compute_k_paths(&graph, &metrics, 5);

        // Should find the full A -> B -> C -> D path.
        assert!(!result.paths.is_empty());
        let longest = &result.paths[0];
        assert_eq!(longest.length, 4);
        assert_eq!(longest.path, vec!["A", "B", "C", "D"]);
    }

    #[test]
    fn linear_chain_parallel_cut() {
        let issues = vec![
            make_issue("A", &[]),
            make_issue("B", &["A"]),
            make_issue("C", &["B"]),
            make_issue("D", &["C"]),
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let result = compute_parallel_cut(&graph, &metrics, 5);

        // Should identify critical-path edges.
        assert!(!result.cuts.is_empty());
        // Depth: D=1, C=2, B=3, A=4 (root blocker has highest depth).
        assert_eq!(result.current_serial_depth, 4);
    }

    // -- Diamond: A -> B, A -> C, B -> D, C -> D --

    #[test]
    fn diamond_graph_top_k() {
        let issues = vec![
            make_issue("A", &[]),
            make_issue("B", &["A"]),
            make_issue("C", &["A"]),
            make_issue("D", &["B", "C"]),
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let result = compute_top_k_set(&graph, &metrics, 5);

        // A unlocks B, C, and transitively D.
        assert!(!result.items.is_empty());
        assert_eq!(result.items[0].id, "A");
    }

    #[test]
    fn diamond_parallel_gain_identifies_parallelizable_edges() {
        let issues = vec![
            make_issue("A", &[]),
            make_issue("B", &["A"]),
            make_issue("C", &["A"]),
            make_issue("D", &["B", "C"]),
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let result = compute_parallel_gain(&graph, &metrics, 5);

        // There should be critical-path edges to potentially cut.
        assert!(result.current_max_chain >= 2);
    }

    // -- Cycle: A -> B -> C -> A --

    #[test]
    fn cycle_break_identifies_edges_to_remove() {
        let issues = vec![
            make_issue("A", &["C"]),
            make_issue("B", &["A"]),
            make_issue("C", &["B"]),
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();

        assert!(!metrics.cycles.is_empty(), "expected cycles in graph");

        let result = compute_cycle_break(&graph, &metrics, 5);
        assert!(!result.suggestions.is_empty());
        assert!(result.cycles_before > 0);
        assert!(result.estimated_cycles_after < result.cycles_before);
    }

    // -- Closed issues are excluded --

    #[test]
    fn closed_issues_excluded_from_top_k() {
        let issues = vec![
            make_issue("A", &[]),
            make_closed_issue("B"),
            make_issue("C", &["A"]),
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let result = compute_top_k_set(&graph, &metrics, 5);

        // B is closed, only A and C are open.
        for item in &result.items {
            assert_ne!(item.id, "B");
        }
    }

    // -- No cycles --

    #[test]
    fn no_cycles_returns_empty_cycle_break() {
        let issues = vec![make_issue("A", &[]), make_issue("B", &["A"])];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let result = compute_cycle_break(&graph, &metrics, 5);

        assert!(result.suggestions.is_empty());
        assert_eq!(result.cycles_before, 0);
        assert_eq!(result.estimated_cycles_after, 0);
    }

    // -- Larger graph (12 nodes) --

    #[test]
    fn larger_graph_all_algorithms_produce_results() {
        // Build a graph with multiple paths and a cycle.
        //
        // Epic1: E1 -> T1, T2, T3
        // Epic2: E2 -> T4, T5
        // T3 -> T6 -> T7
        // T5 -> T7 (cross-epic dep)
        // T7 -> T8 -> T9
        // Cycle: T10 -> T11 -> T12 -> T10
        let issues = vec![
            make_issue("E1", &[]),
            make_issue("E2", &[]),
            make_issue("T1", &["E1"]),
            make_issue("T2", &["E1"]),
            make_issue("T3", &["E1"]),
            make_issue("T4", &["E2"]),
            make_issue("T5", &["E2"]),
            make_issue("T6", &["T3"]),
            make_issue("T7", &["T6", "T5"]),
            make_issue("T8", &["T7"]),
            make_issue("T9", &["T8"]),
            make_issue("T10", &["T12"]),
            make_issue("T11", &["T10"]),
            make_issue("T12", &["T11"]),
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let result = compute_advanced_insights(&graph, &metrics);

        // TopKSet should identify E1 and E2 as high-impact.
        assert!(!result.top_k_set.items.is_empty());
        assert!(result.top_k_set.total_unlocked >= 5);

        // KPaths should find paths of length >= 4.
        assert!(!result.k_paths.paths.is_empty());
        assert!(result.k_paths.paths[0].length >= 4);

        // CycleBreak should identify cycle T10-T11-T12.
        assert!(result.cycle_break.cycles_before > 0);
        assert!(!result.cycle_break.suggestions.is_empty());

        // ParallelCut and ParallelGain require an acyclic open graph. The
        // T10-T11-T12 cycle makes depth unavailable; a partial traversal of the
        // other components must not produce a misleading parallelism estimate.
        assert_eq!(
            result.parallel_cut.current_serial_depth,
            result.parallel_gain.current_max_chain
        );
    }

    // -- Config customization --

    #[test]
    fn custom_config_limits_results() {
        let issues = vec![
            make_issue("A", &[]),
            make_issue("B", &["A"]),
            make_issue("C", &["B"]),
            make_issue("D", &["C"]),
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();

        let config = AdvancedInsightsConfig {
            top_k: 1,
            k_paths_k: 1,
            max_cycle_break: 1,
            max_parallel_cut: 1,
        };
        let result = compute_advanced_insights_with_config(&graph, &metrics, &config);

        assert!(result.top_k_set.items.len() <= 1);
        assert!(result.k_paths.paths.len() <= 1);
    }

    // -- bfs_downstream ---

    #[test]
    fn bfs_downstream_no_dependents() {
        let issues = vec![make_issue("A", &[])];
        let graph = IssueGraph::build(&issues);
        let downstream = bfs_downstream(&graph, "A");
        assert!(downstream.is_empty());
    }

    #[test]
    fn bfs_downstream_transitive() {
        let issues = vec![
            make_issue("A", &[]),
            make_issue("B", &["A"]),
            make_issue("C", &["B"]),
        ];
        let graph = IssueGraph::build(&issues);
        let downstream = bfs_downstream(&graph, "A");
        assert!(downstream.contains("B"));
        assert!(downstream.contains("C"));
    }

    // -- TopKSet edge cases --

    #[test]
    fn top_k_set_all_closed_returns_empty() {
        let issues = vec![make_closed_issue("A"), make_closed_issue("B")];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let result = compute_top_k_set(&graph, &metrics, 5);
        assert!(result.items.is_empty());
        assert_eq!(result.total_unlocked, 0);
    }

    #[test]
    fn top_k_set_independent_issues_stops_after_first() {
        // Issues with no deps and no dependents: marginal=0 after first pick
        let issues = vec![
            make_issue("A", &[]),
            make_issue("B", &[]),
            make_issue("C", &[]),
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let result = compute_top_k_set(&graph, &metrics, 10);
        // First pick has marginal=0 but is selected; second marginal=0 should stop
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.total_unlocked, 0);
        assert_eq!(result.items[0].cumulative_unlocks, 0);
    }

    // -- CoverageSet edge cases --

    #[test]
    fn coverage_set_no_critical_paths() {
        let issues = vec![make_issue("A", &[]), make_issue("B", &[])];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let result = compute_coverage_set(&graph, &metrics);
        // Independent nodes have critical_depth 0, so no critical edges
        assert!(result.items.is_empty());
        assert_eq!(result.total_paths, 0);
    }

    // -- KPaths edge cases --

    #[test]
    fn k_paths_all_closed_returns_empty() {
        let issues = vec![make_closed_issue("A"), make_closed_issue("B")];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let result = compute_k_paths(&graph, &metrics, 5);
        assert!(result.paths.is_empty());
    }

    #[test]
    fn k_paths_single_edge_returns_path_of_length_2() {
        let issues = vec![make_issue("A", &[]), make_issue("B", &["A"])];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let result = compute_k_paths(&graph, &metrics, 5);
        assert!(!result.paths.is_empty());
        assert_eq!(result.paths[0].length, 2);
    }

    // -- ParallelCut edge cases --

    #[test]
    fn parallel_cut_no_deps_returns_empty_cuts() {
        let issues = vec![make_issue("A", &[]), make_issue("B", &[])];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let result = compute_parallel_cut(&graph, &metrics, 5);
        // No dependency edges means no critical-path edges to cut
        assert!(result.cuts.is_empty());
    }

    // -- ParallelGain edge cases --

    #[test]
    fn parallel_gain_no_open_returns_empty() {
        let issues = vec![make_closed_issue("A"), make_closed_issue("B")];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let result = compute_parallel_gain(&graph, &metrics, 5);
        assert!(result.gains.is_empty());
    }

    #[test]
    fn parallel_gain_two_independent_chains() {
        let issues = vec![
            make_issue("A", &[]),
            make_issue("B", &["A"]),
            make_issue("C", &[]),
            make_issue("D", &["C"]),
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let result = compute_parallel_gain(&graph, &metrics, 5);
        assert!(result.current_components >= 2);
    }

    // -- Config default values --

    #[test]
    fn default_config_values() {
        let config = AdvancedInsightsConfig::default();
        assert_eq!(config.top_k, 10);
        assert_eq!(config.k_paths_k, 5);
        assert_eq!(config.max_cycle_break, 10);
        assert_eq!(config.max_parallel_cut, 10);
    }

    // -- Usage hints --

    #[test]
    fn usage_hints_populated() {
        let graph = IssueGraph::build(&[]);
        let metrics = graph.compute_metrics();
        let result = compute_advanced_insights(&graph, &metrics);
        assert!(!result.usage_hints.is_empty());
        assert!(result.usage_hints.iter().any(|h| h.contains("jq")));
    }

    // -- Independent edge removal --

    #[test]
    fn parallel_gain_edge_removal_splits_components() {
        // A -> B has node depth 2. Removing the edge leaves two isolated
        // nodes, each of depth 1; counting remaining edges would report 0.
        let issues = vec![make_issue("A", &[]), make_issue("B", &["A"])];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let result = compute_parallel_gain(&graph, &metrics, 1);
        assert_eq!(result.current_max_chain, 2);
        assert_eq!(result.gains.len(), 1);
        assert_eq!(result.gains[0].new_components, 2);
        assert_eq!(result.gains[0].new_max_chain, 1);
        assert_eq!(result.gains[0].parallelism_delta, 0.5);
    }

    // -- Large acyclic graph: parallel_cut and parallel_gain require DAG --

    #[test]
    fn acyclic_graph_parallel_cut_and_gain() {
        // Same structure as larger_graph minus the cycle nodes.
        // E1 -> T1, T2, T3; E2 -> T4, T5; T3 -> T6 -> T7; T5 -> T7; T7 -> T8 -> T9
        let issues = vec![
            make_issue("E1", &[]),
            make_issue("E2", &[]),
            make_issue("T1", &["E1"]),
            make_issue("T2", &["E1"]),
            make_issue("T3", &["E1"]),
            make_issue("T4", &["E2"]),
            make_issue("T5", &["E2"]),
            make_issue("T6", &["T3"]),
            make_issue("T7", &["T6", "T5"]),
            make_issue("T8", &["T7"]),
            make_issue("T9", &["T8"]),
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let result = compute_advanced_insights(&graph, &metrics);

        // DAG with no cycles: critical_depth is computed properly.
        assert!(!result.parallel_cut.cuts.is_empty());
        // Longest chain: E1 -> T3 -> T6 -> T7 -> T8 -> T9 = depth 6.
        assert!(result.parallel_cut.current_serial_depth >= 5);
        assert!(!result.parallel_gain.gains.is_empty());
        assert!(result.parallel_gain.current_max_chain >= 5);
    }

    fn matrix_issues(edges: &[Vec<bool>]) -> Vec<Issue> {
        (0..edges.len())
            .map(|to| {
                let blockers = (0..edges.len())
                    .filter(|&from| edges[from][to])
                    .map(|from| format!("N{from}"))
                    .collect::<Vec<_>>();
                let refs = blockers.iter().map(String::as_str).collect::<Vec<_>>();
                make_issue(&format!("N{to}"), &refs)
            })
            .collect()
    }

    // These small-graph oracles intentionally use matrix closure and exhaustive
    // path enumeration, independently of the production SCC/topological code.
    fn oracle_reachable(edges: &[Vec<bool>]) -> Vec<Vec<bool>> {
        let mut reachable = edges.to_vec();
        for via in 0..edges.len() {
            for from in 0..edges.len() {
                if reachable[from][via] {
                    for to in 0..edges.len() {
                        let via_reaches_to = reachable[via][to];
                        reachable[from][to] |= via_reaches_to;
                    }
                }
            }
        }
        reachable
    }

    fn oracle_cyclic_components(edges: &[Vec<bool>]) -> usize {
        let reachable = oracle_reachable(edges);
        let mut seen = vec![false; edges.len()];
        let mut count = 0;
        for node in 0..edges.len() {
            if seen[node] {
                continue;
            }
            let members = (0..edges.len())
                .filter(|&other| {
                    other == node || (reachable[node][other] && reachable[other][node])
                })
                .collect::<Vec<_>>();
            if members.len() > 1 || edges[node][node] {
                count += 1;
            }
            for member in members {
                seen[member] = true;
            }
        }
        count
    }

    fn oracle_components(edges: &[Vec<bool>]) -> usize {
        let mut seen = vec![false; edges.len()];
        let mut count = 0;
        for root in 0..edges.len() {
            if seen[root] {
                continue;
            }
            count += 1;
            let mut stack = vec![root];
            seen[root] = true;
            while let Some(node) = stack.pop() {
                for other in 0..edges.len() {
                    if !seen[other] && (edges[node][other] || edges[other][node]) {
                        seen[other] = true;
                        stack.push(other);
                    }
                }
            }
        }
        count
    }

    fn oracle_longest_paths(edges: &[Vec<bool>]) -> Vec<Vec<usize>> {
        fn visit(
            node: usize,
            edges: &[Vec<bool>],
            path: &mut Vec<usize>,
            longest: &mut Vec<Vec<usize>>,
        ) {
            path.push(node);
            let mut leaf = true;
            for next in 0..edges.len() {
                if edges[node][next] {
                    leaf = false;
                    visit(next, edges, path, longest);
                }
            }
            if leaf {
                let best = longest.first().map_or(0, Vec::len);
                if path.len() > best {
                    longest.clear();
                }
                if path.len() >= best {
                    longest.push(path.clone());
                }
            }
            path.pop();
        }
        let mut longest = Vec::new();
        for root in 0..edges.len() {
            visit(root, edges, &mut Vec::new(), &mut longest);
        }
        longest
    }

    fn oracle_depth(edges: &[Vec<bool>]) -> usize {
        oracle_longest_paths(edges).first().map_or(0, Vec::len)
    }

    fn oracle_critical_edges(edges: &[Vec<bool>]) -> std::collections::BTreeSet<(usize, usize)> {
        oracle_longest_paths(edges)
            .iter()
            .flat_map(|path| path.windows(2).map(|pair| (pair[0], pair[1])))
            .collect()
    }

    fn node_number(id: &str) -> usize {
        id.strip_prefix('N')
            .expect("fixture ID")
            .parse()
            .expect("node number")
    }

    #[test]
    fn cycle_break_matches_replayed_scc_oracle_on_every_three_node_graph() {
        let metrics = IssueGraph::build(&[]).compute_metrics();
        for mask in 0..512 {
            let mut edges = vec![vec![false; 3]; 3];
            for from in 0..3 {
                for to in 0..3 {
                    edges[from][to] = mask & (1 << (from * 3 + to)) != 0;
                }
            }
            let graph = IssueGraph::build(&matrix_issues(&edges));
            for budget in [0, 1, 2, 9] {
                let result = compute_cycle_break(&graph, &metrics, budget);
                let mut residual = edges.clone();
                let mut before = oracle_cyclic_components(&residual);
                assert_eq!(result.cycles_before, before, "mask={mask}, budget={budget}");
                assert!(result.suggestions.len() <= budget);
                for suggestion in &result.suggestions {
                    let from = node_number(&suggestion.from);
                    let to = node_number(&suggestion.to);
                    assert!(
                        residual[from][to],
                        "suggestion must remove a remaining edge"
                    );
                    assert!(
                        oracle_reachable(&residual)[to][from],
                        "edge must lie in a cycle"
                    );
                    residual[from][to] = false;
                    let after = oracle_cyclic_components(&residual);
                    assert_eq!(suggestion.cycles_broken, before.saturating_sub(after));
                    before = after;
                }
                assert_eq!(result.estimated_cycles_after, before);
                if budget == 9 {
                    assert_eq!(before, 0, "full edge budget must break every cycle: {mask}");
                }
            }
        }
    }

    #[test]
    fn cycle_break_reports_scc_splitting_without_claiming_resolution() {
        // A<->Y and B<->Z, joined by A->B and Z->Y, initially form one SCC.
        // The first lexical cut A->B leaves two cyclic SCCs, not zero cycles.
        let issues = vec![
            make_issue("A", &["Y"]),
            make_issue("B", &["A", "Z"]),
            make_issue("Y", &["A", "Z"]),
            make_issue("Z", &["B"]),
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = IssueGraph::build(&[]).compute_metrics();
        let first = compute_cycle_break(&graph, &metrics, 1);
        assert_eq!(first.cycles_before, 1);
        assert_eq!(first.estimated_cycles_after, 2);
        assert_eq!(first.suggestions[0].from, "A");
        assert_eq!(first.suggestions[0].to, "B");
        assert_eq!(first.suggestions[0].cycles_broken, 0);
        let complete = compute_cycle_break(&graph, &metrics, 6);
        assert_eq!(complete.estimated_cycles_after, 0);
        assert_eq!(complete.suggestions.len(), 3);
    }

    #[test]
    fn parallel_results_match_exhaustive_paths_on_every_five_node_ordered_dag() {
        let metrics = IssueGraph::build(&[]).compute_metrics();
        let possible = (0..5)
            .flat_map(|from| ((from + 1)..5).map(move |to| (from, to)))
            .collect::<Vec<_>>();
        for mask in 0..(1 << possible.len()) {
            let mut edges = vec![vec![false; 5]; 5];
            for (bit, &(from, to)) in possible.iter().enumerate() {
                edges[from][to] = mask & (1 << bit) != 0;
            }
            let graph = IssueGraph::build(&matrix_issues(&edges));
            let depth = oracle_depth(&edges);
            let mut expected = oracle_critical_edges(&edges)
                .into_iter()
                .map(|(from, to)| {
                    let mut residual = edges.clone();
                    residual[from][to] = false;
                    (
                        oracle_depth(&residual),
                        from,
                        to,
                        oracle_components(&residual),
                    )
                })
                .collect::<Vec<_>>();
            expected.sort();
            let gains = compute_parallel_gain(&graph, &metrics, 10);
            assert_eq!(gains.current_max_chain, depth);
            assert_eq!(gains.current_components, oracle_components(&edges));
            assert_eq!(gains.gains.len(), expected.len());
            for (actual, &(after, from, to, components)) in gains.gains.iter().zip(&expected) {
                assert_eq!(
                    (node_number(&actual.edge_from), node_number(&actual.edge_to)),
                    (from, to)
                );
                assert_eq!(actual.new_max_chain, after, "mask={mask}");
                assert_eq!(actual.new_components, components);
                let expected_delta = f64::from(u32::try_from(depth - after).expect("small depth"))
                    / f64::from(u32::try_from(depth).expect("small depth"));
                assert!((actual.parallelism_delta - expected_delta).abs() < 1e-12);
            }

            for budget in [0, 1, 3, 10] {
                let cuts = compute_parallel_cut(&graph, &metrics, budget);
                assert_eq!(cuts.current_serial_depth, depth);
                let mut residual = edges.clone();
                let mut before = depth;
                for cut in &cuts.cuts {
                    let best = oracle_critical_edges(&residual)
                        .into_iter()
                        .map(|(from, to)| {
                            let mut trial = residual.clone();
                            trial[from][to] = false;
                            (oracle_depth(&trial), from, to)
                        })
                        .min()
                        .expect("remaining critical edge");
                    let from = node_number(&cut.from);
                    let to = node_number(&cut.to);
                    assert_eq!((from, to), (best.1, best.2), "greedy cut: mask={mask}");
                    residual[from][to] = false;
                    assert_eq!(cut.depth_reduction, before - best.0);
                    before = best.0;
                }
                assert!(cuts.cuts.len() <= budget);
                assert_eq!(cuts.estimated_depth_after, oracle_depth(&residual));
                if cuts.cuts.len() < budget {
                    assert!(oracle_critical_edges(&residual).is_empty());
                }
            }
        }
    }

    #[test]
    fn parallel_depth_uses_open_graph_even_when_closed_nodes_contain_cycles() {
        let mut closed_c = make_issue("C", &["D"]);
        closed_c.status = "closed".to_string();
        let mut closed_d = make_issue("D", &["C"]);
        closed_d.status = "tombstone".to_string();
        let issues = vec![
            make_issue("A", &[]),
            make_issue("B", &["A"]),
            closed_c,
            closed_d,
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        assert!(metrics.critical_depth.values().all(|depth| *depth == 0));
        let cuts = compute_parallel_cut(&graph, &metrics, 10);
        assert_eq!(cuts.current_serial_depth, 2);
        assert_eq!(cuts.estimated_depth_after, 1);
        assert_eq!(cuts.cuts.len(), 1);
        let gains = compute_parallel_gain(&graph, &metrics, 10);
        assert_eq!(gains.current_max_chain, 2);
        assert_eq!(gains.current_components, 1);
        assert_eq!(gains.gains[0].new_max_chain, 1);
        // CycleBreak intentionally diagnoses the full graph, like `Cycles`.
        assert_eq!(compute_cycle_break(&graph, &metrics, 10).cycles_before, 1);
    }

    #[test]
    fn cyclic_open_graph_does_not_fabricate_parallel_depth_or_gains() {
        let graph = IssueGraph::build(&[
            make_issue("A", &["B"]),
            make_issue("B", &["A"]),
            make_issue("C", &[]),
            make_issue("D", &["C"]),
        ]);
        let mut metrics = IssueGraph::build(&[]).compute_metrics();
        metrics.critical_depth.insert("A".to_string(), 100);
        let cuts = compute_parallel_cut(&graph, &metrics, 10);
        let gains = compute_parallel_gain(&graph, &metrics, 10);
        assert_eq!(cuts.current_serial_depth, 0);
        assert_eq!(cuts.estimated_depth_after, 0);
        assert!(cuts.cuts.is_empty());
        assert_eq!(gains.current_max_chain, 0);
        assert_eq!(gains.current_components, 2);
        assert!(gains.gains.is_empty());
    }

    #[test]
    fn zero_parallel_budgets_preserve_baseline_and_return_no_suggestions() {
        let graph = IssueGraph::build(&[make_issue("A", &[]), make_issue("B", &["A"])]);
        let metrics = IssueGraph::build(&[]).compute_metrics();
        let cuts = compute_parallel_cut(&graph, &metrics, 0);
        let gains = compute_parallel_gain(&graph, &metrics, 0);
        assert_eq!(cuts.current_serial_depth, 2);
        assert_eq!(cuts.estimated_depth_after, 2);
        assert!(cuts.cuts.is_empty());
        assert_eq!(gains.current_max_chain, 2);
        assert_eq!(gains.current_components, 1);
        assert!(gains.gains.is_empty());
    }

    #[test]
    fn residual_cycle_and_depth_analysis_are_stack_safe_on_twenty_five_thousand_nodes() {
        let count = 25_000;
        let issues = (0..count)
            .map(|node| {
                make_issue(
                    &format!("N{node:05}"),
                    &[&format!("N{:05}", (node + count - 1) % count)],
                )
            })
            .collect::<Vec<_>>();
        let graph = IssueGraph::build(&issues);
        let metrics = IssueGraph::build(&[]).compute_metrics();
        let result = compute_cycle_break(&graph, &metrics, 1);
        assert_eq!(result.cycles_before, 1);
        assert_eq!(result.estimated_cycles_after, 0);
        assert_eq!(result.suggestions.len(), 1);
        let mut residual = ResidualGraph::from_issue_graph(&graph, true);
        let edge = residual
            .edges
            .iter()
            .position(|&(from, to)| {
                residual.ids[from] == result.suggestions[0].from
                    && residual.ids[to] == result.suggestions[0].to
            })
            .expect("suggested edge in ring");
        residual.removed[edge] = true;
        assert_eq!(
            residual
                .dag_depths()
                .expect("broken ring is a DAG")
                .max_depth,
            count
        );
        assert_eq!(residual.component_count(), 1);
    }
}
