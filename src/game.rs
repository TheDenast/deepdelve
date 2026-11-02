//! Game state that doesn't depend on a window: camera, streaming and editing.

use crate::render::{ChunkGpu, FrameParams, HudRect, font};
use crate::stream::{ChunkSink, Streamer};
use crate::world::block::{
    self, AIR, COBBLE, CRYSTAL, DIRT, GRASS, LAVA, LEAVES, LOG, SAND, SLATE, STONE, WATER,
    face_layer, is_breakable, is_emissive, is_opaque, is_placeable,
};
use crate::world::generator::{WORLD_BOTTOM, WORLD_TOP};
use crate::world::noise::{smoothstep, splitmix};
use glam::{DVec3, Mat4, Vec2, Vec3, Vec4};

const REACH: f64 = 96.0;
const BREAK_REPEAT: f32 = 0.12;
/// Slower than breaking, so that a click places one block rather than two.
const PLACE_REPEAT: f32 = 0.2;
const FOV_Y_DEG: f32 = 72.0;
/// How long the selected block's name stays above the hotbar.
const NAME_SHOWN: f32 = 2.0;
const HOTBAR: [u8; 9] = [
    STONE, COBBLE, DIRT, GRASS, SAND, LOG, LEAVES, SLATE, CRYSTAL,
];

#[derive(Default, Clone, Copy)]
pub struct Input {
    /// x: strafe, y: up/down, z: forward. Each in -1..=1.
    pub axis: Vec3,
    pub sprint: bool,
    pub look: Vec2,
    pub scroll: f32,
    pub breaking: bool,
    pub placing: bool,
    /// Takes the targeted block into the hotbar. Set for one step per click.
    pub pick: bool,
    /// A hotbar slot chosen directly, from the number keys.
    pub slot: Option<usize>,
    pub reset: bool,
}

/// What the crosshair points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub block: [i64; 3],
    /// The cell the ray crossed just before, where a placed block goes.
    pub before: [i64; 3],
}

pub struct Game {
    pub streamer: Streamer,
    pub seed: u64,
    pub pos: DVec3,
    pub yaw: f32,
    pub pitch: f32,
    pub speed: f32,
    pub time: f32,
    pub target: Option<Hit>,
    pub blocks_broken: u64,
    pub blocks_placed: u64,
    pub last_remesh: Option<(usize, f32)>,
    pub occlusion: bool,
    pub hotbar: [u8; 9],
    pub slot: usize,
    slot_changed_at: f32,
    break_timer: f32,
    place_timer: f32,
    fog: Vec3,
    ambient: Vec3,
    fog_distance: f32,
    fog_primed: bool,
}

impl Game {
    pub fn new(seed: u64, workers: usize, pool_quads: u32, max_slots: u32) -> Self {
        let streamer = Streamer::new(seed, workers, pool_quads, max_slots);
        let pos = DVec3::from_array(streamer.generator().find_spawn(0));
        Self {
            streamer,
            seed,
            pos,
            yaw: 0.0,
            pitch: 0.0,
            speed: 40.0,
            time: 0.0,
            target: None,
            blocks_broken: 0,
            blocks_placed: 0,
            last_remesh: None,
            occlusion: true,
            hotbar: HOTBAR,
            slot: 0,
            slot_changed_at: f32::MIN,
            break_timer: 0.0,
            place_timer: 0.0,
            fog: Vec3::ZERO,
            ambient: Vec3::ZERO,
            fog_distance: 2000.0,
            fog_primed: false,
        }
    }

    /// Wipes the world and starts a new one from a derived seed.
    pub fn new_world(&mut self) {
        self.seed = splitmix(self.seed ^ self.time.to_bits() as u64);
        self.start_world(self.seed);
    }

    pub fn start_world(&mut self, seed: u64) {
        self.seed = seed;
        self.streamer.reset(seed);
        self.pos = DVec3::from_array(self.streamer.generator().find_spawn(0));
        self.yaw = 0.0;
        self.pitch = 0.0;
        self.target = None;
        self.blocks_broken = 0;
        self.blocks_placed = 0;
        self.last_remesh = None;
        self.fog_primed = false;
    }

    pub fn forward(&self) -> Vec3 {
        Vec3::new(
            self.yaw.sin() * self.pitch.cos(),
            self.pitch.sin(),
            -self.yaw.cos() * self.pitch.cos(),
        )
    }

    pub fn view_matrix(&self) -> Mat4 {
        let f = self.forward();
        let s = f.cross(Vec3::Y).normalize();
        let u = s.cross(f);
        Mat4::from_cols(
            Vec4::new(s.x, u.x, -f.x, 0.0),
            Vec4::new(s.y, u.y, -f.y, 0.0),
            Vec4::new(s.z, u.z, -f.z, 0.0),
            Vec4::W,
        )
    }

