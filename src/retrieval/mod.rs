//! Search orchestration: runs the store's lexical search, then decorates
//! hits with directed space traversal. Traversal follows `from -> to` links
//! only ("project:owlry -> coding" never implies "coding -> project:owlry")
//! and is cycle-safe through a visited set.

use std::collections::{HashMap, VecDeque};

use crate::core::{Result, SearchHit, SearchQuery, SpaceLink, SpaceMatch, Uuid};
use crate::storage::Store;

pub fn search(store: &dyn Store, mut query: SearchQuery) -> Result<Vec<SearchHit>> {
    let roots: Vec<Uuid> = query.space_ids.clone().unwrap_or_default();

    // Traversal context: expand the requested spaces to the reachable set
    // before querying, so the store filters over the roots and everything
    // they import, while decoration can still explain the path per hit.
    let traversal = if roots.is_empty() {
        None
    } else {
        let spaces = store.list_spaces()?;
        let id_to_name: HashMap<Uuid, String> =
            spaces.iter().map(|s| (s.id, s.name.clone())).collect();
        let links = store.list_space_links()?;
        let parents = bfs_reachable(&roots, &links, &id_to_name);
        query.space_ids = Some(parents.keys().copied().collect());
        Some((parents, id_to_name, spaces))
    };

    let raw = store.search(&query)?;

    let decorated = match traversal {
        None => raw
            .into_iter()
            .map(|h| SearchHit {
                memory_id: h.memory_id,
                subject: h.subject,
                content: h.content,
                snippet: h.snippet,
                score: h.score,
                state: h.state,
                created_at: h.created_at,
                extraction: h.extraction,
                spaces: h
                    .space_names
                    .into_iter()
                    .map(|name| SpaceMatch { name, via: None })
                    .collect(),
                source_title: h.source_title,
                source_location: h.source_location,
            })
            .collect(),
        Some((parents, id_to_name, spaces)) => {
            raw.into_iter()
                .map(|h| {
                    let mut space_matches = Vec::new();
                    for name in h.space_names {
                        // Names are unique, so the name identifies the space.
                        let Some(sid) = spaces.iter().find(|s| s.name == name).map(|s| s.id) else {
                            continue;
                        };
                        if roots.contains(&sid) {
                            space_matches.push(SpaceMatch { name, via: None });
                        } else if let Some(path) = path_from_root(sid, &parents, &id_to_name) {
                            space_matches.push(SpaceMatch {
                                name,
                                via: Some(path[1..].join(" -> ")),
                            });
                        }
                        // Spaces not reachable from any root stay out of the
                        // filtered view; the memory still exists in them.
                    }
                    SearchHit {
                        memory_id: h.memory_id,
                        subject: h.subject,
                        content: h.content,
                        snippet: h.snippet,
                        score: h.score,
                        state: h.state,
                        created_at: h.created_at,
                        extraction: h.extraction,
                        spaces: space_matches,
                        source_title: h.source_title,
                        source_location: h.source_location,
                    }
                })
                .collect()
        }
    };
    Ok(decorated)
}

/// BFS over directed links from the root spaces. Returns `child -> parent`
/// for every reachable space (roots map to `None`). Neighbors are expanded
/// in lexicographic name order so traversal order is deterministic; cycles
/// terminate through the visited set.
fn bfs_reachable(
    roots: &[Uuid],
    links: &[SpaceLink],
    id_to_name: &HashMap<Uuid, String>,
) -> HashMap<Uuid, Option<Uuid>> {
    let mut adjacency: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for link in links {
        adjacency
            .entry(link.from_space)
            .or_default()
            .push(link.to_space);
    }
    let name_of = |id: &Uuid| id_to_name.get(id).cloned().unwrap_or_default();

    let mut visited: HashMap<Uuid, Option<Uuid>> = HashMap::new();
    let mut queue: VecDeque<Uuid> = VecDeque::new();
    let mut roots_sorted: Vec<Uuid> = roots.to_vec();
    roots_sorted.sort_by_key(name_of);
    for root in roots_sorted {
        match visited.entry(root) {
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(None);
                queue.push_back(root);
            }
            std::collections::hash_map::Entry::Occupied(_) => {}
        }
    }
    while let Some(current) = queue.pop_front() {
        let mut neighbors = adjacency.get(&current).cloned().unwrap_or_default();
        neighbors.sort_by_key(name_of);
        for neighbor in neighbors {
            match visited.entry(neighbor) {
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(Some(current));
                    queue.push_back(neighbor);
                }
                std::collections::hash_map::Entry::Occupied(_) => {}
            }
        }
    }
    visited
}

