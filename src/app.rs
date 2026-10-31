//! The windowed game: event loop, input and presentation.

use crate::game::{Game, Input};
use crate::render::{FrameStats, HudRect, MAX_SLOTS, POOL_QUADS, Renderer};
use glam::Vec2;
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{
    DeviceEvent, DeviceId, ElementState, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Fullscreen, Window, WindowId};

const MOUSE_SENSITIVITY: f32 = 0.0022;

struct Running {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
    game: Game,
    hud: Vec<HudRect>,
    last_stats: FrameStats,
}

struct App {
    opts: crate::Options,
    instance: Option<wgpu::Instance>,
    running: Option<Running>,
    last_frame: std::time::Instant,
    keys: Keys,
    look: Vec2,
    scroll: f32,
    reset_pressed: bool,
    captured: bool,
    show_hud: bool,
    screenshot: bool,
    fps_time: f32,
    fps_frames: u32,
    fps: f32,
    started: std::time::Instant,
    frames: u64,
}

#[derive(Default)]
struct Keys {
    forward: bool,
    back: bool,
    left: bool,
    right: bool,
    up: bool,
    down: bool,
    sprint: bool,
    breaking: bool,
}

fn render_size(window: PhysicalSize<u32>, cap: u32) -> (u32, u32) {
    let (w, h) = (window.width.max(1), window.height.max(1));
    if h <= cap {
        (w, h)
    } else {
        let k = cap as f32 / h as f32;
        (((w as f32 * k) as u32).max(1), cap)
    }
}

impl App {
    fn set_capture(&mut self, on: bool) {
        let Some(r) = &self.running else { return };
        self.captured = on;
        if on {
            let grabbed = r
                .window
                .set_cursor_grab(CursorGrabMode::Locked)
                .or_else(|_| r.window.set_cursor_grab(CursorGrabMode::Confined));
            if let Err(e) = grabbed {
                log::warn!("could not grab the cursor: {e}");
            }
            r.window.set_cursor_visible(false);
        } else {
            let _ = r.window.set_cursor_grab(CursorGrabMode::None);
            r.window.set_cursor_visible(true);
        }
    }

    fn frame(&mut self, event_loop: &ActiveEventLoop) {
        let now = std::time::Instant::now();
        let dt = (now - self.last_frame).as_secs_f32().min(0.1);
        self.last_frame = now;
        self.fps_time += dt;
        self.fps_frames += 1;
        if self.fps_time >= 0.5 {
            self.fps = self.fps_frames as f32 / self.fps_time;
            self.fps_time = 0.0;
            self.fps_frames = 0;
        }

        let k = &self.keys;
        let axis = |pos: bool, neg: bool| pos as i32 as f32 - neg as i32 as f32;
        let input = Input {
            axis: glam::Vec3::new(
                axis(k.right, k.left),
                axis(k.up, k.down),
                axis(k.forward, k.back),
            ),
            sprint: k.sprint,
            look: self.look * MOUSE_SENSITIVITY,
            scroll: self.scroll,
            breaking: k.breaking && self.captured,
            reset: self.reset_pressed,
        };
        self.look = Vec2::ZERO;
        self.scroll = 0.0;
        self.reset_pressed = false;

        let Some(r) = &mut self.running else { return };
        if self.opts.exit_after > 0.0
            && self.started.elapsed().as_secs_f32() >= self.opts.exit_after
        {
            let s = r.game.streamer.stats;
            println!(
                "windowed run ok: {} frames in {:.1}s, surface {}x{} {:?}, {} of {} chunks, {:.2}M quads",
                self.frames,
                self.started.elapsed().as_secs_f32(),
                r.config.width,
                r.config.height,
                r.renderer.output_format(),
                s.loaded,
                s.desired,
                s.used_quads as f32 / 1.0e6,
            );
            event_loop.exit();
            return;
        }
        if r.game.update(dt, &input, &mut r.renderer.chunks) {
            r.renderer.invalidate_occlusion();
        }

        let surface_texture = match r.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                r.surface.configure(&r.renderer.device, &r.config);
                return;
            }
            other => {
                log::error!("surface unusable: {other:?}");
                event_loop.exit();
                return;
            }
        };
        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor {
                format: Some(r.renderer.output_format()),
                ..Default::default()
            });
        let size = (r.config.width, r.config.height);
        r.game
            .build_hud(&mut r.hud, size, self.show_hud, self.fps, &r.last_stats);
        let params = r.game.frame_params(&r.hud);
        r.last_stats = r.renderer.render(&view, size, &r.game.streamer, &params);

        if std::mem::take(&mut self.screenshot) {
            save_screenshot(r, size);
        }
        r.window.pre_present_notify();
        r.renderer.queue.present(surface_texture);
        self.frames += 1;
    }
}

