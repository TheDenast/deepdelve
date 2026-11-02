//! Player movement against the voxel grid: walking, jumping, swimming and creative
//! flight, as in Minecraft. Free flight through everything (noclip) stays in `game.rs`,
//! since it ignores the world.

use crate::world::block::{LAVA, is_liquid, is_opaque};
use glam::DVec3;

/// The player's box, in blocks. Minecraft's sizes, so that one-block gaps and
/// two-block doorways work as players expect.
pub const WIDTH: f64 = 0.6;
pub const HEIGHT: f64 = 1.8;
/// Eye height above the feet. The game's position is the eye.
pub const EYE: f64 = 1.62;

const GRAVITY: f64 = 32.0;
const TERMINAL: f64 = 78.0;
/// Reaches about 1.27 blocks: one block up, but not two.
const JUMP: f64 = 9.0;
const WALK: f64 = 4.3;
const SPRINT: f64 = 5.6;
const SNEAK: f64 = 1.3;
/// How quickly velocity follows what the keys ask for, per second. Low in the air, so
/// that a jump keeps its momentum.
const GROUND_ACCEL: f64 = 14.0;
const AIR_ACCEL: f64 = 3.0;
const FLY: f64 = 11.0;
const FLY_VERTICAL: f64 = 8.0;
/// Caverns are kilometres across, so sprinting flies faster than Minecraft's double.
const FLY_SPRINT: f64 = 3.0;
const FLY_ACCEL: f64 = 6.0;
const SWIM: f64 = 2.2;
const SWIM_VERTICAL: f64 = 4.0;
const SINK: f64 = 1.0;
const SWIM_ACCEL: f64 = 6.0;
/// Swimming up against a wall climbs fast enough to step out onto a ledge one block
/// above the surface.
const CLIMB_OUT: f64 = 8.5;
/// Lava is thicker than water.
const LAVA_SLOWDOWN: f64 = 0.5;
/// How far below its feet a sneaking player looks for ground before refusing a step.
const SNEAK_DROP: f64 = 0.6;
/// Steps short enough that a fall at terminal speed moves under one block per step.
const MAX_STEP: f64 = 1.0 / 120.0;
/// Keeps boxes that touch a face exactly from counting as overlapping it.
const EPS: f64 = 1e-6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Gravity and collision.
    Walk,
    /// Creative flight: no gravity, but still collision. Landing ends it.
    Fly,
    /// Free flight through everything.
    Noclip,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Walk => "WALK",
            Mode::Fly => "FLY",
            Mode::Noclip => "NOCLIP",
        }
    }
}

/// What the keys ask for this step.
#[derive(Clone, Copy, Debug, Default)]
pub struct Wish {
    /// Horizontal direction in world space, of length up to 1.
    pub dir: DVec3,
    /// From -1 to 1. Up jumps, swims up or climbs; down sneaks, sinks or descends.
    pub up: f64,
    pub sprint: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Body {
    pub mode: Mode,
    pub vel: DVec3,
    pub on_ground: bool,
    /// Set when the last step waited for chunks to load.
    pub waiting: bool,
}

/// What a block lookup says about one cell.
enum Cell {
    Unloaded,
    Solid,
    Clear,
}

fn cell(world: &impl Fn(i64, i64, i64) -> Option<u8>, x: i64, y: i64, z: i64) -> Cell {
    match world(x, y, z) {
        None => Cell::Unloaded,
        Some(b) if is_opaque(b) => Cell::Solid,
        Some(_) => Cell::Clear,
    }
}

/// An axis-aligned box in world blocks.
#[derive(Clone, Copy, Debug)]
pub struct Aabb {
    pub min: DVec3,
    pub max: DVec3,
}

impl Aabb {
    /// The player's box for an eye position.
    pub fn player(eye: DVec3) -> Self {
        let feet = DVec3::new(eye.x, eye.y - EYE, eye.z);
        let half = DVec3::new(WIDTH * 0.5, 0.0, WIDTH * 0.5);
        Self {
            min: feet - half,
            max: feet + half + DVec3::Y * HEIGHT,
        }
    }

