//! In-memory folder hierarchy rebuilt from flat parent-ID links.
//!
//! `GET /api/v1/folders` returns every folder with a `parent` reference.
//! [`FolderTree::build`] turns that list into a navigable tree in
//! `O(n log n)` time, sorting children by name.
//!
//! The server is not trusted to send a well-formed forest. The builder
//! repairs, and records as [`TreeAnomaly`]:
//!
//! - **Duplicate IDs**: the first occurrence wins.
//! - **Self-parented folders** become top-level.
//! - **Orphans** (parent ID not in the list) become top-level, so they stay
//!   reachable instead of vanishing from the UI.
//! - **Cycles** (A → B → A) are broken at the member with the smallest ID,
//!   which becomes top-level. The choice is deterministic, so repeated
//!   refreshes produce the same tree.
//!
//! After `build`, the effective parent links form a forest. Every traversal
//! is iterative, so "unlimited" nesting depth cannot overflow the stack.

use std::collections::HashMap;

use super::models::{Folder, FolderId};

/// A repaired inconsistency in the server's folder list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeAnomaly {
    /// Two folders shared an ID; the later one was dropped.
    DuplicateId(FolderId),
    /// A folder named itself as its parent.
    SelfParent(FolderId),
    /// A folder's parent does not exist.
    MissingParent {
        /// The orphaned folder.
        folder: FolderId,
        /// The parent ID that was not found.
        parent: FolderId,
    },
    /// Parent links formed a cycle. Members are sorted. The first member was
    /// detached and made top-level.
    Cycle(Vec<FolderId>),
}

/// DFS colouring for cycle detection.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Unvisited,
    OnPath,
    Done,
}

#[derive(Debug, Clone)]
struct Node {
    folder: Folder,
    /// Parent after repairs; `None` means top level.
    parent: Option<FolderId>,
    children: Vec<FolderId>,
}

/// Navigable folder hierarchy.
#[derive(Debug, Clone, Default)]
pub struct FolderTree {
    nodes: HashMap<FolderId, Node>,
    roots: Vec<FolderId>,
    anomalies: Vec<TreeAnomaly>,
}

impl FolderTree {
    /// Build the tree, repairing anomalies (see module docs).
    #[must_use]
    pub fn build(folders: Vec<Folder>) -> Self {
        let mut anomalies = Vec::new();
        let mut nodes = index_folders(folders, &mut anomalies);
        // Sorted IDs make every pass, and so the repairs, deterministic.
        let mut ids: Vec<FolderId> = nodes.keys().cloned().collect();
        ids.sort_unstable();

        detach_invalid_parents(&mut nodes, &ids, &mut anomalies);
        for cycle in find_cycles(&nodes, &ids) {
            if let Some(first) = cycle.first() {
                set_parent(&mut nodes, first, None);
            }
            anomalies.push(TreeAnomaly::Cycle(cycle));
        }
        let roots = link_children(&mut nodes, &ids);

        Self {
            nodes,
            roots,
            anomalies,
        }
    }

    /// Number of folders.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether there are no folders.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Repairs made while building.
    #[must_use]
    pub fn anomalies(&self) -> &[TreeAnomaly] {
        &self.anomalies
    }

    /// Look up a folder.
    #[must_use]
    pub fn get(&self, id: &FolderId) -> Option<&Folder> {
        self.nodes.get(id).map(|n| &n.folder)
    }

    /// Top-level folders, sorted by name.
    pub fn roots(&self) -> impl Iterator<Item = &Folder> {
        self.roots.iter().filter_map(|id| self.get(id))
    }

    /// Immediate children, sorted by name. Empty for an unknown ID.
    pub fn children(&self, id: &FolderId) -> impl Iterator<Item = &Folder> {
        self.nodes
            .get(id)
            .map(|n| n.children.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|c| self.get(c))
    }

