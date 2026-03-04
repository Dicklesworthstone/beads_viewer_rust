use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};

use petgraph::algo::kosaraju_scc;
use petgraph::graph::DiGraph;
use petgraph::visit::EdgeRef;

use crate::model::Issue;

#[derive(Debug, Clone)]
pub struct IssueGraph {
    graph: DiGraph<(), ()>,
    node_to_id: Vec<String>,
    issues: HashMap<String, Issue>,
    blockers_by_issue: HashMap<String, Vec<String>>,
    dependents_by_issue: HashMap<String, Vec<String>>,
    successors: Vec<Vec<usize>>,   // issue -> blockers
    predecessors: Vec<Vec<usize>>, // issue <- dependents
    edge_count: usize,
}

#[derive(Debug, Clone)]
pub struct GraphMetrics {
    pub pagerank: HashMap<String, f64>,
    pub betweenness: HashMap<String, f64>,
    pub eigenvector: HashMap<String, f64>,
    pub hubs: HashMap<String, f64>,
    pub authorities: HashMap<String, f64>,
    pub blocks_count: HashMap<String, usize>,
    pub blocked_by_count: HashMap<String, usize>,
    pub critical_depth: HashMap<String, usize>,
    pub k_core: HashMap<String, u32>,
    pub articulation_points: HashSet<String>,
    pub slack: HashMap<String, f64>,
    pub cycles: Vec<Vec<String>>,
}

impl IssueGraph {
    #[must_use]
    pub fn build(issues: &[Issue]) -> Self {
        let mut graph = DiGraph::<(), ()>::new();
        let mut node_indices = Vec::with_capacity(issues.len());
        let mut node_to_id = Vec::with_capacity(issues.len());
        let mut id_to_index = HashMap::with_capacity(issues.len());
        let mut issue_map = HashMap::with_capacity(issues.len());

        for (index, issue) in issues.iter().enumerate() {
            let node = graph.add_node(());
            node_indices.push(node);
            node_to_id.push(issue.id.clone());
            id_to_index.insert(issue.id.clone(), index);
            issue_map.insert(issue.id.clone(), issue.clone());
        }

        let mut blockers_by_issue: HashMap<String, Vec<String>> = HashMap::new();
        let mut dependents_by_issue: HashMap<String, Vec<String>> = HashMap::new();
        let mut successors = vec![Vec::<usize>::new(); issues.len()];
        let mut predecessors = vec![Vec::<usize>::new(); issues.len()];
        let mut seen = HashSet::<(usize, usize)>::new();
        let mut edge_count = 0usize;

        for issue in issues {
            let Some(&source_index) = id_to_index.get(&issue.id) else {
                continue;
            };

            for dep in &issue.dependencies {
                if !dep.is_blocking() || dep.depends_on_id.trim().is_empty() {
                    continue;
                }

                let Some(&target_index) = id_to_index.get(&dep.depends_on_id) else {
                    continue;
                };

                if !seen.insert((source_index, target_index)) {
                    continue;
                }

                graph.add_edge(node_indices[source_index], node_indices[target_index], ());
                successors[source_index].push(target_index);
                predecessors[target_index].push(source_index);

                blockers_by_issue
                    .entry(issue.id.clone())
                    .or_default()
                    .push(dep.depends_on_id.clone());
                dependents_by_issue
                    .entry(dep.depends_on_id.clone())
                    .or_default()
                    .push(issue.id.clone());

                edge_count = edge_count.saturating_add(1);
            }
        }

        for neighbors in &mut successors {
            neighbors.sort_unstable();
        }
        for neighbors in &mut predecessors {
            neighbors.sort_unstable();
        }
        for blockers in blockers_by_issue.values_mut() {
            blockers.sort();
            blockers.dedup();
        }
        for dependents in dependents_by_issue.values_mut() {
            dependents.sort();
            dependents.dedup();
        }

        Self {
            graph,
            node_to_id,
            issues: issue_map,
            blockers_by_issue,
            dependents_by_issue,
            successors,
            predecessors,
            edge_count,
        }
    }

    #[must_use]
    pub fn issue(&self, id: &str) -> Option<&Issue> {
        self.issues.get(id)
    }

    #[must_use]
    pub fn issue_ids_sorted(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.issues.keys().cloned().collect();
        ids.sort();
        ids
    }

    #[must_use]
    pub fn node_count(&self) -> usize {
        self.node_to_id.len()
    }

