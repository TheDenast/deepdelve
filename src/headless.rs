//! Runs the game without a window, for automated checks: screenshots, flight
//! benchmarks and an end-to-end block-breaking test.

use crate::game::{Game, Input};
use crate::render::{MAX_SLOTS, POOL_QUADS, Renderer};
use crate::world::block::{BEDROCK, is_opaque};
use crate::world::generator::{WORLD_BOTTOM, WORLD_TOP};
use glam::{DVec3, Vec3};

#[derive(Debug, Clone)]
pub struct Shot {
    pub path: String,
    pub cam: Option<[f32; 5]>,
}

impl Shot {
    pub fn parse(s: &str) -> Self {
        match s.split_once('@') {
            None => Shot {
                path: s.to_string(),
                cam: None,
            },
            Some((path, v)) => {
                let n: Vec<f32> = v.split(',').filter_map(|x| x.parse().ok()).collect();
                if n.len() != 5 {
                    eprintln!("--shot wants FILE@x,y,z,yaw,pitch");
                    std::process::exit(2);
                }
                Shot {
                    path: path.to_string(),
                    cam: Some([n[0], n[1], n[2], n[3], n[4]]),
                }
            }
        }
    }
}

struct Offscreen {
    renderer: Renderer,
    game: Game,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    size: (u32, u32),
    /// Draws the HUD into screenshots, without the stats overlay.
    hud: Option<Vec<crate::render::HudRect>>,
}

impl Offscreen {
    fn step(&mut self, dt: f32, input: &Input) -> crate::render::FrameStats {
        if self.game.update(dt, input, &mut self.renderer.chunks) {
            self.renderer.invalidate_occlusion();
        }
        let mut hud = self.hud.take();
        if let Some(h) = &mut hud {
            let stats = Default::default();
            self.game.build_hud(h, self.size, false, 0.0, &stats);
        }
        let params = self.game.frame_params(hud.as_deref().unwrap_or(&[]));
        let stats = self
            .renderer
            .render(&self.view, self.size, &self.game.streamer, &params);
        self.hud = hud;
        stats
    }

