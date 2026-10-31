//! Game state that doesn't depend on a window: camera, streaming and editing.

use crate::render::{ChunkGpu, FrameParams, HudRect, font};
use crate::stream::{ChunkSink, Streamer};
use crate::world::block::{AIR, WATER, is_opaque};
use crate::world::noise::{smoothstep, splitmix};
use glam::{DVec3, Mat4, Vec2, Vec3, Vec4};

const REACH: f64 = 96.0;
const BREAK_REPEAT: f32 = 0.12;
const FOV_Y_DEG: f32 = 72.0;

#[derive(Default, Clone, Copy)]
pub struct Input {
    /// x: strafe, y: up/down, z: forward. Each in -1..=1.
    pub axis: Vec3,
    pub sprint: bool,
    pub look: Vec2,
    pub scroll: f32,
    pub breaking: bool,
    pub reset: bool,
}

pub struct Game {
    pub streamer: Streamer,
    pub seed: u64,
    pub pos: DVec3,
    pub yaw: f32,
    pub pitch: f32,
    pub speed: f32,
    pub time: f32,
    pub target: Option<[i64; 3]>,
    pub blocks_broken: u64,
    pub last_remesh: Option<(usize, f32)>,
    pub occlusion: bool,
    break_timer: f32,
    fog: Vec3,
    ambient: Vec3,
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
            last_remesh: None,
            occlusion: true,
            break_timer: 0.0,
            fog: Vec3::ZERO,
            ambient: Vec3::ZERO,
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

        self.target = self.raycast();
        self.break_timer -= dt;
        if input.breaking && self.break_timer <= 0.0 {
            if let Some(t) = self.target {
                self.break_block(t, gpu);
                self.break_timer = BREAK_REPEAT;
            }
        } else if !input.breaking {
            self.break_timer = 0.0;
        }

        self.update_atmosphere(dt);
        reset
    }

    pub fn break_block(&mut self, t: [i64; 3], sink: &mut impl ChunkSink) {
        let start = std::time::Instant::now();
        if let Some(n) = self.streamer.set_block(t[0], t[1], t[2], AIR, sink) {
            self.blocks_broken += 1;
            self.last_remesh = Some((n, start.elapsed().as_secs_f32() * 1000.0));
        }
    }

    /// Steps through the voxel grid until it hits something solid.
    pub fn raycast(&self) -> Option<[i64; 3]> {
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
        while t < REACH {
            let b = self.streamer.block_at(p.x, p.y, p.z)?;
            if is_opaque(b) {
                return Some([p.x, p.y, p.z]);
            }
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
        let lin = |c: u8| {
            let v = c as f32 / 255.0;
            v * v
        };
        let tint = Vec3::new(lin(t.tint[0]), lin(t.tint[1]), lin(t.tint[2]));
        let inside = smoothstep(0.0, 0.8, f.open)
            * smoothstep(f.floor - 30.0, f.floor + 5.0, self.pos.y as f32)
            * (1.0 - smoothstep(f.ceil - 5.0, f.ceil + 30.0, self.pos.y as f32));
        let glow = 0.5 + 0.5 * f.glow;
        let fog_target = tint * (0.018 + 0.19 * inside * glow) * Vec3::new(0.86, 0.96, 1.08);
        let amb_target = tint * (0.035 + 0.5 * inside * glow);
        let k = if self.fog_primed {
            1.0 - (-dt * 0.8).exp()
        } else {
            1.0
        };
        self.fog = self.fog.lerp(fog_target, k);
        self.ambient = self.ambient.lerp(amb_target, k);
        self.fog_primed = true;
    }

    pub fn frame_params<'a>(&self, hud: &'a [HudRect]) -> FrameParams<'a> {
        let b = self.pos.floor().as_i64vec3();
        let underwater = self.streamer.block_at(b.x, b.y, b.z) == Some(WATER);
        FrameParams {
            cam_pos: self.pos,
            view: self.view_matrix(),
            fov_y: FOV_Y_DEG.to_radians(),
            time: self.time,
            fog_color: self.fog,
            ambient: self.ambient,
            underwater,
            outline: self.target,
            occlusion: self.occlusion,
            hud,
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
        out.push(HudRect {
            rect: [w * 0.5 - arm, h * 0.5 - thick * 0.5, arm * 2.0, thick],
            color: white,
        });
        out.push(HudRect {
            rect: [w * 0.5 - thick * 0.5, h * 0.5 - arm, thick, arm * 2.0],
            color: white,
        });
        if !show_text {
            return;
        }
        let s = &self.streamer.stats;
        let tier = self.streamer.generator().tier_near(self.pos.y);
        let lines = [
            format!(
                "FPS {fps:.0}   GPU CULL {:.1} OPAQUE {:.1} WATER {:.1} POST {:.1} MS",
                frame.gpu_ms[0], frame.gpu_ms[1], frame.gpu_ms[2], frame.gpu_ms[3]
            ),
            format!(
                "POS {:.0} {:.0} {:.0}   TIER {}   WATER LEVEL {:.0}",
                self.pos.x, self.pos.y, self.pos.z, tier.index, tier.water_y
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
                    "BROKEN {}   LAST EDIT {} CHUNKS IN {:.2} MS",
                    self.blocks_broken, n, ms
                ),
                None => "WASD SPACE C MOVE   SHIFT FAST   WHEEL SPEED   LMB BREAK   R NEW WORLD"
                    .to_string(),
            },
        ];
        let lh = 9.0 * scale;
        for (i, line) in lines.iter().enumerate() {
            let y = 8.0 + i as f32 * lh;
            font::draw_text(out, line, 9.0, y + 1.0, scale, [0.0, 0.0, 0.0, 0.7]);
            font::draw_text(out, line, 8.0, y, scale, [1.0, 1.0, 0.92, 1.0]);
        }
    }
}
