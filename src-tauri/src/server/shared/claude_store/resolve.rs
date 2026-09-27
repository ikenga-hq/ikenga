//! Forward-dependency resolver core (Ọba Phase 4, ADR-015 §3b / WP-13).
//!
//! Given a primitive's `requires` (the compiled forward-dependency edges, WP-11
//! field / WP-12 lift) and a snapshot of what's already present, compute the
//! **install closure** — the set of missing dependencies in **topological
//! order** so that installing the plan front-to-back always installs a dep
//! before anything that requires it.
//!
//! Design (WP-13 decisions):
//!   * **Pure single-pass core.** `resolve_requires_core` is a pure function over
//!     injected data — a `requires` graph (name → that primitive's requires) and
//!     a `satisfied` set. No disk, no network, trivially unit-testable. WP-14
//!     assembles the graph (the catalog's embedded-manifest `requires` lives on
//!     the TS side today; Rust has no native catalog reader) and the satisfied
//!     set, then drives the install loop over the plan.
//!   * **Satisfaction = store registry ∪ external masters.** A dep already in the
//!     Ọba vault (store registry) OR a CLI-installed external master
//!     (`scan_external_masters`) counts as satisfied and prunes its subtree.
//!     Pkg-bundled `EngineAssets` skills are **deliberately NOT** a satisfaction
//!     source — ADR-015 decision 4 hard-retires bundling (WP-17), so the resolver
//!     never couples to that doomed path.
//!   * **Cycle-safe.** The skill star is acyclic, but a pkg→skill `requires` DAG
//!     isn't guaranteed — a back-edge returns `ResolveError::Cycle(path)`, never
//!     a hang.
//!
//! This module is not yet wired into a Tauri command — that is WP-14 (wire the
//! resolver into `oba_install_*`/enable + transactional multi-install). The
//! `#![allow(dead_code)]` drops when WP-14 consumes this API.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::pkg::manifest::RequiresEntry;

/// A primitive's identity in the dependency graph: `(kind, name)`.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct PrimitiveRef {
    pub kind: String,
    pub name: String,
}

impl PrimitiveRef {
    fn of(e: &RequiresEntry) -> Self {
        Self {
            kind: e.kind.clone(),
            name: e.name.clone(),
        }
    }
}

/// Maps a primitive to ITS OWN `requires` (the child edges). Assembled by WP-14
/// from the catalog (uninstalled deps) + installed store entries.
pub type RequiresGraph = HashMap<PrimitiveRef, Vec<RequiresEntry>>;

/// The computed install closure.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct InstallPlan {
    /// Missing deps to install, **topologically ordered** — installing
    /// front-to-back installs a dependency before any primitive that requires it.
    /// Each item is the `requires` edge that introduced the dep (carries the
    /// declared `source`/`ref` fetch hints for the installer).
    pub ordered: Vec<RequiresEntry>,
    /// Deps already present (store registry or external master). Surfaced for the
    /// consent UX (WP-15); never reinstalled.
    pub already_satisfied: Vec<PrimitiveRef>,
}

/// Resolver failure. The skill star is acyclic; this guards a pkg→skill DAG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// A `requires` cycle — the path that closes it (the repeated ref is last).
    Cycle(Vec<PrimitiveRef>),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::Cycle(path) => {
                let chain = path
                    .iter()
                    .map(|r| format!("{}:{}", r.kind, r.name))
                    .collect::<Vec<_>>()
                    .join(" → ");
                write!(f, "requires cycle detected: {chain}")
            }
        }
    }
}

/// Internal DFS state.
struct Resolver<'a> {
    graph: &'a RequiresGraph,
    satisfied: &'a HashSet<PrimitiveRef>,
    ordered: Vec<RequiresEntry>,
    done: HashSet<PrimitiveRef>,    // pushed to `ordered` already (dedupe diamonds)
    sat_seen: Vec<PrimitiveRef>,    // satisfied deps encountered (consent UX)
    sat_set: HashSet<PrimitiveRef>, // dedupe `sat_seen`
    onstack: HashSet<PrimitiveRef>, // ancestors on the current DFS path (cycle)
    stack: Vec<PrimitiveRef>,       // the path, for the cycle error
}

