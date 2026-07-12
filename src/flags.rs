//! Level-scoped flag/condition state (#27) — the generalized substrate that
//! objective gating is built on.
//!
//! A [`Flags`] set holds the raised condition ids for the current level.
//! **Sources** raise flags and **consumers** read them; decoupling the two is
//! what lets one gate mechanism *multiply* (pillar 3) instead of a bespoke
//! enemy→door wire:
//!
//! - **Source — zone-clear** ([`clear_zone`], the "arena trap"): once every
//!   objective enemy in the active zone is resolved, raise the zone's
//!   `clear_flag`. Freeform zones (`clear_flag == 0`) raise nothing.
//! - **Consumer — zone crossings** ([`crate::transition::transition_spaces`]): a
//!   connection's `gate` is a required-flag id; the crossing opens once the flag
//!   is raised.
//! - **Future sources** (switches, keys/items, score) and the **level-exit as a
//!   location** ride the same resource with no change to consumers.
//!
//! Per #26's OQ-3 lock, an objective enemy counts as cleared on **either** exit
//! (destroy *or* liberate) — gating uses neutralize; liberate is the rewarded
//! path (#32), not the price of progression.

use alloc::collections::BTreeSet;
use alloc::string::String;

use bevy_ecs::prelude::*;

use crate::capture::Capture;
use crate::transition::Zone;
use crate::{Enemy, NeighbourInstance};

/// **Gate-objective** bit in a scene instance's `flags`: set on an enemy that
/// **counts toward its zone's clear flag** (#27, tier 1) — clearing all of a
/// zone's gate objectives opens an *adjacent gate*. Freeform / optional enemies
/// omit it and gate nothing. Authored in the level RON (`flags: 1`) and read by
/// `specialize_scene`, which tags the entity with [`Objective`].
pub const OBJECTIVE: u32 = 0x1;

/// **Level-objective** bit in a scene instance's `flags` (#27, tier 2): a
/// **freeform** enemy that contributes to the **level-wide** objective — the one
/// that opens the *level exit* ([`LEVEL_EXIT`]) — rather than any per-zone gate.
/// Authored as `flags: 2` (or `3` for an enemy that is both). Read by
/// `specialize_scene`, which tags the entity with [`LevelObjectiveTag`].
pub const LEVEL_OBJECTIVE: u32 = 0x2;

/// Reserved engine flag id raised when the **level objective** is met — the
/// consumer for a future level-exit-as-location (#27). Authored *gate* flags stay
/// small (1, 2, …); the engine reserves the high range so the two never collide.
pub const LEVEL_EXIT: u32 = 0x1000_0000;

/// The level's raised-flag set — **persists across zone crossings** (clearing an
/// arena stays cleared; `swap_zone` deliberately doesn't touch it). `0` is never
/// a real flag id (it means "always open" on a connection), so it's never stored.
#[derive(Resource, Default)]
pub struct Flags(BTreeSet<u32>);

impl Flags {
    /// Raise a flag (idempotent). `0` is a no-op — it's the always-open sentinel.
    pub fn raise(&mut self, id: u32) {
        if id != 0 {
            self.0.insert(id);
        }
    }

    /// Is this flag raised? `0` ("always open") is treated as raised so a
    /// consumer can test a connection's `gate` uniformly.
    pub fn is_raised(&self, id: u32) -> bool {
        id == 0 || self.0.contains(&id)
    }

    /// Drop every flag — a fresh run (START reset).
    pub fn clear(&mut self) {
        self.0.clear();
    }
}

/// Marker on an enemy that counts toward its zone's clear flag (the [`OBJECTIVE`]
/// bit). Freeform enemies lack it, so [`clear_zone`] ignores them.
#[derive(Component)]
pub struct Objective;

