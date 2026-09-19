//! `kts_schema` — *Kill the Serpent*'s authored vocabulary.
//!
//! The one definition of what a level may say: the [`Role`] set, each role's
//! [`kinds`](Role::kinds) (its sub-archetypes), the instance-[`flag_bits`] an
//! author may set, and the reserved runtime [`flag_ids`]. Shared by the game
//! (`kts`), the host baker (`scene2bin`, which re-exports it as
//! `scene2bin::schema`) and — transitively, through that re-export — the desktop
//! editor, so all three agree by construction instead of by three hand-kept
//! string tables.
//!
//! It is **game-owned**, hence `kts_schema` and not `bevy_nds_*`:
//! `bevy_nds_scene` stays game-agnostic and keeps an instance's `role` an opaque
//! string and its `kind` an opaque byte. The library never depends on this
//! crate; only the game and the baker do.
//!
//! # The two `u32` namespaces
//!
//! They look alike and are *not* the same space:
//!
//! - [`flag_bits`] — **instance-flag bits**, authored on an instance
//!   (`flags: 1`). A bitmask: `OBJECTIVE | LEVEL_OBJECTIVE`. Undefined bits and
//!   bits a role can't carry ([`Role::allowed_flags`]) are bake Errors.
//! - [`flag_ids`] — **flag ids**, raised in the runtime `Flags` *set* (a zone's
//!   `clear_flag`, a `Gate`'s required `flag`). These are identifiers, not a
//!   mask. The engine reserves the high range ([`flag_ids::RESERVED_MIN`]) for
//!   its own ids such as [`flag_ids::LEVEL_EXIT`]; an authored `clear_flag` /
//!   `Gate.flag` in that range is a bake Error.
//!
//! # Wire stability
//!
//! A role's [`kinds`](Role::kinds) list is **positional**: the index *is* the
//! `u8` baked into `.scene` v4. Extend a list only at the end — the same rule
//! that governs `scene2bin::Camera`'s variant order. [`EnemyKind`]'s and
//! [`BlockKind`]'s discriminants are that index, so each must stay in lockstep
//! with its row (a test asserts it).
//!
//! # One sub-archetype channel
//!
//! [`Role::kinds`] is the **only** way to add a sub-archetype to a role — a new
//! block shape, a new enemy family, a new prop class all become kind rows.
//! Nothing may add another instance-flag bit: [`flag_bits::ALL`] is frozen at
//! `OBJECTIVE | LEVEL_OBJECTIVE`, which is what lets
//! [`flag_bits::unknown_bits`] and [`Role::allowed_flags`] stay meaningful.
//!
//! Block kinds are the worked example: static blocking (#12) needed three
//! shapes (`box` / `ramp` / `round`) and took a [`Role::kinds`] row rather than
//! two flag bits or a `.scene` VERSION bump — no wire change at all, since v4
//! already carries a kind byte per instance.
//!
//! # What is *not* decided here
//!
//! The kind → `VulnerabilityShape` pairing is **open on #29** and lives in the
//! game (`src/capture.rs`'s `VulnerabilityShape::for_kind`), not in this crate.
//! Today every kind is circle-vulnerable. The one load-bearing wire fact is that
//! [`EnemyKind::Basic`]`.wire() == 0`: an instance that authors no `kind` bakes
//! to `0` and specialises to exactly today's circle-vulnerable enemy, so v4 is
//! behaviour-neutral for every existing level.

#![cfg_attr(not(test), no_std)]

/// What an authored instance *is* — the closed vocabulary of roles a level may
/// use. Extending it is a design decision (#27), not a convenience: a new
/// sub-archetype of an existing role belongs in [`Role::kinds`] instead.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    /// The player. Exactly one per **level**, authored only in the level's
    /// `entry` zone (#27 / #54) — it is the single persistent entity that
    /// carries across zone crossings.
    Avatar,
    /// A capturable machine (the capture model, #26).
    Enemy,
    /// A static obstacle the avatar collides with.
    Landmark,
    /// Gray-box level geometry (#44) the avatar collides with: its `kind`
    /// picks the blocking shape (`box` / `ramp` / `round`), and its collider is
    /// derived from the mesh AABB × the authored scale (#12).
    Block,
    /// Set dressing. Scenery by design.
    Prop,
}