impl<'a> Resolver<'a> {
    fn visit(&mut self, edge: &RequiresEntry) -> Result<(), ResolveError> {
        let r = PrimitiveRef::of(edge);

        // Already present → prune the whole subtree (its own deps are its problem,
        // already resolved when IT was installed).
        if self.satisfied.contains(&r) {
            if self.sat_set.insert(r.clone()) {
                self.sat_seen.push(r);
            }
            return Ok(());
        }
        // Already planned via another edge (diamond) → skip; first edge wins.
        if self.done.contains(&r) {
            return Ok(());
        }
        // Back-edge to an ancestor → cycle.
        if self.onstack.contains(&r) {
            let mut path = self.stack.clone();
            path.push(r);
            return Err(ResolveError::Cycle(path));
        }

        self.onstack.insert(r.clone());
        self.stack.push(r.clone());
        if let Some(children) = self.graph.get(&r) {
            for child in children {
                self.visit(child)?;
            }
        }
        self.stack.pop();
        self.onstack.remove(&r);
        self.done.insert(r);
        // Post-order: children are already in `ordered`, so this dep lands after
        // its own deps → topological (deepest first).
        self.ordered.push(edge.clone());
        Ok(())
    }
}

/// Compute the install closure for `target`'s `requires`. Pure: no disk/network.
///
/// * `graph` — each known primitive → its own `requires` (child edges).
/// * `satisfied` — primitives already present (store ∪ external); prune on hit.
///
/// A dep absent from `graph` is treated as a leaf (no known children) and still
/// planned — the installer fetches it and its manifest reveals any further
/// `requires` at install time (the WP-14 install loop re-resolves).
pub fn resolve_requires_core(
    target: &[RequiresEntry],
    graph: &RequiresGraph,
    satisfied: &HashSet<PrimitiveRef>,
) -> Result<InstallPlan, ResolveError> {
    let mut r = Resolver {
        graph,
        satisfied,
        ordered: Vec::new(),
        done: HashSet::new(),
        sat_seen: Vec::new(),
        sat_set: HashSet::new(),
        onstack: HashSet::new(),
        stack: Vec::new(),
    };
    for edge in target {
        r.visit(edge)?;
    }
    Ok(InstallPlan {
        ordered: r.ordered,
        already_satisfied: r.sat_seen,
    })
}

/// Failure of the transactional closure install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClosureError {
    /// The resolver itself failed (e.g. a `requires` cycle).
    Resolve(ResolveError),
    /// A dependency install failed. The already-installed deps were rolled back
    /// (in reverse order); `rolled_back` is how many were undone.
    Install {
        item: RequiresEntry,
        error: String,
        rolled_back: usize,
    },
}

impl std::fmt::Display for ClosureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClosureError::Resolve(e) => write!(f, "{e}"),
            ClosureError::Install {
                item,
                error,
                rolled_back,
            } => write!(
                f,
                "failed installing dependency {}:{} ({error}); rolled back {rolled_back} prior install(s)",
                item.kind, item.name
            ),
        }
    }
}

