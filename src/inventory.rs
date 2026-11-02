//! The creative inventory, after Minecraft's early versions: one window listing every
//! placeable block in a scrolling grid, with the hotbar below it. Blocks are dragged, or
//! clicked up and clicked down, onto the hotbar.

use crate::render::{HudRect, font};
use crate::world::block::{self, AIR, CRYSTAL, face_layer, is_emissive};
use glam::{Vec2, Vec3};
use std::mem;

const COLS: usize = 9;
const ROWS: usize = 5;
// The window's layout, in GUI pixels of `gui_scale` screen pixels each.
const WIDTH: f32 = 196.0;
const HEIGHT: f32 = 140.0;
const SLOT: f32 = 18.0;
const GRID: Vec2 = Vec2::new(8.0, 18.0);
const HOTBAR: Vec2 = Vec2::new(8.0, 114.0);
const TRACK: [f32; 4] = [174.0, 18.0, 14.0, SLOT * ROWS as f32];
const THUMB: f32 = 15.0;

/// Grows in whole steps with the screen, like Minecraft's GUI scale.
pub fn gui_scale(height: u32) -> f32 {
    (height as f32 / 360.0).floor().max(1.0)
}

/// An sRGB colour as the HUD wants it: the output is sRGB, so it takes linear values.
fn rgb(c: u32, alpha: f32) -> [f32; 4] {
    let ch = |s: u32| ((c >> s) & 255) as f32 / 255.0;
    let lin = |v: f32| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    [lin(ch(16)), lin(ch(8)), lin(ch(0)), alpha]
}

/// What is under a point on the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spot {
    /// A block in the list, by its index in the whole list.
    List(usize),
    Hotbar(usize),
    Thumb,
    Track,
    /// Anywhere else inside the window.
    Window,
    Outside,
}

/// Converts between screen pixels and the window's GUI pixels.
struct Layout {
    origin: Vec2,
    scale: f32,
}

impl Layout {
    fn new(size: (u32, u32)) -> Self {
        let scale = gui_scale(size.1);
        let screen = Vec2::new(size.0 as f32, size.1 as f32);
        Self {
            origin: ((screen - Vec2::new(WIDTH, HEIGHT) * scale) * 0.5).floor(),
            scale,
        }
    }

    fn to_gui(&self, p: Vec2) -> Vec2 {
        (p - self.origin) / self.scale
    }

    fn rect(&self, x: f32, y: f32, w: f32, h: f32) -> [f32; 4] {
        let s = self.scale;
        [self.origin.x + x * s, self.origin.y + y * s, w * s, h * s]
    }
}

pub struct Inventory {
    pub open: bool,
    items: Vec<u8>,
    /// The first list row in view.
    scroll: usize,
    /// The block following the cursor, or air.
    pub held: u8,
    /// Where the current press picked up the held block. Letting go anywhere else drops
    /// it there, which makes a drag; letting go in place keeps holding it, which makes a
    /// click.
    picked_from: Option<Spot>,
    /// While the scrollbar is held: the cursor's distance below the thumb's top.
    thumb_grab: Option<f32>,
    pub cursor: Vec2,
}

impl Inventory {
    pub fn new(items: Vec<u8>) -> Self {
        Self {
            open: false,
            items,
            scroll: 0,
            held: AIR,
            picked_from: None,
            thumb_grab: None,
            cursor: Vec2::ZERO,
        }
    }

    fn max_scroll(&self) -> usize {
        self.items.len().div_ceil(COLS).saturating_sub(ROWS)
    }

    /// The thumb's top, in GUI pixels.
    fn thumb_y(&self) -> f32 {
        let travel = TRACK[3] - 2.0 - THUMB;
        let m = self.max_scroll();
        let f = if m == 0 {
            0.0
        } else {
            self.scroll as f32 / m as f32
        };
        TRACK[1] + 1.0 + travel * f
    }