    /// One game step. Returns true when the world was reset.
    pub fn update(&mut self, dt: f32, input: &Input, gpu: &mut ChunkGpu) -> bool {
        self.time += dt;
        let mut reset = false;
        if input.reset {
            self.new_world();
            reset = true;
        }

        self.yaw += input.look.x;
        self.pitch = (self.pitch - input.look.y).clamp(-1.55, 1.55);
        if input.scroll != 0.0 {
            self.speed = (self.speed * 1.25f32.powf(input.scroll)).clamp(2.0, 4000.0);
        }

        // Flying is relative to where the camera faces; up and down stay vertical.
        let f = self.forward();
        let right = Vec3::new(self.yaw.cos(), 0.0, self.yaw.sin());
        let dir = f * input.axis.z + right * input.axis.x + Vec3::Y * input.axis.y;
        let v = self.speed * if input.sprint { 6.0 } else { 1.0 };
        self.pos += dir.as_dvec3() * (v * dt) as f64;

        self.streamer
            .update(self.pos.to_array(), f.to_array(), dt as f64, 4000, gpu);

        if let Some(s) = input.slot {
            self.select(s);
        }

        self.target = self.raycast();
        self.break_timer -= dt;
        if input.breaking && self.break_timer <= 0.0 {
            if let Some(t) = self.target {
                self.break_block(t.block, gpu);
                self.break_timer = BREAK_REPEAT;
            }
        } else if !input.breaking {
            self.break_timer = 0.0;
        }
        self.place_timer -= dt;
        if input.placing && self.place_timer <= 0.0 {
            if let Some(t) = self.target {
                self.place_block(t.before, self.hotbar[self.slot], gpu);
                self.place_timer = PLACE_REPEAT;
                // A new block changes what the crosshair points at.
                self.target = self.raycast();
            }
        } else if !input.placing {
            self.place_timer = 0.0;
        }
        if input.pick
            && let Some(t) = self.target
        {
            self.pick_block(t.block);
        }

        self.update_atmosphere(dt);
        reset
    }

    pub fn select(&mut self, slot: usize) {
        self.slot = slot % self.hotbar.len();
        self.slot_changed_at = self.time;
    }

    /// Selects the slot holding this block, or puts it in the current one.
    pub fn pick_block(&mut self, b: [i64; 3]) {
        let Some(block) = self.streamer.block_at(b[0], b[1], b[2]) else {
            return;
        };
        if !is_placeable(block) {
            return;
        }
        match self.hotbar.iter().position(|&h| h == block) {
            Some(i) => self.select(i),
            None => {
                self.hotbar[self.slot] = block;
                self.select(self.slot);
            }
        }
    }

    /// Puts a block into an empty or liquid cell.
    pub fn place_block(&mut self, at: [i64; 3], block: u8, sink: &mut impl ChunkSink) {
        let start = std::time::Instant::now();
        if !is_placeable(block)
            || self
                .streamer
                .block_at(at[0], at[1], at[2])
                .is_none_or(is_opaque)
        {
            return;
        }
        if let Some(n) = self.streamer.set_block(at[0], at[1], at[2], block, sink) {
            self.blocks_placed += 1;
            self.last_remesh = Some((n, start.elapsed().as_secs_f32() * 1000.0));
        }
    }

    pub fn break_block(&mut self, t: [i64; 3], sink: &mut impl ChunkSink) {
        let start = std::time::Instant::now();
        if !self
            .streamer
            .block_at(t[0], t[1], t[2])
            .is_some_and(is_breakable)
        {
            return;
        }
        if let Some(n) = self.streamer.set_block(t[0], t[1], t[2], AIR, sink) {
            self.blocks_broken += 1;
            self.last_remesh = Some((n, start.elapsed().as_secs_f32() * 1000.0));
        }
    }

    /// Steps through the voxel grid until it hits something solid.
    pub fn raycast(&self) -> Option<Hit> {
        let d = self.forward().as_dvec3();
        let mut p = self.pos.floor().as_i64vec3();
        let step = d.signum().as_i64vec3();
        let inv = DVec3::new(
            1.0 / d.x.abs().max(1e-12),
            1.0 / d.y.abs().max(1e-12),
            1.0 / d.z.abs().max(1e-12),
        );
        let frac = self.pos - self.pos.floor();
        let mut t_max = DVec3::new(
            if d.x > 0.0 { 1.0 - frac.x } else { frac.x } * inv.x,
            if d.y > 0.0 { 1.0 - frac.y } else { frac.y } * inv.y,
            if d.z > 0.0 { 1.0 - frac.z } else { frac.z } * inv.z,
        );
        let mut t = 0.0;
        let mut before = p;
        while t < REACH {
            let b = self.streamer.block_at(p.x, p.y, p.z)?;
            if is_opaque(b) {
                return Some(Hit {
                    block: p.to_array(),
                    before: before.to_array(),
                });
            }
            before = p;
            if t_max.x < t_max.y && t_max.x < t_max.z {
                p.x += step.x;
                t = t_max.x;
                t_max.x += inv.x;
            } else if t_max.y < t_max.z {
                p.y += step.y;
                t = t_max.y;
                t_max.y += inv.y;
            } else {
                p.z += step.z;
                t = t_max.z;
                t_max.z += inv.z;
            }
        }
        None
    }

