//! The inventory screen's geometry.
//!
//! **One layout, two consumers.** [`InventoryPanel::layout`] is a pure function
//! that both the drawing code and [`hit`](InventoryPanel::hit) read. If those
//! two each computed their own rectangles they would drift, and the symptom —
//! clicking one slot and affecting another — is miserable to diagnose and
//! trivially avoided. It also makes hit-testing a pure function a unit test can
//! check with no GPU: *a click at these pixels is that slot*.
//!
//! Nothing here knows what an item or an inventory is. [`PanelSlotKind`] is
//! render-local; `cubara-render` cannot depend on `cubara-sim` (Rule 3,
//! dependencies point one way) and has no business knowing what a `SlotRef` is.
//! The app maps one to the other.

/// Which group a slot belongs to. Deliberately not `cubara_sim::SlotRef` — see
/// the module docs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PanelSlotKind {
    Inventory,
    Grid,
    Result,
    /// A furnace's fuel slot. Distinct from [`Grid`](Self::Grid) because a
    /// furnace's two inputs are not interchangeable -- putting a log where the
    /// ore goes should not smelt it.
    Fuel,
}

/// One slot's place on screen, in pixels from the top-left.
#[derive(Clone, Copy, Debug)]
pub struct PanelSlot {
    pub kind: PanelSlotKind,
    pub index: usize,
    pub x: f32,
    pub y: f32,
    pub size: f32,
}

impl PanelSlot {
    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.size && y < self.y + self.size
    }
}

/// Slot edge length, in pixels. Fixed rather than a fraction of the viewport,
/// like the hotbar (#145): the same physical size on 1080p and 4K, rather than
/// four times bigger.
pub const SLOT: f32 = 40.0;
/// Gap between slots. A real gap, not a border: a click landing in it hits
/// nothing, which is better than silently rounding to the nearest slot.
pub const GAP: f32 = 4.0;
/// Inventory columns. The hotbar is the *last* row of the same 36 slots.
pub const COLS: usize = 9;
/// How many of the 36 are the hotbar row.
pub const HOTBAR: usize = 9;
/// Total inventory slots the panel shows.
pub const SLOTS: usize = 36;

