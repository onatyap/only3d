//! wgpu side of the 3D viewport.
//!
//! The scene is drawn off-screen (4x MSAA into an sRGB texture with a reversed-Z depth buffer)
//! inside [`egui_wgpu::CallbackTrait::prepare`], then blitted into egui's render pass in `paint`,
//! where the selection outline is composited from a 1x mask texture. GPU buffers are cached per
//! mesh `Arc` pointer (the cache keeps the `Arc` alive, so a pointer is never reused while
//! cached) and per slice for toolpaths, so nothing is re-uploaded unless the geometry changed.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use eframe::egui;
use egui_wgpu::wgpu;
use only3d_geom::Mesh;
use wgpu::util::DeviceExt;

pub const MSAA: u32 = 4;

/// Samples per pixel for the scene: 4x MSAA on a GPU, none in software rendering, where every
/// sample costs CPU time (llvmpipe, WARP). `ONLY3D_MSAA` (1, 2, 4, 8) overrides, for comparing.
pub fn samples_for(info: &wgpu::AdapterInfo) -> u32 {
    if let Some(n) = std::env::var("ONLY3D_MSAA").ok().and_then(|v| v.trim().parse::<u32>().ok()).filter(|n| [1, 2, 4, 8].contains(n)) {
        return n;
    }
    if info.device_type == wgpu::DeviceType::Cpu {
        1
    } else {
        MSAA
    }
}
const SCENE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const MASK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct Globals {
    pub view_proj: [[f32; 4]; 4],
    pub eye: [f32; 4],
    /// width px, height px, focal length px, outline radius px
    pub viewport: [f32; 4],
    pub key_dir: [f32; 4],
    pub fill_dir: [f32; 4],
    pub accent: [f32; 4],
    pub outline: [f32; 4],
    pub bg_top: [f32; 4],
    pub bg_bottom: [f32; 4],
    /// feature mask, first highlighted toolpath instance, gamma-encode output, has selection
    pub flags: [u32; 4],
    /// Background dot grid: linear RGB + spacing in px (0 = off).
    pub dots: [f32; 4],
    /// Overhang highlight: linear RGB + on (1) / off (0).
    pub overhang: [f32; 4],
}

/// Per-draw data for the mesh pipeline (instance-rate vertex attributes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct ObjInstance {
    /// Linear RGB + unused alpha.
    pub color: [f32; 4],
    /// bit 0 = unlit, bit 1 = selected
    pub flags: u32,
    /// Live drag preview: translate (dx, dy) and rotate `angle` radians about Z around `pivot`.
    pub xf: [f32; 4],
    pub pivot: [f32; 2],
}

impl ObjInstance {
    pub fn plain(color: [f32; 4], flags: u32) -> Self {
        ObjInstance { color, flags, xf: [0.0; 4], pivot: [0.0; 2] }
    }
}

/// One ribbon segment (toolpath or plate line). `color` is sRGB; its alpha byte is the feature
/// index (255 = always visible). `width` > 0 is world mm, < 0 is screen px.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct Seg {
    pub a: [f32; 3],
    pub b: [f32; 3],
    pub color: [u8; 4],
    pub width: f32,
}

/// Toolpaths of one slice. Segments are stored **top layer first** (so opaque ribbons are drawn
/// roughly front to back and early depth rejection works), which keeps any layer range one
/// contiguous instance range.
pub struct ToolpathData {
    pub segs: Vec<Seg>,
    /// `stored_starts[k]` = first segment of the k-th stored layer (layer `n-1-k`), plus an end.
    pub stored_starts: Vec<u32>,
    pub layer_z: Vec<f32>,
}

impl ToolpathData {
    pub fn layers(&self) -> usize {
        self.layer_z.len()
    }

    /// Instances for layers `lo..=hi`, and where the top layer's instances end (for highlighting).
    pub fn range(&self, lo: usize, hi: usize) -> (Range<u32>, u32) {
        let n = self.layers();
        if n == 0 {
            return (0..0, 0);
        }
        let hi = hi.min(n - 1);
        let lo = lo.min(hi);
        let (k_hi, k_lo) = (n - 1 - hi, n - 1 - lo);
        (self.stored_starts[k_hi]..self.stored_starts[k_lo + 1], self.stored_starts[k_hi + 1])
    }
}