    /// Fog and ambient follow the player's surroundings, easing slowly so that
    /// moving between caverns reads as the air itself changing.
    fn update_atmosphere(&mut self, dt: f32) {
        let g = self.streamer.generator();
        let t = g.tier_near(self.pos.y);
        let f = g.field(t, self.pos.x, self.pos.z);
        let air = Vec3::from_array(t.theme.fog);
        let inside = smoothstep(0.0, 0.8, f.open)
            * smoothstep(f.floor - 30.0, f.floor + 5.0, self.pos.y as f32)
            * (1.0 - smoothstep(f.ceil - 5.0, f.ceil + 30.0, self.pos.y as f32));
        let glow = 0.5 + 0.5 * f.glow;
        // Beyond the bedrock there's nothing to light the air.
        let in_world = self.pos.y < WORLD_TOP as f64 && self.pos.y >= WORLD_BOTTOM as f64;
        let (fog_target, amb_target, distance_target) = if in_world {
            let brightness = 0.1 + 0.9 * inside * glow;
            (
                air * brightness,
                air * 2.4 * brightness,
                t.theme.fog_distance,
            )
        } else {
            (Vec3::ZERO, Vec3::ZERO, 6000.0)
        };
        let k = if self.fog_primed {
            1.0 - (-dt * 0.8).exp()
        } else {
            1.0
        };
        self.fog = self.fog.lerp(fog_target, k);
        self.ambient = self.ambient.lerp(amb_target, k);
        self.fog_distance += (distance_target - self.fog_distance) * k;
        self.fog_primed = true;
    }