    #[must_use]
    pub const fn edge_count(&self) -> usize {
        self.edge_count
    }

    #[must_use]
    pub fn blockers(&self, issue_id: &str) -> Vec<String> {
        self.blockers_by_issue
            .get(issue_id)
            .cloned()
            .unwrap_or_default()
    }

    #[must_use]
    pub fn dependents(&self, issue_id: &str) -> Vec<String> {
        self.dependents_by_issue
            .get(issue_id)
            .cloned()
            .unwrap_or_default()
    }

    #[must_use]
    pub fn open_blockers(&self, issue_id: &str) -> Vec<String> {
        self.blockers(issue_id)
            .into_iter()
            .filter(|blocker_id| self.issues.get(blocker_id).is_some_and(Issue::is_open_like))
            .collect()
    }

    #[must_use]
    pub fn actionable_ids(&self) -> Vec<String> {
        // Phase 1: Compute directly blocked issues (open blocking dependencies).
        let mut directly_blocked = HashSet::<String>::new();
        for (id, issue) in &self.issues {
            if issue.is_closed_like() {
                continue;
            }
            if !self.open_blockers(id).is_empty() {
                directly_blocked.insert(id.clone());
            }
        }

        // Phase 2: Build parent->children index from parent-child dependencies.
        // A child has a dep with dep_type="parent-child" and depends_on_id pointing
        // to the parent. We invert: parent -> [children].
        let mut children_of: HashMap<String, Vec<String>> = HashMap::new();
        for issue in self.issues.values() {
            for dep in &issue.dependencies {
                if dep.is_parent_child() && !dep.depends_on_id.trim().is_empty() {
                    if self.issues.contains_key(&dep.depends_on_id) {
                        children_of
                            .entry(dep.depends_on_id.clone())
                            .or_default()
                            .push(issue.id.clone());
                    }
                }
            }
        }

        // Phase 3: Propagate blocked status through parent-child relationships.
        // If a parent is blocked, its children are also blocked (transitively).
        let mut blocked = directly_blocked.clone();
        let max_depth = 50;
        for _ in 0..max_depth {
            let mut newly_blocked = Vec::<String>::new();
            for parent_id in &blocked {
                if let Some(children) = children_of.get(parent_id) {
                    for child_id in children {
                        if !blocked.contains(child_id) {
                            if self
                                .issues
                                .get(child_id)
                                .is_some_and(|issue| !issue.is_closed_like())
                            {
                                newly_blocked.push(child_id.clone());
                            }
                        }
                    }
                }
            }
            if newly_blocked.is_empty() {
                break;
            }
            for id in newly_blocked {
                blocked.insert(id);
            }
        }

        // Phase 4: Collect actionable issues (open, not blocked).
        let mut ids = self.issue_ids_sorted();
        ids.retain(|id| {
            self.issues
                .get(id)
                .is_some_and(|issue| issue.is_open_like())
                && !blocked.contains(id)
        });
        ids
    }

    #[must_use]
    pub fn connected_open_components(&self) -> Vec<Vec<String>> {
        let open_ids: HashSet<String> = self
            .issues
            .values()
            .filter(|issue| issue.is_open_like())
            .map(|issue| issue.id.clone())
            .collect();

        let mut seen = HashSet::<String>::new();
        let mut components = Vec::<Vec<String>>::new();

        for start_id in &open_ids {
            if seen.contains(start_id) {
                continue;
            }

            let mut stack = vec![start_id.clone()];
            let mut component = Vec::<String>::new();
            seen.insert(start_id.clone());

            while let Some(id) = stack.pop() {
                component.push(id.clone());

                let neighbors = self
                    .blockers(&id)
                    .into_iter()
                    .chain(self.dependents(&id).into_iter());

                for neighbor in neighbors {
                    if !open_ids.contains(&neighbor) {
                        continue;
                    }
                    if seen.insert(neighbor.clone()) {
                        stack.push(neighbor);
                    }
                }
            }

            component.sort();
            components.push(component);
        }

        components.sort_by(|a, b| a.first().cmp(&b.first()));
        components
    }