/// Whether a role has runtime behaviour yet, or only renders.
///
/// `Scenery` is a *deliberate* state, not a bug: a prop is authored to be seen
/// and nothing more (since the collide item promoted `Block` to `Gameplay`,
/// `prop` is the only role left in it). The bake surfaces it as one aggregated
/// per-zone Warning so an author knows the thing they placed has no runtime
/// behaviour, without it ever failing a bake.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Consumption {
    /// The game attaches components for this role.
    Gameplay,
    /// Renders **by design**; no runtime behaviour, and `prop` is not waiting
    /// for any — set dressing is the whole job.
    Scenery,
}

impl Role {
    /// Every role, in declaration order. The authoritative list for error
    /// messages and round-trip tests.
    pub const ALL: &'static [Role] = &[
        Role::Avatar,
        Role::Enemy,
        Role::Landmark,
        Role::Block,
        Role::Prop,
    ];

    /// The roles a zone may author freely — [`ALL`](Role::ALL) minus
    /// [`Avatar`](Role::Avatar), which is one-per-level and lives in the entry
    /// zone only (#27 / #54). The editor's role picker offers exactly these.
    pub const AUTHORABLE: &'static [Role] = &[Role::Enemy, Role::Landmark, Role::Block, Role::Prop];

    /// The role's wire/RON spelling. Lower-case, no aliases.
    pub const fn as_str(self) -> &'static str {
        match self {
            Role::Avatar => "avatar",
            Role::Enemy => "enemy",
            Role::Landmark => "landmark",
            Role::Block => "block",
            Role::Prop => "prop",
        }
    }

    /// Parse a RON/wire role string. **Case-sensitive and exact** — no aliases,
    /// no trimming, and the old `"spawn"` role is gone. An unparsed role is a
    /// bake Error host-side and renders-without-behaviour at runtime.
    pub fn parse(s: &str) -> Option<Role> {
        let mut i = 0;
        while i < Self::ALL.len() {
            let r = Self::ALL[i];
            if r.as_str().as_bytes() == s.as_bytes() {
                return Some(r);
            }
            i += 1;
        }
        None
    }

    /// Does this role have runtime behaviour, or does it only render?
    ///
    /// [`Block`](Role::Block) was promoted from `Scenery` to `Gameplay` by the
    /// collide item, 2026-09-18 (#12): a block is now solid, so it is no longer
    /// something the bake warns you is inert. [`Prop`](Role::Prop) is the only
    /// deliberately-inert role left.
    pub const fn consumption(self) -> Consumption {
        match self {
            Role::Avatar | Role::Enemy | Role::Landmark | Role::Block => Consumption::Gameplay,
            Role::Prop => Consumption::Scenery,
        }
    }

    /// This role's sub-archetype names. **The index is the wire byte** baked
    /// into `.scene` v4 — append only, never reorder or remove. Empty ⇒ the role
    /// has no sub-archetypes yet and authoring a `kind` on it is a bake Error.
    ///
    /// This is the one channel for role sub-archetypes; later work adds rows
    /// here (block shapes, enemy families) and never a new instance-flag bit.
    pub const fn kinds(self) -> &'static [&'static str] {
        match self {
            Role::Enemy => &["basic", "shielded", "advanced", "heavy"],
            // The blocking shape (#12). A `landmark` is deliberately kindless —
            // it is always a box — so the two solid roles stay distinguishable.
            Role::Block => &["box", "ramp", "round"],
            Role::Avatar | Role::Landmark | Role::Prop => &[],
        }
    }

    /// The [`flag_bits`] an author may set on this role. Only an enemy carries
    /// objective bits today; every other role is `0`, so `flags: 1` on a
    /// landmark is a bake Error rather than a silently ignored field.
    pub const fn allowed_flags(self) -> u32 {
        match self {
            Role::Enemy => flag_bits::ALL,
            Role::Avatar | Role::Landmark | Role::Block | Role::Prop => 0,
        }
    }
}