/// Space names along the path from a root to `target`, root first; `None`
/// when the target is not reachable from any root.
fn path_from_root(
    target: Uuid,
    parents: &HashMap<Uuid, Option<Uuid>>,
    id_to_name: &HashMap<Uuid, String>,
) -> Option<Vec<String>> {
    if !parents.contains_key(&target) {
        return None;
    }
    let mut path = vec![target];
    let mut current = target;
    while let Some(Some(parent)) = parents.get(&current) {
        path.push(*parent);
        current = *parent;
    }
    path.reverse();
    Some(
        path.iter()
            .map(|id| id_to_name.get(id).cloned().unwrap_or_default())
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Ts;

    fn link(from: Uuid, to: Uuid) -> SpaceLink {
        SpaceLink {
            from_space: from,
            to_space: to,
            created_at: Ts(0),
        }
    }

    fn name_map(entries: &[(Uuid, &str)]) -> HashMap<Uuid, String> {
        entries.iter().map(|(id, n)| (*id, n.to_string())).collect()
    }

    #[test]
    fn directed_links_do_not_imply_reverse_visibility() {
        let owlry = Uuid::now_v7();
        let coding = Uuid::now_v7();
        let linux = Uuid::now_v7();
        let names = name_map(&[
            (owlry, "project:owlry"),
            (coding, "coding"),
            (linux, "linux"),
        ]);
        let links = vec![link(owlry, coding), link(coding, linux)];

        let from_owlry = bfs_reachable(&[owlry], &links, &names);
        assert!(from_owlry.contains_key(&coding));
        assert!(from_owlry.contains_key(&linux));

        let from_linux = bfs_reachable(&[linux], &links, &names);
        assert!(from_linux.contains_key(&linux));
        assert!(
            !from_linux.contains_key(&coding),
            "reverse visibility leaked"
        );
        assert!(
            !from_linux.contains_key(&owlry),
            "reverse visibility leaked"
        );
    }

    #[test]
    fn cycles_terminate() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let names = name_map(&[(a, "a"), (b, "b")]);
        let links = vec![link(a, b), link(b, a)];
        let visited = bfs_reachable(&[a], &links, &names);
        assert_eq!(visited.len(), 2);
        assert_eq!(visited[&b], Some(a));
        assert_eq!(visited[&a], None);
    }

    #[test]
    fn neighbors_expand_in_lexicographic_order() {
        let root = Uuid::now_v7();
        let zeta = Uuid::now_v7();
        let alpha = Uuid::now_v7();
        let names = name_map(&[(root, "root"), (zeta, "zeta"), (alpha, "alpha")]);
        // Deliberately inserted in reverse lexicographic order.
        let links = vec![link(root, zeta), link(root, alpha)];
        let visited = bfs_reachable(&[root], &links, &names);
        assert_eq!(visited[&alpha], Some(root));
        assert_eq!(visited[&zeta], Some(root));
    }

    #[test]
    fn path_records_the_traversal() {
        let owlry = Uuid::now_v7();
        let coding = Uuid::now_v7();
        let linux = Uuid::now_v7();
        let names = name_map(&[
            (owlry, "project:owlry"),
            (coding, "coding"),
            (linux, "linux"),
        ]);
        let links = vec![link(owlry, coding), link(coding, linux)];
        let parents = bfs_reachable(&[owlry], &links, &names);

        let to_linux = path_from_root(linux, &parents, &names).unwrap();
        assert_eq!(to_linux, vec!["project:owlry", "coding", "linux"]);

        let to_coding = path_from_root(coding, &parents, &names).unwrap();
        assert_eq!(to_coding, vec!["project:owlry", "coding"]);

        let missing = Uuid::now_v7();
        assert!(path_from_root(missing, &parents, &names).is_none());
    }
}