/// Drive the **transactional** closure install (ADR-015 §3b / WP-14, the
/// install-then-reresolve loop). Pure over injected effects so it is fully
/// unit-testable without network/disk:
///
/// * `install_one(item)` — fetch + adopt one dependency; **returns that
///   dependency's OWN `requires`** (revealed by its freshly-fetched manifest), so
///   transitive deps surface across iterations without the discovery catalog
///   needing to carry `requires`.
/// * `rollback_one(item)` — undo a previously-installed dep (vault-delete +
///   registry record) when a later install fails.
///
/// `satisfied` is the **pre-existing** present set (store ∪ external) and is read
/// only — never grown with freshly-fetched nodes, since marking a just-fetched
/// node satisfied would prune its *just-revealed* children and they'd never
/// install. Instead each iteration re-resolves the full plan over the growing
/// `graph`, then fetches the first plan item **not yet fetched this run** (the
/// topological frontier); revealing its `requires` grows the graph so transitive
/// deps appear in the next resolve. On any install failure every prior install is
/// rolled back in reverse order and `ClosureError::Install` is returned.
/// Converges (one new fetch per iteration, bounded by the closure); a `requires`
/// cycle is caught by the inner resolver → `ClosureError::Resolve`.
///
/// Returns the fetched dependency closure in **topological order** (deepest dep
/// first) — the order the caller then enables them in. Installs **only the
/// closure**; the caller installs the target (the dependent) afterwards.
pub fn resolve_install_loop_core<I, R>(
    target: &[RequiresEntry],
    satisfied: &HashSet<PrimitiveRef>,
    graph: &mut RequiresGraph,
    mut install_one: I,
    mut rollback_one: R,
) -> Result<Vec<RequiresEntry>, ClosureError>
where
    I: FnMut(&RequiresEntry) -> Result<Vec<RequiresEntry>, String>,
    R: FnMut(&RequiresEntry),
{
    let mut installed: Vec<RequiresEntry> = Vec::new();
    let mut fetched: HashSet<PrimitiveRef> = HashSet::new();
    // Roll back every prior install (reverse order) and surface `err`.
    macro_rules! abort {
        ($err:expr) => {{
            for done in installed.iter().rev() {
                rollback_one(done);
            }
            return Err($err);
        }};
    }
    loop {
        let plan = match resolve_requires_core(target, graph, satisfied) {
            Ok(p) => p,
            Err(e) => abort!(ClosureError::Resolve(e)),
        };
        // The first topological item we haven't fetched yet (its known deps are
        // already earlier in `ordered`, hence already fetched).
        let next = plan
            .ordered
            .into_iter()
            .find(|i| !fetched.contains(&PrimitiveRef::of(i)));
        let Some(item) = next else {
            break; // every planned item fetched → closure complete
        };
        match install_one(&item) {
            Ok(revealed) => {
                let r = PrimitiveRef::of(&item);
                fetched.insert(r.clone());
                graph.insert(r, revealed); // newly-revealed transitive edges
                installed.push(item);
            }
            Err(error) => {
                let rolled_back = installed.len();
                abort!(ClosureError::Install {
                    item,
                    error,
                    rolled_back,
                })
            }
        }
    }
    // Final resolve over the now-complete graph → the closure in topological
    // (enable) order. Everything is fetched + acyclic (an earlier iteration would
    // have caught a cycle), so this never installs and won't error — but stay
    // transactional if it somehow does.
    match resolve_requires_core(target, graph, satisfied) {
        Ok(plan) => Ok(plan.ordered),
        Err(e) => abort!(ClosureError::Resolve(e)),
    }
}

