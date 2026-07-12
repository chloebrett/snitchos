//! A precedence-constrained list scheduler (design:
//! `docs/snemu-itest-snapshot-tree-design.md`, increment 3).
//!
//! The snapshot tree turns the audit's flat, phase-barriered packing into a
//! dependency graph: a scenario can't run until its snapshot node is materialised,
//! a shared stream can't run until its workload has booted, and so on. This module
//! schedules that graph across a worker pool with **critical-path (bottom-level)
//! list scheduling** — Hu's level idea, weighted: whenever a worker frees, it takes
//! the highest-priority *ready* node (all deps complete). It's online-greedy, so
//! prior-run instret estimates only steer priority; they never commit to a plan.

use std::sync::{Condvar, Mutex};

/// One node in the precedence graph: the nodes it depends on (by index) and its
/// scheduling priority. Higher priority runs first among the *ready* nodes; the
/// caller sets it to the node's **bottom-level** — the longest remaining
/// estimated-instret path from this node down to a leaf — so the critical path
/// launches earliest even under rough estimates.
pub struct Node {
    pub deps: Vec<usize>,
    pub priority: f64,
}

/// Shared scheduler state behind one mutex: which nodes are runnable now, how many
/// unmet deps each node still has, and how many nodes are running / done.
struct State {
    ready: Vec<usize>,
    remaining: Vec<usize>,
    running: usize,
    completed: usize,
}

/// Run `nodes` across `workers` threads, honouring precedence (a node runs only
/// after every node in its `deps` has completed) and, among the currently-ready
/// nodes, preferring the highest `priority` (bottom-level list scheduling).
/// `run(worker, i)` does node `i`'s work on host thread `worker` (`0..workers`,
/// surfaced so per-worker accounting keeps working) — reading its deps' outputs from
/// and writing its own into caller-owned shared state, which the precedence
/// guarantees are safe to touch by the time it runs. Online-greedy: a freed worker
/// takes the best ready node, so estimates steer order without committing to a plan.
pub fn run_scheduled(nodes: &[Node], workers: usize, run: impl Fn(usize, usize) + Sync) {
    let n = nodes.len();
    if n == 0 {
        return;
    }
    let remaining: Vec<usize> = nodes.iter().map(|node| node.deps.len()).collect();
    let ready: Vec<usize> = (0..n).filter(|&i| remaining[i] == 0).collect();
    // Reverse edges: when node `i` completes, these are the nodes to decrement.
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, node) in nodes.iter().enumerate() {
        for &d in &node.deps {
            dependents[d].push(i);
        }
    }

    let state = Mutex::new(State { ready, remaining, running: 0, completed: 0 });
    let woken = Condvar::new();

    std::thread::scope(|scope| {
        for worker in 0..workers.max(1) {
            let state = &state;
            let woken = &woken;
            let dependents = &dependents;
            let run = &run;
            scope.spawn(move || {
                loop {
                    // Claim the highest-priority ready node, or wait / exit.
                    let idx = {
                        let mut st = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        loop {
                            if let Some(pos) = highest_priority(&st.ready, nodes) {
                                let i = st.ready.swap_remove(pos);
                                st.running += 1;
                                break Some(i);
                            }
                            if st.completed == n || st.running == 0 {
                                // All done, or nothing ready and nothing in flight to
                                // produce more (a DAG can't reach the latter with work
                                // left; guard against a cycle by exiting, not hanging).
                                break None;
                            }
                            st = woken.wait(st).unwrap_or_else(std::sync::PoisonError::into_inner);
                        }
                    };
                    let Some(i) = idx else { break };

                    run(worker, i);

                    let mut st = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    st.running -= 1;
                    st.completed += 1;
                    for &d in &dependents[i] {
                        st.remaining[d] -= 1;
                        if st.remaining[d] == 0 {
                            st.ready.push(d);
                        }
                    }
                    // Wake peers: new ready nodes to claim, or the run just finished.
                    woken.notify_all();
                }
            });
        }
    });
}

/// Index into `ready` of the node with the greatest priority, or `None` if empty.
/// Ties break toward the earlier `ready` entry (stable, deterministic).
fn highest_priority(ready: &[usize], nodes: &[Node]) -> Option<usize> {
    ready
        .iter()
        .enumerate()
        .max_by(|a, b| nodes[*a.1].priority.total_cmp(&nodes[*b.1].priority))
        .map(|(pos, _)| pos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Record the order nodes run in, so tests can assert precedence + priority.
    fn run_order(nodes: &[Node], workers: usize) -> Vec<usize> {
        let order = Mutex::new(Vec::new());
        run_scheduled(nodes, workers, |_worker, i| {
            order.lock().unwrap().push(i);
        });
        order.into_inner().unwrap()
    }

    #[test]
    fn a_linear_chain_runs_in_dependency_order() {
        // 0 → 1 → 2: each depends on the previous, so the only valid order is 0,1,2.
        let nodes = vec![
            Node { deps: vec![], priority: 0.0 },
            Node { deps: vec![0], priority: 0.0 },
            Node { deps: vec![1], priority: 0.0 },
        ];
        assert_eq!(run_order(&nodes, 4), vec![0, 1, 2]);
    }

    #[test]
    fn among_ready_nodes_the_highest_priority_runs_first() {
        // Four independent (all-ready) nodes; one worker drains them strictly by
        // priority, so the critical-path node leads.
        let nodes = vec![
            Node { deps: vec![], priority: 1.0 },
            Node { deps: vec![], priority: 9.0 },
            Node { deps: vec![], priority: 3.0 },
            Node { deps: vec![], priority: 7.0 },
        ];
        assert_eq!(run_order(&nodes, 1), vec![1, 3, 2, 0]);
    }

    #[test]
    fn a_node_runs_only_after_all_of_its_deps() {
        // Diamond: 0 → {1, 2} → 3. Whatever the interleaving, 0 is first and 3 last.
        let nodes = vec![
            Node { deps: vec![], priority: 0.0 },
            Node { deps: vec![0], priority: 0.0 },
            Node { deps: vec![0], priority: 0.0 },
            Node { deps: vec![1, 2], priority: 0.0 },
        ];
        for workers in [1usize, 2, 4, 8] {
            let order = run_order(&nodes, workers);
            let pos = |x: usize| order.iter().position(|&y| y == x).unwrap();
            assert_eq!(order.len(), 4, "workers={workers}: every node runs once");
            assert_eq!(pos(0), 0, "workers={workers}: the root runs first");
            assert!(pos(1) > pos(0) && pos(2) > pos(0), "workers={workers}: children after root");
            assert!(pos(3) > pos(1) && pos(3) > pos(2), "workers={workers}: join after both children");
        }
    }

    #[test]
    fn every_node_runs_exactly_once_across_worker_counts() {
        // A wider graph: two independent chains plus a shared leaf, run under several
        // worker counts — each node fires exactly once regardless of parallelism.
        let nodes = vec![
            Node { deps: vec![], priority: 5.0 },
            Node { deps: vec![0], priority: 4.0 },
            Node { deps: vec![], priority: 3.0 },
            Node { deps: vec![2], priority: 2.0 },
            Node { deps: vec![1, 3], priority: 1.0 },
        ];
        for workers in [1usize, 3, 16] {
            let mut order = run_order(&nodes, workers);
            order.sort_unstable();
            assert_eq!(order, vec![0, 1, 2, 3, 4], "workers={workers}: all run once");
        }
    }
}