/// Every slot's rectangle, for one window size and grid width.
#[derive(Clone, Debug)]
pub struct InventoryPanel {
    slots: Vec<PanelSlot>,
    /// The panel's own background rectangle.
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl InventoryPanel {
    /// Lay the panel out centred in a `width` x `height` window.
    ///
    /// `grid_width` is 2 for the inventory's own grid and 3 at a bench; the
    /// panel grows to fit it rather than the grid being squeezed into a fixed
    /// box, so a bench looks like a bigger version of the same screen.
    pub fn layout(width: u32, height: u32, grid_width: usize) -> Self {
        let grid_width = grid_width.clamp(1, 3);
        const PAD: f32 = 16.0;
        let step = SLOT + GAP;

        // Rows: the crafting area on top, then 3 rows of main inventory, then a
        // gap, then the hotbar row.
        let inv_rows = (SLOTS - HOTBAR) / COLS;
        let craft_h = grid_width as f32 * step - GAP;
        let inv_h = inv_rows as f32 * step - GAP;
        let content_w = COLS as f32 * step - GAP;
        let content_h = craft_h + PAD + inv_h + PAD + SLOT;

        let px = (width as f32 - content_w - PAD * 2.0) * 0.5;
        let py = (height as f32 - content_h - PAD * 2.0) * 0.5;
        let ox = px + PAD;
        let oy = py + PAD;

        let mut slots = Vec::with_capacity(SLOTS + grid_width * grid_width + 1);

        // Crafting grid, top-left of the panel.
        for row in 0..grid_width {
            for col in 0..grid_width {
                slots.push(PanelSlot {
                    kind: PanelSlotKind::Grid,
                    // Indexed against the sim's fixed 3x3 storage, not against
                    // `grid_width` -- so cell (1,1) is index 4 whether or not a
                    // bench is open, and the app never has to re-map.
                    index: row * 3 + col,
                    x: ox + col as f32 * step,
                    y: oy + row as f32 * step,
                    size: SLOT,
                });
            }
        }

        // Result, to the right of the grid and vertically centred against it.
        slots.push(PanelSlot {
            kind: PanelSlotKind::Result,
            index: 0,
            x: ox + (grid_width as f32 + 0.5) * step,
            y: oy + (craft_h - SLOT) * 0.5,
            size: SLOT,
        });

        // Main inventory, then the hotbar row below a deliberate gap: the gap is
        // what tells the player those nine are the ones the number keys reach.
        let inv_y = oy + craft_h + PAD;
        for i in 0..SLOTS - HOTBAR {
            let (row, col) = (i / COLS, i % COLS);
            slots.push(PanelSlot {
                kind: PanelSlotKind::Inventory,
                // Slots 0..9 are the hotbar in the sim, so the main inventory
                // starts at 9 -- the screen shows them in the opposite order to
                // their indices, which is exactly why this mapping lives here
                // and not at the call site.
                index: i + HOTBAR,
                x: ox + col as f32 * step,
                y: inv_y + row as f32 * step,
                size: SLOT,
            });
        }
        let hotbar_y = inv_y + inv_h + PAD;
        for i in 0..HOTBAR {
            slots.push(PanelSlot {
                kind: PanelSlotKind::Inventory,
                index: i,
                x: ox + i as f32 * step,
                y: hotbar_y,
                size: SLOT,
            });
        }

        Self {
            slots,
            x: px,
            y: py,
            width: content_w + PAD * 2.0,
            height: content_h + PAD * 2.0,
        }
    }

    /// A furnace screen: input above fuel on the left, output on the right,
    /// with the same inventory and hotbar below.
    ///
    /// Built from [`layout`](Self::layout)'s 1-wide form rather than as a
    /// second layout routine, so the inventory half -- which is most of the
    /// panel, and all of the fiddly index mapping -- has exactly one
    /// implementation (Rule 5). Only the three furnace slots are positioned
    /// here; `Grid(0)` is reused as the input slot, since a furnace input *is*
    /// a one-cell grid as far as the click router is concerned.
    pub fn layout_furnace(width: u32, height: u32) -> Self {
        // Built on the **2-wide** form, not the 1-wide one: the furnace stacks
        // input above fuel, so it needs two rows of vertical space in the
        // crafting area. A 1-wide base reserves one, and the fuel slot would
        // then overlap the first row of the inventory below it.
        let mut panel = Self::layout(width, height, 2);
        const PAD: f32 = 16.0;
        let step = SLOT + GAP;
        let ox = panel.x + PAD;
        let oy = panel.y + PAD;

        // Keep only grid cell 0 as the input; the other three cells of the 2x2
        // base have no meaning here.
        panel
            .slots
            .retain(|s| s.kind != PanelSlotKind::Grid || s.index == 0);

        // Output to the right, vertically centred against the input/fuel pair.
        for slot in &mut panel.slots {
            if slot.kind == PanelSlotKind::Result {
                slot.x = ox + 2.0 * step;
                slot.y = oy + step * 0.5;
            }
        }

        // Fuel directly below the input -- the arrangement the genre uses, and
        // so the one a player will guess at. Its bottom edge lands exactly on
        // the base layout's crafting-area height, so nothing below moves.
        panel.slots.push(PanelSlot {
            kind: PanelSlotKind::Fuel,
            index: 0,
            x: ox,
            y: oy + step,
            size: SLOT,
        });
        panel
    }

    pub fn slots(&self) -> &[PanelSlot] {
        &self.slots
    }

    /// Which slot a point lands in, if any.
    ///
    /// Returns `None` for a click in a gap or outside the panel. Deliberately
    /// not "the nearest slot": a click that lands between two slots did not
    /// mean either of them, and guessing would move items the player did not
    /// ask to move.
    pub fn hit(&self, x: f32, y: f32) -> Option<(PanelSlotKind, usize)> {
        self.slots
            .iter()
            .find(|s| s.contains(x, y))
            .map(|s| (s.kind, s.index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_furnace_panel_has_three_slots_that_do_not_overlap_the_inventory() {
        let p = InventoryPanel::layout_furnace(1920, 1080);
        let furnace: Vec<&PanelSlot> = p
            .slots()
            .iter()
            .filter(|s| {
                matches!(
                    s.kind,
                    PanelSlotKind::Grid | PanelSlotKind::Fuel | PanelSlotKind::Result
                )
            })
            .collect();
        assert_eq!(furnace.len(), 3, "input, fuel, output -- and nothing else");

        // The bug this pins: a 1-wide base reserves one row of crafting height,
        // so the fuel slot underneath the input would sit on top of the first
        // inventory row.
        let inv_top = p
            .slots()
            .iter()
            .filter(|s| s.kind == PanelSlotKind::Inventory)
            .map(|s| s.y)
            .fold(f32::INFINITY, f32::min);
        for s in &furnace {
            assert!(
                s.y + s.size <= inv_top + 0.001,
                "{:?} at y={} overlaps the inventory at y={inv_top}",
                s.kind,
                s.y
            );
        }
    }

    #[test]
    fn a_click_finds_the_furnace_fuel_slot() {
        let p = InventoryPanel::layout_furnace(1920, 1080);
        let fuel = p
            .slots()
            .iter()
            .find(|s| s.kind == PanelSlotKind::Fuel)
            .copied()
            .expect("a fuel slot");
        assert_eq!(
            p.hit(fuel.x + 1.0, fuel.y + 1.0),
            Some((PanelSlotKind::Fuel, 0))
        );
    }

    fn panel() -> InventoryPanel {
        InventoryPanel::layout(1280, 720, 2)
    }

    /// The centre of a slot, which is where a test should click.
    fn centre(s: &PanelSlot) -> (f32, f32) {
        (s.x + s.size * 0.5, s.y + s.size * 0.5)
    }

    #[test]
    fn a_click_in_a_slot_hits_that_slot() {
        // Every slot, not a sample: an off-by-one in one row would otherwise
        // pass a spot check.
        let p = panel();
        for s in p.slots() {
            let (x, y) = centre(s);
            assert_eq!(
                p.hit(x, y),
                Some((s.kind, s.index)),
                "the centre of {:?} {} did not hit itself",
                s.kind,
                s.index
            );
        }
    }

    #[test]
    fn every_kind_of_slot_is_reachable() {
        // Guards against a layout that quietly stops emitting one group -- the
        // result slot is the easy one to lose, and the screen would still look
        // fine.
        let p = panel();
        for kind in [
            PanelSlotKind::Inventory,
            PanelSlotKind::Grid,
            PanelSlotKind::Result,
        ] {
            let s = p
                .slots()
                .iter()
                .find(|s| s.kind == kind)
                .unwrap_or_else(|| panic!("no {kind:?} slot in the layout"));
            let (x, y) = centre(s);
            assert_eq!(p.hit(x, y).map(|(k, _)| k), Some(kind));
        }
    }

    #[test]
    fn slots_never_overlap() {
        // The property that makes "I clicked one slot and it moved another"
        // impossible by construction, rather than by careful arithmetic.
        let p = panel();
        let s = p.slots();
        for (i, a) in s.iter().enumerate() {
            for b in &s[i + 1..] {
                let disjoint = a.x + a.size <= b.x
                    || b.x + b.size <= a.x
                    || a.y + a.size <= b.y
                    || b.y + b.size <= a.y;
                assert!(
                    disjoint,
                    "{:?} {} overlaps {:?} {}",
                    a.kind, a.index, b.kind, b.index
                );
            }
        }
    }

    #[test]
    fn a_click_in_the_gap_hits_nothing() {
        // Not "the nearest slot": a click between two slots did not mean either
        // of them, and guessing would move items the player did not ask to move.
        let p = panel();
        let first = p.slots()[0];
        let in_gap = (first.x + first.size + GAP * 0.5, first.y + first.size * 0.5);
        assert_eq!(p.hit(in_gap.0, in_gap.1), None);
    }

    #[test]
    fn a_click_outside_the_panel_hits_nothing() {
        let p = panel();
        assert_eq!(p.hit(-10.0, -10.0), None);
        assert_eq!(p.hit(5000.0, 5000.0), None);
    }

    #[test]
    fn the_layout_moves_with_the_window_but_keeps_its_slots() {
        // Resizing must re-centre the panel, not change what is in it. A layout
        // that dropped or added slots on resize would make the screen's contents
        // depend on the window, which nothing downstream expects.
        let small = InventoryPanel::layout(1280, 720, 2);
        let large = InventoryPanel::layout(2560, 1440, 2);
        assert_eq!(small.slots().len(), large.slots().len());
        assert!(
            large.x > small.x,
            "a wider window centres the panel further right"
        );

        for (a, b) in small.slots().iter().zip(large.slots()) {
            assert_eq!((a.kind, a.index), (b.kind, b.index), "slot order changed");
            assert_eq!(a.size, b.size, "slots must not scale with the window");
        }
    }

    #[test]
    fn a_bench_adds_cells_without_renumbering_the_others() {
        // Grid indices address the sim's fixed 3x3 storage, so cell (1,1) is 4
        // whether or not a bench is open. If they were numbered against
        // `grid_width`, opening a bench would silently re-point every click.
        let inv = InventoryPanel::layout(1280, 720, 2);
        let bench = InventoryPanel::layout(1280, 720, 3);

        let grid_indices = |p: &InventoryPanel| -> Vec<usize> {
            p.slots()
                .iter()
                .filter(|s| s.kind == PanelSlotKind::Grid)
                .map(|s| s.index)
                .collect()
        };
        assert_eq!(grid_indices(&inv), vec![0, 1, 3, 4]);
        assert_eq!(grid_indices(&bench), vec![0, 1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn the_hotbar_row_is_the_first_nine_slots() {
        // The screen draws the hotbar last, at the bottom, but those are slots
        // 0..9 in the sim -- the same nine the number keys select. Getting this
        // backwards would make the number keys appear to control the wrong row.
        let p = panel();
        let inventory: Vec<&PanelSlot> = p
            .slots()
            .iter()
            .filter(|s| s.kind == PanelSlotKind::Inventory)
            .collect();
        let bottom_row_y = inventory.iter().map(|s| s.y).fold(f32::MIN, f32::max);
        let bottom: Vec<usize> = inventory
            .iter()
            .filter(|s| s.y == bottom_row_y)
            .map(|s| s.index)
            .collect();
        assert_eq!(bottom, (0..HOTBAR).collect::<Vec<_>>());
    }
}