/// Resolve a role-scoped kind name to its wire byte (its index in
/// [`Role::kinds`]). `None` for an unknown name, or for any role with no kinds.
pub fn kind_from_str(role: Role, s: &str) -> Option<u8> {
    let kinds = role.kinds();
    let mut i = 0;
    while i < kinds.len() {
        if kinds[i].as_bytes() == s.as_bytes() {
            return Some(i as u8);
        }
        i += 1;
    }
    None
}

/// The name of a role-scoped kind byte, or `None` if the code is out of range
/// for that role (a stale or hand-edited blob).
pub fn kind_name(role: Role, k: u8) -> Option<&'static str> {
    role.kinds().get(k as usize).copied()
}

/// The enemy sub-archetypes (#27 vocabulary; the shape pairing is open on #29).
///
/// The discriminants **are** the `.scene` wire bytes and must match
/// `Role::Enemy.kinds()` position for position. `Basic == 0` is load-bearing: an
/// instance with no authored `kind` bakes to `0`.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum EnemyKind {
    /// The plain patroller — today's enemy, and what an absent `kind` means.
    #[default]
    Basic = 0,
    Shielded = 1,
    Advanced = 2,
    Heavy = 3,
}

impl EnemyKind {
    /// The byte baked into `.scene`.
    pub const fn wire(self) -> u8 {
        self as u8
    }

    /// Decode a wire byte. `None` for a code this build doesn't know (the
    /// runtime then falls back to [`EnemyKind::default`]).
    pub const fn from_wire(k: u8) -> Option<Self> {
        match k {
            0 => Some(EnemyKind::Basic),
            1 => Some(EnemyKind::Shielded),
            2 => Some(EnemyKind::Advanced),
            3 => Some(EnemyKind::Heavy),
            _ => None,
        }
    }

    /// The RON spelling — the same string as `Role::Enemy.kinds()[wire]`.
    pub const fn as_str(self) -> &'static str {
        match self {
            EnemyKind::Basic => "basic",
            EnemyKind::Shielded => "shielded",
            EnemyKind::Advanced => "advanced",
            EnemyKind::Heavy => "heavy",
        }
    }
}

/// The blocking shapes a [`Role::Block`] may take (#12).
///
/// The discriminants **are** the `.scene` wire bytes and must match
/// `Role::Block.kinds()` position for position. `Box == 0` is load-bearing: an
/// instance with no authored `kind` bakes to `0` and blocks as a plain box,
/// which is what every gray-box primitive did before the kind table existed.
///
/// Shape, not behaviour: `Ramp` walks up along the mesh's local **+Z**, `Round`
/// collides as a disc of the footprint's X half extent. The mapping onto
/// `bevy_nds_collide::Collider` constructors lives in the game (`src/collide.rs`),
/// not here.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum BlockKind {
    /// A flat-topped box — what an absent `kind` means.
    #[default]
    Box = 0,
    /// A wedge rising along the mesh's local `+Z`; `rot.y` aims it.
    Ramp = 1,
    /// A column that collides as a circle rather than its bounding rectangle.
    Round = 2,
}

impl BlockKind {
    /// The byte baked into `.scene`.
    pub const fn wire(self) -> u8 {
        self as u8
    }

    /// Decode a wire byte. `None` for a code this build doesn't know (the
    /// runtime then falls back to [`BlockKind::default`]).
    pub const fn from_wire(k: u8) -> Option<Self> {
        match k {
            0 => Some(BlockKind::Box),
            1 => Some(BlockKind::Ramp),
            2 => Some(BlockKind::Round),
            _ => None,
        }
    }

    /// The RON spelling — the same string as `Role::Block.kinds()[wire]`.
    pub const fn as_str(self) -> &'static str {
        match self {
            BlockKind::Box => "box",
            BlockKind::Ramp => "ramp",
            BlockKind::Round => "round",
        }
    }
}