    /// Effective parent after repairs.
    #[must_use]
    pub fn parent(&self, id: &FolderId) -> Option<&Folder> {
        self.nodes
            .get(id)?
            .parent
            .as_ref()
            .and_then(|p| self.get(p))
    }

    /// Path from the top level down to `id`, inclusive (breadcrumbs).
    /// Empty for an unknown ID.
    #[must_use]
    pub fn path_to(&self, id: &FolderId) -> Vec<&Folder> {
        let mut path = Vec::new();
        let mut cursor = self.nodes.get(id);
        // The forest is acyclic after build(); the bound is defence in depth.
        while let Some(node) = cursor {
            if path.len() > self.nodes.len() {
                break;
            }
            path.push(&node.folder);
            cursor = node.parent.as_ref().and_then(|p| self.nodes.get(p));
        }
        path.reverse();
        path
    }

    /// Whether `candidate` is `ancestor` or lies anywhere beneath it. Use
    /// this to disable invalid move targets before asking the server.
    #[must_use]
    pub fn is_within(&self, candidate: &FolderId, ancestor: &FolderId) -> bool {
        self.path_to(candidate).iter().any(|f| &f.id == ancestor)
    }

    /// Pre-order traversal as `(depth, folder)`, with top level at depth 0.
    #[must_use]
    pub fn depth_first(&self) -> Vec<(usize, &Folder)> {
        let mut out = Vec::with_capacity(self.nodes.len());
        let mut stack: Vec<(usize, &FolderId)> =
            self.roots.iter().rev().map(|id| (0, id)).collect();
        while let Some((depth, id)) = stack.pop() {
            let Some(node) = self.nodes.get(id) else {
                continue;
            };
            out.push((depth, &node.folder));
            stack.extend(node.children.iter().rev().map(|c| (depth + 1, c)));
        }
        out
    }
}

/// Index folders by ID. Later duplicates are dropped and recorded.
fn index_folders(
    folders: Vec<Folder>,
    anomalies: &mut Vec<TreeAnomaly>,
) -> HashMap<FolderId, Node> {
    let mut nodes = HashMap::with_capacity(folders.len());
    for folder in folders {
        if nodes.contains_key(&folder.id) {
            anomalies.push(TreeAnomaly::DuplicateId(folder.id));
            continue;
        }
        let parent = folder.parent.clone();
        nodes.insert(
            folder.id.clone(),
            Node {
                folder,
                parent,
                children: Vec::new(),
            },
        );
    }
    nodes
}

/// Pass 1: self-parented folders and orphans become top-level.
fn detach_invalid_parents(
    nodes: &mut HashMap<FolderId, Node>,
    ids: &[FolderId],
    anomalies: &mut Vec<TreeAnomaly>,
) {
    for id in ids {
        let Some(parent) = nodes[id].parent.clone() else {
            continue;
        };
        if &parent == id {
            anomalies.push(TreeAnomaly::SelfParent(id.clone()));
            set_parent(nodes, id, None);
        } else if !nodes.contains_key(&parent) {
            anomalies.push(TreeAnomaly::MissingParent {
                folder: id.clone(),
                parent,
            });
            set_parent(nodes, id, None);
        }
    }
}

/// Pass 2: find every cycle, each returned with its members sorted.
///
/// Every node has at most one parent, so each connected component contains
/// at most one cycle, and breaking one edge per cycle yields a forest. Each
/// node is visited once: `O(n log n)` including the ID lookups.
fn find_cycles(nodes: &HashMap<FolderId, Node>, ids: &[FolderId]) -> Vec<Vec<FolderId>> {
    let mut marks: HashMap<&FolderId, Mark> = ids.iter().map(|id| (id, Mark::Unvisited)).collect();
    let mut cycles = Vec::new();
    for start in ids {
        let mut path: Vec<&FolderId> = Vec::new();
        let mut cursor = Some(start);
        while let Some(id) = cursor {
            match marks[id] {
                Mark::Done => break,
                Mark::OnPath => {
                    let pos = path.iter().position(|p| *p == id).unwrap_or(0);
                    let mut cycle: Vec<FolderId> =
                        path[pos..].iter().map(|f| (*f).clone()).collect();
                    cycle.sort_unstable();
                    cycles.push(cycle);
                    break;
                }
                Mark::Unvisited => {
                    marks.insert(id, Mark::OnPath);
                    path.push(id);
                    // Borrow the parent from `ids` so it lives as long as `marks`.
                    cursor = nodes[id]
                        .parent
                        .as_ref()
                        .and_then(|p| ids.get(ids.binary_search(p).ok()?));
                }
            }
        }
        for id in path {
            marks.insert(id, Mark::Done);
        }
    }
    cycles
}