    #[must_use]
    pub fn compute_metrics(&self) -> GraphMetrics {
        let pagerank = self.compute_pagerank();
        let betweenness = self.compute_betweenness();
        let eigenvector = self.compute_eigenvector();
        let (hubs, authorities) = self.compute_hits();

        let mut blocks_count = HashMap::new();
        let mut blocked_by_count = HashMap::new();
        for id in self.issue_ids_sorted() {
            blocks_count.insert(id.clone(), self.dependents(&id).len());
            blocked_by_count.insert(id.clone(), self.blockers(&id).len());
        }

        let critical_depth = self.compute_critical_depth();
        let k_core = self.compute_k_core();
        let articulation_points = self.compute_articulation_points();
        let slack = self.compute_slack();
        let cycles = self.find_cycles();

        GraphMetrics {
            pagerank,
            betweenness,
            eigenvector,
            hubs,
            authorities,
            blocks_count,
            blocked_by_count,
            critical_depth,
            k_core,
            articulation_points,
            slack,
            cycles,
        }
    }

    fn compute_pagerank(&self) -> HashMap<String, f64> {
        let node_count = self.node_to_id.len();
        if node_count == 0 {
            return HashMap::new();
        }

        let damping = 0.85_f64;
        let base = (1.0_f64 - damping) / node_count as f64;
        let mut ranks = vec![1.0_f64 / node_count as f64; node_count];

        for _ in 0..100 {
            let mut next = vec![base; node_count];

            let dangling_sum = (0..node_count)
                .filter(|&node| self.successors[node].is_empty())
                .map(|node| ranks[node])
                .sum::<f64>();
            let dangling_contrib = damping * dangling_sum / node_count as f64;
            for value in &mut next {
                *value += dangling_contrib;
            }

            for (node, rank) in ranks.iter().enumerate().take(node_count) {
                let out_degree = self.successors[node].len();
                if out_degree == 0 {
                    continue;
                }

                let share = *rank / out_degree as f64;
                for &target in &self.successors[node] {
                    next[target] += damping * share;
                }
            }

            let delta = ranks
                .iter()
                .zip(next.iter())
                .map(|(a, b)| (a - b).abs())
                .sum::<f64>();

            ranks = next;
            if delta < 1e-9 {
                break;
            }
        }

        self.map_from_f64_scores(&ranks)
    }

    fn compute_betweenness(&self) -> HashMap<String, f64> {
        let n = self.node_to_id.len();
        if n == 0 {
            return HashMap::new();
        }

        if n > 512 {
            let sample_size = (n / 5).clamp(128, 512);
            return self.compute_betweenness_sampled(sample_size);
        }

        let mut bc = vec![0.0_f64; n];
        for source in 0..n {
            self.single_source_betweenness(source, &mut bc);
        }

        self.map_from_f64_scores(&bc)
    }

    fn compute_betweenness_sampled(&self, sample_size: usize) -> HashMap<String, f64> {
        let n = self.node_to_id.len();
        if n == 0 {
            return HashMap::new();
        }

        let pivot_count = sample_size.min(n);
        let mut pivots = Vec::<usize>::with_capacity(pivot_count);
        let mut used = HashSet::<usize>::with_capacity(pivot_count);
        let step = (n / pivot_count.max(1)).max(1);

        for i in 0..pivot_count {
            let mut candidate = (i * step) % n;
            while !used.insert(candidate) {
                candidate = (candidate + 1) % n;
            }
            pivots.push(candidate);
        }

        let mut bc = vec![0.0_f64; n];
        for pivot in pivots {
            self.single_source_betweenness(pivot, &mut bc);
        }

        let scale = n as f64 / pivot_count.max(1) as f64;
        for value in &mut bc {
            *value *= scale;
        }

        self.map_from_f64_scores(&bc)
    }

    fn single_source_betweenness(&self, source: usize, bc: &mut [f64]) {
        let n = self.node_to_id.len();

        let mut stack = Vec::<usize>::with_capacity(n);
        let mut pred = vec![Vec::<usize>::new(); n];
        let mut sigma = vec![0.0_f64; n];
        let mut dist = vec![-1_i32; n];
        let mut delta = vec![0.0_f64; n];

        sigma[source] = 1.0;
        dist[source] = 0;

        let mut queue = VecDeque::<usize>::new();
        queue.push_back(source);

        while let Some(v) = queue.pop_front() {
            stack.push(v);

            for &w in &self.successors[v] {
                if dist[w] < 0 {
                    dist[w] = dist[v] + 1;
                    queue.push_back(w);
                }

                if dist[w] == dist[v] + 1 {
                    sigma[w] += sigma[v];
                    pred[w].push(v);
                }
            }
        }

        while let Some(w) = stack.pop() {
            for &v in &pred[w] {
                if sigma[w] > 0.0 {
                    delta[v] += (sigma[v] / sigma[w]) * (1.0 + delta[w]);
                }
            }

            if w != source {
                bc[w] += delta[w];
            }
        }
    }