/// Static plate geometry (rebuilt when the bed changes).
pub struct PlateData {
    pub key: u64,
    pub meshes: Vec<(Mesh, ObjInstance)>,
    pub lines: Vec<Seg>,
}

pub struct DrawObj {
    pub mesh: Arc<Mesh>,
    pub inst: ObjInstance,
}

pub struct ToolpathDraw {
    pub data: Arc<ToolpathData>,
    pub range: Range<u32>,
}

/// Everything one frame of the viewport needs. Built by the app each frame (cheap: Arcs).
pub struct FrameData {
    pub globals: Globals,
    pub size_px: [u32; 2],
    pub objects: Vec<DrawObj>,
    /// Meshes to keep cached even if not drawn this frame (hidden objects, model view while in
    /// toolpath mode).
    pub keep: Vec<Arc<Mesh>>,
    pub plate: Arc<PlateData>,
    pub toolpath: Option<ToolpathDraw>,
}

struct GpuMesh {
    _src: Arc<Mesh>,
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    n_idx: u32,
}

struct PlateGpu {
    key: u64,
    meshes: Vec<(wgpu::Buffer, wgpu::Buffer, u32, ObjInstance)>,
    lines: Option<(wgpu::Buffer, u32)>,
}

struct ToolpathGpu {
    data: Arc<ToolpathData>,
    buf: Option<wgpu::Buffer>,
}

struct Targets {
    size: [u32; 2],
    /// Multisampled colour and mask, resolved into `resolved` and `mask`; `None` without MSAA,
    /// when the scene draws into those directly.
    msaa: Option<(wgpu::TextureView, wgpu::TextureView)>,
    resolved: wgpu::TextureView,
    depth: wgpu::TextureView,
    mask: wgpu::TextureView,
    blit_bind: wgpu::BindGroup,
}

pub struct SceneGpu {
    bg_pipe: wgpu::RenderPipeline,
    mesh_pipe: wgpu::RenderPipeline,
    line_pipe: wgpu::RenderPipeline,
    blit_pipe: wgpu::RenderPipeline,
    blit_bgl: wgpu::BindGroupLayout,
    globals_buf: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    inst_buf: wgpu::Buffer,
    inst_cap: usize,
    meshes: HashMap<usize, GpuMesh>,
    plate: Option<PlateGpu>,
    toolpath: Option<ToolpathGpu>,
    targets: Option<Targets>,
    /// Draws prepared for this frame: (mesh key, instance index).
    frame_draws: Vec<(usize, u32)>,
    frame_selected: Vec<usize>,
    max_buffer: u64,
    /// Largest texture side the GPU accepts (8192 on Apple M-series): bigger frames are skipped.
    max_dim: u32,
    /// Scene samples per pixel (`samples_for`).
    samples: u32,
}

fn mesh_key(m: &Arc<Mesh>) -> usize {
    Arc::as_ptr(m) as usize
}

