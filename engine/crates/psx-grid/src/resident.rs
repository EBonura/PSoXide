//! The resident room set: which rooms of the level are in memory right now.
//!
//! The level core describes every room (footprint, portals, bounds) and is
//! always resident. A room's chunk (collision, then render data) is only
//! resident while the set holds it. Everything that touches room data goes
//! through this set: the collision world reads collision blocks from it, the
//! renderer reads render blocks and the room's material bindings, activation
//! asks it whether a room can be drawn or hold an entity.
//!
//! A room that is not resident is not an error and not a hole. The collision
//! world stands a solid box in for it over its footprint (bodies and traces
//! stop at it), the renderer skips it, nothing can be activated in it. That
//! is what lets a streamer load and evict rooms at any time.
//!
//! Storage is slot indexed: one small table maps a room to a slot and the
//! slots hold the views. Slots are reused after an eviction, so the memory
//! of the set follows how many rooms are resident, not how many exist.
//! Views borrow the chunk bytes, so a caller that moves a chunk (a pool
//! compaction) removes the room first and inserts it again at the new
//! address; [`ResidentRooms::epoch`] changes on every insert and remove so a
//! cache of anything derived from the set knows to refresh.

use alloc::vec::Vec;
use core::cell::Cell;

use crate::collision::{GridCollision, LeafDims};
use crate::format::{ROOM_MAGIC, VERSION};
use crate::height::{weight_recip, Recip};
use crate::probe::ProbeView;
use crate::render::GridMaterial;
use crate::view::{CollisionView, LevelView, RenderView, RoomView};

/// Marker in the room to slot table: not resident.
pub const NO_SLOT: u16 = u16::MAX;

/// Why a room could not be added.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ResidentError {
    /// The room index is not in the level core.
    NoSuchRoom,
    /// The room is already resident; remove it first.
    AlreadyResident,
    /// The chunk's size does not match the room's record.
    Mismatch,
    /// Every slot is taken.
    Full,
}

/// One resident room.
pub struct RoomSlot<'a> {
    pub room: u16,
    pub col: CollisionView<'a>,
    /// The render block, `None` while only the collision block has landed.
    pub render: Option<RenderView<'a>>,
    /// The room's light probes; empty until the render block (or a host
    /// that parses them alone) provides them.
    pub probes: ProbeView<'a>,
    /// Bindings of the room's materials: entry `i` serves quads whose
    /// `material` is `i`, and stands for `render.material_use[i]`.
    pub materials: Vec<GridMaterial>,
    /// Every binding is resolved and its texture resident. A room that is
    /// not ready is not drawn.
    pub materials_ready: bool,
}

impl<'a> RoomSlot<'a> {
    /// The whole chunk, when the render block is here.
    pub fn view(&self) -> Option<RoomView<'a>> {
        Some(RoomView {
            col: self.col,
            render: self.render?,
        })
    }
}

/// The set of resident rooms of one level.
pub struct ResidentRooms<'a> {
    level: LevelView<'a>,
    slot_of: Vec<u16>,
    /// Room to the collision block of its slot, null when not resident: the
    /// one-load lookup collision uses. Entries point into `slots` and are
    /// refreshed whenever a slot is placed, removed or moved by growth.
    views: Vec<*const CollisionView<'a>>,
    slots: Vec<Option<RoomSlot<'a>>>,
    free: Vec<u16>,
    /// Material binding lists of rooms that left, kept for the next room to load. The guest
    /// allocator never frees, so a list dropped with its slot would leak its block every time
    /// a room streams out.
    spare_materials: Vec<Vec<GridMaterial>>,
    capacity: usize,
    resident: usize,
    /// Resident rooms holding collidable free planes.
    plane_rooms: usize,
    epoch: u32,
    pub(crate) s: i32,
    pub(crate) recip: Recip,
    pub(crate) inv24: u32,
    /// Edge and weight reciprocal of every leaf size code.
    pub(crate) leaf: LeafDims,
    /// Room that served the last light probe lookup.
    pub(crate) probe_hint: Cell<u16>,
    /// Per room, whether no other room overlaps it in plan: 0 not known yet,
    /// 1 alone, 2 shared. Depends on the core alone, so never reset.
    pub(crate) solo: Vec<Cell<u8>>,
}

impl<'a> ResidentRooms<'a> {
    /// An empty set over `level` that can hold every room. `None` when no
    /// exact cell reciprocal exists for the level's sector size.
    pub fn new(level: LevelView<'a>) -> Option<Self> {
        Self::with_capacity(level, level.rooms.len())
    }

    /// Slots the table has grown to: the most rooms that were resident at once, counting the
    /// ones an eviction left a hole for.
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Make room for `rooms` resident rooms now, so loading them never grows the tables. The
    /// guest allocator never frees: a table that grows leaves its old block behind for good.
    pub fn reserve_rooms(&mut self, rooms: usize) {
        let rooms = rooms.min(self.capacity);
        self.slots
            .reserve_exact(rooms.saturating_sub(self.slots.len()));
        self.free
            .reserve_exact(rooms.saturating_sub(self.free.len()));
        self.spare_materials
            .reserve_exact(rooms.saturating_sub(self.spare_materials.len()));
    }