    fn compute_eigenvector(&self) -> HashMap<String, f64> {
        let n = self.node_to_id.len();
        if n == 0 {
            return HashMap::new();
        }

        let init = 1.0 / (n as f64).sqrt();
        let mut current = vec![init; n];
        let mut next = vec![0.0_f64; n];

        for _ in 0..80 {
            next.fill(0.0);

            for (node, target) in next.iter_mut().enumerate() {
                for &pred in &self.predecessors[node] {
                    *target += current[pred];
                }
            }

            let norm = next.iter().map(|value| value * value).sum::<f64>().sqrt();
            if norm < 1e-12 {
                break;
            }
            for value in &mut next {
                *value /= norm;
            }

            let delta = current
                .iter()
                .zip(next.iter())
                .map(|(a, b)| (a - b).abs())
                .sum::<f64>();

            current.clone_from_slice(&next);
            if delta < 1e-7 {
                break;
            }
        }

        self.map_from_f64_scores(&current)
    }

    fn compute_hits(&self) -> (HashMap<String, f64>, HashMap<String, f64>) {
        let n = self.node_to_id.len();
        if n == 0 {
            return (HashMap::new(), HashMap::new());
        }

        let mut hubs = vec![1.0 / n as f64; n];
        let mut authorities = vec![1.0 / n as f64; n];

        for _ in 0..100 {
            let mut next_auth = vec![0.0_f64; n];
            let mut next_hubs = vec![0.0_f64; n];

            for (node, target) in next_auth.iter_mut().enumerate() {
                for &pred in &self.predecessors[node] {
                    *target += hubs[pred];
                }
            }

            for (node, target) in next_hubs.iter_mut().enumerate() {
                for &succ in &self.successors[node] {
                    *target += next_auth[succ];
                }
            }

            normalize_l2(&mut next_auth);
            normalize_l2(&mut next_hubs);

            let auth_delta = authorities
                .iter()
                .zip(next_auth.iter())
                .map(|(a, b)| (a - b).abs())
                .sum::<f64>();
            let hubs_delta = hubs
                .iter()
                .zip(next_hubs.iter())
                .map(|(a, b)| (a - b).abs())
                .sum::<f64>();

            authorities = next_auth;
            hubs = next_hubs;

            if auth_delta + hubs_delta < 1e-7 {
                break;
            }
        }

        (
            self.map_from_f64_scores(&hubs),
            self.map_from_f64_scores(&authorities),
        )
    }

    fn compute_critical_depth(&self) -> HashMap<String, usize> {
        let n = self.node_to_id.len();
        if n == 0 {
            return HashMap::new();
        }

        let mut heights = vec![0usize; n];
        if let Some(order) = self.topological_order() {
            for node in order {
                let max_pred = self.predecessors[node]
                    .iter()
                    .map(|&pred| heights[pred])
                    .max()
                    .unwrap_or(0);
                heights[node] = max_pred.saturating_add(1);
            }
        }

        self.map_from_usize_scores(&heights)
    }

    fn compute_slack(&self) -> HashMap<String, f64> {
        let n = self.node_to_id.len();
        if n == 0 {
            return HashMap::new();
        }

        let Some(order) = self.topological_order() else {
            return self.map_from_f64_scores(&vec![0.0; n]);
        };

        let mut dist_from_start = vec![0usize; n];
        for &node in &order {
            let max_pred = self.predecessors[node]
                .iter()
                .map(|&pred| dist_from_start[pred])
                .max()
                .unwrap_or(0);
            dist_from_start[node] = max_pred.saturating_add(1);
        }

        let mut dist_to_end = vec![0usize; n];
        for &node in order.iter().rev() {
            let max_succ = self.successors[node]
                .iter()
                .map(|&succ| dist_to_end[succ])
                .max()
                .unwrap_or(0);
            dist_to_end[node] = max_succ.saturating_add(1);
        }

        let longest_path = (0..n)
            .map(|index| dist_from_start[index] + dist_to_end[index] - 1)
            .max()
            .unwrap_or(0);

        let slack = (0..n)
            .map(|index| {
                let path_through_node = dist_from_start[index] + dist_to_end[index] - 1;
                longest_path.saturating_sub(path_through_node) as f64
            })
            .collect::<Vec<_>>();

        self.map_from_f64_scores(&slack)
    }