/// Renders the current view again into a readable texture and writes a PNG.
fn save_screenshot(r: &mut Running, size: (u32, u32)) {
    let texture = r.renderer.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("screenshot"),
        size: wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: r.renderer.output_format(),
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    let params = r.game.frame_params(&[]);
    r.renderer.render(&view, size, &r.game.streamer, &params);
    let mut pixels = r.renderer.read_texture(&texture, size);
    if matches!(
        r.renderer.output_format(),
        wgpu::TextureFormat::Bgra8UnormSrgb | wgpu::TextureFormat::Bgra8Unorm
    ) {
        for px in pixels.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = std::fs::create_dir_all("captures");
    let path = format!("captures/shot_{stamp}.png");
    match image::save_buffer(&path, &pixels, size.0, size.1, image::ColorType::Rgba8) {
        Ok(()) => log::info!("saved {path}"),
        Err(e) => log::error!("screenshot failed: {e}"),
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.running.is_some() {
            return;
        }
        let mut attrs = Window::default_attributes()
            .with_title("Deepdelve")
            .with_inner_size(PhysicalSize::new(1600, 900));
        if self.opts.fullscreen {
            attrs = attrs.with_fullscreen(Some(Fullscreen::Borderless(None)));
        }
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_with_display_handle(Box::new(
                event_loop.owned_display_handle(),
            ))
        });
        let surface = instance
            .create_surface(window.clone())
            .expect("create surface");

        let size = window.inner_size();
        let (renderer, adapter) = {
            // The output format must be known before pipelines are built, so the
            // adapter is queried once here for what the surface supports.
            let probe =
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    force_fallback_adapter: false,
                    compatible_surface: Some(&surface),
                    apply_limit_buckets: false,
                }))
                .expect("no Vulkan adapter found");
            let caps = surface.get_capabilities(&probe);
            let format = caps
                .formats
                .iter()
                .copied()
                .find(|f| f.is_srgb())
                .unwrap_or(caps.formats[0]);
            pollster::block_on(Renderer::new(
                &instance,
                Some(&surface),
                Some(format),
                render_size(size, self.opts.render_height),
            ))
        };
        let caps = surface.get_capabilities(&adapter);
        let present_mode = if self.opts.vsync {
            wgpu::PresentMode::AutoVsync
        } else {
            [wgpu::PresentMode::Immediate, wgpu::PresentMode::Mailbox]
                .into_iter()
                .find(|m| caps.present_modes.contains(m))
                .unwrap_or(wgpu::PresentMode::AutoNoVsync)
        };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: renderer.output_format(),
            color_space: Default::default(),
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
        };
        surface.configure(&renderer.device, &config);

        let mut game = Game::new(self.opts.seed, crate::worker_count(), POOL_QUADS, MAX_SLOTS);
        game.occlusion = self.opts.occlusion;
        log::info!("seed {}", self.opts.seed);
        self.instance = Some(instance);
        self.running = Some(Running {
            window,
            surface,
            config,
            renderer,
            game,
            hud: Vec::new(),
            last_stats: FrameStats::default(),
        });
        self.last_frame = std::time::Instant::now();
        self.set_capture(true);
        event_loop.set_control_flow(ControlFlow::Poll);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(r) = &mut self.running
                    && size.width > 0
                    && size.height > 0
                {
                    r.config.width = size.width;
                    r.config.height = size.height;
                    r.surface.configure(&r.renderer.device, &r.config);
                    r.renderer
                        .resize(render_size(size, self.opts.render_height));
                }
            }
            WindowEvent::Focused(false) => {
                self.keys = Keys::default();
                self.set_capture(false);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                let PhysicalKey::Code(code) = event.physical_key else {
                    return;
                };
                match code {
                    KeyCode::KeyW => self.keys.forward = pressed,
                    KeyCode::KeyS => self.keys.back = pressed,
                    KeyCode::KeyA => self.keys.left = pressed,
                    KeyCode::KeyD => self.keys.right = pressed,
                    KeyCode::Space => self.keys.up = pressed,
                    KeyCode::KeyC | KeyCode::ControlLeft => self.keys.down = pressed,
                    KeyCode::ShiftLeft | KeyCode::ShiftRight => self.keys.sprint = pressed,
                    KeyCode::KeyR if pressed && !event.repeat => self.reset_pressed = true,
                    KeyCode::F1 if pressed && !event.repeat => self.show_hud = !self.show_hud,
                    KeyCode::F2 if pressed && !event.repeat => {
                        if let Some(r) = &mut self.running {
                            r.game.occlusion = !r.game.occlusion;
                        }
                    }
                    KeyCode::F12 if pressed && !event.repeat => self.screenshot = true,
                    KeyCode::Escape if pressed && !event.repeat => {
                        let on = !self.captured;
                        self.set_capture(on);
                    }
                    _ => {}
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                let pressed = state == ElementState::Pressed;
                if pressed && !self.captured {
                    self.set_capture(true);
                } else {
                    self.keys.breaking = pressed;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.scroll += match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40.0,
                };
            }
            WindowEvent::RedrawRequested => self.frame(event_loop),
            _ => {}
        }
    }

    fn device_event(&mut self, _el: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta } = event
            && self.captured
        {
            self.look += Vec2::new(delta.0 as f32, delta.1 as f32);
        }
    }

    fn about_to_wait(&mut self, _el: &ActiveEventLoop) {
        if let Some(r) = &self.running {
            r.window.request_redraw();
        }
    }
}

pub fn run(opts: crate::Options) {
    let event_loop = EventLoop::new().expect("create event loop");
    let mut app = App {
        opts,
        instance: None,
        running: None,
        last_frame: std::time::Instant::now(),
        keys: Keys::default(),
        look: Vec2::ZERO,
        scroll: 0.0,
        reset_pressed: false,
        captured: false,
        show_hud: true,
        screenshot: false,
        fps_time: 0.0,
        fps_frames: 0,
        fps: 0.0,
        started: std::time::Instant::now(),
        frames: 0,
    };
    event_loop.run_app(&mut app).expect("event loop");
}