/// Pass 3: fill child lists from the repaired parent links and return the
/// roots. Siblings are sorted case-insensitively by name, then by ID.
fn link_children(nodes: &mut HashMap<FolderId, Node>, ids: &[FolderId]) -> Vec<FolderId> {
    let mut roots = Vec::new();
    for id in ids {
        match nodes[id].parent.clone() {
            Some(parent) => {
                if let Some(p) = nodes.get_mut(&parent) {
                    p.children.push(id.clone());
                }
            }
            None => roots.push(id.clone()),
        }
    }
    let sort_key = |nodes: &HashMap<FolderId, Node>, id: &FolderId| {
        (
            nodes
                .get(id)
                .map(|n| n.folder.name.to_lowercase())
                .unwrap_or_default(),
            id.clone(),
        )
    };
    roots.sort_by_cached_key(|id| sort_key(nodes, id));
    let child_lists: Vec<(FolderId, Vec<FolderId>)> = ids
        .iter()
        .map(|id| {
            let mut children = nodes[id].children.clone();
            children.sort_by_cached_key(|c| sort_key(nodes, c));
            (id.clone(), children)
        })
        .collect();
    for (id, children) in child_lists {
        if let Some(node) = nodes.get_mut(&id) {
            node.children = children;
        }
    }
    roots
}