impl SceneGpu {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat, msaa_target: u32, samples: u32) -> Self {
        let scene_mod = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("o3d_scene"), source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()) });
        let blit_mod = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("o3d_blit"), source: wgpu::ShaderSource::Wgsl(include_str!("blit.wgsl").into()) });

        let globals_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("o3d_globals_bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            }],
        });
        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("o3d_globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("o3d_globals_bind"),
            layout: &globals_bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: globals_buf.as_entire_binding() }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("o3d_scene_layout"), bind_group_layouts: &[Some(&globals_bgl)], immediate_size: 0 });

        let depth = |write: bool, cmp: wgpu::CompareFunction| {
            Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(write),
                depth_compare: Some(cmp),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            })
        };
        let ms = wgpu::MultisampleState { count: samples, mask: !0, alpha_to_coverage_enabled: false };
        let color_target = [
            Some(wgpu::ColorTargetState { format: SCENE_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL }),
            Some(wgpu::ColorTargetState { format: MASK_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL }),
        ];
        let prim = wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, ..Default::default() };

        let bg_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("o3d_bg"),
            layout: Some(&layout),
            vertex: wgpu::VertexState { module: &scene_mod, entry_point: Some("vs_bg"), buffers: &[], compilation_options: Default::default() },
            primitive: prim,
            depth_stencil: depth(false, wgpu::CompareFunction::Always),
            multisample: ms,
            fragment: Some(wgpu::FragmentState { module: &scene_mod, entry_point: Some("fs_bg"), targets: &color_target, compilation_options: Default::default() }),
            multiview_mask: None,
            cache: None,
        });

        let pos_layout = wgpu::VertexBufferLayout { array_stride: 12, step_mode: wgpu::VertexStepMode::Vertex, attributes: &wgpu::vertex_attr_array![0 => Float32x3] };
        let inst_attrs = wgpu::vertex_attr_array![1 => Float32x4, 2 => Uint32, 3 => Float32x4, 4 => Float32x2];
        let inst_layout = wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<ObjInstance>() as u64, step_mode: wgpu::VertexStepMode::Instance, attributes: &inst_attrs };
        let mesh_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("o3d_mesh"),
            layout: Some(&layout),
            vertex: wgpu::VertexState { module: &scene_mod, entry_point: Some("vs_mesh"), buffers: &[Some(pos_layout), Some(inst_layout)], compilation_options: Default::default() },
            primitive: prim,
            depth_stencil: depth(true, wgpu::CompareFunction::GreaterEqual),
            multisample: ms,
            fragment: Some(wgpu::FragmentState { module: &scene_mod, entry_point: Some("fs_mesh"), targets: &color_target, compilation_options: Default::default() }),
            multiview_mask: None,
            cache: None,
        });

        let seg_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Unorm8x4, 3 => Float32];
        let seg_layout = wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<Seg>() as u64, step_mode: wgpu::VertexStepMode::Instance, attributes: &seg_attrs };
        let line_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("o3d_line"),
            layout: Some(&layout),
            vertex: wgpu::VertexState { module: &scene_mod, entry_point: Some("vs_line"), buffers: &[Some(seg_layout)], compilation_options: Default::default() },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
            depth_stencil: depth(true, wgpu::CompareFunction::GreaterEqual),
            multisample: ms,
            fragment: Some(wgpu::FragmentState { module: &scene_mod, entry_point: Some("fs_line"), targets: &color_target, compilation_options: Default::default() }),
            multiview_mask: None,
            cache: None,
        });

        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: false }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
            count: None,
        };
        let blit_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("o3d_blit_bgl"),
            entries: &[
                tex_entry(0),
                tex_entry(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
            ],
        });
        let blit_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("o3d_blit_layout"), bind_group_layouts: &[Some(&blit_bgl)], immediate_size: 0 });
        let blit_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("o3d_blit"),
            layout: Some(&blit_layout),
            vertex: wgpu::VertexState { module: &blit_mod, entry_point: Some("vs_blit"), buffers: &[], compilation_options: Default::default() },
            primitive: prim,
            depth_stencil: None,
            multisample: wgpu::MultisampleState { count: msaa_target.max(1), mask: !0, alpha_to_coverage_enabled: false },
            fragment: Some(wgpu::FragmentState {
                module: &blit_mod,
                entry_point: Some("fs_blit"),
                targets: &[Some(wgpu::ColorTargetState { format: target_format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        });

        let inst_cap = 256;
        let inst_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("o3d_instances"),
            size: (inst_cap * std::mem::size_of::<ObjInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        SceneGpu {
            bg_pipe,
            mesh_pipe,
            line_pipe,
            blit_pipe,
            blit_bgl,
            globals_buf,
            globals_bind,
            inst_buf,
            inst_cap,
            meshes: HashMap::new(),
            plate: None,
            toolpath: None,
            targets: None,
            frame_draws: Vec::new(),
            frame_selected: Vec::new(),
            max_buffer: device.limits().max_buffer_size,
            max_dim: device.limits().max_texture_dimension_2d,
            samples,
        }
    }

    fn ensure_targets(&mut self, device: &wgpu::Device, size: [u32; 2]) {
        if self.targets.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        let ext = wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 };
        let tex = |label, format, samples, usage| {
            device
                .create_texture(&wgpu::TextureDescriptor { label: Some(label), size: ext, mip_level_count: 1, sample_count: samples, dimension: wgpu::TextureDimension::D2, format, usage, view_formats: &[] })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let n = self.samples;
        let msaa = (n > 1).then(|| (tex("o3d_msaa_color", SCENE_FORMAT, n, wgpu::TextureUsages::RENDER_ATTACHMENT), tex("o3d_msaa_mask", MASK_FORMAT, n, wgpu::TextureUsages::RENDER_ATTACHMENT)));
        let resolved = tex("o3d_resolved", SCENE_FORMAT, 1, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING);
        let depth = tex("o3d_depth", DEPTH_FORMAT, n, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let mask = tex("o3d_mask", MASK_FORMAT, 1, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING);
        let blit_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("o3d_blit_bind"),
            layout: &self.blit_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&resolved) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&mask) },
                wgpu::BindGroupEntry { binding: 2, resource: self.globals_buf.as_entire_binding() },
            ],
        });
        self.targets = Some(Targets { size, msaa, resolved, depth, mask, blit_bind });
    }

    fn upload_mesh(&self, device: &wgpu::Device, m: &Arc<Mesh>) -> Option<GpuMesh> {
        if m.indices.is_empty() || m.positions.is_empty() {
            return None;
        }
        let vbytes: &[u8] = bytemuck::cast_slice(&m.positions);
        let ibytes: &[u8] = bytemuck::cast_slice(&m.indices);
        if vbytes.len() as u64 > self.max_buffer || ibytes.len() as u64 > self.max_buffer {
            return None;
        }
        let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("o3d_mesh_v"), contents: vbytes, usage: wgpu::BufferUsages::VERTEX });
        let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("o3d_mesh_i"), contents: ibytes, usage: wgpu::BufferUsages::INDEX });
        Some(GpuMesh { _src: m.clone(), vbuf, ibuf, n_idx: (m.indices.len() * 3) as u32 })
    }

    fn sync(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, f: &FrameData) {
        // meshes
        let mut live: HashSet<usize> = HashSet::new();
        for m in f.objects.iter().map(|d| &d.mesh).chain(f.keep.iter()) {
            let k = mesh_key(m);
            live.insert(k);
            if !self.meshes.contains_key(&k) {
                if let Some(g) = self.upload_mesh(device, m) {
                    self.meshes.insert(k, g);
                }
            }
        }
        self.meshes.retain(|k, _| live.contains(k));

        // plate
        if self.plate.as_ref().map(|p| p.key) != Some(f.plate.key) {
            let meshes = f
                .plate
                .meshes
                .iter()
                .filter(|(m, _)| !m.indices.is_empty())
                .map(|(m, inst)| {
                    let v = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("o3d_plate_v"), contents: bytemuck::cast_slice(&m.positions), usage: wgpu::BufferUsages::VERTEX });
                    let i = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("o3d_plate_i"), contents: bytemuck::cast_slice(&m.indices), usage: wgpu::BufferUsages::INDEX });
                    (v, i, (m.indices.len() * 3) as u32, *inst)
                })
                .collect();
            let lines = (!f.plate.lines.is_empty()).then(|| {
                (device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("o3d_plate_lines"), contents: bytemuck::cast_slice(&f.plate.lines), usage: wgpu::BufferUsages::VERTEX }), f.plate.lines.len() as u32)
            });
            self.plate = Some(PlateGpu { key: f.plate.key, meshes, lines });
        }

        // toolpaths: upload once per slice
        if let Some(tp) = &f.toolpath {
            let same = self.toolpath.as_ref().is_some_and(|t| Arc::ptr_eq(&t.data, &tp.data));
            if !same {
                let bytes: &[u8] = bytemuck::cast_slice(&tp.data.segs);
                let buf = (!bytes.is_empty() && (bytes.len() as u64) <= self.max_buffer)
                    .then(|| device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("o3d_toolpaths"), contents: bytes, usage: wgpu::BufferUsages::VERTEX }));
                self.toolpath = Some(ToolpathGpu { data: tp.data.clone(), buf });
            }
        }

        // per-draw instances: plate meshes first, then objects
        let plate_n = self.plate.as_ref().map(|p| p.meshes.len()).unwrap_or(0);
        let mut inst: Vec<ObjInstance> = Vec::with_capacity(plate_n + f.objects.len());
        if let Some(p) = &self.plate {
            inst.extend(p.meshes.iter().map(|m| m.3));
        }
        self.frame_draws.clear();
        self.frame_selected.clear();
        for d in &f.objects {
            let k = mesh_key(&d.mesh);
            if self.meshes.contains_key(&k) {
                self.frame_draws.push((k, inst.len() as u32));
                if d.inst.flags & 2 != 0 {
                    self.frame_selected.push(k);
                }
                inst.push(d.inst);
            }
        }
        if inst.len() > self.inst_cap {
            self.inst_cap = inst.len().next_power_of_two();
            self.inst_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("o3d_instances"),
                size: (self.inst_cap * std::mem::size_of::<ObjInstance>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !inst.is_empty() {
            queue.write_buffer(&self.inst_buf, 0, bytemuck::cast_slice(&inst));
        }
        let mut g = f.globals;
        g.flags[3] = u32::from(!self.frame_selected.is_empty());
        queue.write_buffer(&self.globals_buf, 0, bytemuck::bytes_of(&g));
    }

    fn encode(&self, enc: &mut wgpu::CommandEncoder, f: &FrameData) {
        let Some(t) = &self.targets else { return };
        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("o3d_scene_pass"),
                // with MSAA: draw into the multisampled textures and resolve; without: draw into
                // the resolved ones directly and keep what's drawn
                color_attachments: &[
                    Some(wgpu::RenderPassColorAttachment {
                        view: t.msaa.as_ref().map(|m| &m.0).unwrap_or(&t.resolved),
                        resolve_target: t.msaa.as_ref().map(|_| &t.resolved),
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: if t.msaa.is_some() { wgpu::StoreOp::Discard } else { wgpu::StoreOp::Store } },
                        depth_slice: None,
                    }),
                    Some(wgpu::RenderPassColorAttachment {
                        view: t.msaa.as_ref().map(|m| &m.1).unwrap_or(&t.mask),
                        resolve_target: t.msaa.as_ref().map(|_| &t.mask),
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: if t.msaa.is_some() { wgpu::StoreOp::Discard } else { wgpu::StoreOp::Store } },
                        depth_slice: None,
                    }),
                ],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &t.depth,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(0.0), store: wgpu::StoreOp::Discard }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            rp.set_bind_group(0, &self.globals_bind, &[]);
            rp.set_pipeline(&self.bg_pipe);
            rp.draw(0..3, 0..1);

            rp.set_pipeline(&self.mesh_pipe);
            rp.set_vertex_buffer(1, self.inst_buf.slice(..));
            if let Some(p) = &self.plate {
                for (i, (v, ib, n, _)) in p.meshes.iter().enumerate() {
                    rp.set_vertex_buffer(0, v.slice(..));
                    rp.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                    rp.draw_indexed(0..*n, 0, i as u32..i as u32 + 1);
                }
            }
            for (k, ii) in &self.frame_draws {
                if let Some(m) = self.meshes.get(k) {
                    rp.set_vertex_buffer(0, m.vbuf.slice(..));
                    rp.set_index_buffer(m.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    rp.draw_indexed(0..m.n_idx, 0, *ii..*ii + 1);
                }
            }

            rp.set_pipeline(&self.line_pipe);
            if let Some((buf, n)) = self.plate.as_ref().and_then(|p| p.lines.as_ref()) {
                rp.set_vertex_buffer(0, buf.slice(..));
                rp.draw(0..4, 0..*n);
            }
            if let (Some(tp), Some(gpu)) = (&f.toolpath, &self.toolpath) {
                if let Some(buf) = &gpu.buf {
                    let r = tp.range.start.min(tp.data.segs.len() as u32)..tp.range.end.min(tp.data.segs.len() as u32);
                    if r.end > r.start {
                        rp.set_vertex_buffer(0, buf.slice(..));
                        rp.draw(0..4, r);
                    }
                }
            }
        }

    }
}