    /// Steps until every wanted chunk is built, or a time limit passes.
    fn settle(&mut self, limit: f32) -> f32 {
        let start = std::time::Instant::now();
        let mut quiet = 0;
        while start.elapsed().as_secs_f32() < limit {
            self.step(0.016, &Input::default());
            if self.game.streamer.is_settled() {
                quiet += 1;
                if quiet > 5 {
                    break;
                }
            } else {
                quiet = 0;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        start.elapsed().as_secs_f32()
    }

    fn capture(&mut self, path: &str) {
        // A few frames let the occlusion culling use this viewpoint's depth.
        for _ in 0..4 {
            self.step(0.0, &Input::default());
        }
        let mut stats = crate::render::FrameStats::default();
        for _ in 0..6 {
            stats = self.step(0.0, &Input::default());
            let _ = self
                .renderer
                .device
                .poll(wgpu::PollType::wait_indefinitely());
        }
        println!(
            "  view at {:?}: {:.2}M of {:.2}M quads, {} of {} draws, gpu cull {:.2} opaque {:.2} water {:.2} ms",
            self.game.pos.as_i64vec3().to_array(),
            stats.gpu_quads as f64 / 1.0e6,
            stats.candidate_quads as f64 / 1.0e6,
            stats.gpu_drawn,
            stats.candidates,
            stats.gpu_ms[0],
            stats.gpu_ms[1],
            stats.gpu_ms[2],
        );
        let pixels = self.renderer.read_texture(&self.texture, self.size);
        if let Some(dir) = std::path::Path::new(path).parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        image::save_buffer(
            path,
            &pixels,
            self.size.0,
            self.size.1,
            image::ColorType::Rgba8,
        )
        .expect("save png");
        println!("saved {path}");
    }
}

pub fn run(opts: crate::Options) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let size = opts.size;
    let (renderer, _adapter) = pollster::block_on(Renderer::new(&instance, None, None, size));
    let texture = renderer.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("output"),
        size: wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: renderer.output_format(),
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    let mut game = Game::new(opts.seed, crate::worker_count(), POOL_QUADS, MAX_SLOTS);
    if opts.tier != 0 {
        game.pos = DVec3::from_array(game.streamer.generator().find_spawn(opts.tier));
    }
    println!("deepdelve {}", crate::version());
    println!("seed {}  spawn {:?}", opts.seed, game.pos.to_array());
    game.occlusion = opts.occlusion;
    let mut o = Offscreen {
        renderer,
        game,
        texture,
        view,
        size,
        hud: opts.hud.then(Vec::new),
    };

    let t = o.settle(120.0);
    let s = o.game.streamer.stats;
    println!(
        "world ready in {t:.2}s: {} chunks, {} meshes, {:.2}M quads ({:.0} MB), avg {:.0} us/chunk",
        s.loaded,
        s.meshes,
        s.used_quads as f64 / 1.0e6,
        s.used_quads as f64 * 8.0 / 1.0e6,
        s.avg_job_us,
    );

    if opts.tour {
        let dir = opts.shots[0].path.clone();
        tour(&mut o, &dir);
    } else {
        for shot in opts.shots.clone() {
            if let Some(c) = shot.cam {
                o.game.pos = DVec3::new(c[0] as f64, c[1] as f64, c[2] as f64);
                o.game.yaw = c[3];
                o.game.pitch = c[4];
                o.settle(60.0);
            }
            o.capture(&shot.path);
        }
    }

    if opts.bench_seconds > 0.0 {
        bench(&mut o, opts.bench_seconds, opts.bench_speed);
    }

    if opts.shots.is_empty() && opts.bench_seconds <= 0.0 {
        break_test(&mut o);
    }
}

/// Reports GPU and frame cost while flying down the length of the caverns.
fn bench(o: &mut Offscreen, seconds: f32, speed: f32) {
    // Settling doesn't wait for the GPU. Clear that backlog so the first timed frame
    // doesn't pay for it.
    let _ = o.renderer.device.poll(wgpu::PollType::wait_indefinitely());
    let mut times = Vec::new();
    let mut update_times = Vec::new();
    let mut gpu_sum = [0f32; 4];
    let mut quads = 0f64;
    let mut draws = 0f64;
    let mut cand_quads = 0f64;
    let start = std::time::Instant::now();
    let mut last = start;
    let g = o.game.streamer.generator().clone();
    let tier = g.tier_near(o.game.pos.y).clone();
    let mut heading = 0.3f32;
    let mut peak_queue = 0usize;
    o.game.yaw = heading;
    o.game.speed = speed;
    while start.elapsed().as_secs_f32() < seconds {
        let now = std::time::Instant::now();
        let dt = (now - last).as_secs_f32();
        last = now;
        // Steer slowly around rock, and hold a height inside the cavern.
        let ahead =
            o.game.pos + DVec3::new(heading.sin() as f64, 0.0, -heading.cos() as f64) * 300.0;
        let f = g.field(&tier, ahead.x, ahead.z);
        if f.open < 0.6 {
            heading += dt * 0.9;
        }
        o.game.yaw = heading;
        let want_y = if f.open > 0.3 {
            (f.floor + (f.ceil - f.floor) * 0.3) as f64
        } else {
            o.game.pos.y
        };
        o.game.pos.y += (want_y - o.game.pos.y) * (dt as f64 * 0.8);
        o.game.pitch = -0.08;
        let input = Input {
            axis: Vec3::new(0.0, 0.0, 1.0),
            ..Default::default()
        };
        let t_step = std::time::Instant::now();
        o.game.update(dt, &input, &mut o.renderer.chunks);
        update_times.push(t_step.elapsed().as_secs_f32() * 1000.0);
        let params = o.game.frame_params(&[]);
        let stats = o
            .renderer
            .render(&o.view, o.size, &o.game.streamer, &params);
        // Wait for the GPU so that times below reflect real rendering cost.
        let _ = o.renderer.device.poll(wgpu::PollType::wait_indefinitely());
        times.push(dt * 1000.0);
        for (sum, ms) in gpu_sum.iter_mut().zip(stats.gpu_ms) {
            *sum += ms;
        }
        quads += stats.gpu_quads as f64;
        draws += stats.gpu_drawn as f64;
        cand_quads += stats.candidate_quads as f64;
        peak_queue = peak_queue.max(o.game.streamer.stats.queued);
    }
    let worst_at = times
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .map_or(0, |(i, _)| i);
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = times.len().max(1);
    println!("  worst frame was number {worst_at}");
    let pct = |p: f32| times[((n as f32 * p) as usize).min(n - 1)];
    let f = n as f32;
    println!(
        "bench: {n} frames in {seconds}s at {speed} blocks/s, flew to {:?}",
        o.game.pos.as_i64vec3().to_array()
    );
    println!(
        "  frame ms: mean {:.2}  p50 {:.2}  p95 {:.2}  p99 {:.2}  max {:.2}",
        times.iter().sum::<f32>() / f,
        pct(0.5),
        pct(0.95),
        pct(0.99),
        times[n - 1],
    );
    println!(
        "  gpu ms: cull {:.2}  opaque {:.2}  water {:.2}  composite {:.2}",
        gpu_sum[0] / f,
        gpu_sum[1] / f,
        gpu_sum[2] / f,
        gpu_sum[3] / f,
    );
    println!(
        "  drawn after occlusion: {:.2}M quads in {:.0} draws, of {:.2}M quads in frustum",
        quads / f as f64 / 1.0e6,
        draws / f as f64,
        cand_quads / f as f64 / 1.0e6,
    );
    update_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let u = update_times.len().max(1);
    println!(
        "  update ms (streaming, uploads): mean {:.2}  p99 {:.2}  max {:.2}",
        update_times.iter().sum::<f32>() / u as f32,
        update_times[(u as f32 * 0.99) as usize],
        update_times[u - 1],
    );
    let st = o.game.streamer.stats;
    println!(
        "  worst single step us: plan {}  accept {}  select {}",
        st.plan_us, st.accept_us, st.select_us
    );
    println!("  peak queue while flying: {peak_queue}");
}

/// Viewpoints across every layer, so a change to the terrain or the renderer can be
/// judged at a glance.
fn tour(o: &mut Offscreen, dir: &str) {
    let g = o.game.streamer.generator().clone();
    let tiers = g.tiers().to_vec();
    for t in &tiers {
        let n = t.index;
        let name = t.theme.name.to_lowercase().replace(' ', "_");
        let mut spot = g.find_spawn(n);
        let f = g.field(t, spot[0], spot[2]);
        // Wide shot from the upper part of the cavern, looking along it.
        spot[1] = (f.floor + (f.ceil - f.floor) * 0.62) as f64;
        o.game.pos = DVec3::from_array(spot);
        o.game.yaw = 0.7 * n as f32;
        o.game.pitch = -0.22;
        o.settle(60.0);
        o.capture(&format!("{dir}/{n}_{name}_wide.png"));
        // Close to the ground.
        o.game.pos.y = (f.floor + 26.0) as f64;
        o.game.pitch = 0.06;
        o.settle(60.0);
        o.capture(&format!("{dir}/{n}_{name}_ground.png"));
        // Across the water or lava.
        o.game.pos = DVec3::from_array(g.find_shore(n));
        o.game.pitch = -0.1;
        o.settle(60.0);
        o.capture(&format!("{dir}/{n}_{name}_shore.png"));
        // At a mountain, from whichever side has open air around it.
        let view = g.find_mountain(n).and_then(|m| {
            (0..16).find_map(|i| {
                let a = i as f32 * std::f32::consts::TAU / 16.0;
                let (x, z) = (m[0] - a.sin() as f64 * 330.0, m[2] + a.cos() as f64 * 330.0);
                let f = g.field(t, x, z);
                let y = f.floor as f64 + (m[1] - f.floor as f64).max(40.0) * 0.75;
                (f.open > 0.9 && (y as f32) < f.ceil - 30.0).then_some((x, y, z, a))
            })
        });
        if let Some((x, y, z, yaw)) = view {
            o.game.pos = DVec3::new(x, y, z);
            o.game.yaw = yaw;
            o.game.pitch = -0.04;
            o.settle(60.0);
            o.capture(&format!("{dir}/{n}_{name}_mountain.png"));
        }
        if n + 1 < tiers.len() as i32
            && let Some(c) = g.find_chasm(n)
        {
            // Looking down the shaft from just above its rim.
            o.game.pos = DVec3::new(c[0] - 150.0, c[1] + 60.0, c[2]);
            o.game.yaw = std::f32::consts::FRAC_PI_2;
            o.game.pitch = -0.55;
            o.settle(60.0);
            o.capture(&format!("{dir}/{n}_{name}_chasm.png"));
        }
    }
    // Both ends of the world, seen from the void.
    let (x, z) = (o.game.pos.x, o.game.pos.z);
    o.game.pos = DVec3::new(x, WORLD_TOP as f64 + 90.0, z);
    o.game.pitch = -0.5;
    o.settle(60.0);
    o.capture(&format!("{dir}/void_above.png"));
    o.game.pos = DVec3::new(x, WORLD_BOTTOM as f64 - 90.0, z);
    o.game.pitch = 0.5;
    o.settle(60.0);
    o.capture(&format!("{dir}/void_below.png"));
}

/// Exercises the whole break path against generated terrain and checks it.
fn break_test(o: &mut Offscreen) {
    let (pos, target) = {
        let g = &o.game.streamer;
        let mut y = o.game.pos.y.floor() as i64;
        let x = o.game.pos.x.floor() as i64;
        let z = o.game.pos.z.floor() as i64;
        while y > WORLD_BOTTOM && !g.block_at(x, y, z).is_some_and(is_opaque) {
            y -= 1;
        }
        (
            DVec3::new(x as f64 + 0.5, y as f64 + 6.5, z as f64 + 0.5),
            [x, y, z],
        )
    };
    o.game.pos = pos;
    o.game.pitch = -1.55;
    o.game.yaw = 0.0;
    o.step(0.016, &Input::default());
    assert_eq!(
        o.game.raycast().map(|h| h.block),
        Some(target),
        "ray must hit the block below"
    );
    let before = o.game.streamer.block_at(target[0], target[1], target[2]);
    assert!(before.is_some_and(is_opaque));

    let input = Input {
        breaking: true,
        ..Default::default()
    };
    o.step(0.016, &input);
    let after = o.game.streamer.block_at(target[0], target[1], target[2]);
    assert_eq!(after, Some(crate::world::block::AIR), "block must be gone");
    println!(
        "break test ok: {:?} -> air, remesh {:?}",
        before, o.game.last_remesh
    );

    // The edit has to outlive unloading: fly away, come back, and look again.
    o.game.pos.x += 3000.0;
    o.settle(60.0);
    o.game.pos = pos;
    o.settle(60.0);
    let reloaded = o.game.streamer.block_at(target[0], target[1], target[2]);
    assert_eq!(
        reloaded,
        Some(crate::world::block::AIR),
        "edit must survive a reload"
    );
    println!("edit survived unloading: ok");

    // Placing: the ray now passes through the hole and hits the block below it, so a
    // new block goes back into the hole.
    o.step(
        0.016,
        &Input {
            slot: Some(1),
            placing: true,
            ..Default::default()
        },
    );
    let placed = o.game.hotbar[1];
    assert_eq!(
        o.game.streamer.block_at(target[0], target[1], target[2]),
        Some(placed),
        "block must be placed in the hole"
    );
    assert_eq!(o.game.target.map(|h| h.block), Some(target));
    let count = o.game.blocks_placed;
    o.game.place_block(target, placed, &mut o.renderer.chunks);
    assert_eq!(
        o.game.blocks_placed, count,
        "a solid cell must not be replaced"
    );
    // Picking the new block selects the slot it came from.
    o.step(
        0.016,
        &Input {
            slot: Some(4),
            ..Default::default()
        },
    );
    o.step(
        0.016,
        &Input {
            pick: true,
            ..Default::default()
        },
    );
    assert_eq!(o.game.slot, 1, "pick must select the block's slot");
    println!(
        "place test ok: {} placed, remesh {:?}",
        crate::world::block::name(placed),
        o.game.last_remesh
    );

    // Bedrock is the one thing that can't be broken.
    o.game.pos = DVec3::new(pos.x, WORLD_TOP as f64 + 6.5, pos.z);
    o.settle(60.0);
    let cap = [target[0], WORLD_TOP - 1, target[2]];
    o.step(0.016, &Input::default());
    assert_eq!(
        o.game.raycast().map(|h| h.block),
        Some(cap),
        "ray must hit the bedrock"
    );
    o.step(0.016, &input);
    assert_eq!(
        o.game.streamer.block_at(cap[0], cap[1], cap[2]),
        Some(BEDROCK),
        "bedrock must not break"
    );
    println!("bedrock held: ok");

    // R: everything from the old world has to go, edits included.
    let old_seed = o.game.seed;
    let reset = Input {
        reset: true,
        ..Default::default()
    };
    o.step(0.016, &reset);
    assert_ne!(o.game.seed, old_seed, "reset must change the seed");
    assert_eq!(o.game.blocks_broken, 0);
    let t = o.settle(60.0);
    let s = o.game.streamer.stats;
    assert!(
        s.loaded > 1000 && s.loaded == s.desired,
        "new world must load fully: {s:?}"
    );
    println!(
        "reset ok: new seed {}, {} chunks in {t:.2}s, {:.2}M quads",
        o.game.seed,
        s.loaded,
        s.used_quads as f64 / 1.0e6
    );
}
