//! GPU side: terrain multi-draw with occlusion culling, water, HDR post and HUD.

pub mod font;
pub mod pool;
pub mod textures;

use crate::mesher::{Quad, WATER_RANGE};
use crate::stream::{ChunkSink, Streamer};
use crate::world::generator::ChunkLook;
use bytemuck::{Pod, Zeroable};
use glam::{DVec3, Mat4, Vec3, Vec4};
use wgpu::util::DeviceExt;

pub const POOL_QUADS: u32 = 96 * 1024 * 1024;
pub const MAX_SLOTS: u32 = 64 * 1024;
const MAX_DRAWS: u32 = 256 * 1024;
const MAX_WATER_DRAWS: u32 = 16 * 1024;
const QUADS_PER_DRAW: u32 = 16 * 1024;
const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const BLOOM_LEVELS: usize = 5;
pub const NEAR_PLANE: f32 = 0.08;
const MACRO_TILE: f64 = 2048.0;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    cam_wrapped: [f32; 4],
    cam_info: [f32; 4],
    fog_color: [f32; 4],
    ambient: [f32; 4],
    screen: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct DrawData {
    origin: [f32; 3],
    slot: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ChunkInfoGpu {
    liquid_y: f32,
    scale: f32,
    tint: u32,
    crystal: u32,
    light: [u32; 32],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Candidate {
    bounds_min: [f32; 3],
    index_count: u32,
    bounds_max: [f32; 3],
    base_vertex: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CullParams {
    prev_view_proj: [[f32; 4]; 4],
    shift: [f32; 3],
    count: u32,
    pyramid_size: [f32; 2],
    mip_count: f32,
    enabled: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LineParams {
    view_proj: [[f32; 4]; 4],
    origin: [f32; 3],
    pad: f32,
}

/// A HUD parallelogram in output pixels: a corner and the two edges leaving it, so that
/// block icons can be drawn as cubes. `layer` picks a block texture to multiply `color`
/// by, or is negative for flat colour.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct HudRect {
    pub pos: [f32; 2],
    pub edges: [f32; 4],
    pub layer: f32,
    pub pad: f32,
    pub color: [f32; 4],
}

impl HudRect {
    /// A flat rectangle: x, y, width, height.
    pub fn flat(rect: [f32; 4], color: [f32; 4]) -> Self {
        Self {
            pos: [rect[0], rect[1]],
            edges: [rect[2], 0.0, 0.0, rect[3]],
            layer: -1.0,
            pad: 0.0,
            color,
        }
    }
}

/// Everything one frame needs from the game.
pub struct FrameParams<'a> {
    pub cam_pos: DVec3,
    pub view: Mat4,
    pub fov_y: f32,
    pub time: f32,
    pub fog_color: Vec3,
    pub fog_distance: f32,
    pub ambient: Vec3,
    pub underwater: bool,
    pub in_lava: bool,
    pub outline: Option<[i64; 3]>,
    pub occlusion: bool,
    pub hud: &'a [HudRect],
}

#[derive(Clone, Copy, Default, Debug)]
pub struct FrameStats {
    pub chunks_in_frustum: u32,
    pub candidates: u32,
    pub candidate_quads: u64,
    pub water_draws: u32,
    /// From the GPU, a few frames late.
    pub gpu_drawn: u32,
    pub gpu_quads: u32,
    pub gpu_ms: [f32; 4],
}

/// The parts of the renderer the streamer writes chunk data through.
pub struct ChunkGpu {
    queue: wgpu::Queue,
    quads: wgpu::Buffer,
    infos: wgpu::Buffer,
}

impl ChunkSink for ChunkGpu {
    fn write_quads(&mut self, first_quad: u32, quads: &[Quad]) {
        self.queue.write_buffer(
            &self.quads,
            first_quad as u64 * 8,
            bytemuck::cast_slice(quads),
        );
    }

    fn write_chunk_info(&mut self, slot: u32, look: &ChunkLook, light: &[u8; 125], lod: u8) {
        let mut info = ChunkInfoGpu {
            liquid_y: look.liquid_y,
            scale: (1u32 << lod) as f32,
            tint: look.tint[0] as u32 | (look.tint[1] as u32) << 8 | (look.tint[2] as u32) << 16,
            crystal: u32::from_le_bytes(look.crystal),
            light: [0; 32],
        };
        for (i, &b) in light.iter().enumerate() {
            info.light[i >> 2] |= (b as u32) << ((i & 3) * 8);
        }
        self.queue.write_buffer(
            &self.infos,
            slot as u64 * std::mem::size_of::<ChunkInfoGpu>() as u64,
            bytemuck::bytes_of(&info),
        );
    }
}

struct SizedTargets {
    hdr_view: wgpu::TextureView,
    depth: wgpu::Texture,
    depth_view: wgpu::TextureView,
    depth_copy: wgpu::Texture,
    pyramid_views: Vec<wgpu::TextureView>,
    pyramid_binds: Vec<wgpu::BindGroup>,
    bloom_views: Vec<wgpu::TextureView>,
    post_binds: PostBinds,
    water_bind: wgpu::BindGroup,
    cull_bind: wgpu::BindGroup,
    render_size: (u32, u32),
    pyramid_size: (u32, u32),
}

struct PostBinds {
    /// Reads HDR, writes bloom level 0.
    bright: wgpu::BindGroup,
    /// Reads bloom level i, writes level i + 1.
    down: Vec<wgpu::BindGroup>,
    /// Reads bloom level i + 1, adds to level i.
    up: Vec<wgpu::BindGroup>,
    composite: wgpu::BindGroup,
}

pub struct Renderer {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub chunks: ChunkGpu,
    output_format: wgpu::TextureFormat,

    index_buffer: wgpu::Buffer,
    globals_buf: wgpu::Buffer,
    draws_buf: wgpu::Buffer,
    candidates_buf: wgpu::Buffer,
    indirect_buf: wgpu::Buffer,
    water_indirect_buf: wgpu::Buffer,
    cull_params_buf: wgpu::Buffer,
    stats_buf: wgpu::Buffer,
    line_buf: wgpu::Buffer,
    hud_screen_buf: wgpu::Buffer,
    hud_buf: wgpu::Buffer,
    hud_capacity: usize,

    terrain_bind: wgpu::BindGroup,
    line_bind: wgpu::BindGroup,
    hud_bind: wgpu::BindGroup,
    linear_sampler: wgpu::Sampler,

    opaque_pipe: wgpu::RenderPipeline,
    water_pipe: wgpu::RenderPipeline,
    line_pipe: wgpu::RenderPipeline,
    hud_pipe: wgpu::RenderPipeline,
    cull_pipe: wgpu::ComputePipeline,
    hzb_first_pipe: wgpu::ComputePipeline,
    hzb_down_pipe: wgpu::ComputePipeline,
    bright_pipe: wgpu::RenderPipeline,
    down_pipe: wgpu::RenderPipeline,
    up_pipe: wgpu::RenderPipeline,
    composite_pipe: wgpu::RenderPipeline,

    water_bgl: wgpu::BindGroupLayout,
    cull_bgl: wgpu::BindGroupLayout,
    hzb_bgl: wgpu::BindGroupLayout,
    post_bgl: wgpu::BindGroupLayout,
    targets: SizedTargets,

    dummy_depth_view: wgpu::TextureView,
    dummy_color_view: wgpu::TextureView,

    prev_view_proj: Mat4,
    prev_cam: DVec3,
    /// False until a depth buffer exists to test against, and after a world reset.
    hzb_ready: bool,

    query_set: Option<wgpu::QuerySet>,
    query_resolve: Option<wgpu::Buffer>,
    readback: wgpu::Buffer,
    readback_state: std::sync::Arc<std::sync::atomic::AtomicU8>,
    timestamp_period: f32,
    last_gpu: ([f32; 4], u32, u32),
}

fn fullscreen_pipeline(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::PipelineLayout,
    module: &wgpu::ShaderModule,
    entry: &str,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs_fullscreen"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(entry),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn storage_entry(
    binding: u32,
    vis: wgpu::ShaderStages,
    read_only: bool,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: vis,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn uniform_entry(binding: u32, vis: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: vis,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn texture_entry(
    binding: u32,
    vis: wgpu::ShaderStages,
    sample_type: wgpu::TextureSampleType,
    dim: wgpu::TextureViewDimension,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: vis,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension: dim,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32, vis: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: vis,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn shader(device: &wgpu::Device, label: &str, src: &str) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(src.into()),
    })
}

impl Renderer {
    pub async fn new(
        instance: &wgpu::Instance,
        surface: Option<&wgpu::Surface<'static>>,
        output_format: Option<wgpu::TextureFormat>,
        render_size: (u32, u32),
    ) -> (Self, wgpu::Adapter) {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: surface,
                apply_limit_buckets: false,
            })
            .await
            .expect("no Vulkan adapter found");
        let info = adapter.get_info();
        log::info!(
            "adapter: {} ({:?}, {})",
            info.name,
            info.backend,
            info.driver_info
        );

        let wanted_optional =
            wgpu::Features::MULTI_DRAW_INDIRECT_COUNT | wgpu::Features::TIMESTAMP_QUERY;
        let features =
            wgpu::Features::INDIRECT_FIRST_INSTANCE | (adapter.features() & wanted_optional);
        let limits = adapter.limits();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("device"),
                required_features: features,
                required_limits: limits,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .expect("failed to create device");
        device.on_uncaptured_error(std::sync::Arc::new(|e| {
            log::error!("wgpu error: {e}");
        }));

        let output_format = output_format.unwrap_or(wgpu::TextureFormat::Rgba8UnormSrgb);

        // Static index pattern: quads select their data through base_vertex.
        let mut indices = Vec::with_capacity(QUADS_PER_DRAW as usize * 6);
        for q in 0..QUADS_PER_DRAW {
            let v = (q * 4) as u16;
            indices.extend_from_slice(&[v, v + 1, v + 2, v + 2, v + 1, v + 3]);
        }
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("quad indices"),
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        let buf = |label: &str, size: u64, usage: wgpu::BufferUsages| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        use wgpu::BufferUsages as U;
        let globals_buf = buf(
            "globals",
            std::mem::size_of::<Globals>() as u64,
            U::UNIFORM | U::COPY_DST,
        );
        let quads_buf = buf("quads", POOL_QUADS as u64 * 8, U::STORAGE | U::COPY_DST);
        let infos_buf = buf(
            "chunk infos",
            MAX_SLOTS as u64 * std::mem::size_of::<ChunkInfoGpu>() as u64,
            U::STORAGE | U::COPY_DST,
        );
        let draws_buf = buf(
            "draws",
            (MAX_DRAWS + MAX_WATER_DRAWS) as u64 * std::mem::size_of::<DrawData>() as u64,
            U::STORAGE | U::COPY_DST,
        );
        let candidates_buf = buf(
            "candidates",
            MAX_DRAWS as u64 * std::mem::size_of::<Candidate>() as u64,
            U::STORAGE | U::COPY_DST,
        );
        let indirect_buf = buf("indirect", MAX_DRAWS as u64 * 20, U::STORAGE | U::INDIRECT);
        let water_indirect_buf = buf(
            "water indirect",
            MAX_WATER_DRAWS as u64 * 20,
            U::COPY_DST | U::INDIRECT,
        );
        let cull_params_buf = buf(
            "cull params",
            std::mem::size_of::<CullParams>() as u64,
            U::UNIFORM | U::COPY_DST,
        );
        let stats_buf = buf("cull stats", 16, U::STORAGE | U::COPY_DST | U::COPY_SRC);
        let line_buf = buf(
            "line",
            std::mem::size_of::<LineParams>() as u64,
            U::UNIFORM | U::COPY_DST,
        );
        let hud_screen_buf = buf("hud screen", 16, U::UNIFORM | U::COPY_DST);
        let hud_capacity = 16 * 1024;
        let hud_buf = buf(
            "hud rects",
            hud_capacity as u64 * std::mem::size_of::<HudRect>() as u64,
            U::VERTEX | U::COPY_DST,
        );

        let (tex_view, macro_view) = textures::create(&device, &queue);
        let tex_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("block sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let macro_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("macro sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let linear_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("post sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let vis_vf = wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT;
        let terrain_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("terrain"),
            entries: &[
                uniform_entry(0, vis_vf),
                storage_entry(1, wgpu::ShaderStages::VERTEX, true),
                storage_entry(2, wgpu::ShaderStages::VERTEX, true),
                storage_entry(3, wgpu::ShaderStages::VERTEX, true),
                texture_entry(
                    4,
                    wgpu::ShaderStages::FRAGMENT,
                    wgpu::TextureSampleType::Float { filterable: true },
                    wgpu::TextureViewDimension::D2Array,
                ),
                sampler_entry(5, wgpu::ShaderStages::FRAGMENT),
                texture_entry(
                    6,
                    wgpu::ShaderStages::FRAGMENT,
                    wgpu::TextureSampleType::Float { filterable: true },
                    wgpu::TextureViewDimension::D2,
                ),
                sampler_entry(7, wgpu::ShaderStages::FRAGMENT),
            ],
        });
        let terrain_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("terrain"),
            layout: &terrain_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: globals_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: quads_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: draws_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: infos_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&tex_view),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Sampler(&tex_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(&macro_view),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: wgpu::BindingResource::Sampler(&macro_sampler),
                },
            ],
        });
        let water_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("water"),
            entries: &[texture_entry(
                0,
                wgpu::ShaderStages::FRAGMENT,
                wgpu::TextureSampleType::Depth,
                wgpu::TextureViewDimension::D2,
            )],
        });

        let terrain_shader = shader(
            &device,
            "terrain",
            include_str!("../../shaders/terrain.wgsl"),
        );
        let opaque_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("opaque"),
            bind_group_layouts: &[Some(&terrain_bgl)],
            immediate_size: 0,
        });
        let water_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("water"),
            bind_group_layouts: &[Some(&terrain_bgl), Some(&water_bgl)],
            immediate_size: 0,
        });
        let opaque_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("opaque"),
            layout: Some(&opaque_layout),
            vertex: wgpu::VertexState {
                module: &terrain_shader,
                entry_point: Some("vs_opaque"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Greater),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &terrain_shader,
                entry_point: Some("fs_opaque"),
                compilation_options: Default::default(),
                targets: &[Some(HDR_FORMAT.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        let water_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("water"),
            layout: Some(&water_layout),
            vertex: wgpu::VertexState {
                module: &terrain_shader,
                entry_point: Some("vs_water"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            // Water is drawn from both sides so the surface shows from below too.
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Greater),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &terrain_shader,
                entry_point: Some("fs_water"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: HDR_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        // Outline.
        let line_shader = shader(&device, "line", include_str!("../../shaders/line.wgsl"));
        let line_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("line"),
            entries: &[uniform_entry(0, wgpu::ShaderStages::VERTEX)],
        });
        let line_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("line"),
            layout: &line_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: line_buf.as_entire_binding(),
            }],
        });
        let line_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("line"),
            bind_group_layouts: &[Some(&line_bgl)],
            immediate_size: 0,
        });
        let line_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("line"),
            layout: Some(&line_layout),
            vertex: wgpu::VertexState {
                module: &line_shader,
                entry_point: Some("vs_line"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &line_shader,
                entry_point: Some("fs_line"),
                compilation_options: Default::default(),
                targets: &[Some(HDR_FORMAT.into())],
            }),
            multiview_mask: None,
            cache: None,
        });

        // HUD.
        let hud_shader = shader(&device, "hud", include_str!("../../shaders/hud.wgsl"));
        let hud_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("hud"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::VERTEX),
                texture_entry(
                    1,
                    wgpu::ShaderStages::FRAGMENT,
                    wgpu::TextureSampleType::Float { filterable: true },
                    wgpu::TextureViewDimension::D2Array,
                ),
                sampler_entry(2, wgpu::ShaderStages::FRAGMENT),
            ],
        });
        let hud_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("hud"),
            layout: &hud_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: hud_screen_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&tex_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&tex_sampler),
                },
            ],
        });
        let hud_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("hud"),
            bind_group_layouts: &[Some(&hud_bgl)],
            immediate_size: 0,
        });
        let hud_attrs = wgpu::vertex_attr_array![
            0 => Float32x2,
            1 => Float32x4,
            2 => Float32x2,
            3 => Float32x4,
        ];
        let hud_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("hud"),
            layout: Some(&hud_layout),
            vertex: wgpu::VertexState {
                module: &hud_shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<HudRect>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &hud_attrs,
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &hud_shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: output_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        // Culling.
        let cull_shader = shader(&device, "cull", include_str!("../../shaders/cull.wgsl"));
        let vis_c = wgpu::ShaderStages::COMPUTE;
        let cull_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cull"),
            entries: &[
                uniform_entry(0, vis_c),
                storage_entry(1, vis_c, true),
                storage_entry(2, vis_c, false),
                texture_entry(
                    3,
                    vis_c,
                    wgpu::TextureSampleType::Float { filterable: false },
                    wgpu::TextureViewDimension::D2,
                ),
                storage_entry(4, vis_c, false),
            ],
        });
        let cull_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cull"),
            bind_group_layouts: &[Some(&cull_bgl)],
            immediate_size: 0,
        });
        let cull_pipe = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("cull"),
            layout: Some(&cull_layout),
            module: &cull_shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });

        // Depth pyramid.
        let hzb_shader = shader(&device, "hzb", include_str!("../../shaders/hzb.wgsl"));
        let hzb_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("hzb"),
            entries: &[
                texture_entry(
                    0,
                    vis_c,
                    wgpu::TextureSampleType::Depth,
                    wgpu::TextureViewDimension::D2,
                ),
                texture_entry(
                    1,
                    vis_c,
                    wgpu::TextureSampleType::Float { filterable: false },
                    wgpu::TextureViewDimension::D2,
                ),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: vis_c,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::R32Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });
        let hzb_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("hzb"),
            bind_group_layouts: &[Some(&hzb_bgl)],
            immediate_size: 0,
        });
        let hzb_first_pipe = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("hzb first"),
            layout: Some(&hzb_layout),
            module: &hzb_shader,
            entry_point: Some("from_depth"),
            compilation_options: Default::default(),
            cache: None,
        });
        let hzb_down_pipe = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("hzb down"),
            layout: Some(&hzb_layout),
            module: &hzb_shader,
            entry_point: Some("downsample"),
            compilation_options: Default::default(),
            cache: None,
        });

        // Post.
        let post_shader = shader(&device, "post", include_str!("../../shaders/post.wgsl"));
        let vis_f = wgpu::ShaderStages::FRAGMENT;
        let post_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post"),
            entries: &[
                texture_entry(
                    0,
                    vis_f,
                    wgpu::TextureSampleType::Float { filterable: true },
                    wgpu::TextureViewDimension::D2,
                ),
                sampler_entry(1, vis_f),
                texture_entry(
                    2,
                    vis_f,
                    wgpu::TextureSampleType::Float { filterable: true },
                    wgpu::TextureViewDimension::D2,
                ),
            ],
        });
        let post_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("post"),
            bind_group_layouts: &[Some(&post_bgl)],
            immediate_size: 0,
        });
        let bright_pipe = fullscreen_pipeline(
            &device,
            "bright",
            &post_layout,
            &post_shader,
            "fs_bright",
            HDR_FORMAT,
            None,
        );
        let down_pipe = fullscreen_pipeline(
            &device,
            "down",
            &post_layout,
            &post_shader,
            "fs_down",
            HDR_FORMAT,
            None,
        );
        let additive = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent::REPLACE,
        };
        let up_pipe = fullscreen_pipeline(
            &device,
            "up",
            &post_layout,
            &post_shader,
            "fs_up",
            HDR_FORMAT,
            Some(additive),
        );
        let composite_pipe = fullscreen_pipeline(
            &device,
            "composite",
            &post_layout,
            &post_shader,
            "fs_composite",
            output_format,
            None,
        );

        // Bind-group slots that a given shader doesn't read still need a valid view.
        let small = |format: wgpu::TextureFormat, usage: wgpu::TextureUsages, label: &str| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: 1,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let dummy_depth_view = small(
            DEPTH_FORMAT,
            wgpu::TextureUsages::TEXTURE_BINDING,
            "dummy depth",
        );
        let dummy_color_view = small(
            wgpu::TextureFormat::R32Float,
            wgpu::TextureUsages::TEXTURE_BINDING,
            "dummy r32",
        );

        // GPU timing and culling stats.
        let timing = device.features().contains(wgpu::Features::TIMESTAMP_QUERY);
        let query_set = timing.then(|| {
            device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("timing"),
                ty: wgpu::QueryType::Timestamp,
                count: 8,
            })
        });
        let query_resolve = timing.then(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("query resolve"),
                size: 8 * 8,
                usage: U::QUERY_RESOLVE | U::COPY_SRC,
                mapped_at_creation: false,
            })
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: 8 * 8 + 16,
            usage: U::COPY_DST | U::MAP_READ,
            mapped_at_creation: false,
        });
        let timestamp_period = queue.get_timestamp_period();

        let targets = Self::create_targets(
            &device,
            render_size,
            &water_bgl,
            &cull_bgl,
            &hzb_bgl,
            &post_bgl,
            &linear_sampler,
            &cull_params_buf,
            &candidates_buf,
            &indirect_buf,
            &stats_buf,
            &dummy_depth_view,
            &dummy_color_view,
        );

        let chunks = ChunkGpu {
            queue: queue.clone(),
            quads: quads_buf,
            infos: infos_buf,
        };

        (
            Self {
                device,
                queue,
                chunks,
                output_format,
                index_buffer,
                globals_buf,
                draws_buf,
                candidates_buf,
                indirect_buf,
                water_indirect_buf,
                cull_params_buf,
                stats_buf,
                line_buf,
                hud_screen_buf,
                hud_buf,
                hud_capacity,
                terrain_bind,
                line_bind,
                hud_bind,
                linear_sampler,
                opaque_pipe,
                water_pipe,
                line_pipe,
                hud_pipe,
                cull_pipe,
                hzb_first_pipe,
                hzb_down_pipe,
                bright_pipe,
                down_pipe,
                up_pipe,
                composite_pipe,
                water_bgl,
                cull_bgl,
                hzb_bgl,
                post_bgl,
                targets,
                dummy_depth_view,
                dummy_color_view,
                prev_view_proj: Mat4::IDENTITY,
                prev_cam: DVec3::ZERO,
                hzb_ready: false,
                query_set,
                query_resolve,
                readback,
                readback_state: Default::default(),
                timestamp_period,
                last_gpu: ([0.0; 4], 0, 0),
            },
            adapter,
        )
    }

    pub fn output_format(&self) -> wgpu::TextureFormat {
        self.output_format
    }

    #[allow(clippy::too_many_arguments)]
    fn create_targets(
        device: &wgpu::Device,
        (w, h): (u32, u32),
        water_bgl: &wgpu::BindGroupLayout,
        cull_bgl: &wgpu::BindGroupLayout,
        hzb_bgl: &wgpu::BindGroupLayout,
        post_bgl: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        cull_params: &wgpu::Buffer,
        candidates: &wgpu::Buffer,
        indirect: &wgpu::Buffer,
        stats: &wgpu::Buffer,
        dummy_depth: &wgpu::TextureView,
        dummy_color: &wgpu::TextureView,
    ) -> SizedTargets {
        let tex = |label: &str,
                   w: u32,
                   h: u32,
                   mips: u32,
                   format: wgpu::TextureFormat,
                   usage: wgpu::TextureUsages| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: mips,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        use wgpu::TextureUsages as T;
        let hdr = tex(
            "hdr",
            w,
            h,
            1,
            HDR_FORMAT,
            T::RENDER_ATTACHMENT | T::TEXTURE_BINDING,
        );
        let hdr_view = hdr.create_view(&Default::default());
        let depth = tex(
            "depth",
            w,
            h,
            1,
            DEPTH_FORMAT,
            T::RENDER_ATTACHMENT | T::TEXTURE_BINDING | T::COPY_SRC,
        );
        let depth_view = depth.create_view(&Default::default());
        let depth_copy = tex(
            "depth copy",
            w,
            h,
            1,
            DEPTH_FORMAT,
            T::COPY_DST | T::TEXTURE_BINDING,
        );
        let depth_copy_view = depth_copy.create_view(&Default::default());

        // The pyramid base is the largest power of two that fits the screen, so
        // that every level halves exactly.
        let pw = 1u32 << (31 - w.max(2).leading_zeros());
        let ph = 1u32 << (31 - h.max(2).leading_zeros());
        let mips = 32 - pw.max(ph).leading_zeros();
        let pyramid = tex(
            "hzb",
            pw,
            ph,
            mips,
            wgpu::TextureFormat::R32Float,
            T::STORAGE_BINDING | T::TEXTURE_BINDING,
        );
        let pyramid_full = pyramid.create_view(&Default::default());
        let pyramid_views: Vec<_> = (0..mips)
            .map(|m| {
                pyramid.create_view(&wgpu::TextureViewDescriptor {
                    base_mip_level: m,
                    mip_level_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        let pyramid_binds: Vec<_> = (0..mips as usize)
            .map(|m| {
                let color = if m == 0 {
                    dummy_color
                } else {
                    &pyramid_views[m - 1]
                };
                let depth_src = if m == 0 { &depth_view } else { dummy_depth };
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("hzb level"),
                    layout: hzb_bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(depth_src),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(color),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(&pyramid_views[m]),
                        },
                    ],
                })
            })
            .collect();

        let cull_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("cull"),
            layout: cull_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: cull_params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: candidates.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: indirect.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&pyramid_full),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: stats.as_entire_binding(),
                },
            ],
        });
        let water_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("water"),
            layout: water_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&depth_copy_view),
            }],
        });

        // Bloom chain, starting at half resolution.
        let mut bloom_views = Vec::new();
        let (mut bw, mut bh) = ((w / 2).max(1), (h / 2).max(1));
        for _ in 0..BLOOM_LEVELS {
            let t = tex(
                "bloom",
                bw,
                bh,
                1,
                HDR_FORMAT,
                T::RENDER_ATTACHMENT | T::TEXTURE_BINDING,
            );
            bloom_views.push(t.create_view(&Default::default()));
            bw = (bw / 2).max(1);
            bh = (bh / 2).max(1);
        }
        let post_bg = |src: &wgpu::TextureView, bloom: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("post"),
                layout: post_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(src),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(bloom),
                    },
                ],
            })
        };
        // Passes other than the composite never read this slot, but it must not alias
        // the texture they are writing to.
        let unused_tex = tex("unused", 1, 1, 1, HDR_FORMAT, T::TEXTURE_BINDING);
        let unused_view = unused_tex.create_view(&Default::default());
        let unused = &unused_view;
        let post_binds = PostBinds {
            bright: post_bg(&hdr_view, unused),
            down: (0..BLOOM_LEVELS - 1)
                .map(|i| post_bg(&bloom_views[i], unused))
                .collect(),
            up: (0..BLOOM_LEVELS - 1)
                .map(|i| post_bg(&bloom_views[i + 1], unused))
                .collect(),
            composite: post_bg(&hdr_view, &bloom_views[0]),
        };

        SizedTargets {
            hdr_view,
            depth,
            depth_view,
            depth_copy,
            pyramid_views,
            pyramid_binds,
            bloom_views,
            post_binds,
            water_bind,
            cull_bind,
            render_size: (w, h),
            pyramid_size: (pw, ph),
        }
    }

    pub fn resize(&mut self, size: (u32, u32)) {
        if size == self.targets.render_size || size.0 == 0 || size.1 == 0 {
            return;
        }
        self.targets = Self::create_targets(
            &self.device,
            size,
            &self.water_bgl,
            &self.cull_bgl,
            &self.hzb_bgl,
            &self.post_bgl,
            &self.linear_sampler,
            &self.cull_params_buf,
            &self.candidates_buf,
            &self.indirect_buf,
            &self.stats_buf,
            &self.dummy_depth_view,
            &self.dummy_color_view,
        );
        self.hzb_ready = false;
    }

    /// Call on world reset: last frame's depth no longer means anything.
    pub fn invalidate_occlusion(&mut self) {
        self.hzb_ready = false;
    }

    /// Draws one frame into `output`, whose size may differ from the render size.
    pub fn render(
        &mut self,
        output: &wgpu::TextureView,
        output_size: (u32, u32),
        stream: &Streamer,
        p: &FrameParams,
    ) -> FrameStats {
        let (rw, rh) = self.targets.render_size;
        let aspect = rw as f32 / rh.max(1) as f32;
        // Right-handed, depth reversed (near = 1), with no far plane.
        let f = 1.0 / (p.fov_y * 0.5).tan();
        let proj = Mat4::from_cols(
            Vec4::new(f / aspect, 0.0, 0.0, 0.0),
            Vec4::new(0.0, f, 0.0, 0.0),
            Vec4::new(0.0, 0.0, 0.0, -1.0),
            Vec4::new(0.0, 0.0, NEAR_PLANE, 0.0),
        );
        let view_proj = proj * p.view;

        let mut fog = p.fog_color;
        let mut fog_distance = p.fog_distance;
        if p.underwater {
            fog = Vec3::new(0.016, 0.085, 0.115);
            fog_distance = 150.0;
        } else if p.in_lava {
            fog = Vec3::new(1.4, 0.34, 0.03);
            fog_distance = 7.0;
        }
        let wrap = |v: f64| v.rem_euclid(MACRO_TILE) as f32;
        let globals = Globals {
            view_proj: view_proj.to_cols_array_2d(),
            cam_wrapped: [
                wrap(p.cam_pos.x),
                wrap(p.cam_pos.y),
                wrap(p.cam_pos.z),
                p.time,
            ],
            cam_info: [
                p.cam_pos.y as f32,
                NEAR_PLANE,
                fog_distance,
                p.underwater as u32 as f32,
            ],
            fog_color: [fog.x, fog.y, fog.z, 1.0],
            ambient: [p.ambient.x, p.ambient.y, p.ambient.z, 1.0],
            screen: [rw as f32, rh as f32, 0.0, 0.0],
        };
        self.queue
            .write_buffer(&self.globals_buf, 0, bytemuck::bytes_of(&globals));

        // CPU pass: frustum test plus removing face directions turned away.
        let planes = frustum_planes(view_proj);
        let mut draws: Vec<DrawData> = Vec::with_capacity(32 * 1024);
        let mut candidates: Vec<Candidate> = Vec::with_capacity(32 * 1024);
        let mut water: Vec<(u32, u32, DrawData)> = Vec::new();
        let mut stats = FrameStats::default();
        for &id in &stream.selected {
            let Some(l) = stream.slab[id as usize].as_ref() else {
                continue;
            };
            let Some((first, _)) = l.alloc else {
                continue;
            };
            let k = l.key;
            let o = k.origin();
            let s = k.voxel_size() as f32;
            let origin = [
                (o[0] as f64 - p.cam_pos.x) as f32,
                (o[1] as f64 - p.cam_pos.y) as f32,
                (o[2] as f64 - p.cam_pos.z) as f32,
            ];
            let mut bmin = [0f32; 3];
            let mut bmax = [0f32; 3];
            for a in 0..3 {
                bmin[a] = origin[a] + l.bounds_min[a] as f32 * s;
                bmax[a] = origin[a] + l.bounds_max[a] as f32 * s;
            }
            // Water top faces get snapped to the real water level, which can lie
            // up to half a voxel outside the mesh bounds.
            if l.ranges[WATER_RANGE].1 > 0 {
                bmin[1] -= s;
                bmax[1] += s;
            }
            if !aabb_in_frustum(&planes, bmin, bmax) {
                continue;
            }
            stats.chunks_in_frustum += 1;
            let draw = DrawData {
                origin,
                slot: l.slot,
            };

            // The camera sits at the origin of this space. A +x face can only be
            // visible if the camera is on its positive side, and so on.
            for dir in 0..6usize {
                let (start, count) = l.ranges[dir];
                if count == 0 {
                    continue;
                }
                let axis = dir / 2;
                let visible = if dir & 1 == 0 {
                    bmin[axis] < 0.0
                } else {
                    bmax[axis] > 0.0
                };
                if !visible {
                    continue;
                }
                let mut off = first + start;
                let mut left = count;
                while left > 0 {
                    let n = left.min(QUADS_PER_DRAW);
                    if candidates.len() < MAX_DRAWS as usize {
                        draws.push(draw);
                        candidates.push(Candidate {
                            bounds_min: bmin,
                            index_count: n * 6,
                            bounds_max: bmax,
                            base_vertex: (off * 4) as i32,
                        });
                        stats.candidate_quads += n as u64;
                    }
                    off += n;
                    left -= n;
                }
            }
            let (wstart, wcount) = l.ranges[WATER_RANGE];
            if wcount > 0 {
                let mut off = first + wstart;
                let mut left = wcount;
                while left > 0 {
                    let n = left.min(QUADS_PER_DRAW);
                    water.push((off, n, draw));
                    off += n;
                    left -= n;
                }
            }
        }
        stats.candidates = candidates.len() as u32;

        // Water blends, so it goes back to front: `selected` is nearest first.
        water.reverse();
        water.truncate(MAX_WATER_DRAWS as usize);
        let water_base = draws.len() as u32;
        let mut water_indirect: Vec<[u32; 5]> = Vec::with_capacity(water.len());
        for (i, (off, n, draw)) in water.iter().enumerate() {
            draws.push(*draw);
            water_indirect.push([n * 6, 1, 0, off * 4, water_base + i as u32]);
        }
        stats.water_draws = water_indirect.len() as u32;

        if !draws.is_empty() {
            self.queue
                .write_buffer(&self.draws_buf, 0, bytemuck::cast_slice(&draws));
        }
        if !candidates.is_empty() {
            self.queue
                .write_buffer(&self.candidates_buf, 0, bytemuck::cast_slice(&candidates));
        }
        if !water_indirect.is_empty() {
            self.queue.write_buffer(
                &self.water_indirect_buf,
                0,
                bytemuck::cast_slice(&water_indirect),
            );
        }

        let shift = p.cam_pos - self.prev_cam;
        let mips = self.targets.pyramid_views.len() as f32;
        let (pw, ph) = self.targets.pyramid_size;
        let cull_params = CullParams {
            prev_view_proj: self.prev_view_proj.to_cols_array_2d(),
            shift: [shift.x as f32, shift.y as f32, shift.z as f32],
            count: candidates.len() as u32,
            pyramid_size: [pw as f32, ph as f32],
            mip_count: mips,
            enabled: (self.hzb_ready && p.occlusion) as u32,
        };
        self.queue
            .write_buffer(&self.cull_params_buf, 0, bytemuck::bytes_of(&cull_params));
        self.queue.write_buffer(&self.stats_buf, 0, &[0u8; 16]);

        if let Some(rect) = p.outline {
            let params = LineParams {
                view_proj: view_proj.to_cols_array_2d(),
                origin: [
                    (rect[0] as f64 - p.cam_pos.x) as f32,
                    (rect[1] as f64 - p.cam_pos.y) as f32,
                    (rect[2] as f64 - p.cam_pos.z) as f32,
                ],
                pad: 0.0,
            };
            self.queue
                .write_buffer(&self.line_buf, 0, bytemuck::bytes_of(&params));
        }

        self.queue.write_buffer(
            &self.hud_screen_buf,
            0,
            bytemuck::cast_slice(&[output_size.0 as f32, output_size.1 as f32, 0.0, 0.0]),
        );
        let hud_count = p.hud.len().min(self.hud_capacity);
        if hud_count > 0 {
            self.queue
                .write_buffer(&self.hud_buf, 0, bytemuck::cast_slice(&p.hud[..hud_count]));
        }

        let mut enc = self.device.create_command_encoder(&Default::default());
        let qs = self.query_set.as_ref();
        let ts_c = |b: u32, e: u32| {
            qs.map(|q| wgpu::ComputePassTimestampWrites {
                query_set: q,
                beginning_of_pass_write_index: Some(b),
                end_of_pass_write_index: Some(e),
            })
        };
        let ts_r = |b: u32, e: u32| {
            qs.map(|q| wgpu::RenderPassTimestampWrites {
                query_set: q,
                beginning_of_pass_write_index: Some(b),
                end_of_pass_write_index: Some(e),
            })
        };

        // Depth pyramid from the frame before, then the occlusion test.
        {
            let mut c = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("cull"),
                timestamp_writes: ts_c(0, 1),
            });
            if self.hzb_ready && p.occlusion {
                for (m, bind) in self.targets.pyramid_binds.iter().enumerate() {
                    c.set_pipeline(if m == 0 {
                        &self.hzb_first_pipe
                    } else {
                        &self.hzb_down_pipe
                    });
                    c.set_bind_group(0, bind, &[]);
                    let w = (pw >> m).max(1);
                    let h = (ph >> m).max(1);
                    c.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
                }
            }
            if !candidates.is_empty() {
                c.set_pipeline(&self.cull_pipe);
                c.set_bind_group(0, &self.targets.cull_bind, &[]);
                c.dispatch_workgroups((candidates.len() as u32).div_ceil(64), 1, 1);
            }
        }

        // Opaque terrain.
        {
            let mut r = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("opaque"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.targets.hdr_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: fog.x as f64,
                            g: fog.y as f64,
                            b: fog.z as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.targets.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: ts_r(2, 3),
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if !candidates.is_empty() {
                r.set_pipeline(&self.opaque_pipe);
                r.set_bind_group(0, &self.terrain_bind, &[]);
                r.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
                r.multi_draw_indexed_indirect(&self.indirect_buf, 0, candidates.len() as u32);
            }
        }

        // Water reads the opaque depth to work out how deep each pixel is.
        if !water_indirect.is_empty() {
            enc.copy_texture_to_texture(
                self.targets.depth.as_image_copy(),
                self.targets.depth_copy.as_image_copy(),
                wgpu::Extent3d {
                    width: rw,
                    height: rh,
                    depth_or_array_layers: 1,
                },
            );
        }
        {
            let mut r = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("transparent"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.targets.hdr_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.targets.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: ts_r(4, 5),
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if !water_indirect.is_empty() {
                r.set_pipeline(&self.water_pipe);
                r.set_bind_group(0, &self.terrain_bind, &[]);
                r.set_bind_group(1, &self.targets.water_bind, &[]);
                r.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
                r.multi_draw_indexed_indirect(
                    &self.water_indirect_buf,
                    0,
                    water_indirect.len() as u32,
                );
            }
            if p.outline.is_some() {
                r.set_pipeline(&self.line_pipe);
                r.set_bind_group(0, &self.line_bind, &[]);
                r.draw(0..24, 0..1);
            }
        }

        // Bloom: bright-pass down the chain, then blur back up.
        let t = &self.targets;
        let post_pass = |enc: &mut wgpu::CommandEncoder,
                         view: &wgpu::TextureView,
                         load: wgpu::LoadOp<wgpu::Color>,
                         pipe: &wgpu::RenderPipeline,
                         bind: &wgpu::BindGroup,
                         ts: Option<wgpu::RenderPassTimestampWrites>| {
            let mut r = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("post"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: ts,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            r.set_pipeline(pipe);
            r.set_bind_group(0, bind, &[]);
            r.draw(0..3, 0..1);
        };
        let clear = wgpu::LoadOp::Clear(wgpu::Color::BLACK);
        post_pass(
            &mut enc,
            &t.bloom_views[0],
            clear,
            &self.bright_pipe,
            &t.post_binds.bright,
            None,
        );
        for i in 0..BLOOM_LEVELS - 1 {
            post_pass(
                &mut enc,
                &t.bloom_views[i + 1],
                clear,
                &self.down_pipe,
                &t.post_binds.down[i],
                None,
            );
        }
        for i in (0..BLOOM_LEVELS - 1).rev() {
            post_pass(
                &mut enc,
                &t.bloom_views[i],
                wgpu::LoadOp::Load,
                &self.up_pipe,
                &t.post_binds.up[i],
                None,
            );
        }

        // Composite to the output, with the HUD on top.
        {
            let mut r = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("composite"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: output,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: clear,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: ts_r(6, 7),
                occlusion_query_set: None,
                multiview_mask: None,
            });
            r.set_pipeline(&self.composite_pipe);
            r.set_bind_group(0, &t.post_binds.composite, &[]);
            r.draw(0..3, 0..1);
            if hud_count > 0 {
                r.set_pipeline(&self.hud_pipe);
                r.set_bind_group(0, &self.hud_bind, &[]);
                r.set_vertex_buffer(0, self.hud_buf.slice(..));
                r.draw(0..4, 0..hud_count as u32);
            }
        }

        // Timing and stats come back asynchronously, whenever the buffer is free.
        use std::sync::atomic::Ordering;
        let can_read = self.readback_state.load(Ordering::Acquire) == 0;
        if can_read {
            if let (Some(q), Some(res)) = (&self.query_set, &self.query_resolve) {
                enc.resolve_query_set(q, 0..8, res, 0);
                enc.copy_buffer_to_buffer(res, 0, &self.readback, 0, 64);
            }
            enc.copy_buffer_to_buffer(&self.stats_buf, 0, &self.readback, 64, 16);
        }
        self.queue.submit([enc.finish()]);

        if can_read {
            self.readback_state.store(1, Ordering::Release);
            let state = self.readback_state.clone();
            self.readback
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |res| {
                    state.store(if res.is_ok() { 2 } else { 0 }, Ordering::Release);
                });
        }
        let _ = self.device.poll(wgpu::PollType::Poll);
        if self.readback_state.load(Ordering::Acquire) == 2 {
            {
                let data = self
                    .readback
                    .slice(..)
                    .get_mapped_range()
                    .expect("mapped readback");
                let ts: &[u64] = bytemuck::cast_slice(&data[..64]);
                let period_ms = self.timestamp_period as f64 / 1.0e6;
                let dur =
                    |a: usize, b: usize| (ts[b].wrapping_sub(ts[a]) as f64 * period_ms) as f32;
                let s: &[u32] = bytemuck::cast_slice(&data[64..80]);
                self.last_gpu = ([dur(0, 1), dur(2, 3), dur(4, 5), dur(6, 7)], s[0], s[1]);
            }
            self.readback.unmap();
            self.readback_state.store(0, Ordering::Release);
        }
        stats.gpu_ms = self.last_gpu.0;
        stats.gpu_drawn = self.last_gpu.1;
        stats.gpu_quads = self.last_gpu.2;

        self.prev_view_proj = view_proj;
        self.prev_cam = p.cam_pos;
        self.hzb_ready = true;
        stats
    }

    /// Reads an RGBA8 texture back to memory. Blocks until the GPU is done.
    pub fn read_texture(&self, texture: &wgpu::Texture, size: (u32, u32)) -> Vec<u8> {
        let row = (size.0 * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture"),
            size: row as u64 * size.1 as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([enc.finish()]);
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, |r| r.expect("map capture"));
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        let data = buffer.slice(..).get_mapped_range().expect("mapped capture");
        let mut out = Vec::with_capacity((size.0 * size.1 * 4) as usize);
        for y in 0..size.1 as usize {
            let start = y * row as usize;
            out.extend_from_slice(&data[start..start + size.0 as usize * 4]);
        }
        out
    }
}

/// Side planes and near plane of the camera, facing inward. The reversed
/// infinite projection has no far plane.
fn frustum_planes(m: Mat4) -> [Vec4; 5] {
    let r0 = m.row(0);
    let r1 = m.row(1);
    let r2 = m.row(2);
    let r3 = m.row(3);
    [r3 + r0, r3 - r0, r3 + r1, r3 - r1, r3 - r2]
}

#[inline]
fn aabb_in_frustum(planes: &[Vec4; 5], bmin: [f32; 3], bmax: [f32; 3]) -> bool {
    for p in planes {
        let x = if p.x >= 0.0 { bmax[0] } else { bmin[0] };
        let y = if p.y >= 0.0 { bmax[1] } else { bmin[1] };
        let z = if p.z >= 0.0 { bmax[2] } else { bmin[2] };
        if p.x * x + p.y * y + p.z * z + p.w < 0.0 {
            return false;
        }
    }
    true
}