    /// The screen position of a slot's centre, if it's in view.
    pub fn slot_centre(&self, spot: Spot, size: (u32, u32)) -> Option<Vec2> {
        let (origin, i, rows) = match spot {
            Spot::List(i) => (GRID, i.checked_sub(self.scroll * COLS)?, ROWS),
            Spot::Hotbar(i) => (HOTBAR, i, 1),
            _ => return None,
        };
        if i >= rows * COLS {
            return None;
        }
        let (c, r) = (i % COLS, i / COLS);
        let l = Layout::new(size);
        let g = origin + (Vec2::new(c as f32, r as f32) + 0.5) * SLOT;
        Some(l.origin + g * l.scale)
    }

    pub fn spot(&self, p: Vec2, size: (u32, u32)) -> Spot {
        let g = Layout::new(size).to_gui(p);
        if g.x < 0.0 || g.y < 0.0 || g.x >= WIDTH || g.y >= HEIGHT {
            return Spot::Outside;
        }
        let cell = |origin: Vec2| {
            let c = ((g - origin) / SLOT).floor();
            (c.x >= 0.0 && c.x < COLS as f32 && c.y >= 0.0).then_some((c.x as usize, c.y as usize))
        };
        if let Some((c, r)) = cell(GRID)
            && r < ROWS
        {
            let i = (self.scroll + r) * COLS + c;
            return if i < self.items.len() {
                Spot::List(i)
            } else {
                Spot::Window
            };
        }
        if let Some((c, 0)) = cell(HOTBAR) {
            return Spot::Hotbar(c);
        }
        let [tx, ty, tw, th] = TRACK;
        if g.x >= tx && g.x < tx + tw && g.y >= ty && g.y < ty + th {
            let top = self.thumb_y();
            return if g.y >= top && g.y < top + THUMB {
                Spot::Thumb
            } else {
                Spot::Track
            };
        }
        Spot::Window
    }

    pub fn toggle(&mut self) {
        if self.open {
            self.close();
        } else {
            self.open = true;
        }
    }

    /// Closing drops whatever is held, as nothing in creative mode is worth keeping.
    pub fn close(&mut self) {
        self.open = false;
        self.held = AIR;
        self.picked_from = None;
        self.thumb_grab = None;
    }

    /// A left button press. Shift-clicking a listed block puts it straight into the
    /// first empty hotbar slot, or the selected one if none is empty.
    pub fn press(
        &mut self,
        p: Vec2,
        shift: bool,
        hotbar: &mut [u8; 9],
        selected: usize,
        size: (u32, u32),
    ) {
        self.cursor = p;
        match self.spot(p, size) {
            Spot::List(i) if shift => {
                let slot = hotbar.iter().position(|&b| b == AIR).unwrap_or(selected);
                hotbar[slot] = self.items[i];
            }
            Spot::List(i) => {
                // Holding something already, a click on the list puts it back.
                if self.held != AIR {
                    self.held = AIR;
                } else {
                    self.held = self.items[i];
                    self.picked_from = Some(Spot::List(i));
                }
            }
            Spot::Hotbar(i) => {
                let empty_handed = self.held == AIR;
                mem::swap(&mut self.held, &mut hotbar[i]);
                if empty_handed && self.held != AIR {
                    self.picked_from = Some(Spot::Hotbar(i));
                }
            }
            Spot::Thumb => {
                let g = Layout::new(size).to_gui(p);
                self.thumb_grab = Some(g.y - self.thumb_y());
            }
            Spot::Track => {
                // Jumps the thumb to the cursor, then drags it from its middle.
                self.thumb_grab = Some(THUMB * 0.5);
                self.drag_thumb(p, size);
            }
            Spot::Window => {}
            Spot::Outside => self.held = AIR,
        }
    }