/// The egui paint callback for the viewport.
pub struct ViewportCallback {
    pub frame: Arc<FrameData>,
}

impl egui_wgpu::CallbackTrait for ViewportCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(scene) = resources.get_mut::<SceneGpu>() else { return Vec::new() };
        let f = &self.frame;
        // Nothing to draw, or a frame larger than any texture the GPU can hold. The latter happens
        // on the first frame at launch, before the window reports its real size (egui assumes a
        // 10 000-point screen); creating the targets would abort the app. The next frame is right.
        if f.size_px[0] == 0 || f.size_px[1] == 0 || f.size_px[0] > scene.max_dim || f.size_px[1] > scene.max_dim {
            return Vec::new();
        }
        scene.ensure_targets(device, f.size_px);
        scene.sync(device, queue, f);
        scene.encode(encoder, f);
        Vec::new()
    }

    fn paint(&self, _info: egui::PaintCallbackInfo, rp: &mut wgpu::RenderPass<'static>, resources: &egui_wgpu::CallbackResources) {
        if let Some(scene) = resources.get::<SceneGpu>() {
            scene.blit(rp, self.frame.size_px);
        }
    }
}

impl SceneGpu {
    /// Composite the off-screen scene (and selection outline) into the current pass.
    fn blit(&self, rp: &mut wgpu::RenderPass<'_>, size_px: [u32; 2]) {
        let Some(t) = &self.targets else { return };
        if t.size != size_px {
            return;
        }
        rp.set_pipeline(&self.blit_pipe);
        rp.set_bind_group(0, &t.blit_bind, &[]);
        rp.draw(0..3, 0..1);
    }
}