    fn compute_k_core(&self) -> HashMap<String, u32> {
        let n = self.node_to_id.len();
        if n == 0 {
            return HashMap::new();
        }

        let neighbors = self.undirected_neighbors();
        let mut degree = neighbors.iter().map(HashSet::len).collect::<Vec<_>>();
        let mut removed = vec![false; n];
        let mut core = vec![0u32; n];

        let mut heap = BinaryHeap::<Reverse<(usize, usize)>>::new();
        for (index, &deg) in degree.iter().enumerate() {
            heap.push(Reverse((deg, index)));
        }

        let mut current_core = 0usize;

        while let Some(Reverse((deg, node))) = heap.pop() {
            if removed[node] || deg != degree[node] {
                continue;
            }

            removed[node] = true;
            current_core = current_core.max(deg);
            core[node] = u32::try_from(current_core).unwrap_or(u32::MAX);

            for &neighbor in &neighbors[node] {
                if removed[neighbor] {
                    continue;
                }

                degree[neighbor] = degree[neighbor].saturating_sub(1);
                heap.push(Reverse((degree[neighbor], neighbor)));
            }
        }

        self.map_from_u32_scores(&core)
    }

    fn compute_articulation_points(&self) -> HashSet<String> {
        let n = self.node_to_id.len();
        if n <= 2 {
            return HashSet::new();
        }

        let neighbors = self
            .undirected_neighbors()
            .into_iter()
            .map(|set| {
                let mut values = set.into_iter().collect::<Vec<_>>();
                values.sort_unstable();
                values
            })
            .collect::<Vec<_>>();

        let mut disc = vec![0usize; n];
        let mut low = vec![0usize; n];
        let mut parent = vec![usize::MAX; n];
        let mut visited = vec![false; n];
        let mut is_ap = vec![false; n];
        let mut time = 0usize;

        for node in 0..n {
            if !visited[node] {
                tarjan_articulation_dfs(
                    node,
                    &neighbors,
                    &mut disc,
                    &mut low,
                    &mut parent,
                    &mut visited,
                    &mut is_ap,
                    &mut time,
                );
            }
        }

        is_ap
            .iter()
            .enumerate()
            .filter_map(|(index, &value)| {
                if value {
                    Some(self.node_to_id[index].clone())
                } else {
                    None
                }
            })
            .collect()
    }

    fn find_cycles(&self) -> Vec<Vec<String>> {
        let mut cycles = Vec::new();

        for component in kosaraju_scc(&self.graph) {
            if component.len() > 1 {
                let mut ids: Vec<String> = component
                    .iter()
                    .map(|node| self.node_to_id[node.index()].clone())
                    .collect();
                ids.sort();
                cycles.push(ids);
                continue;
            }

            let node = component[0];
            let has_self_loop = self
                .graph
                .edges(node)
                .any(|edge| edge.target().index() == node.index());
            if has_self_loop {
                cycles.push(vec![self.node_to_id[node.index()].clone()]);
            }
        }

        cycles.sort_by(|a, b| a.first().cmp(&b.first()));
        cycles
    }

    fn topological_order(&self) -> Option<Vec<usize>> {
        let n = self.node_to_id.len();
        if n == 0 {
            return Some(Vec::new());
        }

        let mut in_degree = self.predecessors.iter().map(Vec::len).collect::<Vec<_>>();
        let mut heap = BinaryHeap::<Reverse<usize>>::new();

        for (node, &degree) in in_degree.iter().enumerate() {
            if degree == 0 {
                heap.push(Reverse(node));
            }
        }

        let mut order = Vec::with_capacity(n);
        while let Some(Reverse(node)) = heap.pop() {
            order.push(node);

            for &succ in &self.successors[node] {
                in_degree[succ] = in_degree[succ].saturating_sub(1);
                if in_degree[succ] == 0 {
                    heap.push(Reverse(succ));
                }
            }
        }

        if order.len() == n { Some(order) } else { None }
    }

