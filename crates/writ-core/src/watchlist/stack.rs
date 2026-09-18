//! Infer stack identity and check order from watched PR bases.

use std::collections::BTreeMap;

use super::schema::{StackGroup, Watchlist, repos_match};

pub(crate) fn ordered_identities(list: &Watchlist) -> Vec<(String, u64)> {
    let mut stacked: Vec<(u32, String, u64)> = list
        .prs
        .iter()
        .filter(|entry| entry.stack_id.is_some())
        .map(|entry| {
            (
                entry.stack_position.unwrap_or(0),
                entry.repo.clone(),
                entry.number,
            )
        })
        .collect();
    stacked.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));

    let mut rest: Vec<(String, u64)> = list
        .prs
        .iter()
        .filter(|entry| entry.stack_id.is_none())
        .map(|entry| (entry.repo.clone(), entry.number))
        .collect();
    rest.sort();

    let mut out: Vec<(String, u64)> = stacked
        .into_iter()
        .map(|(_, repo, number)| (repo, number))
        .collect();
    out.extend(rest);
    out
}

pub(crate) fn detect_stacks(list: &mut Watchlist) {
    let parent = compute_parents(list);
    assign_positions(list, &parent);
    rebuild_groups(list);
}

/// For each entry, find its parent index: another PR in the same repo whose
/// head branch is this PR's base. When several PRs share the same head branch
/// we pick the one with the lowest PR number so the inferred parent is
/// deterministic regardless of add order (ambiguous head-branch collisions are
/// otherwise unresolvable). Combined with `seen`-guarded root walking this
/// stays terminating even for cyclic base chains.
pub(crate) fn compute_parents(list: &Watchlist) -> Vec<Option<usize>> {
    let n = list.prs.len();
    let mut parent: Vec<Option<usize>> = vec![None; n];
    for (i, entry) in list.prs.iter().enumerate() {
        let Some(base) = entry.base.as_deref() else {
            continue;
        };
        parent[i] = list
            .prs
            .iter()
            .enumerate()
            .filter(|(j, other)| {
                *j != i && repos_match(&other.repo, &entry.repo) && other.branch == base
            })
            .min_by_key(|(_, other)| other.number)
            .map(|(j, _)| j);
    }
    parent
}

/// Assign `stack_id`/`stack_position` from the parent map. Standalone PRs (no
/// parent and no child) are cleared. Root discovery walks parents with a
/// `seen` guard so a cycle terminates rather than looping forever.
fn assign_positions(list: &mut Watchlist, parent: &[Option<usize>]) {
    let n = list.prs.len();
    let mut has_child = vec![false; n];
    for parent_idx in parent.iter().flatten() {
        has_child[*parent_idx] = true;
    }
    let preserved: Vec<Option<String>> = list
        .prs
        .iter()
        .map(|entry| entry.stack_id.clone().filter(|id| !id.is_empty()))
        .collect();

    for i in 0..n {
        if parent[i].is_none() && !has_child[i] {
            list.prs[i].stack_id = None;
            list.prs[i].stack_position = None;
            continue;
        }
        let (root, pos) = walk_to_root(i, parent);
        let stack_id = preserved[root]
            .clone()
            .unwrap_or_else(|| format!("{}#{}", list.prs[root].repo, list.prs[root].number));
        list.prs[i].stack_id = Some(stack_id);
        list.prs[i].stack_position = Some(pos);
    }
}

/// Follow parent links from `start` to the stack root, returning
/// `(root_index, depth)`. Cycle-safe: a repeated node ends the walk.
fn walk_to_root(start: usize, parent: &[Option<usize>]) -> (usize, u32) {
    let mut root = start;
    let mut pos = 0_u32;
    let mut seen = vec![false; parent.len()];
    while let Some(next) = parent[root] {
        if seen[root] {
            break;
        }
        seen[root] = true;
        root = next;
        pos = pos.saturating_add(1);
    }
    (root, pos)
}

pub(crate) fn rebuild_groups(list: &mut Watchlist) {
    let mut groups: BTreeMap<String, StackGroup> = BTreeMap::new();
    for entry in &list.prs {
        let Some(stack_id) = entry.stack_id.as_ref() else {
            continue;
        };
        let group = groups
            .entry(stack_id.clone())
            .or_insert_with(|| StackGroup {
                repo: entry.repo.clone(),
                numbers: Vec::new(),
            });
        if !group.numbers.contains(&entry.number) {
            group.numbers.push(entry.number);
        }
    }
    for group in groups.values_mut() {
        group.numbers.sort_by_key(|number| {
            list.prs
                .iter()
                .find(|entry| entry.number == *number && repos_match(&entry.repo, &group.repo))
                .and_then(|entry| entry.stack_position)
                .unwrap_or(0)
        });
    }
    list.groups = groups;
}