fn set_parent(nodes: &mut HashMap<FolderId, Node>, id: &FolderId, parent: Option<FolderId>) {
    if let Some(node) = nodes.get_mut(id) {
        node.parent = parent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(id: &str, name: &str, parent: Option<&str>) -> Folder {
        Folder {
            id: FolderId::new(id),
            name: name.to_owned(),
            public: false,
            file_count: 0,
            folder_count: 0,
            created_at: None,
            parent: parent.map(FolderId::new),
        }
    }

    fn names<'a>(it: impl Iterator<Item = &'a Folder>) -> Vec<&'a str> {
        it.map(|f| f.name.as_str()).collect()
    }

    fn id(s: &str) -> FolderId {
        FolderId::new(s)
    }

    #[test]
    fn builds_sorted_hierarchy() {
        let tree = FolderTree::build(vec![
            f("3", "episodes", Some("1")),
            f("1", "Photos", None),
            f("2", "archive", None),
            f("4", "Bloopers", Some("1")),
            f("5", "S01", Some("3")),
        ]);
        assert_eq!(tree.anomalies(), []);
        assert_eq!(tree.len(), 5);
        assert_eq!(names(tree.roots()), ["archive", "Photos"]);
        assert_eq!(names(tree.children(&id("1"))), ["Bloopers", "episodes"]);
        assert_eq!(tree.parent(&id("5")).unwrap().name, "episodes");
        assert_eq!(
            names(tree.path_to(&id("5")).into_iter()),
            ["Photos", "episodes", "S01"]
        );
        let dfs: Vec<(usize, &str)> = tree
            .depth_first()
            .into_iter()
            .map(|(d, f)| (d, f.name.as_str()))
            .collect();
        assert_eq!(
            dfs,
            [
                (0, "archive"),
                (0, "Photos"),
                (1, "Bloopers"),
                (1, "episodes"),
                (2, "S01")
            ]
        );
    }

    #[test]
    fn orphans_become_roots() {
        let tree = FolderTree::build(vec![f("a", "A", Some("missing")), f("b", "B", Some("a"))]);
        assert_eq!(names(tree.roots()), ["A"]);
        assert_eq!(names(tree.children(&id("a"))), ["B"]);
        assert_eq!(
            tree.anomalies(),
            [TreeAnomaly::MissingParent {
                folder: id("a"),
                parent: id("missing")
            }]
        );
    }

    #[test]
    fn self_parent_becomes_root() {
        let tree = FolderTree::build(vec![f("a", "A", Some("a"))]);
        assert_eq!(names(tree.roots()), ["A"]);
        assert_eq!(tree.anomalies(), [TreeAnomaly::SelfParent(id("a"))]);
    }

    #[test]
    fn duplicate_ids_keep_first() {
        let tree = FolderTree::build(vec![f("a", "First", None), f("a", "Second", None)]);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree.get(&id("a")).unwrap().name, "First");
        assert_eq!(tree.anomalies(), [TreeAnomaly::DuplicateId(id("a"))]);
    }

    #[test]
    fn cycles_are_broken_deterministically() {
        // c -> a -> b -> c, plus d hanging off b.
        let tree = FolderTree::build(vec![
            f("b", "B", Some("a")),
            f("c", "C", Some("b")),
            f("a", "A", Some("c")),
            f("d", "D", Some("b")),
        ]);
        assert_eq!(
            tree.anomalies(),
            [TreeAnomaly::Cycle(vec![id("a"), id("b"), id("c")])]
        );
        assert_eq!(names(tree.roots()), ["A"]);
        assert_eq!(names(tree.children(&id("a"))), ["B"]);
        assert_eq!(names(tree.children(&id("b"))), ["C", "D"]);
        assert_eq!(tree.depth_first().len(), 4);
    }

    #[test]
    fn two_independent_cycles_and_a_tail() {
        let tree = FolderTree::build(vec![
            f("x", "X", Some("y")),
            f("y", "Y", Some("x")),
            f("p", "P", Some("q")),
            f("q", "Q", Some("p")),
            f("t", "T", Some("x")),
        ]);
        assert_eq!(tree.anomalies().len(), 2);
        assert_eq!(names(tree.roots()), ["P", "X"]);
        assert_eq!(tree.depth_first().len(), 5);
    }

    #[test]
    fn is_within_detects_descendants() {
        let tree = FolderTree::build(vec![
            f("1", "A", None),
            f("2", "B", Some("1")),
            f("3", "C", Some("2")),
            f("4", "D", None),
        ]);
        assert!(tree.is_within(&id("3"), &id("1")));
        assert!(tree.is_within(&id("1"), &id("1")));
        assert!(!tree.is_within(&id("1"), &id("3")));
        assert!(!tree.is_within(&id("4"), &id("1")));
        assert!(!tree.is_within(&id("nope"), &id("1")));
    }

    #[test]
    fn unknown_ids_are_harmless() {
        let tree = FolderTree::build(vec![]);
        assert!(tree.is_empty());
        assert_eq!(tree.children(&id("x")).count(), 0);
        assert_eq!(tree.path_to(&id("x")), Vec::<&Folder>::new());
        assert!(tree.parent(&id("x")).is_none());
    }

    #[test]
    fn deep_chain_does_not_overflow() {
        let n = 50_000;
        let folders = (0..n)
            .map(|i| {
                let parent = (i > 0).then(|| format!("{:06}", i - 1));
                f(&format!("{i:06}"), &format!("n{i}"), parent.as_deref())
            })
            .collect();
        let tree = FolderTree::build(folders);
        assert_eq!(tree.anomalies(), []);
        assert_eq!(tree.depth_first().len(), n);
        assert_eq!(tree.path_to(&id(&format!("{:06}", n - 1))).len(), n);
    }
}