    fn undirected_neighbors(&self) -> Vec<HashSet<usize>> {
        let n = self.node_to_id.len();
        let mut neighbors = vec![HashSet::<usize>::new(); n];

        for node in 0..n {
            for &succ in &self.successors[node] {
                if node == succ {
                    continue;
                }
                neighbors[node].insert(succ);
                neighbors[succ].insert(node);
            }
        }

        neighbors
    }

    fn map_from_f64_scores(&self, scores: &[f64]) -> HashMap<String, f64> {
        let mut map = HashMap::with_capacity(scores.len());
        for (index, value) in scores.iter().enumerate() {
            map.insert(self.node_to_id[index].clone(), *value);
        }
        map
    }

    fn map_from_usize_scores(&self, scores: &[usize]) -> HashMap<String, usize> {
        let mut map = HashMap::with_capacity(scores.len());
        for (index, value) in scores.iter().enumerate() {
            map.insert(self.node_to_id[index].clone(), *value);
        }
        map
    }

    fn map_from_u32_scores(&self, scores: &[u32]) -> HashMap<String, u32> {
        let mut map = HashMap::with_capacity(scores.len());
        for (index, value) in scores.iter().enumerate() {
            map.insert(self.node_to_id[index].clone(), *value);
        }
        map
    }
}