    pub fn release(&mut self, p: Vec2, hotbar: &mut [u8; 9], size: (u32, u32)) {
        self.cursor = p;
        self.thumb_grab = None;
        let Some(from) = self.picked_from.take() else {
            return;
        };
        let to = self.spot(p, size);
        if to == from {
            return;
        }
        let held = mem::replace(&mut self.held, AIR);
        match (to, from) {
            // Dragging between hotbar slots swaps them.
            (Spot::Hotbar(i), Spot::Hotbar(j)) => hotbar[j] = mem::replace(&mut hotbar[i], held),
            (Spot::Hotbar(i), _) => hotbar[i] = held,
            // Off the window or onto the list, the block is thrown away.
            (Spot::Outside | Spot::List(_), _) => {}
            // Anywhere else it goes back where it came from.
            (_, Spot::Hotbar(j)) => hotbar[j] = held,
            _ => {}
        }
    }

    pub fn moved(&mut self, p: Vec2, size: (u32, u32)) {
        self.cursor = p;
        if self.thumb_grab.is_some() {
            self.drag_thumb(p, size);
        }
    }

    fn drag_thumb(&mut self, p: Vec2, size: (u32, u32)) {
        let m = self.max_scroll();
        let Some(grab) = self.thumb_grab else { return };
        if m == 0 {
            return;
        }
        let top = Layout::new(size).to_gui(p).y - grab;
        let f = (top - TRACK[1] - 1.0) / (TRACK[3] - 2.0 - THUMB);
        self.scroll = (f * m as f32).round().clamp(0.0, m as f32) as usize;
    }

    /// Scrolls by whole rows. Positive steps move toward the top of the list.
    pub fn wheel(&mut self, steps: i32) {
        self.scroll = (self.scroll as i32 - steps).clamp(0, self.max_scroll() as i32) as usize;
    }

    /// Draws the window over a dimmed screen. `crystal` colours the crystal icon.
    pub fn build(&self, out: &mut Vec<HudRect>, hotbar: &[u8; 9], size: (u32, u32), crystal: Vec3) {
        let l = Layout::new(size);
        let s = l.scale;
        out.push(HudRect::flat(
            [0.0, 0.0, size.0 as f32, size.1 as f32],
            [0.0, 0.0, 0.0, 0.55],
        ));
        panel(out, &l, [0.0, 0.0, WIDTH, HEIGHT], true);
        let title = rgb(0x404040, 1.0);
        let t = l.rect(8.0, 6.0, 0.0, 0.0);
        font::draw_text(out, "BLOCKS", t[0], t[1], s, title);

        let hover = self.spot(self.cursor, size);
        let slot = |out: &mut Vec<HudRect>, x: f32, y: f32, b: u8, spot: Spot| {
            inset(out, &l, [x, y, SLOT, SLOT]);
            if b != AIR {
                let r = l.rect(x, y, SLOT, SLOT);
                let r_icon = 7.0 * s;
                block_icon(
                    out,
                    b,
                    crystal,
                    r[0] + r[2] * 0.5,
                    r[1] + r[3] * 0.5,
                    r_icon,
                );
            }
            if hover == spot {
                out.push(HudRect::flat(
                    l.rect(x + 1.0, y + 1.0, SLOT - 2.0, SLOT - 2.0),
                    [1.0, 1.0, 1.0, 0.35],
                ));
            }
        };
        for r in 0..ROWS {
            for c in 0..COLS {
                let i = (self.scroll + r) * COLS + c;
                let b = self.items.get(i).copied().unwrap_or(AIR);
                let (x, y) = (GRID.x + c as f32 * SLOT, GRID.y + r as f32 * SLOT);
                slot(out, x, y, b, Spot::List(i));
            }
        }
        for (i, &b) in hotbar.iter().enumerate() {
            slot(
                out,
                HOTBAR.x + i as f32 * SLOT,
                HOTBAR.y,
                b,
                Spot::Hotbar(i),
            );
        }

        inset(out, &l, TRACK);
        let enabled = self.max_scroll() > 0;
        let thumb = [TRACK[0] + 1.0, self.thumb_y(), TRACK[2] - 2.0, THUMB];
        panel(out, &l, thumb, enabled);

        if self.held != AIR {
            let c = self.cursor;
            block_icon(out, self.held, crystal, c.x, c.y, 7.0 * s);
        } else {
            let name = match hover {
                Spot::List(i) => self.items[i],
                Spot::Hotbar(i) => hotbar[i],
                _ => AIR,
            };
            if name != AIR {
                tooltip(out, block::name(name), self.cursor, s, size);
            }
        }
    }
}

