//! Deepdelve: an underground voxel world tech demo. See docs/ for the design.

mod app;
mod game;
mod headless;
mod mesher;
mod player;
mod render;
mod stream;
mod world;

use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct Options {
    pub seed: u64,
    pub render_height: u32,
    pub vsync: bool,
    pub fullscreen: bool,
    pub headless: bool,
    pub size: (u32, u32),
    pub shots: Vec<headless::Shot>,
    pub bench_seconds: f32,
    pub bench_speed: f32,
    pub tour: bool,
    pub tier: i32,
    pub occlusion: bool,
    pub exit_after: f32,
    pub hud: bool,
}

/// The release name, such as "a0.1.0". Only packaged builds (see flake.nix) claim to be
/// that release; anything built with cargo directly is marked as a development build.
pub fn version() -> String {
    let dev = if option_env!("DEEPDELVE_PACKAGED").is_some() {
        ""
    } else {
        "-dev"
    };
    format!("a{}{dev}", env!("CARGO_PKG_VERSION"))
}

fn print_help() {
    println!(
        "deepdelve [options]\n\n\
         General:\n  \
           --version           print the version\n\n\
         Playing:\n  \
           --seed N            world seed (default: random)\n  \
           --render-height N   internal render height in pixels, upscaled to the window (default 1440)\n  \
           --no-vsync          uncapped frame rate\n  \
           --fullscreen        borderless fullscreen\n\n\
         Testing without a window:\n  \
           --headless          render off-screen\n  \
           --size WxH          output size (default 1600x900)\n  \
           --shot FILE[@x,y,z,yaw,pitch]  wait for the world to load, then save a PNG\n  \
           --tour DIR          capture a tour of generated viewpoints into DIR\n  \
           --bench SECONDS     fly through caverns and report frame times\n  \
           --bench-speed N     flight speed in blocks per second (default 120)\n  \
           --tier N            start in this layer: 0 top, 1 middle, 2 bottom (default 0)\n  \
           --no-occlusion      turn occlusion culling off, for comparisons\n  \
           --hud               draw the crosshair and hotbar in screenshots\n  \
           --exit-after SECS   windowed mode: quit after this long and print a summary\n"
    );
}

fn parse_args() -> Options {
    let mut o = Options {
        seed: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1),
        render_height: 1440,
        vsync: true,
        fullscreen: false,
        headless: false,
        size: (1600, 900),
        shots: Vec::new(),
        bench_seconds: 0.0,
        bench_speed: 120.0,
        tour: false,
        tier: 0,
        occlusion: true,
        exit_after: 0.0,
        hud: false,
    };
    let mut args = std::env::args().skip(1);
    let need = |v: Option<String>, flag: &str| -> String {
        v.unwrap_or_else(|| {
            eprintln!("{flag} needs a value");
            std::process::exit(2);
        })
    };
    let num = |s: String, flag: &str| -> f64 {
        s.parse().unwrap_or_else(|_| {
            eprintln!("{flag}: not a number: {s}");
            std::process::exit(2);
        })
    };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--version" | "-V" => {
                println!("deepdelve {}", version());
                std::process::exit(0);
            }
            "--seed" => o.seed = num(need(args.next(), &a), &a) as u64,
            "--render-height" => o.render_height = num(need(args.next(), &a), &a) as u32,
            "--no-vsync" => o.vsync = false,
            "--fullscreen" => o.fullscreen = true,
            "--headless" => o.headless = true,
            "--size" => {
                let v = need(args.next(), &a);
                let (w, h) = v.split_once('x').unwrap_or_else(|| {
                    eprintln!("--size wants WxH");
                    std::process::exit(2);
                });
                o.size = (num(w.into(), &a) as u32, num(h.into(), &a) as u32);
            }
            "--shot" => o.shots.push(headless::Shot::parse(&need(args.next(), &a))),
            "--tour" => {
                o.tour = true;
                o.shots.push(headless::Shot {
                    path: need(args.next(), &a),
                    cam: None,
                });
            }
            "--bench" => o.bench_seconds = num(need(args.next(), &a), &a) as f32,
            "--bench-speed" => o.bench_speed = num(need(args.next(), &a), &a) as f32,
            "--tier" => o.tier = num(need(args.next(), &a), &a) as i32,
            "--no-occlusion" => o.occlusion = false,
            "--hud" => o.hud = true,
            "--exit-after" => o.exit_after = num(need(args.next(), &a), &a) as f32,
            other => {
                eprintln!("unknown option: {other}\n");
                print_help();
                std::process::exit(2);
            }
        }
    }
    o
}

fn main() {
    env_logger::Builder::from_env(
        env_logger::Env::default()
            .default_filter_or("deepdelve=info,wgpu_core=warn,wgpu_hal=error"),
    )
    .init();
    let opts = parse_args();
    if opts.headless {
        headless::run(opts);
    } else {
        app::run(opts);
    }
}

pub fn worker_count() -> usize {
    let n = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    n.saturating_sub(2).max(1)
}