/// **Instance-flag bits** — the bitmask an author may set on an instance's
/// `flags` field. Frozen for Milestone 2: new sub-archetypes go in
/// [`Role::kinds`], never here. Distinct from [`flag_ids`] (see the crate docs).
pub mod flag_bits {
    /// Gate objective: this enemy counts toward its zone's `clear_flag` (#27).
    pub const OBJECTIVE: u32 = 0x1;
    /// Level objective: this enemy rolls up to the level exit instead of a zone
    /// gate (#27 tier 2).
    pub const LEVEL_OBJECTIVE: u32 = 0x2;
    /// Every defined bit, OR'd. Anything outside this is undefined.
    pub const ALL: u32 = 0x3;

    /// The defined bits with their authoring names (editor checkboxes, bake
    /// messages). Kept in sync with [`ALL`] by a test.
    pub const NAMED: &[(u32, &str)] = &[
        (OBJECTIVE, "OBJECTIVE"),
        (LEVEL_OBJECTIVE, "LEVEL_OBJECTIVE"),
    ];

    /// The bits of `f` this build doesn't define — non-zero ⇒ a bake Error.
    pub const fn unknown_bits(f: u32) -> u32 {
        f & !ALL
    }
}

/// **Flag ids** — identifiers raised in the runtime `Flags` set (a zone's
/// `clear_flag`, a `Gate`'s required `flag`). Authored ids stay small (1, 2, …);
/// the engine reserves the high range for its own. Distinct from [`flag_bits`]
/// (see the crate docs).
pub mod flag_ids {
    /// Start of the engine-reserved id range. An authored `clear_flag` or
    /// `Gate.flag` at or above this is a bake Error.
    pub const RESERVED_MIN: u32 = 0x1000_0000;
    /// Raised when the level objective is met — the consumer for the level-exit
    /// seam (#27, still open).
    pub const LEVEL_EXIT: u32 = RESERVED_MIN;

