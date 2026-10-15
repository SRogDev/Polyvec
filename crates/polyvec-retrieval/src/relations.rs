//! Bounded multi-hop traversal over supplied relations (Phase 6).
//!
//! Pure graph logic: [`traverse`] walks an adjacency function with a
//! visited set (cycle protection), depth / node / fan-out caps, and a
//! wall-clock budget. Storage lives behind the `adjacency` callback, so
//! this module is testable with in-memory graphs and has no redb
//! dependency.

use std::collections::{HashSet, VecDeque};
use std::time::Instant;

use polyvec_core::{Error, RelationType, Result};

/// Which way an edge is walked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    /// `seed --rel--> neighbor` (edge stored source → target).
    Outgoing,
    /// `neighbor --rel--> seed` (edge stored target → source, walked back).
    Incoming,
}

/// One directed step offered by the adjacency function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    /// The relation's stable id.
    pub relation_id: String,
    /// The relation's type.
    pub relation_type: RelationType,
    /// Which way this step walks the stored edge.
    pub direction: Direction,
    /// The node reached by this step.
    pub neighbor: String,
}

/// One hop of a returned path (evidence).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathHop {
    /// The relation walked.
    pub relation_id: String,
    /// The relation's type.
    pub relation_type: RelationType,
    /// Which way the stored edge was walked.
    pub direction: Direction,
}

/// A node reached by traversal, with its evidence path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpandedNode {
    /// The reached node (document or chunk id).
    pub node: String,
    /// Number of hops from the nearest seed.
    pub hops: u32,
    /// The evidence path: relation types + directions per hop.
    pub path: Vec<PathHop>,
}

/// Bounds for [`traverse`]. See `docs/retrieval.md` for the rationale
/// behind each default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraversalLimits {
    /// Max hops from a seed (1–8).
    pub max_depth: u8,
    /// Max expanded nodes for the whole call.
    pub max_nodes: usize,
    /// Max edges walked per node (supernode protection).
    pub max_fanout: usize,
    /// Wall-clock backstop for the whole call (milliseconds).
    pub max_millis: u64,
}

impl Default for TraversalLimits {
    fn default() -> Self {
        Self {
            max_depth: 3,
            max_nodes: 100,
            max_fanout: 64,
            max_millis: 2000,
        }
    }
}

impl TraversalLimits {
    /// Builds validated limits.
    ///
    /// # Errors
    /// [`Error::Validation`] when `max_depth` is 0 or above 8, or when
    /// `max_nodes` / `max_fanout` is 0.
    pub fn new(
        max_depth: u8,
        max_nodes: usize,
        max_fanout: usize,
        max_millis: u64,
    ) -> Result<Self> {
        if max_depth == 0 || max_depth > 8 {
            return Err(Error::Validation {
                detail: format!("max_depth must be within 1..=8, got {max_depth}"),
            });
        }
        if max_nodes == 0 {
            return Err(Error::Validation {
                detail: "max_nodes must be at least 1".to_string(),
            });
        }
        if max_fanout == 0 {
            return Err(Error::Validation {
                detail: "max_fanout must be at least 1".to_string(),
            });
        }
        Ok(Self {
            max_depth,
            max_nodes,
            max_fanout,
            max_millis,
        })
    }
}