/// A raised grey panel with a light top-left edge and a dark bottom-right one. A
/// disabled panel is flat and darker.
fn panel(out: &mut Vec<HudRect>, l: &Layout, [x, y, w, h]: [f32; 4], enabled: bool) {
    let (fill, light, dark) = if enabled {
        (0xC6C6C6, 0xFFFFFF, 0x555555)
    } else {
        (0x8B8B8B, 0xA0A0A0, 0x6A6A6A)
    };
    out.push(HudRect::flat(l.rect(x, y, w, h), rgb(0x000000, 1.0)));
    out.push(HudRect::flat(
        l.rect(x + 1.0, y + 1.0, w - 2.0, h - 2.0),
        rgb(dark, 1.0),
    ));
    out.push(HudRect::flat(
        l.rect(x + 1.0, y + 1.0, w - 3.0, h - 3.0),
        rgb(light, 1.0),
    ));
    out.push(HudRect::flat(
        l.rect(x + 2.0, y + 2.0, w - 4.0, h - 4.0),
        rgb(fill, 1.0),
    ));
}

/// A sunken slot: dark top-left edge, light bottom-right one.
fn inset(out: &mut Vec<HudRect>, l: &Layout, [x, y, w, h]: [f32; 4]) {
    out.push(HudRect::flat(
        l.rect(x, y, w - 1.0, h - 1.0),
        rgb(0x373737, 1.0),
    ));
    out.push(HudRect::flat(
        l.rect(x + 1.0, y + 1.0, w - 1.0, h - 1.0),
        rgb(0xFFFFFF, 1.0),
    ));
    out.push(HudRect::flat(
        l.rect(x + 1.0, y + 1.0, w - 2.0, h - 2.0),
        rgb(0x8B8B8B, 1.0),
    ));
}

/// A block's name in a dark box beside the cursor, kept on screen.
fn tooltip(out: &mut Vec<HudRect>, name: &str, cursor: Vec2, s: f32, size: (u32, u32)) {
    let text = name.to_uppercase();
    let (w, h) = ((text.len() as f32 * 6.0 + 5.0) * s, 11.0 * s);
    let x = (cursor.x + 10.0 * s).min(size.0 as f32 - w);
    let y = (cursor.y - 14.0 * s).max(0.0);
    out.push(HudRect::flat([x, y, w, h], rgb(0x100010, 0.94)));
    out.push(HudRect::flat(
        [x + s, y + s, w - 2.0 * s, h - 2.0 * s],
        rgb(0x2D0A63, 1.0),
    ));
    out.push(HudRect::flat(
        [x + 2.0 * s, y + 2.0 * s, w - 4.0 * s, h - 4.0 * s],
        rgb(0x100010, 1.0),
    ));
    font::draw_text(
        out,
        &text,
        x + 3.0 * s,
        y + 2.0 * s,
        s,
        [1.0, 1.0, 1.0, 1.0],
    );
}