/// Zone-clear source (#27, the "arena trap"): once every objective enemy in the
/// active zone is resolved, raise the zone's `clear_flag` — opening any
/// connection gated on it. A `clear_flag == 0` zone is freeform and raises
/// nothing. Idempotent ([`Flags::raise`] only ever adds), so a re-entered /
/// re-spawned arena stays open.
///
/// Resident-neighbour enemies are now gameplay entities too (#27 follow-up), so
/// the query filters to the **active** zone with `Without<NeighbourInstance>` — a
/// neighbour's objectives belong to *its* zone's clear flag, not this one's.
pub fn clear_zone(
    zone: Res<Zone>,
    mut flags: ResMut<Flags>,
    // Active-zone objectives only — resident-neighbour enemies now carry `Enemy`
    // + tags too, but they belong to a different zone's clear flag (#27 follow-up).
    objectives: Query<&Capture, (With<Enemy>, With<Objective>, Without<NeighbourInstance>)>,
) {
    if zone.clear_flag == 0 || flags.is_raised(zone.clear_flag) {
        return;
    }
    // Raise only when there is at least one objective enemy and all are resolved
    // (an arena with no objectives never auto-clears — that would open its gate
    // for free).
    let mut any = false;
    for cap in &objectives {
        any = true;
        if !cap.is_resolved() {
            return;
        }
    }
    if any {
        flags.raise(zone.clear_flag);
    }
}

/// Marker on a **level-objective** enemy (the [`LEVEL_OBJECTIVE`] bit) — a
/// freeform capture that contributes to the level exit, not a zone gate.
#[derive(Component)]
pub struct LevelObjectiveTag;

/// Level-wide objective progress (#27, tier 2): the set of **zone stems** whose
/// level-objective enemies are all resolved. Persists for the run; a stem is
/// re-inserted idempotently, so re-entering (and re-clearing a respawned) zone
/// never over-counts.
///
/// `needed` is the total number of level-objective zones — a **hardcoded
/// stand-in** for the deferred runtime level-header (#27); it's set at boot from
/// a game constant keyed to the (hardcoded) boot level. Finer *per-enemy*
/// counting + the live liberate/destroy tally wait on persistent per-zone
/// gameplay state (the dormant-neighbour follow-up).
#[derive(Resource, Default)]
pub struct LevelProgress {
    done: BTreeSet<String>,
    pub needed: usize,
}

impl LevelProgress {
    /// How many level-objective zones are complete.
    pub fn done(&self) -> usize {
        self.done.len()
    }

    /// Is the whole level objective met? (`needed == 0` ⇒ no level objective, so
    /// never "met" — the exit stays closed rather than opening for free.)
    pub fn complete(&self) -> bool {
        self.needed > 0 && self.done.len() >= self.needed
    }

    /// Fresh run (START reset): drop progress but keep `needed`.
    pub fn reset(&mut self) {
        self.done.clear();
    }
}

/// Level-objective source (#27, tier 2): when every level-objective enemy in the
/// **active** zone is resolved, mark that zone complete; once all level-objective
/// zones are complete, raise [`LEVEL_EXIT`]. Mirrors [`clear_zone`] but rolls up
/// to the level exit instead of a zone gate — per #26's OQ-3 lock, **either
/// exit** (destroy or liberate) counts; the liberate/destroy split feeds ranking
/// (#32), not whether the exit opens.
///
/// Tracked per zone-completion (not per enemy) so it stays respawn-safe without
/// persistent per-enemy state; the stem comes from [`Zone::stem`].
pub fn tally_level_objective(
    zone: Res<Zone>,
    mut progress: ResMut<LevelProgress>,
    mut flags: ResMut<Flags>,
    // Active-zone level objectives only (keyed to `zone.stem`); a neighbour's
    // level enemies are counted when *that* zone is active (#27 follow-up).
    objectives: Query<
        &Capture,
        (
            With<Enemy>,
            With<LevelObjectiveTag>,
            Without<NeighbourInstance>,
        ),
    >,
) {
    if !progress.done.contains(&zone.stem) {
        // Complete this zone only when it has ≥1 level objective and all resolve.
        let mut any = false;
        let mut all_resolved = true;
        for cap in &objectives {
            any = true;
            if !cap.is_resolved() {
                all_resolved = false;
                break;
            }
        }
        if any && all_resolved {
            progress.done.insert(zone.stem.clone());
        }
    }
    if progress.complete() {
        flags.raise(LEVEL_EXIT);
    }
}