/// Breadth-first expansion over `adjacency`, bounded by `limits`.
///
/// * Seeds are never returned (even if reachable from another seed).
/// * Each node is visited once — first visit wins, so the recorded path
///   is a shortest path and cycles terminate.
/// * Adjacency is sorted by `(relation_type, relation_id)` before walking,
///   so output is deterministic for a fixed graph.
/// * Fan-out beyond `max_fanout` is truncated after sorting (documented).
/// * Stops when any bound is hit; hitting the time budget returns the
///   nodes found so far (a bound, not an error).
/// * The adjacency function may fail (e.g. storage I/O); the error
///   propagates and aborts the traversal — bounds never mask failures.
///
/// # Errors
/// Whatever `E` the adjacency function returns.
pub fn traverse<E>(
    seeds: &[String],
    adjacency: impl Fn(&str) -> std::result::Result<Vec<Edge>, E>,
    limits: &TraversalLimits,
) -> std::result::Result<Vec<ExpandedNode>, E> {
    let start = Instant::now();
    // Owned ids: traversal must not leak memory (no Box::leak) and must
    // not borrow the caller's seed slice beyond the call.
    let mut visited: HashSet<String> = HashSet::with_capacity(seeds.len());
    for seed in seeds {
        visited.insert(seed.clone());
    }
    let mut out: Vec<ExpandedNode> = Vec::new();
    // BFS layers: (node, hops, path).
    let mut frontier: VecDeque<(String, u32, Vec<PathHop>)> = VecDeque::new();
    for seed in seeds {
        frontier.push_back((seed.clone(), 0, Vec::new()));
    }

    while let Some((node, hops, path)) = frontier.pop_front() {
        if out.len() >= limits.max_nodes {
            break;
        }
        if start.elapsed().as_millis() >= u128::from(limits.max_millis) {
            break;
        }
        if hops >= u32::from(limits.max_depth) {
            continue;
        }
        let mut edges = adjacency(&node)?;
        edges.sort_by(|a, b| {
            (a.relation_type, &a.relation_id).cmp(&(b.relation_type, &b.relation_id))
        });
        edges.truncate(limits.max_fanout);
        for edge in edges {
            if !visited.insert(edge.neighbor.clone()) {
                continue;
            }
            let mut next_path = path.clone();
            next_path.push(PathHop {
                relation_id: edge.relation_id.clone(),
                relation_type: edge.relation_type,
                direction: edge.direction,
            });
            let next_hops = hops + 1;
            out.push(ExpandedNode {
                node: edge.neighbor.clone(),
                hops: next_hops,
                path: next_path.clone(),
            });
            if out.len() >= limits.max_nodes {
                break;
            }
            frontier.push_back((edge.neighbor, next_hops, next_path));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::convert::Infallible;

    fn edge(id: &str, rel_type: RelationType, direction: Direction, neighbor: &str) -> Edge {
        Edge {
            relation_id: id.to_string(),
            relation_type: rel_type,
            direction,
            neighbor: neighbor.to_string(),
        }
    }

    /// Builds an adjacency closure from `(node, edges)` pairs.
    fn adjacency_of(
        pairs: Vec<(&str, Vec<Edge>)>,
    ) -> impl Fn(&str) -> std::result::Result<Vec<Edge>, Infallible> {
        let map: HashMap<String, Vec<Edge>> =
            pairs.into_iter().map(|(n, e)| (n.to_string(), e)).collect();
        move |node: &str| std::result::Result::Ok(map.get(node).cloned().unwrap_or_default())
    }

    fn chain(n: usize) -> Vec<(&'static str, Vec<Edge>)> {
        // a -> b -> c -> ... (references, outgoing), fixed names.
        let names = ["a", "b", "c", "d", "e", "f"];
        (0..n)
            .map(|i| {
                (
                    names[i],
                    vec![edge(
                        &format!("r{i}"),
                        RelationType::References,
                        Direction::Outgoing,
                        names[i + 1],
                    )],
                )
            })
            .collect()
    }

    #[test]
    fn chain_multi_hop_returns_evidence_paths() {
        let adj = adjacency_of(chain(3));
        let out = traverse(&["a".to_string()], adj, &TraversalLimits::default()).unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].node, "b");
        assert_eq!(out[0].hops, 1);
        assert_eq!(out[0].path.len(), 1);
        assert_eq!(out[0].path[0].relation_type, RelationType::References);
        assert_eq!(out[0].path[0].direction, Direction::Outgoing);
        assert_eq!(out[0].path[0].relation_id, "r0");
        assert_eq!(out[2].node, "d");
        assert_eq!(out[2].hops, 3);
        assert_eq!(out[2].path.len(), 3);
        // Path chains: d's path extends c's path.
        assert_eq!(out[2].path[..2], out[1].path);
    }

    #[test]
    fn cycle_terminates_and_visits_each_node_once() {
        let adj = adjacency_of(vec![
            (
                "a",
                vec![edge(
                    "r0",
                    RelationType::References,
                    Direction::Outgoing,
                    "b",
                )],
            ),
            (
                "b",
                vec![edge(
                    "r1",
                    RelationType::References,
                    Direction::Outgoing,
                    "c",
                )],
            ),
            (
                "c",
                vec![edge(
                    "r2",
                    RelationType::References,
                    Direction::Outgoing,
                    "a",
                )],
            ),
        ]);
        let out = traverse(&["a".to_string()], adj, &TraversalLimits::default()).unwrap();
        let nodes: Vec<_> = out.iter().map(|n| n.node.as_str()).collect();
        assert_eq!(nodes, vec!["b", "c"]);
    }

    #[test]
    fn depth_limit_stops_expansion() {
        let adj = adjacency_of(chain(5));
        let limits = TraversalLimits::new(2, 100, 64, 2000).unwrap();
        let out = traverse(&["a".to_string()], adj, &limits).unwrap();
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|n| n.hops <= 2));
    }

    #[test]
    fn node_budget_caps_total_expansion() {
        // center -> 200 leaves.
        let leaves: Vec<Edge> = (0..200)
            .map(|i| {
                edge(
                    &format!("r{i:03}"),
                    RelationType::References,
                    Direction::Outgoing,
                    &format!("leaf-{i:03}"),
                )
            })
            .collect();
        let adj = adjacency_of(vec![("center", leaves)]);
        let limits = TraversalLimits::new(3, 10, 1000, 2000).unwrap();
        let out = traverse(&["center".to_string()], adj, &limits).unwrap();
        assert_eq!(out.len(), 10);
    }

    #[test]
    fn fanout_cap_truncates_supernode_deterministically() {
        let leaves: Vec<Edge> = (0..200)
            .map(|i| {
                edge(
                    &format!("r{i:03}"),
                    RelationType::References,
                    Direction::Outgoing,
                    &format!("leaf-{i:03}"),
                )
            })
            .collect();
        let adj = adjacency_of(vec![("center", leaves)]);
        let limits = TraversalLimits::new(3, 1000, 5, 2000).unwrap();
        let out = traverse(&["center".to_string()], adj, &limits).unwrap();
        assert_eq!(out.len(), 5);
        // Truncation happens after sorting by (type, id): the lowest ids win.
        let nodes: Vec<_> = out.iter().map(|n| n.node.as_str()).collect();
        assert_eq!(
            nodes,
            vec!["leaf-000", "leaf-001", "leaf-002", "leaf-003", "leaf-004"]
        );
    }

    #[test]
    fn seeds_are_never_returned() {
        let adj = adjacency_of(vec![
            (
                "a",
                vec![edge(
                    "r0",
                    RelationType::References,
                    Direction::Outgoing,
                    "b",
                )],
            ),
            (
                "b",
                vec![edge(
                    "r1",
                    RelationType::References,
                    Direction::Outgoing,
                    "c",
                )],
            ),
        ]);
        let out = traverse(
            &["a".to_string(), "b".to_string()],
            adj,
            &TraversalLimits::default(),
        )
        .unwrap();
        let nodes: Vec<_> = out.iter().map(|n| n.node.as_str()).collect();
        assert_eq!(nodes, vec!["c"]);
    }

    #[test]
    fn incoming_edges_walked_with_direction_marked() {
        // b references a (stored b -> a); walking from a goes incoming.
        let adj = adjacency_of(vec![(
            "a",
            vec![edge(
                "r0",
                RelationType::References,
                Direction::Incoming,
                "b",
            )],
        )]);
        let out = traverse(&["a".to_string()], adj, &TraversalLimits::default()).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].node, "b");
        assert_eq!(out[0].path[0].direction, Direction::Incoming);
    }

    #[test]
    fn traversal_is_deterministic() {
        let adj = adjacency_of(chain(4));
        let first = traverse(
            &["a".to_string()],
            adjacency_of(chain(4)),
            &TraversalLimits::default(),
        )
        .unwrap();
        let second = traverse(&["a".to_string()], adj, &TraversalLimits::default()).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn limits_validation() {
        assert!(TraversalLimits::new(0, 100, 64, 2000).is_err());
        assert!(TraversalLimits::new(9, 100, 64, 2000).is_err());
        assert!(TraversalLimits::new(3, 0, 64, 2000).is_err());
        assert!(TraversalLimits::new(3, 100, 0, 2000).is_err());
        assert!(TraversalLimits::new(8, 1, 1, 0).is_ok());
    }

    #[test]
    fn zero_time_budget_returns_nothing() {
        // A 0 ms budget trips before the first layer: deterministic,
        // no timing flakes.
        let adj = adjacency_of(chain(3));
        let limits = TraversalLimits::new(3, 100, 64, 0).unwrap();
        let out = traverse(&["a".to_string()], adj, &limits).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn diamond_visits_shared_node_once_with_shortest_path() {
        // a -> b -> d, a -> c -> d: d is reached once, via 2 hops.
        let adj = adjacency_of(vec![
            (
                "a",
                vec![
                    edge("r0", RelationType::References, Direction::Outgoing, "b"),
                    edge("r1", RelationType::References, Direction::Outgoing, "c"),
                ],
            ),
            (
                "b",
                vec![edge(
                    "r2",
                    RelationType::References,
                    Direction::Outgoing,
                    "d",
                )],
            ),
            (
                "c",
                vec![edge(
                    "r3",
                    RelationType::References,
                    Direction::Outgoing,
                    "d",
                )],
            ),
        ]);
        let out = traverse(&["a".to_string()], adj, &TraversalLimits::default()).unwrap();
        let d: Vec<_> = out.iter().filter(|n| n.node == "d").collect();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].hops, 2);
    }
}