    pub fn frame_params<'a>(&self, hud: &'a [HudRect]) -> FrameParams<'a> {
        let b = self.pos.floor().as_i64vec3();
        let inside = self.streamer.block_at(b.x, b.y, b.z);
        FrameParams {
            cam_pos: self.pos,
            view: self.view_matrix(),
            fov_y: FOV_Y_DEG.to_radians(),
            time: self.time,
            fog_color: self.fog,
            fog_distance: self.fog_distance,
            ambient: self.ambient,
            underwater: inside == Some(WATER),
            in_lava: inside == Some(LAVA),
            outline: self.target.map(|t| t.block),
            occlusion: self.occlusion,
            hud,
        }
    }

    /// Nine slots along the bottom of the screen, with the selected block's name above
    /// them for a moment after it changes.
    fn build_hotbar(&self, out: &mut Vec<HudRect>, w: f32, h: f32) {
        // Grows in whole steps with the screen, like Minecraft's GUI scale.
        let scale = (h / 360.0).floor().max(1.0);
        // Crystal takes the colour of the layer it's in, so the icon shows this one's.
        let c = self
            .streamer
            .generator()
            .tier_near(self.pos.y)
            .theme
            .crystal;
        let crystal = Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32).powf(2.0);
        let crystal = crystal / crystal.max_element().max(1.0);
        let slot = 20.0 * scale;
        let n = self.hotbar.len() as f32;
        let x0 = (w * 0.5 - slot * n * 0.5).round();
        let y0 = h - slot - 4.0 * scale;
        out.push(HudRect::flat(
            [
                x0 - scale,
                y0 - scale,
                slot * n + 2.0 * scale,
                slot + 2.0 * scale,
            ],
            [0.0, 0.0, 0.0, 0.45],
        ));
        for (i, &b) in self.hotbar.iter().enumerate() {
            let x = x0 + i as f32 * slot;
            out.push(HudRect::flat(
                [
                    x + scale,
                    y0 + scale,
                    slot - 2.0 * scale,
                    slot - 2.0 * scale,
                ],
                [0.3, 0.3, 0.3, 0.35],
            ));
            let tint = if b == CRYSTAL { crystal } else { Vec3::ONE };
            block_icon(out, b, tint, x + slot * 0.5, y0 + slot * 0.5, slot * 0.36);
        }
        // The selection frame, two pixels wide and a little outside its slot.
        let (fx, fy) = (x0 + self.slot as f32 * slot - scale, y0 - scale);
        let (fs, ft) = (slot + 2.0 * scale, 2.0 * scale);
        let white = [1.0, 1.0, 1.0, 0.9];
        for r in [
            [fx, fy, fs, ft],
            [fx, fy + fs - ft, fs, ft],
            [fx, fy, ft, fs],
            [fx + fs - ft, fy, ft, fs],
        ] {
            out.push(HudRect::flat(r, white));
        }

        let age = self.time - self.slot_changed_at;
        if age < NAME_SHOWN {
            let alpha = smoothstep(NAME_SHOWN, NAME_SHOWN - 0.5, age);
            let text = block::name(self.hotbar[self.slot]).to_uppercase();
            let tw = text.len() as f32 * 6.0 * scale - scale;
            let (x, y) = ((w * 0.5 - tw * 0.5).round(), y0 - 12.0 * scale);
            font::draw_text(
                out,
                &text,
                x + 1.0,
                y + 1.0,
                scale,
                [0.0, 0.0, 0.0, 0.7 * alpha],
            );
            font::draw_text(out, &text, x, y, scale, [1.0, 1.0, 1.0, alpha]);
        }
    }

    /// Builds the debug overlay and crosshair, in output pixels.
    pub fn build_hud(
        &self,
        out: &mut Vec<HudRect>,
        size: (u32, u32),
        show_text: bool,
        fps: f32,
        frame: &crate::render::FrameStats,
    ) {
        out.clear();
        let (w, h) = (size.0 as f32, size.1 as f32);
        let scale = (h / 720.0).round().max(1.0);
        let arm = 8.0 * scale;
        let thick = scale.max(2.0);
        let white = [1.0, 1.0, 1.0, 0.75];
        out.push(HudRect::flat(
            [w * 0.5 - arm, h * 0.5 - thick * 0.5, arm * 2.0, thick],
            white,
        ));
        out.push(HudRect::flat(
            [w * 0.5 - thick * 0.5, h * 0.5 - arm, thick, arm * 2.0],
            white,
        ));
        self.build_hotbar(out, w, h);
        if !show_text {
            return;
        }
        let s = &self.streamer.stats;
        let tier = self.streamer.generator().tier_near(self.pos.y);
        let lines = [
            format!("DEEPDELVE {}", crate::version()),
            format!(
                "FPS {fps:.0}   GPU CULL {:.1} OPAQUE {:.1} WATER {:.1} POST {:.1} MS",
                frame.gpu_ms[0], frame.gpu_ms[1], frame.gpu_ms[2], frame.gpu_ms[3]
            ),
            format!(
                "POS {:.0} {:.0} {:.0}   LAYER {} {}   LIQUID LEVEL {:.0}",
                self.pos.x,
                self.pos.y,
                self.pos.z,
                tier.index + 1,
                tier.theme.name,
                tier.liquid_y
            ),
            format!("SPEED {:.0}   SEED {}", self.speed, self.seed),
            format!(
                "CHUNKS {} / {}   QUEUED {}   BUILDING {}   AVG {:.0} US",
                s.loaded, s.desired, s.queued, s.in_flight, s.avg_job_us
            ),
            format!(
                "MESHES {}   IN VIEW {}   QUADS {:.1}M STORED",
                s.meshes,
                frame.chunks_in_frustum,
                s.used_quads as f32 / 1.0e6
            ),
            format!(
                "DRAWS {} OF {}   QUADS {:.2}M OF {:.2}M   OCCLUSION {}",
                frame.gpu_drawn,
                frame.candidates,
                frame.gpu_quads as f32 / 1.0e6,
                frame.candidate_quads as f32 / 1.0e6,
                if self.occlusion { "ON" } else { "OFF" }
            ),
            match self.last_remesh {
                Some((n, ms)) => format!(
                    "BROKEN {}   PLACED {}   LAST EDIT {} CHUNKS IN {:.2} MS",
                    self.blocks_broken, self.blocks_placed, n, ms
                ),
                None => "WASD SPACE C MOVE   SHIFT FAST   WHEEL SPEED   R NEW WORLD".to_string(),
            },
            "LMB BREAK   RMB PLACE   MMB PICK   1-9 BLOCK".to_string(),
        ];
        let lh = 9.0 * scale;
        for (i, line) in lines.iter().enumerate() {
            let y = 8.0 + i as f32 * lh;
            font::draw_text(out, line, 9.0, y + 1.0, scale, [0.0, 0.0, 0.0, 0.7]);
            font::draw_text(out, line, 8.0, y, scale, [1.0, 1.0, 0.92, 1.0]);
        }
    }
}

/// A block drawn as a cube seen from above, as three shaded faces of a regular hexagon
/// of radius `r` around (cx, cy). `tint` multiplies its texture.
fn block_icon(out: &mut Vec<HudRect>, b: u8, tint: Vec3, cx: f32, cy: f32, r: f32) {
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