    /// The cells this box overlaps, as inclusive ranges per axis.
    fn cells(&self) -> ([i64; 3], [i64; 3]) {
        let lo = (self.min + EPS).floor().as_i64vec3().to_array();
        let hi = (self.max - EPS).floor().as_i64vec3().to_array();
        (lo, hi)
    }

    pub fn overlaps_cell(&self, c: [i64; 3]) -> bool {
        let (lo, hi) = self.cells();
        (0..3).all(|i| lo[i] <= c[i] && c[i] <= hi[i])
    }

    /// `None` if any of the cells isn't loaded.
    fn any_solid(&self, world: &impl Fn(i64, i64, i64) -> Option<u8>) -> Option<bool> {
        let (lo, hi) = self.cells();
        let mut solid = false;
        for y in lo[1]..=hi[1] {
            for z in lo[2]..=hi[2] {
                for x in lo[0]..=hi[0] {
                    match cell(world, x, y, z) {
                        Cell::Unloaded => return None,
                        Cell::Solid => solid = true,
                        Cell::Clear => {}
                    }
                }
            }
        }
        Some(solid)
    }

    /// How far the box can move along one axis, up to `delta`, before touching a solid
    /// cell. `None` if the cells it would pass through aren't loaded.
    fn sweep(
        &self,
        axis: usize,
        delta: f64,
        world: &impl Fn(i64, i64, i64) -> Option<u8>,
    ) -> Option<f64> {
        if delta == 0.0 {
            return Some(0.0);
        }
        let (lo, hi) = self.cells();
        let (a, b) = ((axis + 1) % 3, (axis + 2) % 3);
        // The layers of cells the leading face enters, nearest first.
        let (first, last, step) = if delta > 0.0 {
            let face = self.max[axis];
            (hi[axis] + 1, (face + delta - EPS).floor() as i64, 1)
        } else {
            let face = self.min[axis];
            (lo[axis] - 1, (face + delta + EPS).floor() as i64, -1)
        };
        let mut layer = first;
        while (step > 0 && layer <= last) || (step < 0 && layer >= last) {
            let mut solid = false;
            for i in lo[a]..=hi[a] {
                for j in lo[b]..=hi[b] {
                    let mut c = [0; 3];
                    c[axis] = layer;
                    c[a] = i;
                    c[b] = j;
                    match cell(world, c[0], c[1], c[2]) {
                        Cell::Unloaded => return None,
                        Cell::Solid => solid = true,
                        Cell::Clear => {}
                    }
                }
            }
            if solid {
                return Some(if step > 0 {
                    layer as f64 - self.max[axis]
                } else {
                    (layer + 1) as f64 - self.min[axis]
                });
            }
            layer += step;
        }
        Some(delta)
    }