/// Build the `satisfied` set from disk: every installed store-registry entry +
/// every external master in the live farm. Tempdir-testable (`store` +
/// `scope_roots` are parameters), mirroring the `*_core` pattern. Bundled
/// `EngineAssets` skills are intentionally excluded (see module docs).
pub fn collect_satisfied(store: &Path, scope_roots: &[PathBuf]) -> HashSet<PrimitiveRef> {
    let mut set = HashSet::new();
    // Installed in the Ọba vault.
    for e in &super::registry::load(store).entries {
        set.insert(PrimitiveRef {
            kind: e.kind.clone(),
            name: e.name.clone(),
        });
    }
    // CLI-installed externals kept in place (the `groundwork` shape).
    for (kind, name, _canon) in super::scan_external_masters(store, scope_roots) {
        set.insert(PrimitiveRef {
            kind: kind.as_str().to_string(),
            name,
        });
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkg::manifest::RequireSource;

    fn req(name: &str) -> RequiresEntry {
        RequiresEntry {
            kind: "skill".into(),
            name: name.into(),
            source: None,
            r#ref: None,
        }
    }
    fn req_src(name: &str, source: RequireSource) -> RequiresEntry {
        RequiresEntry {
            kind: "skill".into(),
            name: name.into(),
            source: Some(source),
            r#ref: None,
        }
    }
    /// A `requires` entry with an arbitrary kind (used for bundle edges).
    fn req_kind(kind: &str, name: &str) -> RequiresEntry {
        RequiresEntry {
            kind: kind.into(),
            name: name.into(),
            source: None,
            r#ref: None,
        }
    }
    fn pref(name: &str) -> PrimitiveRef {
        PrimitiveRef {
            kind: "skill".into(),
            name: name.into(),
        }
    }
    fn pref_kind(kind: &str, name: &str) -> PrimitiveRef {
        PrimitiveRef {
            kind: kind.into(),
            name: name.into(),
        }
    }
    fn graph(edges: &[(&str, &[&str])]) -> RequiresGraph {
        edges
            .iter()
            .map(|(node, deps)| {
                (pref(node), deps.iter().map(|d| req(d)).collect::<Vec<_>>())
            })
            .collect()
    }
    fn names(plan: &InstallPlan) -> Vec<String> {
        plan.ordered.iter().map(|e| e.name.clone()).collect()
    }
    fn names_of(items: &[RequiresEntry]) -> Vec<String> {
        items.iter().map(|e| e.name.clone()).collect()
    }

    #[test]
    fn all_satisfied_yields_empty_plan() {
        let g = graph(&[("a", &["skill-core"])]);
        let satisfied: HashSet<_> = [pref("skill-core")].into_iter().collect();
        let plan = resolve_requires_core(&[req("skill-core")], &g, &satisfied).unwrap();
        assert!(plan.ordered.is_empty());
        assert_eq!(plan.already_satisfied, vec![pref("skill-core")]);
    }

    #[test]
    fn single_missing_leaf_is_planned() {
        // the real skill-core case: target requires skill-core (which depends on
        // nothing), nothing installed → install skill-core.
        let g = graph(&[("skill-core", &[])]);
        let plan = resolve_requires_core(&[req("skill-core")], &g, &HashSet::new()).unwrap();
        assert_eq!(names(&plan), vec!["skill-core"]);
        assert!(plan.already_satisfied.is_empty());
    }

    #[test]
    fn transitive_closure_is_topologically_ordered() {
        // target → b → c (c deepest). Install order: c, b.
        let g = graph(&[("b", &["c"]), ("c", &[])]);
        let plan = resolve_requires_core(&[req("b")], &g, &HashSet::new()).unwrap();
        assert_eq!(names(&plan), vec!["c", "b"]);
    }

    #[test]
    fn satisfied_dep_prunes_its_subtree() {
        // b is external-satisfied → b skipped AND its child c is never visited.
        let g = graph(&[("b", &["c"]), ("c", &[])]);
        let satisfied: HashSet<_> = [pref("b")].into_iter().collect();
        let plan = resolve_requires_core(&[req("b")], &g, &satisfied).unwrap();
        assert!(plan.ordered.is_empty());
        assert_eq!(plan.already_satisfied, vec![pref("b")]);
    }

    #[test]
    fn diamond_dedupes_shared_dep() {
        // target → b, c ; b → d ; c → d. d appears once, before b and c.
        let g = graph(&[("b", &["d"]), ("c", &["d"]), ("d", &[])]);
        let plan =
            resolve_requires_core(&[req("b"), req("c")], &g, &HashSet::new()).unwrap();
        assert_eq!(names(&plan), vec!["d", "b", "c"]);
        // d only once
        assert_eq!(names(&plan).iter().filter(|n| *n == "d").count(), 1);
    }

    #[test]
    fn cycle_returns_error_not_hang() {
        // a → b → a
        let g = graph(&[("a", &["b"]), ("b", &["a"])]);
        let err = resolve_requires_core(&[req("a")], &g, &HashSet::new()).unwrap_err();
        match err {
            ResolveError::Cycle(path) => {
                assert_eq!(path.first(), Some(&pref("a")));
                assert_eq!(path.last(), Some(&pref("a"))); // closes the loop
            }
        }
    }

    #[test]
    fn self_cycle_is_detected() {
        let g = graph(&[("a", &["a"])]);
        assert!(matches!(
            resolve_requires_core(&[req("a")], &g, &HashSet::new()),
            Err(ResolveError::Cycle(_))
        ));
    }

    #[test]
    fn plan_items_carry_source_ref_hint() {
        // the edge's declared source is preserved on the plan item (the fetcher
        // needs it).
        let g = graph(&[("skill-core", &[])]);
        let plan =
            resolve_requires_core(&[req_src("skill-core", RequireSource::Npx)], &g, &HashSet::new())
                .unwrap();
        assert_eq!(plan.ordered[0].source, Some(RequireSource::Npx));
    }

    #[test]
    fn unknown_dep_is_planned_as_leaf() {
        // a dep absent from the graph is still installed (leaf); WP-14 re-resolves
        // its own requires at install time.
        let plan =
            resolve_requires_core(&[req("not-in-graph")], &HashMap::new(), &HashSet::new())
                .unwrap();
        assert_eq!(names(&plan), vec!["not-in-graph"]);
    }

    // ── WP-20 — bundle resolution via the pure resolver ─────────────────────

    #[test]
    fn bundle_requires_edge_is_planned_when_not_satisfied() {
        // A pkg declares requires:[{kind:"bundle", name:"my-bundle"}].
        // The bundle is absent → it should appear in the install plan.
        let g: RequiresGraph = HashMap::new(); // no known children for the bundle
        let plan =
            resolve_requires_core(&[req_kind("bundle", "my-bundle")], &g, &HashSet::new())
                .unwrap();
        assert_eq!(plan.ordered.len(), 1);
        assert_eq!(plan.ordered[0].kind, "bundle");
        assert_eq!(plan.ordered[0].name, "my-bundle");
        assert!(plan.already_satisfied.is_empty());
    }

    #[test]
    fn bundle_requires_edge_is_satisfied_when_registry_record_present() {
        // When the bundle has a registry record (collected_satisfied includes it),
        // it must be deduped — not re-installed.
        let satisfied: HashSet<_> =
            [pref_kind("bundle", "my-bundle")].into_iter().collect();
        let plan = resolve_requires_core(
            &[req_kind("bundle", "my-bundle")],
            &HashMap::new(),
            &satisfied,
        )
        .unwrap();
        assert!(plan.ordered.is_empty(), "already-present bundle not re-planned");
        assert_eq!(plan.already_satisfied, vec![pref_kind("bundle", "my-bundle")]);
    }

    #[test]
    fn bundle_and_skill_requires_together_dedup_correctly() {
        // A pkg requires both a bundle and a standalone skill.
        // Only the bundle is satisfied; the skill must still be planned.
        let satisfied: HashSet<_> =
            [pref_kind("bundle", "b1")].into_iter().collect();
        let plan = resolve_requires_core(
            &[req_kind("bundle", "b1"), req("skill-a")],
            &HashMap::new(),
            &satisfied,
        )
        .unwrap();
        assert_eq!(plan.ordered.len(), 1);
        assert_eq!(plan.ordered[0].name, "skill-a");
        assert_eq!(plan.already_satisfied, vec![pref_kind("bundle", "b1")]);
    }

    #[test]
    fn collect_satisfied_includes_installed_registry_entries() {
        // Seed a registry with one managed entry; collect_satisfied must surface it.
        let tmp = std::env::temp_dir().join(format!(
            "wp13_resolve_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let mut rf = super::super::registry::load(&tmp); // empty/default
        rf.entries.push(super::super::ClaudeStoreEntry {
            kind: "skill".into(),
            name: "groundwork".into(),
            store_path: tmp.join("skills/groundwork").to_string_lossy().into(),
            description: None,
            modified_ms: 0,
            enabled_in: vec![],
            requires: vec![],
            members: vec![],
            provenance: super::super::RegistryProvenance::local(
                tmp.join("skills/groundwork").to_string_lossy().into(),
            ),
        });
        super::super::registry::save(&tmp, &rf).unwrap();

        let satisfied = collect_satisfied(&tmp, &[]);
        assert!(satisfied.contains(&pref("groundwork")));
        // (external-master union is covered by scan_external_masters' own tests.)
        std::fs::remove_dir_all(&tmp).ok();
    }

    // ── WP-14 — transactional resolve-install loop ──────────────────────────

    /// Drive `resolve_install_loop_core` with canned per-dep reveals + an optional
    /// failure, returning (result, fetch-order, rollback-order).
    fn run_loop(
        target: &[RequiresEntry],
        satisfied: HashSet<PrimitiveRef>,
        reveals: &HashMap<&'static str, Vec<RequiresEntry>>,
        fail_on: Option<&str>,
    ) -> (
        Result<Vec<RequiresEntry>, ClosureError>,
        Vec<String>,
        Vec<String>,
    ) {
        let mut graph = RequiresGraph::new();
        let mut install_log: Vec<String> = Vec::new();
        let mut rollback_log: Vec<String> = Vec::new();
        let res = {
            let il = &mut install_log;
            let rl = &mut rollback_log;
            resolve_install_loop_core(
                target,
                &satisfied,
                &mut graph,
                |item| {
                    if Some(item.name.as_str()) == fail_on {
                        return Err(format!("boom: {}", item.name));
                    }
                    il.push(item.name.clone());
                    Ok(reveals.get(item.name.as_str()).cloned().unwrap_or_default())
                },
                |item| rl.push(item.name.clone()),
            )
        };
        (res, install_log, rollback_log)
    }

    #[test]
    fn loop_installs_single_leaf_dep() {
        // the real case: skill-pa requires skill-core (which reveals no deps).
        let reveals = HashMap::from([("skill-core", vec![])]);
        let (res, fetched, rolled) = run_loop(&[req("skill-core")], HashSet::new(), &reveals, None);
        assert_eq!(names_of(&res.unwrap()), vec!["skill-core"]);
        assert_eq!(fetched, vec!["skill-core"]);
        assert!(rolled.is_empty());
    }

    #[test]
    fn loop_resolves_transitive_in_topo_order() {
        // a reveals it requires b; the returned closure is topo-ordered [b, a]
        // even though a was fetched first (reveal order).
        let reveals = HashMap::from([("a", vec![req("b")]), ("b", vec![])]);
        let (res, fetched, rolled) = run_loop(&[req("a")], HashSet::new(), &reveals, None);
        assert_eq!(names_of(&res.unwrap()), vec!["b", "a"]); // enable order
        assert_eq!(fetched, vec!["a", "b"]); // fetch order
        assert!(rolled.is_empty());
    }

    #[test]
    fn loop_skips_already_satisfied_without_installing() {
        let reveals = HashMap::new();
        let satisfied: HashSet<_> = [pref("skill-core")].into_iter().collect();
        let (res, fetched, rolled) = run_loop(&[req("skill-core")], satisfied, &reveals, None);
        assert!(res.unwrap().is_empty());
        assert!(fetched.is_empty());
        assert!(rolled.is_empty());
    }

    #[test]
    fn loop_rolls_back_in_reverse_on_install_failure() {
        // target needs a + b; a installs, b fails → a is rolled back.
        let reveals = HashMap::from([("a", vec![]), ("b", vec![])]);
        let (res, fetched, rolled) =
            run_loop(&[req("a"), req("b")], HashSet::new(), &reveals, Some("b"));
        match res {
            Err(ClosureError::Install {
                item,
                rolled_back,
                ..
            }) => {
                assert_eq!(item.name, "b");
                assert_eq!(rolled_back, 1);
            }
            other => panic!("expected Install error, got {other:?}"),
        }
        assert_eq!(fetched, vec!["a"]);
        assert_eq!(rolled, vec!["a"]); // reverse-order rollback of the one prior install
    }

    #[test]
    fn loop_rolls_back_all_on_revealed_cycle() {
        // a → b → a only becomes visible after both are fetched; the next resolve
        // catches the cycle and the loop rolls back BOTH installs.
        let reveals = HashMap::from([("a", vec![req("b")]), ("b", vec![req("a")])]);
        let (res, fetched, rolled) = run_loop(&[req("a")], HashSet::new(), &reveals, None);
        assert!(matches!(res, Err(ClosureError::Resolve(ResolveError::Cycle(_)))));
        assert_eq!(fetched, vec!["a", "b"]);
        assert_eq!(rolled, vec!["b", "a"]); // reverse order
    }
}