    /// An empty set that can hold at most `capacity` rooms at once.
    pub fn with_capacity(level: LevelView<'a>, capacity: usize) -> Option<Self> {
        let s = level.sector_units();
        let recip = Recip::for_sector(s, 255 * s)?;
        let capacity = capacity.min(usize::from(NO_SLOT) - 1);
        Some(Self {
            level,
            slot_of: alloc::vec![NO_SLOT; level.rooms.len()],
            views: alloc::vec![core::ptr::null(); level.rooms.len()],
            slots: Vec::new(),
            free: Vec::new(),
            spare_materials: Vec::new(),
            capacity,
            resident: 0,
            plane_rooms: 0,
            epoch: 0,
            s: s as i32,
            recip,
            inv24: weight_recip(s),
            leaf: LeafDims::for_sector(s as i32),
            probe_hint: Cell::new(0),
            solo: (0..level.rooms.len()).map(|_| Cell::new(0)).collect(),
        })
    }

    /// A set holding the collision block of every `Some` entry of `views`
    /// (indexed by room), for hosts that parse collision alone.
    pub fn from_collisions(
        level: LevelView<'a>,
        views: &[Option<CollisionView<'a>>],
    ) -> Option<Self> {
        let mut set = Self::new(level)?;
        for (room, view) in views.iter().enumerate() {
            if let Some(view) = view {
                set.insert_collision(room, *view).ok()?;
            }
        }
        Some(set)
    }

    /// The level core this set is over.
    pub fn level(&self) -> &LevelView<'a> {
        &self.level
    }

    /// Rooms resident now.
    pub fn len(&self) -> usize {
        self.resident
    }

    /// Whether no room is resident.
    pub fn is_empty(&self) -> bool {
        self.resident == 0
    }

    /// Most rooms the set holds at once.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Changes whenever a room is added or removed.
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// Whether every room of the level is resident.
    pub fn is_complete(&self) -> bool {
        self.resident == self.level.rooms.len()
    }

    /// Whether any resident room holds collidable free planes.
    pub(crate) fn has_planes(&self) -> bool {
        self.plane_rooms > 0
    }

    /// Whether `room` is resident.
    #[inline]
    pub fn contains(&self, room: usize) -> bool {
        self.slot_of.get(room).is_some_and(|&s| s != NO_SLOT)
    }