    fn shifted(&self, d: DVec3) -> Self {
        Self {
            min: self.min + d,
            max: self.max + d,
        }
    }
}

/// Moves `v` toward `target` at `rate` per second, independently of the step length.
fn approach(v: f64, target: f64, rate: f64, dt: f64) -> f64 {
    v + (target - v) * (1.0 - (-rate * dt).exp())
}

impl Body {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            vel: DVec3::ZERO,
            on_ground: false,
            waiting: false,
        }
    }

    /// Advances `eye` by `dt` seconds. Does nothing in noclip, which the caller handles.
    pub fn step(
        &mut self,
        eye: &mut DVec3,
        dt: f64,
        wish: &Wish,
        world: &impl Fn(i64, i64, i64) -> Option<u8>,
    ) {
        if self.mode == Mode::Noclip {
            return;
        }
        let n = (dt / MAX_STEP).ceil().max(1.0);
        for _ in 0..n as usize {
            self.substep(eye, dt / n, wish, world);
        }
    }

    /// The liquid at the player's feet, if any. Swimming lasts until the feet leave it,
    /// which leaves the head above the surface while treading water.
    pub fn liquid_at_feet(eye: DVec3, world: &impl Fn(i64, i64, i64) -> Option<u8>) -> Option<u8> {
        let f = (eye - DVec3::Y * EYE).floor().as_i64vec3();
        world(f.x, f.y, f.z).filter(|&b| is_liquid(b))
    }

    fn substep(
        &mut self,
        eye: &mut DVec3,
        dt: f64,
        wish: &Wish,
        world: &impl Fn(i64, i64, i64) -> Option<u8>,
    ) {
        let bx = Aabb::player(*eye);
        let embedded = match bx.any_solid(world) {
            None => return self.wait(),
            Some(e) => e,
        };
        self.waiting = false;
        let liquid = Self::liquid_at_feet(*eye, world).filter(|_| self.mode == Mode::Walk);
        let sprint = if wish.sprint { FLY_SPRINT } else { 1.0 };

        let v = &mut self.vel;
        if embedded {
            // Somewhere inside rock, such as after noclip: drift out slowly in any
            // direction, without gravity, until the box is clear again.
            *v = (wish.dir + DVec3::Y * wish.up) * WALK;
            *eye += *v * dt;
            self.on_ground = false;
            return;
        }
        match (self.mode, liquid) {
            (Mode::Fly, _) => {
                let target = wish.dir * FLY * sprint;
                v.x = approach(v.x, target.x, FLY_ACCEL, dt);
                v.z = approach(v.z, target.z, FLY_ACCEL, dt);
                v.y = approach(v.y, wish.up * FLY_VERTICAL * sprint, FLY_ACCEL, dt);
            }
            (_, Some(b)) => {
                let thick = if b == LAVA { LAVA_SLOWDOWN } else { 1.0 };
                let target = wish.dir * SWIM * thick;
                v.x = approach(v.x, target.x, SWIM_ACCEL, dt);
                v.z = approach(v.z, target.z, SWIM_ACCEL, dt);
                let up = if wish.up == 0.0 {
                    -SINK
                } else {
                    wish.up * SWIM_VERTICAL
                };
                v.y = approach(v.y, up * thick, SWIM_ACCEL, dt);
            }
            _ => {
                let sneaking = wish.up < 0.0 && self.on_ground;
                let speed = if sneaking {
                    SNEAK
                } else if wish.sprint {
                    SPRINT
                } else {
                    WALK
                };
                let target = wish.dir * speed;
                let rate = if self.on_ground {
                    GROUND_ACCEL
                } else {
                    AIR_ACCEL
                };
                v.x = approach(v.x, target.x, rate, dt);
                v.z = approach(v.z, target.z, rate, dt);
                v.y = (v.y - GRAVITY * dt).max(-TERMINAL);
                if self.on_ground && wish.up > 0.0 {
                    v.y = JUMP;
                }
            }
        }

        // Vertical first, as Minecraft does, so that landing settles before sliding.
        let delta = *v * dt;
        let Some(dy) = bx.sweep(1, delta.y, world) else {
            return self.wait();
        };
        let blocked_y = dy != delta.y;
        let mut bx = bx.shifted(DVec3::Y * dy);
        self.on_ground = blocked_y && delta.y < 0.0;
        if blocked_y {
            self.vel.y = 0.0;
        }
        let sneaking =
            self.mode == Mode::Walk && liquid.is_none() && self.on_ground && wish.up < 0.0;
        let mut blocked_side = false;
        for axis in [0, 2] {
            let Some(d) = bx.sweep(axis, delta[axis], world) else {
                return self.wait();
            };
            if d != delta[axis] {
                blocked_side = true;
                self.vel[axis] = 0.0;
            }
            let mut moved = DVec3::ZERO;
            moved[axis] = d;
            if sneaking && d != 0.0 {
                // Refuse a step that leaves nothing underfoot.
                let ahead = bx.shifted(moved);
                let below = Aabb {
                    min: DVec3::new(ahead.min.x, ahead.min.y - SNEAK_DROP, ahead.min.z),
                    max: DVec3::new(ahead.max.x, ahead.min.y, ahead.max.z),
                };
                if below.any_solid(world) != Some(true) {
                    moved[axis] = 0.0;
                    self.vel[axis] = 0.0;
                }
            }
            bx = bx.shifted(moved);
        }
        *eye = DVec3::new(
            (bx.min.x + bx.max.x) * 0.5,
            bx.min.y + EYE,
            (bx.min.z + bx.max.z) * 0.5,
        );

        if liquid.is_some() && blocked_side && wish.up > 0.0 {
            self.vel.y = CLIMB_OUT
                * if liquid == Some(LAVA) {
                    LAVA_SLOWDOWN
                } else {
                    1.0
                };
        }
        if self.mode == Mode::Fly && self.on_ground {
            self.mode = Mode::Walk;
        }
    }

    fn wait(&mut self) {
        self.vel = DVec3::ZERO;
        self.waiting = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::block::{AIR, STONE, WATER};

    const DT: f64 = 1.0 / 60.0;

    /// Solid below y = 0, open above.
    fn floor(_x: i64, y: i64, _z: i64) -> Option<u8> {
        Some(if y < 0 { STONE } else { AIR })
    }

    fn feet(eye: DVec3) -> f64 {
        eye.y - EYE
    }

    fn run(
        body: &mut Body,
        eye: &mut DVec3,
        seconds: f64,
        wish: Wish,
        world: &impl Fn(i64, i64, i64) -> Option<u8>,
    ) {
        for _ in 0..(seconds / DT) as usize {
            body.step(eye, DT, &wish, world);
        }
    }

    fn standing_at(x: f64, z: f64) -> DVec3 {
        DVec3::new(x, EYE, z)
    }

    #[test]
    fn falls_and_lands_exactly_on_the_floor() {
        let mut b = Body::new(Mode::Walk);
        let mut eye = DVec3::new(0.5, 20.0, 0.5);
        run(&mut b, &mut eye, 3.0, Wish::default(), &floor);
        assert!(b.on_ground);
        assert!(feet(eye).abs() < 1e-9, "feet at {}", feet(eye));
    }

    #[test]
    fn a_jump_clears_one_block_but_not_two() {
        let mut b = Body::new(Mode::Walk);
        let mut eye = standing_at(0.5, 0.5);
        run(&mut b, &mut eye, 0.1, Wish::default(), &floor);
        let jump = Wish {
            up: 1.0,
            ..Default::default()
        };
        b.step(&mut eye, DT, &jump, &floor);
        let mut peak: f64 = 0.0;
        for _ in 0..60 {
            b.step(&mut eye, DT, &Wish::default(), &floor);
            peak = peak.max(feet(eye));
        }
        assert!(peak > 1.1 && peak < 1.5, "jump peaked at {peak}");
        assert!(b.on_ground && feet(eye).abs() < 1e-9);
    }

    #[test]
    fn walls_stop_movement_and_can_be_jumped_onto() {
        // One block step at x >= 3.
        let step = |x: i64, y: i64, _z: i64| {
            Some(if y < 0 || (x >= 3 && y < 1) {
                STONE
            } else {
                AIR
            })
        };
        let mut b = Body::new(Mode::Walk);
        let mut eye = standing_at(0.5, 0.5);
        let forward = Wish {
            dir: DVec3::X,
            ..Default::default()
        };
        run(&mut b, &mut eye, 2.0, forward, &step);
        assert!((eye.x + WIDTH * 0.5 - 3.0).abs() < 1e-9, "x {}", eye.x);
        let jump_forward = Wish { up: 1.0, ..forward };
        run(&mut b, &mut eye, 1.0, jump_forward, &step);
        assert!(eye.x > 3.5, "should be on the step: x {}", eye.x);
        assert!((feet(eye) - 1.0).abs() < 1e-9 || !b.on_ground);
    }

    #[test]
    fn sneaking_stops_at_an_edge() {
        // A platform ending at x = 2, with a long drop after it.
        let ledge = |x: i64, y: i64, _z: i64| Some(if y < 0 && x < 2 { STONE } else { AIR });
        let mut b = Body::new(Mode::Walk);
        let mut eye = standing_at(0.5, 0.5);
        run(&mut b, &mut eye, 0.1, Wish::default(), &ledge);
        let sneak = Wish {
            dir: DVec3::X,
            up: -1.0,
            ..Default::default()
        };
        run(&mut b, &mut eye, 3.0, sneak, &ledge);
        assert!(b.on_ground && feet(eye).abs() < 1e-9);
        assert!(
            eye.x - WIDTH * 0.5 < 2.0,
            "box must still overlap the platform"
        );
        let walk = Wish {
            dir: DVec3::X,
            ..Default::default()
        };
        run(&mut b, &mut eye, 1.0, walk, &ledge);
        assert!(feet(eye) < -1.0, "without sneaking the player walks off");
    }

    #[test]
    fn unloaded_chunks_hold_the_player_still() {
        let unloaded = |_x: i64, y: i64, _z: i64| if y < 10 { None } else { Some(AIR) };
        let mut b = Body::new(Mode::Walk);
        let start = DVec3::new(0.5, 12.0, 0.5);
        let mut eye = start;
        run(&mut b, &mut eye, 1.0, Wish::default(), &unloaded);
        assert!(b.waiting);
        assert!(
            eye.y >= 10.0 + EYE - 1e-9,
            "must not fall into unloaded cells"
        );
    }

    #[test]
    fn landing_ends_flight() {
        let mut b = Body::new(Mode::Fly);
        let mut eye = DVec3::new(0.5, 5.0, 0.5);
        run(&mut b, &mut eye, 1.0, Wish::default(), &floor);
        assert_eq!(b.mode, Mode::Fly, "flight hovers without input");
        assert!((eye.y - 5.0).abs() < 1e-9);
        let down = Wish {
            up: -1.0,
            ..Default::default()
        };
        run(&mut b, &mut eye, 2.0, down, &floor);
        assert_eq!(b.mode, Mode::Walk);
    }

    #[test]
    fn swimmers_can_climb_out_onto_a_ledge() {
        // Water below y = 0 for x < 4. A shore at x >= 4 whose top is one above the surface.
        let lake = |x: i64, y: i64, _z: i64| {
            Some(if y < -6 || (x >= 4 && y < 1) {
                STONE
            } else if y < 0 {
                WATER
            } else {
                AIR
            })
        };
        let mut b = Body::new(Mode::Walk);
        let mut eye = DVec3::new(1.5, -3.0, 0.5);
        run(&mut b, &mut eye, 1.0, Wish::default(), &lake);
        assert!(eye.y < -3.0, "must sink slowly when idle");
        let swim = Wish {
            dir: DVec3::X,
            up: 1.0,
            ..Default::default()
        };
        run(&mut b, &mut eye, 4.0, swim, &lake);
        assert!(eye.x > 4.0 && feet(eye) >= 1.0 - 1e-9, "eye at {eye}");
    }

    #[test]
    fn a_player_inside_rock_can_move_out() {
        let mut b = Body::new(Mode::Walk);
        let mut eye = DVec3::new(0.5, -1.0, 0.5);
        let up = Wish {
            up: 1.0,
            ..Default::default()
        };
        run(&mut b, &mut eye, 2.0, up, &floor);
        assert!(feet(eye) >= 0.0, "feet at {}", feet(eye));
        run(&mut b, &mut eye, 2.0, Wish::default(), &floor);
        assert!(b.on_ground && feet(eye).abs() < 1e-9);
    }

    #[test]
    fn the_box_matches_cells_it_touches() {
        let bx = Aabb::player(standing_at(0.5, 0.5));
        assert!(bx.overlaps_cell([0, 0, 0]) && bx.overlaps_cell([0, 1, 0]));
        assert!(!bx.overlaps_cell([0, -1, 0]) && !bx.overlaps_cell([0, 2, 0]));
        assert!(!bx.overlaps_cell([1, 0, 0]));
    }
}