fn bench_blob(n: usize) -> Mesh {
    let seg = ((n as f64 / 2.0).sqrt() as usize).max(8);
    let mut m = Mesh::new();
    for i in 0..=seg {
        for j in 0..=seg {
            let th = std::f32::consts::PI * i as f32 / seg as f32;
            let ph = std::f32::consts::TAU * j as f32 / seg as f32;
            let r = 30.0 + 2.0 * (7.0 * ph).sin() * th.sin();
            m.positions.push(glam::Vec3::new(175.0 + r * th.sin() * ph.cos(), 160.0 + r * th.sin() * ph.sin(), 30.0 + r * th.cos()));
        }
    }
    let w = seg as u32 + 1;
    for i in 0..seg as u32 {
        for j in 0..seg as u32 {
            let (a, b, c, d) = (i * w + j, (i + 1) * w + j, (i + 1) * w + j + 1, i * w + j + 1);
            m.indices.push([a, b, d]);
            m.indices.push([b, c, d]);
        }
    }
    m
}

fn bench_toolpaths(n: usize) -> ToolpathData {
    let mut segs = Vec::with_capacity(n);
    let mut stored_starts = Vec::new();
    let mut layer_z = Vec::new();
    let per_layer = 10_000;
    while segs.len() < n {
        let l = layer_z.len();
        stored_starts.push(segs.len() as u32);
        let z = 0.2 + l as f32 * 0.2;
        layer_z.push(z);
        for k in 0..per_layer {
            let a = k as f32 / per_layer as f32 * std::f32::consts::TAU * 20.0;
            let r = 10.0 + (k % 500) as f32 * 0.1;
            let p = |a: f32| [175.0 + r * a.cos(), 160.0 + r * a.sin(), z];
            segs.push(Seg { a: p(a), b: p(a + 0.01), color: [255, 120, 30, 2], width: 0.42 });
        }
    }
    stored_starts.push(segs.len() as u32);
    ToolpathData { segs, stored_starts, layer_z }
}