/// A block drawn as a cube seen from above, as three shaded faces of a regular hexagon
/// of radius `r` around (cx, cy). Crystal takes the colour of the layer the player is
/// in, as the block does in the world, so its icon is tinted with `crystal`.
pub fn block_icon(out: &mut Vec<HudRect>, b: u8, crystal: Vec3, cx: f32, cy: f32, r: f32) {
    let tint = if b == CRYSTAL { crystal } else { Vec3::ONE };
    let s = r * 0.866;
    let lit = if is_emissive(b) { 0.3 } else { 0.0 };
    let mut face = |pos: [f32; 2], edges: [f32; 4], dir: u8, shade: f32| {
        let c = tint * (shade + (1.0 - shade) * lit);
        out.push(HudRect {
            pos,
            edges,
            layer: face_layer(b, dir) as f32,
            pad: 0.0,
            color: c.extend(1.0).to_array(),
        });
    };
    let upper_left = [cx - s, cy - r * 0.5];
    face(upper_left, [s, -r * 0.5, s, r * 0.5], 2, 1.0);
    face(upper_left, [s, r * 0.5, 0.0, r], 4, 0.78);
    face([cx, cy], [s, -r * 0.5, 0.0, r], 0, 0.6);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::block::{DIRT, GRASS, SAND, STONE};

    const SIZE: (u32, u32) = (1920, 1080);

    /// The screen position of a point in the window, in GUI pixels.
    fn at(x: f32, y: f32) -> Vec2 {
        let l = Layout::new(SIZE);
        l.origin + Vec2::new(x, y) * l.scale
    }

    fn list_cell(c: usize, r: usize) -> Vec2 {
        at(
            GRID.x + (c as f32 + 0.5) * SLOT,
            GRID.y + (r as f32 + 0.5) * SLOT,
        )
    }

    fn hotbar_cell(i: usize) -> Vec2 {
        at(HOTBAR.x + (i as f32 + 0.5) * SLOT, HOTBAR.y + 0.5 * SLOT)
    }

    fn long_list() -> Inventory {
        let mut inv = Inventory::new((0..100).map(|i| [STONE, DIRT, SAND][i % 3]).collect());
        inv.open = true;
        inv
    }

    #[test]
    fn points_find_their_slots() {
        let inv = long_list();
        assert_eq!(inv.spot(list_cell(0, 0), SIZE), Spot::List(0));
        assert_eq!(inv.spot(list_cell(8, 4), SIZE), Spot::List(44));
        assert_eq!(inv.spot(hotbar_cell(3), SIZE), Spot::Hotbar(3));
        assert_eq!(
            inv.spot(at(TRACK[0] + 2.0, TRACK[1] + 3.0), SIZE),
            Spot::Thumb
        );
        assert_eq!(
            inv.spot(at(TRACK[0] + 2.0, TRACK[1] + 60.0), SIZE),
            Spot::Track
        );
        assert_eq!(inv.spot(at(2.0, 2.0), SIZE), Spot::Window);
        assert_eq!(inv.spot(at(-2.0, 2.0), SIZE), Spot::Outside);
        // Past the end of a short list is just window.
        let short = Inventory::new(vec![STONE, DIRT]);
        assert_eq!(short.spot(list_cell(2, 0), SIZE), Spot::Window);
    }

    #[test]
    fn dragging_a_block_onto_the_hotbar_puts_it_there() {
        let mut inv = long_list();
        let mut hotbar = [GRASS; 9];
        inv.press(list_cell(1, 0), false, &mut hotbar, 0, SIZE);
        assert_eq!(inv.held, DIRT);
        inv.moved(hotbar_cell(4), SIZE);
        inv.release(hotbar_cell(4), &mut hotbar, SIZE);
        assert_eq!(hotbar[4], DIRT);
        assert_eq!(inv.held, AIR);
    }

    #[test]
    fn clicking_picks_up_and_clicking_again_puts_down() {
        let mut inv = long_list();
        let mut hotbar = [GRASS; 9];
        inv.press(list_cell(2, 0), false, &mut hotbar, 0, SIZE);
        inv.release(list_cell(2, 0), &mut hotbar, SIZE);
        assert_eq!(inv.held, SAND, "a click keeps holding the block");
        inv.press(hotbar_cell(0), false, &mut hotbar, 0, SIZE);
        inv.release(hotbar_cell(0), &mut hotbar, SIZE);
        assert_eq!(hotbar[0], SAND);
        assert_eq!(inv.held, GRASS, "the slot's old block is now held");
        inv.press(at(-5.0, -5.0), false, &mut hotbar, 0, SIZE);
        assert_eq!(inv.held, AIR, "clicking outside throws it away");
    }

    #[test]
    fn dragging_between_hotbar_slots_swaps_them() {
        let mut inv = long_list();
        let mut hotbar = [STONE, DIRT, AIR, AIR, AIR, AIR, AIR, AIR, AIR];
        inv.press(hotbar_cell(0), false, &mut hotbar, 0, SIZE);
        inv.release(hotbar_cell(1), &mut hotbar, SIZE);
        assert_eq!(&hotbar[..2], &[DIRT, STONE]);
        // Let go somewhere useless, and the block goes home.
        inv.press(hotbar_cell(0), false, &mut hotbar, 0, SIZE);
        inv.release(at(2.0, 2.0), &mut hotbar, SIZE);
        assert_eq!(hotbar[0], DIRT);
        // Let go outside, and it's gone.
        inv.press(hotbar_cell(0), false, &mut hotbar, 0, SIZE);
        inv.release(at(-5.0, -5.0), &mut hotbar, SIZE);
        assert_eq!(hotbar[0], AIR);
    }

    #[test]
    fn shift_click_fills_the_first_empty_slot() {
        let mut inv = long_list();
        let mut hotbar = [STONE, STONE, AIR, AIR, AIR, AIR, AIR, AIR, AIR];
        inv.press(list_cell(2, 0), true, &mut hotbar, 0, SIZE);
        assert_eq!(hotbar[2], SAND);
        assert_eq!(inv.held, AIR);
    }

    #[test]
    fn the_wheel_and_the_scrollbar_move_through_a_long_list() {
        let mut inv = long_list();
        let last = inv.max_scroll();
        assert_eq!(last, 12 - ROWS, "100 blocks make 12 rows");
        inv.wheel(-2);
        assert_eq!(inv.spot(list_cell(0, 0), SIZE), Spot::List(18));
        inv.wheel(-100);
        assert_eq!(inv.scroll, last);
        inv.wheel(100);
        assert_eq!(inv.scroll, 0);

        // Dragging the thumb to the bottom of the track reaches the end.
        let mut hotbar = [AIR; 9];
        inv.press(
            at(TRACK[0] + 2.0, TRACK[1] + 3.0),
            false,
            &mut hotbar,
            0,
            SIZE,
        );
        inv.moved(at(TRACK[0] + 2.0, TRACK[1] + TRACK[3] + 20.0), SIZE);
        assert_eq!(inv.scroll, last);
        inv.release(at(0.0, 0.0), &mut hotbar, SIZE);
        inv.moved(at(TRACK[0] + 2.0, TRACK[1]), SIZE);
        assert_eq!(inv.scroll, last, "letting go of the thumb stops the drag");
        // Clicking the track jumps there.
        inv.press(
            at(TRACK[0] + 2.0, TRACK[1] + 10.0),
            false,
            &mut hotbar,
            0,
            SIZE,
        );
        assert_eq!(inv.scroll, 0);
    }

    #[test]
    fn slot_centres_lead_back_to_their_slots() {
        let mut inv = long_list();
        inv.wheel(-3);
        for spot in [
            Spot::List(27),
            Spot::List(71),
            Spot::Hotbar(0),
            Spot::Hotbar(8),
        ] {
            let p = inv.slot_centre(spot, SIZE).unwrap();
            assert_eq!(inv.spot(p, SIZE), spot);
        }
        assert_eq!(
            inv.slot_centre(Spot::List(0), SIZE),
            None,
            "scrolled out of view"
        );
        assert_eq!(
            inv.slot_centre(Spot::List(72), SIZE),
            None,
            "not yet in view"
        );
        assert_eq!(inv.slot_centre(Spot::Hotbar(9), SIZE), None);
    }

    #[test]
    fn a_short_list_does_not_scroll() {
        let mut inv = Inventory::new(block::CREATIVE.to_vec());
        inv.wheel(-3);
        assert_eq!(inv.scroll, 0);
    }

    #[test]
    fn closing_drops_the_held_block() {
        let mut inv = long_list();
        let mut hotbar = [AIR; 9];
        inv.press(list_cell(0, 0), false, &mut hotbar, 0, SIZE);
        inv.close();
        assert!(!inv.open && inv.held == AIR);
        assert_eq!(hotbar, [AIR; 9]);
    }
}