    /// Is this id in the engine-reserved range?
    pub const fn is_reserved(id: u32) -> bool {
        id >= RESERVED_MIN
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_round_trips_and_rejects_unknown() {
        for r in Role::ALL {
            assert_eq!(Role::parse(r.as_str()), Some(*r), "{}", r.as_str());
        }
        // Exact + case-sensitive; the old `spawn` role is gone.
        for bad in ["spawn", "Enemy", "", "blok", " enemy", "enemy "] {
            assert_eq!(Role::parse(bad), None, "{bad} should not parse");
        }
        // No two roles share a spelling.
        for (i, a) in Role::ALL.iter().enumerate() {
            for b in &Role::ALL[i + 1..] {
                assert_ne!(a.as_str(), b.as_str());
            }
        }
        // AUTHORABLE is ALL minus exactly Avatar.
        assert_eq!(Role::AUTHORABLE.len(), Role::ALL.len() - 1);
        assert!(!Role::AUTHORABLE.contains(&Role::Avatar));
        for r in Role::AUTHORABLE {
            assert!(Role::ALL.contains(r), "{r:?}");
        }
    }

    #[test]
    fn consumption_split() {
        // `Block` joined Gameplay with the collide item (#12, 2026-09-18).
        for r in [Role::Avatar, Role::Enemy, Role::Landmark, Role::Block] {
            assert_eq!(r.consumption(), Consumption::Gameplay, "{r:?}");
        }
        assert_eq!(Role::Prop.consumption(), Consumption::Scenery);
        // …and it is the *only* Scenery role, so the bake's aggregated warning
        // can only ever name `prop`.
        let scenery: Vec<Role> = Role::ALL
            .iter()
            .copied()
            .filter(|r| r.consumption() == Consumption::Scenery)
            .collect();
        assert_eq!(scenery, vec![Role::Prop]);
    }

    #[test]
    fn enemy_kinds_round_trip_by_index() {
        let kinds = Role::Enemy.kinds();
        assert_eq!(kinds, &["basic", "shielded", "advanced", "heavy"]);
        assert_eq!(kind_from_str(Role::Enemy, "shielded"), Some(1));
        assert_eq!(kind_name(Role::Enemy, 1), Some("shielded"));
        for (i, name) in kinds.iter().enumerate() {
            assert_eq!(kind_from_str(Role::Enemy, name), Some(i as u8));
            assert_eq!(kind_name(Role::Enemy, i as u8), Some(*name));
        }
        // Out of range / unknown.
        assert_eq!(kind_name(Role::Enemy, 4), None);
        assert_eq!(kind_from_str(Role::Enemy, "bogus"), None);

        // `Block` is the second kinded role (#12) — the same index-is-the-wire
        // contract, checked the same way.
        let blocks = Role::Block.kinds();
        assert_eq!(blocks, &["box", "ramp", "round"]);
        for (i, name) in blocks.iter().enumerate() {
            assert_eq!(kind_from_str(Role::Block, name), Some(i as u8));
            assert_eq!(kind_name(Role::Block, i as u8), Some(*name));
        }
        assert_eq!(kind_name(Role::Block, 3), None);
        assert_eq!(kind_from_str(Role::Block, "wedge"), None);

        // The remaining roles have no kinds, so nothing resolves for them — a
        // landmark in particular is *always* a box, never `kind: "ramp"`.
        for r in [Role::Avatar, Role::Landmark, Role::Prop] {
            assert!(r.kinds().is_empty(), "{r:?}");
            assert_eq!(kind_from_str(r, "anything"), None, "{r:?}");
            assert_eq!(kind_name(r, 0), None, "{r:?}");
        }
    }

    #[test]
    fn block_kind_wire_is_stable() {
        let all = [BlockKind::Box, BlockKind::Ramp, BlockKind::Round];
        for k in all {
            assert_eq!(BlockKind::from_wire(k.wire()), Some(k), "{k:?}");
            // The enum discriminant and the `kinds()` index are the same thing.
            assert_eq!(Role::Block.kinds()[k.wire() as usize], k.as_str());
        }
        // The exact discriminants are the wire contract.
        assert_eq!(BlockKind::Box.wire(), 0);
        assert_eq!(BlockKind::Ramp.wire(), 1);
        assert_eq!(BlockKind::Round.wire(), 2);
        assert_eq!(BlockKind::from_wire(3), None);
        // An absent RON kind bakes to 0 and blocks as a plain box.
        assert_eq!(BlockKind::default(), BlockKind::Box);
        assert_eq!(Role::Block.kinds().len(), all.len());
    }

    #[test]
    fn enemy_kind_wire_is_stable() {
        let all = [
            EnemyKind::Basic,
            EnemyKind::Shielded,
            EnemyKind::Advanced,
            EnemyKind::Heavy,
        ];
        for k in all {
            assert_eq!(EnemyKind::from_wire(k.wire()), Some(k), "{k:?}");
            // The enum discriminant and the `kinds()` index are the same thing.
            assert_eq!(Role::Enemy.kinds()[k.wire() as usize], k.as_str());
        }
        // The exact discriminants are the wire contract.
        assert_eq!(EnemyKind::Basic.wire(), 0);
        assert_eq!(EnemyKind::Shielded.wire(), 1);
        assert_eq!(EnemyKind::Advanced.wire(), 2);
        assert_eq!(EnemyKind::Heavy.wire(), 3);
        assert_eq!(EnemyKind::from_wire(4), None);
        // An absent RON kind bakes to 0 and specialises to today's enemy.
        assert_eq!(EnemyKind::default(), EnemyKind::Basic);
        assert_eq!(Role::Enemy.kinds().len(), all.len());
    }

    #[test]
    fn flag_tables_agree() {
        let ored = flag_bits::NAMED.iter().fold(0, |acc, (b, _)| acc | b);
        assert_eq!(ored, flag_bits::ALL);
        assert_eq!(flag_bits::unknown_bits(0x3), 0);
        assert_eq!(flag_bits::unknown_bits(0x4), 0x4);
        assert_eq!(Role::Enemy.allowed_flags(), flag_bits::ALL);
        assert_eq!(Role::Landmark.allowed_flags(), 0);
        for r in [Role::Avatar, Role::Block, Role::Prop] {
            assert_eq!(r.allowed_flags(), 0, "{r:?}");
        }
        assert!(flag_ids::is_reserved(flag_ids::LEVEL_EXIT));
        assert!(!flag_ids::is_reserved(1));
        assert!(!flag_ids::is_reserved(flag_ids::RESERVED_MIN - 1));
    }
}