fn normalize_l2(values: &mut [f64]) {
    let norm = values.iter().map(|value| value * value).sum::<f64>().sqrt();
    if norm > 0.0 {
        for value in values {
            *value /= norm;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn tarjan_articulation_dfs(
    node: usize,
    neighbors: &[Vec<usize>],
    disc: &mut [usize],
    low: &mut [usize],
    parent: &mut [usize],
    visited: &mut [bool],
    is_ap: &mut [bool],
    time: &mut usize,
) {
    visited[node] = true;
    *time = time.saturating_add(1);
    disc[node] = *time;
    low[node] = *time;
    let mut children = 0usize;

    for &next in &neighbors[node] {
        if !visited[next] {
            children = children.saturating_add(1);
            parent[next] = node;

            tarjan_articulation_dfs(next, neighbors, disc, low, parent, visited, is_ap, time);

            low[node] = low[node].min(low[next]);

            if parent[node] == usize::MAX && children > 1 {
                is_ap[node] = true;
            }
            if parent[node] != usize::MAX && low[next] >= disc[node] {
                is_ap[node] = true;
            }
        } else if next != parent[node] {
            low[node] = low[node].min(disc[next]);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{Dependency, Issue};

    use super::IssueGraph;

    #[test]
    fn critical_depth_matches_dependency_direction() {
        let issues = vec![
            Issue {
                id: "A".to_string(),
                title: "A".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 1,
                ..Issue::default()
            },
            Issue {
                id: "B".to_string(),
                title: "B".to_string(),
                status: "blocked".to_string(),
                issue_type: "task".to_string(),
                priority: 2,
                dependencies: vec![Dependency {
                    issue_id: "B".to_string(),
                    depends_on_id: "A".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
        ];

        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();

        assert_eq!(metrics.critical_depth.get("A"), Some(&2));
        assert_eq!(metrics.critical_depth.get("B"), Some(&1));
        assert_eq!(metrics.slack.get("A"), Some(&0.0));
        assert_eq!(metrics.slack.get("B"), Some(&0.0));
    }

    #[test]
    fn articulation_detects_cut_vertex() {
        let issues = vec![
            Issue {
                id: "A".to_string(),
                title: "A".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
            Issue {
                id: "B".to_string(),
                title: "B".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                dependencies: vec![Dependency {
                    issue_id: "B".to_string(),
                    depends_on_id: "A".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "C".to_string(),
                title: "C".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                dependencies: vec![Dependency {
                    issue_id: "C".to_string(),
                    depends_on_id: "A".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
        ];

        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();

        assert!(metrics.articulation_points.contains("A"));
    }

    #[test]
    fn betweenness_finds_middle_node_in_chain() {
        let issues = vec![
            Issue {
                id: "A".to_string(),
                title: "A".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                dependencies: vec![Dependency {
                    issue_id: "A".to_string(),
                    depends_on_id: "B".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "B".to_string(),
                title: "B".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                dependencies: vec![Dependency {
                    issue_id: "B".to_string(),
                    depends_on_id: "C".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "C".to_string(),
                title: "C".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
        ];

        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();

        let a = metrics.betweenness.get("A").copied().unwrap_or_default();
        let b = metrics.betweenness.get("B").copied().unwrap_or_default();
        let c = metrics.betweenness.get("C").copied().unwrap_or_default();

        assert!(b > a);
        assert!(b > c);
    }

    #[test]
    fn connected_open_components_group_blocker_cluster() {
        let issues = vec![
            Issue {
                id: "bd-3q0".to_string(),
                title: "Primary blocker".to_string(),
                status: "in_progress".to_string(),
                issue_type: "feature".to_string(),
                priority: 1,
                ..Issue::default()
            },
            Issue {
                id: "bd-3q1".to_string(),
                title: "Blocked follow-on".to_string(),
                status: "blocked".to_string(),
                issue_type: "task".to_string(),
                priority: 2,
                dependencies: vec![Dependency {
                    issue_id: "bd-3q1".to_string(),
                    depends_on_id: "bd-3q0".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "bd-3q2".to_string(),
                title: "Independent slice".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 3,
                ..Issue::default()
            },
        ];

        let graph = IssueGraph::build(&issues);
        let components = graph.connected_open_components();
        assert_eq!(
            components,
            vec![
                vec!["bd-3q0".to_string(), "bd-3q1".to_string()],
                vec!["bd-3q2".to_string()],
            ]
        );

        let metrics = graph.compute_metrics();
        assert_eq!(metrics.blocks_count.get("bd-3q0"), Some(&1));
        assert!(metrics.cycles.is_empty());
    }

    #[test]
    fn actionable_excludes_children_of_blocked_parent_epic() {
        // Parent epic E is blocked by blocker B.
        // Child task C has a parent-child dep on E.
        // C should NOT be actionable because its parent E is blocked.
        let issues = vec![
            Issue {
                id: "B".to_string(),
                title: "Blocker".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 1,
                ..Issue::default()
            },
            Issue {
                id: "E".to_string(),
                title: "Epic".to_string(),
                status: "blocked".to_string(),
                issue_type: "epic".to_string(),
                priority: 2,
                dependencies: vec![Dependency {
                    issue_id: "E".to_string(),
                    depends_on_id: "B".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "C".to_string(),
                title: "Child task".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 3,
                dependencies: vec![Dependency {
                    issue_id: "C".to_string(),
                    depends_on_id: "E".to_string(),
                    dep_type: "parent-child".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
        ];

        let graph = IssueGraph::build(&issues);
        let actionable = graph.actionable_ids();

        // Only B should be actionable (it has no blockers).
        // E is blocked by B, and C is blocked transitively via parent E.
        assert_eq!(actionable, vec!["B".to_string()]);
    }

    #[test]
    fn actionable_includes_children_of_unblocked_parent() {
        // Parent epic E has no blockers.
        // Child task C has a parent-child dep on E.
        // Both should be actionable.
        let issues = vec![
            Issue {
                id: "E".to_string(),
                title: "Epic".to_string(),
                status: "open".to_string(),
                issue_type: "epic".to_string(),
                priority: 1,
                ..Issue::default()
            },
            Issue {
                id: "C".to_string(),
                title: "Child task".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 2,
                dependencies: vec![Dependency {
                    issue_id: "C".to_string(),
                    depends_on_id: "E".to_string(),
                    dep_type: "parent-child".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
        ];

        let graph = IssueGraph::build(&issues);
        let actionable = graph.actionable_ids();

        assert_eq!(actionable, vec!["C".to_string(), "E".to_string()]);
    }

    #[test]
    fn actionable_handles_mixed_prefix_datasets() {
        // Dataset with mixed prefixes (bd- and bv- style IDs).
        // This tests graceful handling of mixed-prefix datasets.
        let issues = vec![
            Issue {
                id: "bd-100".to_string(),
                title: "Beads style".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 1,
                ..Issue::default()
            },
            Issue {
                id: "bv-200".to_string(),
                title: "Viewer style".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 2,
                dependencies: vec![Dependency {
                    issue_id: "bv-200".to_string(),
                    depends_on_id: "bd-100".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "gh-300".to_string(),
                title: "GitHub style".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 3,
                ..Issue::default()
            },
        ];

        let graph = IssueGraph::build(&issues);
        let actionable = graph.actionable_ids();

        // bd-100 is actionable (no blockers), gh-300 is actionable
        // bv-200 is blocked by bd-100
        assert_eq!(actionable, vec!["bd-100".to_string(), "gh-300".to_string()]);
    }
}