    /// The slot of `room`.
    #[inline(always)]
    pub fn get(&self, room: usize) -> Option<&RoomSlot<'a>> {
        let slot = *self.slot_of.get(room)?;
        if slot == NO_SLOT {
            return None;
        }
        self.slots.get(usize::from(slot))?.as_ref()
    }

    /// The slot of `room`, mutably (to bind its materials).
    pub fn get_mut(&mut self, room: usize) -> Option<&mut RoomSlot<'a>> {
        let slot = *self.slot_of.get(room)?;
        if slot == NO_SLOT {
            return None;
        }
        self.slots.get_mut(usize::from(slot))?.as_mut()
    }

    /// Collision block of `room`: one table load and a null check.
    #[inline(always)]
    pub fn collision_of(&self, room: usize) -> Option<&CollisionView<'a>> {
        let view = *self.views.get(room)?;
        // SAFETY: a non-null entry points at the `col` of a live slot of
        // `self.slots` (kept in step by `place`, `remove` and growth), and
        // the borrow of `self` keeps the slot table from changing while the
        // reference lives.
        unsafe { view.as_ref() }
    }

    /// The table of per-room collision block pointers (null = not
    /// resident), for the collision world to hold directly.
    #[inline(always)]
    pub(crate) fn view_table(&self) -> &[*const CollisionView<'a>] {
        &self.views
    }

    /// The whole chunk of `room`, when it is resident with its render block.
    pub fn room_view(&self, room: usize) -> Option<RoomView<'a>> {
        self.get(room)?.view()
    }

    /// Resident rooms, ascending by slot.
    pub fn iter(&self) -> impl Iterator<Item = &RoomSlot<'a>> {
        self.slots.iter().flatten()
    }

    /// Resident rooms, mutably.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut RoomSlot<'a>> {
        self.slots.iter_mut().flatten()
    }

    fn check(&self, room: usize, header_w: usize, header_d: usize) -> Result<(), ResidentError> {
        let record = self
            .level
            .rooms
            .get(room)
            .ok_or(ResidentError::NoSuchRoom)?;
        if self.contains(room) {
            return Err(ResidentError::AlreadyResident);
        }
        if usize::from(record.width) != header_w || usize::from(record.depth) != header_d {
            return Err(ResidentError::Mismatch);
        }
        if self.resident >= self.capacity {
            return Err(ResidentError::Full);
        }
        Ok(())
    }

    /// Point `views[room]` at the collision block of its slot.
    fn point_view(&mut self, room: usize) {
        let slot = self.slot_of[room];
        self.views[room] = if slot == NO_SLOT {
            core::ptr::null()
        } else {
            match &self.slots[usize::from(slot)] {
                Some(slot) => &slot.col,
                None => core::ptr::null(),
            }
        };
    }

    /// Refresh every pointer (after the slot table moved in memory).
    fn point_all_views(&mut self) {
        for room in 0..self.views.len() {
            self.point_view(room);
        }
    }

    fn place(&mut self, room: usize, slot: RoomSlot<'a>) {
        let planes = !slot.col.col_planes.is_empty();
        let moved_before = self.slots.as_ptr();
        let index = match self.free.pop() {
            Some(free) => {
                self.slots[usize::from(free)] = Some(slot);
                free
            }
            None => {
                // A table that is full grows by two slots, not by doubling: the guest allocator
                // never gives a block that size back, and doubling 16 slots takes 7 KB of a heap
                // that has about 10 KB to spare.
                if self.slots.len() == self.slots.capacity() {
                    self.slots.reserve_exact(2);
                }
                self.slots.push(Some(slot));
                (self.slots.len() - 1) as u16
            }
        };
        self.slot_of[room] = index;
        if self.slots.as_ptr() == moved_before {
            self.point_view(room);
        } else {
            // The table grew into new memory: every pointer is stale.
            self.point_all_views();
        }
        self.resident += 1;
        self.plane_rooms += usize::from(planes);
        self.epoch = self.epoch.wrapping_add(1);
    }

    /// Add a room whose collision block alone has landed.
    pub fn insert_collision(
        &mut self,
        room: usize,
        col: CollisionView<'a>,
    ) -> Result<(), ResidentError> {
        debug_assert!(col.header.magic == ROOM_MAGIC && col.header.version == VERSION);
        self.check(room, col.width(), col.depth())?;
        let materials = self.spare_materials.pop().unwrap_or_default();
        self.place(
            room,
            RoomSlot {
                room: room as u16,
                col,
                render: None,
                probes: ProbeView::EMPTY,
                materials,
                materials_ready: false,
            },
        );
        Ok(())
    }

    /// Add a room with both blocks. Its materials are bound later through
    /// [`RoomSlot::materials`] and [`RoomSlot::materials_ready`]; the room is
    /// not drawn before.
    pub fn insert(&mut self, room: usize, view: RoomView<'a>) -> Result<(), ResidentError> {
        self.check(room, view.col.width(), view.col.depth())?;
        let materials = self.spare_materials.pop().unwrap_or_default();
        self.place(
            room,
            RoomSlot {
                room: room as u16,
                col: view.col,
                render: Some(view.render),
                probes: view.render.probes,
                materials,
                materials_ready: false,
            },
        );
        Ok(())
    }

    /// Attach the render block to a room that so far has collision only.
    pub fn attach_render(&mut self, room: usize, render: RenderView<'a>) -> bool {
        match self.get_mut(room) {
            Some(slot) if slot.render.is_none() => {
                slot.render = Some(render);
                slot.probes = render.probes;
                self.epoch = self.epoch.wrapping_add(1);
                true
            }
            _ => false,
        }
    }

    /// Give a resident room its light probes without the rest of its render
    /// block, for hosts that parse the probes alone.
    pub fn attach_probes(&mut self, room: usize, probes: ProbeView<'a>) -> bool {
        match self.get_mut(room) {
            Some(slot) => {
                slot.probes = probes;
                self.epoch = self.epoch.wrapping_add(1);
                true
            }
            None => false,
        }
    }

    /// Drop a room from the set, returning its slot. The material buffer of
    /// the slot is kept for the next room inserted, so the returned slot's is
    /// empty.
    pub fn remove(&mut self, room: usize) -> Option<RoomSlot<'a>> {
        let index = *self.slot_of.get(room)?;
        if index == NO_SLOT {
            return None;
        }
        let mut slot = self.slots[usize::from(index)].take()?;
        if slot.materials.capacity() > 0 {
            let mut list = core::mem::take(&mut slot.materials);
            list.clear();
            self.spare_materials.push(list);
        }
        self.slot_of[room] = NO_SLOT;
        self.views[room] = core::ptr::null();
        self.free.push(index);
        self.resident -= 1;
        self.plane_rooms -= usize::from(!slot.col.col_planes.is_empty());
        self.epoch = self.epoch.wrapping_add(1);
        Some(slot)
    }

    /// The collision world over the rooms resident now. Cheap: it copies a
    /// few constants, nothing is scanned.
    pub fn collision(&self) -> GridCollision<'_> {
        GridCollision::over(self)
    }

    /// Mark every resident room's materials stale, so they are bound again.
    pub fn invalidate_materials(&mut self) {
        for slot in self.iter_mut() {
            slot.materials_ready = false;
            for material in &mut slot.materials {
                material.resident = false;
            }
        }
    }
}

impl ResidentRooms<'static> {
    /// The collision world of this set with the set leaked for the rest of
    /// the program: for hosts and tests that never change what is resident.
    pub fn leak_collision(self) -> GridCollision<'static> {
        let leaked: &'static ResidentRooms<'static> =
            alloc::boxed::Box::leak(alloc::boxed::Box::new(self));
        leaked.collision()
    }
}