/// What [`bench`] measured.
pub struct Bench {
    pub text: String,
    /// The 1M-triangle mesh, selected (outline passes on), at 2400x1500.
    pub mesh_ms: f64,
    /// 2M toolpath segments at 2400x1500.
    pub toolpath_ms: f64,
    /// A selected 10k-triangle part (an everyday model) at 2400x1500.
    pub part_ms: f64,
}

/// Draws a ~1M-triangle mesh and ~2M toolpath segments the way the viewport does and times each
/// frame (submit and wait, so it overstates a pipelined frame). `quick` takes 3 frames a case
/// instead of 20, for software rendering. Used by `only3d gpu --bench` and a test.
pub fn bench(adapter: &wgpu::Adapter, samples: u32, quick: bool) -> Bench {
        let limits = adapter.limits();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor { required_limits: limits, ..Default::default() })).expect("device");
        let mut scene = SceneGpu::new(&device, wgpu::TextureFormat::Bgra8Unorm, 1, samples);
        let mesh = Arc::new(bench_blob(1_000_000));
        // the size of an everyday part (the example tray is ~10k triangles)
        let part = Arc::new(bench_blob(10_000));
        let tp = Arc::new(bench_toolpaths(2_000_000));
        let size = [2400u32, 1500u32];
        let target = device
            .create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&Default::default());
        let plate = Arc::new(crate::scene::build_plate(&only3d_model::Bed::h2d(), 2.0, &crate::theme::DARK));
        let mut cam = crate::camera::OrbitCamera::default();
        let aspect = size[0] as f32 / size[1] as f32;
        cam.frame(glam::Vec3::new(140.0, 125.0, 0.0), glam::Vec3::new(210.0, 195.0, 60.0), aspect, 1.1);
        let mut run = |toolpath: bool, mesh: &Arc<Mesh>, sel: u32, size: [u32; 2], frames: usize| -> f64 {
            let aspect = size[0] as f32 / size[1] as f32;
            let mut total = 0.0;
            for f in 0..frames + 3 {
                cam.yaw += 0.02;
                let globals = Globals {
                    view_proj: cam.view_proj(aspect).to_cols_array_2d(),
                    eye: cam.eye().extend(1.0).to_array(),
                    viewport: [size[0] as f32, size[1] as f32, cam.focal_px(size[1] as f32), 4.0],
                    key_dir: [0.3, -0.5, 0.8, 0.0],
                    fill_dir: [-0.6, 0.2, 0.3, 0.0],
                    accent: [0.3, 0.5, 0.3, 1.0],
                    outline: [0.3, 0.6, 0.3, 1.0],
                    bg_top: [0.9; 4],
                    bg_bottom: [0.7; 4],
                    flags: [u32::MAX, u32::MAX, 1, 0],
                    dots: [0.5, 0.5, 0.5, 22.0],
                    overhang: [0.9, 0.3, 0.3, 0.0],
                };
                let frame = FrameData {
                    globals,
                    size_px: size,
                    objects: if toolpath { Vec::new() } else { vec![DrawObj { mesh: mesh.clone(), inst: ObjInstance::plain([0.6, 0.5, 0.4, 1.0], sel) }] },
                    keep: vec![mesh.clone()],
                    plate: plate.clone(),
                    toolpath: toolpath.then(|| ToolpathDraw { data: tp.clone(), range: 0..tp.segs.len() as u32 }),
                };
                let t0 = std::time::Instant::now();
                scene.ensure_targets(&device, size);
                scene.sync(&device, &queue, &frame);
                let mut enc = device.create_command_encoder(&Default::default());
                scene.encode(&mut enc, &frame);
                {
                    let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: None,
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &target, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store }, depth_slice: None })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    scene.blit(&mut rp, size);
                }
                queue.submit([enc.finish()]);
                device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
                if f >= 3 {
                    total += t0.elapsed().as_secs_f64() * 1e3;
                }
            }
            total / frames as f64
        };
        let small = [1200u32, 750u32];
        let frames = if quick { 3 } else { 20 };
        let part_ms = run(false, &part, 2, size, frames);
        let part_s = run(false, &part, 2, small, frames);
        let plain = run(false, &mesh, 0, size, frames);
        let plain_s = run(false, &mesh, 0, small, frames);
        let mesh_ms = run(false, &mesh, 2, size, frames);
        let tp_ms = run(true, &mesh, 0, size, frames);
        let tp_s = run(true, &mesh, 0, small, frames);
        let text = format!(
            "[{}, {samples}x MSAA] part 10k, selected: {part_ms:.2} ms (2400x1500) / {part_s:.2} ms (1200x750); mesh 1M: {plain:.2} ms / {plain_s:.2} ms; +outline {mesh_ms:.2} ms; toolpaths 2M: {tp_ms:.2} ms / {tp_s:.2} ms",
            adapter.get_info().name
        );
        Bench { text, part_ms, mesh_ms, toolpath_ms: tp_ms }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The viewport benchmark on this machine's GPU, as a regression guard. Skips without one.
    #[test]
    fn viewport_frame_time() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() })) else {
            eprintln!("no GPU adapter: skipping");
            return;
        };
        if adapter.get_info().device_type == wgpu::DeviceType::Cpu {
            eprintln!("software adapter ({}): skipping the frame-time check", adapter.get_info().name);
            return;
        }
        let b = bench(&adapter, MSAA, false);
        eprintln!("{}", b.text);
        // timings are unpipelined submit-and-wait round trips, so they overstate the real frame
        // cost; this is a regression guard, not a 60 fps gate
        assert!(b.mesh_ms < 33.0 && b.toolpath_ms < 33.0, "viewport rendering regressed badly");
    }
}

