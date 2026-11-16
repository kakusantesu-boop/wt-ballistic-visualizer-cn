//! 3D orbit and aim views. GPUI has no custom shaders, so the scene is drawn off screen with wgpu
//! (shaders in view3d.wgsl), read back and shown as an image; labels and arcs are
//! painted over it with GPUI like the page's 2D overlay canvas.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use gpui::{App, Bounds, Context, Pixels, RenderImage, Window, point, px};
use wgpu::util::DeviceExt;

use crate::app::{COL_A, COL_B, COL_BPT, COL_HIGH, Sw, TARGET_MODEL, Viewer};
use crate::core::{Key, Pt, Terrain};
use crate::paint::{self, color};
use crate::scene;

pub const DETAIL_LABELS: [&str; 4] = ["低（最快）", "中", "高", "极高"];
const LODK: [f64; 4] = [1.5, 3.0, 6.0, 12.0]; // split a node while camera distance < size * k
const P: usize = 64; // quads per patch side
const NG: usize = P + 3; // vertices per side incl. the skirt ring
const ORBIT_FOV: f64 = 45.0;
const SKY: [f32; 3] = [0.62, 0.70, 0.78];
const LIGHT: [f64; 3] = [-0.55, 0.75, 0.37];
const SAMPLES: u32 = 4;
const FMT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm; // what GPUI images are

pub struct Cam {
    pub tx: f64,
    pub ty: f64,
    pub tz: f64,
    pub yaw: f64,
    pub pitch: f64,
    pub dist: f64,
    pub aim_fov: f64,
    /// gunner sight magnification
    pub zoom: f64,
}

pub struct View3D {
    pub cam: Cam,
    gpu: Option<Gpu>,
    err: Option<String>,
    vp: [f64; 16],
    inv: [f64; 16],
    eye: [f64; 3],
    wh: (f32, f32),
    frame: Option<Arc<RenderImage>>,
    key: u64,
    dirty: bool,
    pub n_patches: usize,
}

impl Default for View3D {
    fn default() -> Self {
        Self { cam: Cam { tx: 0.0, ty: 0.0, tz: 0.0, yaw: 0.0, pitch: 0.9, dist: 3000.0, aim_fov: 20.0, zoom: 8.0 }, gpu: None, err: None, vp: [0.0; 16], inv: [0.0; 16], eye: [0.0; 3], wh: (1.0, 1.0), frame: None, key: 0, dirty: true, n_patches: 0 }
    }
}

impl View3D {
    pub fn map_changed(&mut self) {
        self.dirty = true;
    }
    pub fn dirty(&mut self) {
        self.dirty = true;
    }
    pub fn fit(&mut self, t: &Terrain, cx: f64, cz: f64, span: f64) {
        self.cam = Cam { tx: cx, tz: cz, ty: t.height_at(cx, cz), dist: span * 1.1, yaw: 0.0, pitch: 0.9, aim_fov: self.cam.aim_fov, zoom: self.cam.zoom };
        self.dirty = true;
    }
    pub fn center_on(&mut self, t: &Terrain, p: Pt) {
        self.cam.tx = p.x;
        self.cam.tz = p.z;
        self.cam.ty = t.height_at(p.x, p.z);
        self.dirty = true;
    }
    pub fn drag(&mut self, t: &Terrain, dx: f32, dy: f32, pan: bool, h: f32) {
        let (dx, dy) = (dx as f64, dy as f64);
        if !pan {
            self.cam.yaw += dx * 0.005;
            self.cam.pitch = (self.cam.pitch + dy * 0.005).clamp(0.03, 1.55);
        } else {
            let k = 2.0 * self.cam.dist * (ORBIT_FOV.to_radians() / 2.0).tan() / h.max(1.0) as f64;
            let (s, c) = self.cam.yaw.sin_cos();
            self.cam.tx = (self.cam.tx - c * dx * k + s * dy * k).clamp(t.ox, t.x1);
            self.cam.tz = (self.cam.tz + s * dx * k + c * dy * k).clamp(t.oz, t.z1);
            self.cam.ty = t.height_at(self.cam.tx, self.cam.tz);
        }
        self.dirty = true;
    }
    pub fn zoom(&mut self, delta_y: f32, aim: bool, sight: bool) {
        let f = (delta_y as f64 * 0.0015).exp();
        if aim && sight {
            self.cam.zoom = (self.cam.zoom / f).clamp(1.0, 24.0);
        } else if aim {
            self.cam.aim_fov = (self.cam.aim_fov * f).clamp(0.5, 80.0);
        } else {
            self.cam.dist = (self.cam.dist * f).clamp(5.0, 40000.0);
        }
        self.dirty = true;
    }

    /// Screen position (logical px in the view) of a world point, None behind the camera.
    pub fn project(&self, x: f64, y: f64, z: f64) -> Option<(f32, f32)> {
        let c = xform(&self.vp, x, y, z);
        (c[3] > 1e-6).then(|| (((c[0] / c[3] * 0.5 + 0.5) * self.wh.0 as f64) as f32, ((0.5 - c[1] / c[3] * 0.5) * self.wh.1 as f64) as f32))
    }

    /// March the view ray through a screen point against the surface (roofs are hit too, crowns are not).
    pub fn pick(&self, t: &Terrain, px_: f32, py: f32) -> Option<Pt> {
        let nx = px_ as f64 / self.wh.0 as f64 * 2.0 - 1.0;
        let ny = 1.0 - py as f64 / self.wh.1 as f64 * 2.0;
        let un = |z: f64| { let v = xform(&self.inv, nx, ny, z); [v[0] / v[3], v[1] / v[3], v[2] / v[3]] };
        let (a, b) = (un(0.0), un(1.0));
        let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2) + (b[2] - a[2]).powi(2)).sqrt();
        if !(len > 0.0) {
            return None;
        }
        let dir = [(b[0] - a[0]) / len, (b[1] - a[1]) / len, (b[2] - a[2]) / len];
        let (lo, hi) = ([t.ox, t.lo - 1.0, t.oz], [t.x1, t.hi + 1.0, t.z1]);
        let (mut t0, mut t1) = (0.0f64, len);
        for i in 0..3 {
            if dir[i].abs() < 1e-12 {
                if a[i] < lo[i] || a[i] > hi[i] {
                    return None;
                }
                continue;
            }
            let (mut p, mut q) = ((lo[i] - a[i]) / dir[i], (hi[i] - a[i]) / dir[i]);
            if p > q {
                std::mem::swap(&mut p, &mut q);
            }
            t0 = t0.max(p);
            t1 = t1.min(q);
        }
        if t0 > t1 {
            return None;
        }
        let above = |s: f64| a[1] + dir[1] * s - t.height_at(a[0] + dir[0] * s, a[2] + dir[2] * s);
        let step = t.cell / 2.0;
        let (mut prev, mut s) = (t0, t0);
        while s <= t1 + step {
            let tc = s.min(t1);
            if above(tc) <= 0.0 {
                let (mut l, mut h) = (prev, tc);
                for _ in 0..24 {
                    let m = (l + h) / 2.0;
                    if above(m) > 0.0 { l = m } else { h = m }
                }
                return Some(Pt { x: a[0] + dir[0] * h, z: a[2] + dir[2] * h });
            }
            prev = tc;
            s += step;
        }
        None
    }

    fn camera(&mut self, v: &Viewer, w: f32, h: f32) {
        let t = &v.map.as_ref().expect("map").t;
        let (e, tgt, fov, near);
        if let (true, Some(aim), Some(a), Some(b)) = (v.aim_ready(), v.aim.as_ref(), v.a, v.b) {
            e = [a.x, aim.y_a, a.z];
            tgt = [b.x, aim.y_b, b.z];
            fov = if v.view_mode == crate::app::ViewMode::Sight { sight_fov(self.cam.zoom) } else { self.cam.aim_fov };
            near = 0.3;
        } else {
            let c = &self.cam;
            let cp = c.pitch.cos();
            let f = [c.yaw.sin() * cp, -c.pitch.sin(), c.yaw.cos() * cp];
            tgt = [c.tx, c.ty, c.tz];
            e = [tgt[0] - f[0] * c.dist, tgt[1] - f[1] * c.dist, tgt[2] - f[2] * c.dist];
            fov = ORBIT_FOV;
            near = ((e[1] - t.height_at(e[0], e[2])) * 0.3).min(c.dist * 0.05).clamp(0.5, 100.0);
        }
        let mut proj = perspective(fov.to_radians(), w as f64 / h as f64, near, 80000.0);
        proj[0] = -proj[0]; // Dagor is left-handed: mirror X so +X stays screen-right as in the 2D map
        self.vp = mul(&proj, &look_at(e, tgt));
        self.inv = invert(&self.vp);
        self.eye = e;
        self.wh = (w, h);
    }

    /// A segment of the arc is hidden when terrain or roofs are between it and the eye.
    fn hidden_from(&self, tr: &Terrain, p: [f64; 3]) -> bool {
        let e = self.eye;
        (1..40).any(|k| {
            let f = k as f64 / 40.0 * 0.985;
            let (x, y, z) = (e[0] + (p[0] - e[0]) * f, e[1] + (p[1] - e[1]) * f, e[2] + (p[2] - e[2]) * f);
            tr.inside(x, z) && tr.height_at(x, z) > y + 0.3
        })
    }
}

/// Vertical field of view (deg) of the gunner sight at a magnification: 1x = 70°.
pub fn sight_fov(zoom: f64) -> f64 {
    2.0 * ((35f64).to_radians().tan() / zoom).atan().to_degrees()
}

// ------------------------------------------------------------------ 4x4 column-major math

fn perspective(fovy: f64, asp: f64, n: f64, f: f64) -> [f64; 16] {
    let t = 1.0 / (fovy / 2.0).tan(); // depth 0..1 (wgpu)
    [t / asp, 0.0, 0.0, 0.0, 0.0, t, 0.0, 0.0, 0.0, 0.0, f / (n - f), -1.0, 0.0, 0.0, n * f / (n - f), 0.0]
}
fn look_at(e: [f64; 3], t: [f64; 3]) -> [f64; 16] {
    let nrm = |v: [f64; 3]| { let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt(); [v[0] / l, v[1] / l, v[2] / l] };
    let z = nrm([e[0] - t[0], e[1] - t[1], e[2] - t[2]]);
    let x = nrm([z[2], 0.0, -z[0]]); // up = +Y
    let y = [z[1] * x[2] - z[2] * x[1], z[2] * x[0] - z[0] * x[2], z[0] * x[1] - z[1] * x[0]];
    let d = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    [x[0], y[0], z[0], 0.0, x[1], y[1], z[1], 0.0, x[2], y[2], z[2], 0.0, -d(x, e), -d(y, e), -d(z, e), 1.0]
}
fn mul(a: &[f64; 16], b: &[f64; 16]) -> [f64; 16] {
    let mut r = [0.0; 16];
    for c in 0..4 {
        for i in 0..4 {
            r[c * 4 + i] = (0..4).map(|k| a[k * 4 + i] * b[c * 4 + k]).sum();
        }
    }
    r
}
fn invert(m: &[f64; 16]) -> [f64; 16] {
    let mut a: Vec<[f64; 8]> = (0..4).map(|i| { let mut r = [0.0; 8]; r[..4].copy_from_slice(&[m[i], m[4 + i], m[8 + i], m[12 + i]]); r[4 + i] = 1.0; r }).collect();
    for c in 0..4 {
        let p = (c..4).max_by(|&x, &y| a[x][c].abs().total_cmp(&a[y][c].abs())).unwrap();
        a.swap(c, p);
        let d = a[c][c];
        for k in 0..8 {
            a[c][k] /= d;
        }
        for r in 0..4 {
            if r != c {
                let f = a[r][c];
                for k in 0..8 {
                    a[r][k] -= f * a[c][k];
                }
            }
        }
    }
    let mut r = [0.0; 16];
    for i in 0..4 {
        for c in 0..4 {
            r[c * 4 + i] = a[i][4 + c];
        }
    }
    r
}
fn xform(m: &[f64; 16], x: f64, y: f64, z: f64) -> [f64; 4] {
    std::array::from_fn(|i| m[i] * x + m[4 + i] * y + m[8 + i] * z + m[12 + i])
}

// ------------------------------------------------------------------ GPU

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    vp: [f32; 16],
    eye: [f32; 4],
    light: [f32; 4],
    sky: [f32; 4],
    map: [f32; 4],
    uv: [f32; 4],
    zn: [f32; 4],
    ov: [f32; 4],
    flags: [f32; 4],
    tiles: [f32; 32],
}

struct Pipes {
    terrain: wgpu::RenderPipeline,
    build: wgpu::RenderPipeline,
    tree: wgpu::RenderPipeline,
    water: wgpu::RenderPipeline,
    curtain: wgpu::RenderPipeline,
    lines_hidden: wgpu::RenderPipeline,
    lines: wgpu::RenderPipeline,
}

struct Target {
    w: u32,
    h: u32,
    ms: wgpu::TextureView,
    resolve: wgpu::Texture,
    depth: wgpu::TextureView,
    readback: wgpu::Buffer,
    bpr: u32,
}

/// Mesh drawn with the build pipeline: positions, normals (snorm8x4), colours (unorm8x4), optional u32 indices.
struct Mesh {
    pos: wgpu::Buffer,
    nrm: wgpu::Buffer,
    rgba: wgpu::Buffer,
    idx: Option<(wgpu::Buffer, u32)>,
    verts: u32,
}
struct Batch {
    mesh: Mesh,
    inst: wgpu::Buffer,
    count: u32,
}
struct TreeBatch {
    shape: usize,
    inst: wgpu::Buffer,
    count: u32,
}

/// Per-map GPU state; each texture remembers the CPU image it came from (by pointer).
struct SceneRes {
    map_key: usize,
    h: wgpu::Texture,
    c: (usize, wgpu::Texture),
    z: (usize, wgpu::Texture),
    o: (usize, wgpu::Texture),
    det: (usize, wgpu::Texture, wgpu::Texture, [f32; 32]),
    obj: (usize, Vec<Batch>, Vec<TreeBatch>),
    bind: Option<wgpu::BindGroup>,
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    bgl: wgpu::BindGroupLayout,
    pipes: Pipes,
    ubuf: wgpu::Buffer,
    grid: (wgpu::Buffer, wgpu::Buffer, u32),
    nodes: (wgpu::Buffer, u64),
    lines: (wgpu::Buffer, u64),
    curtain: (wgpu::Buffer, u64),
    trees: Vec<(wgpu::Buffer, u32)>,
    tanks: HashMap<String, Mesh>,
    tank_inst: wgpu::Buffer,
    s_lin: wgpu::Sampler,
    s_near: wgpu::Sampler,
    blank: wgpu::Texture,
    blank_arr: wgpu::Texture,
    scene: Option<SceneRes>,
    target: Option<Target>,
}

fn ptr<T>(a: &Arc<T>) -> usize {
    Arc::as_ptr(a) as *const u8 as usize
}

impl Gpu {
    fn new() -> anyhow::Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() }))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("wtv"),
            required_features: wgpu::Features::empty(),
            required_limits: adapter.limits(),
            ..Default::default()
        }))?;
        let tex = |binding, sample_type, dim| wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::VERTEX_FRAGMENT, ty: wgpu::BindingType::Texture { sample_type, view_dimension: dim, multisampled: false }, count: None };
        let f = wgpu::TextureSampleType::Float { filterable: true };
        let d2 = wgpu::TextureViewDimension::D2;
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::VERTEX_FRAGMENT, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
                tex(1, wgpu::TextureSampleType::Float { filterable: false }, d2),
                tex(2, f, d2),
                tex(3, f, d2),
                tex(4, f, d2),
                tex(5, f, d2),
                tex(6, f, wgpu::TextureViewDimension::D2Array),
                wgpu::BindGroupLayoutEntry { binding: 7, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
                wgpu::BindGroupLayoutEntry { binding: 8, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("view3d.wgsl"), source: wgpu::ShaderSource::Wgsl(include_str!("view3d.wgsl").into()) });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });

        use wgpu::VertexFormat as F;
        let attrs = |list: &[(u32, F, u64)]| list.iter().map(|&(l, f, o)| wgpu::VertexAttribute { shader_location: l, format: f, offset: o }).collect::<Vec<_>>();
        let (terrain_g, terrain_n) = (attrs(&[(0, F::Float32x3, 0)]), attrs(&[(1, F::Float32x3, 0)]));
        let line_a = attrs(&[(0, F::Float32x3, 0), (1, F::Float32x4, 12)]);
        let (bp, bn, bc) = (attrs(&[(0, F::Float32x3, 0)]), attrs(&[(1, F::Snorm8x4, 0)]), attrs(&[(6, F::Unorm8x4, 0)]));
        let bi = attrs(&[(2, F::Float32x3, 0), (3, F::Float32x3, 12), (4, F::Float32x3, 24), (5, F::Float32x3, 36), (7, F::Float32x4, 48)]);
        let tv = attrs(&[(0, F::Float32x4, 0), (4, F::Float32x3, 16)]);
        let ti = attrs(&[(1, F::Float32x4, 0), (2, F::Float32x4, 16), (3, F::Float32x4, 32)]);
        let vbl = |stride: u64, step, a: &'static [wgpu::VertexAttribute]| Some(wgpu::VertexBufferLayout { array_stride: stride, step_mode: step, attributes: a });
        let leak = |v: Vec<wgpu::VertexAttribute>| -> &'static [wgpu::VertexAttribute] { Box::leak(v.into_boxed_slice()) }; // built once per process
        let (vx, ix) = (wgpu::VertexStepMode::Vertex, wgpu::VertexStepMode::Instance);
        let blend = Some(wgpu::BlendState::ALPHA_BLENDING);

        let pipe = |vs: &str, fs: &str, buffers: &[Option<wgpu::VertexBufferLayout>], topo, write: bool, cmp, blend: Option<wgpu::BlendState>, alpha: f64| {
            let consts: &[(&str, f64)] = if vs == "vs_lines" { &[("ALPHA", alpha)] } else { &[] };
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(vs),
                layout: Some(&layout),
                vertex: wgpu::VertexState { module: &module, entry_point: Some(vs), compilation_options: wgpu::PipelineCompilationOptions { constants: consts, ..Default::default() }, buffers },
                primitive: wgpu::PrimitiveState { topology: topo, cull_mode: None, ..Default::default() },
                depth_stencil: Some(wgpu::DepthStencilState { format: wgpu::TextureFormat::Depth32Float, depth_write_enabled: Some(write), depth_compare: Some(cmp), stencil: Default::default(), bias: Default::default() }),
                multisample: wgpu::MultisampleState { count: SAMPLES, ..Default::default() },
                fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some(fs), compilation_options: Default::default(), targets: &[Some(wgpu::ColorTargetState { format: FMT, blend, write_mask: wgpu::ColorWrites::ALL })] }),
                multiview_mask: None,
                cache: None,
            })
        };
        use wgpu::{CompareFunction as C, PrimitiveTopology as T};
        let pipes = Pipes {
            terrain: pipe("vs_terrain", "fs_terrain", &[vbl(12, vx, leak(terrain_g)), vbl(12, ix, leak(terrain_n))], T::TriangleList, true, C::LessEqual, None, 1.0),
            build: pipe("vs_build", "fs_build", &[vbl(12, vx, leak(bp)), vbl(4, vx, leak(bn)), vbl(4, vx, leak(bc)), vbl(64, ix, leak(bi))], T::TriangleList, true, C::LessEqual, None, 1.0),
            tree: pipe("vs_tree", "fs_tree", &[vbl(28, vx, leak(tv)), vbl(48, ix, leak(ti))], T::TriangleList, true, C::LessEqual, None, 1.0),
            water: pipe("vs_water", "fs_water", &[], T::TriangleStrip, false, C::LessEqual, blend, 1.0),
            curtain: pipe("vs_lines", "fs_lines", &[vbl(28, vx, leak(line_a.clone()))], T::TriangleList, false, C::LessEqual, blend, 1.0),
            lines_hidden: pipe("vs_lines", "fs_lines", &[vbl(28, vx, leak(line_a.clone()))], T::LineList, false, C::Always, blend, 0.3), // hidden parts faint
            lines: pipe("vs_lines", "fs_lines", &[vbl(28, vx, leak(line_a))], T::LineList, false, C::LessEqual, blend, 1.0),
        };

        // terrain patch grid: u, v (0..P), skirt flag on the outer ring
        let mut g = Vec::with_capacity(NG * NG * 3);
        for v in 0..NG {
            for u in 0..NG {
                g.extend([(u.saturating_sub(1)).min(P) as f32, (v.saturating_sub(1)).min(P) as f32, if u == 0 || v == 0 || u == NG - 1 || v == NG - 1 { 1.0 } else { 0.0 }]);
            }
        }
        let mut idx: Vec<u16> = Vec::with_capacity((NG - 1) * (NG - 1) * 6);
        for v in 0..NG - 1 {
            for u in 0..NG - 1 {
                let a = (v * NG + u) as u16;
                idx.extend([a, a + NG as u16, a + 1, a + 1, a + NG as u16, a + NG as u16 + 1]);
            }
        }
        let init = |c: &[u8], usage| device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: c, usage });
        let grid = (init(bytemuck::cast_slice(&g), wgpu::BufferUsages::VERTEX), init(bytemuck::cast_slice(&idx), wgpu::BufferUsages::INDEX), idx.len() as u32);
        let dynbuf = |size: u64| device.create_buffer(&wgpu::BufferDescriptor { label: None, size, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let trees = tree_proxies().into_iter().map(|v| (init(bytemuck::cast_slice(&v), wgpu::BufferUsages::VERTEX), (v.len() / 7) as u32)).collect();
        let s_lin = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::ClampToEdge, address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, mipmap_filter: wgpu::MipmapFilterMode::Linear, anisotropy_clamp: 8, ..Default::default()
        });
        let s_near = device.create_sampler(&wgpu::SamplerDescriptor { address_mode_u: wgpu::AddressMode::ClampToEdge, address_mode_v: wgpu::AddressMode::ClampToEdge, ..Default::default() });
        let ubuf = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: std::mem::size_of::<Uniforms>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let blank = texture(&device, &queue, 1, 1, 1, wgpu::TextureFormat::Rgba8Unorm, &[vec![0u8; 4]], 4);
        let blank_arr = texture_array(&device, &queue, 1, &[vec![vec![128u8; 4]]]);
        let tank_inst = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 128, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        Ok(Self { nodes: (dynbuf(12 * 4096), 12 * 4096), lines: (dynbuf(28 * 4096), 28 * 4096), curtain: (dynbuf(28 * 4096), 28 * 4096), device, queue, bgl, pipes, ubuf, grid, trees, tanks: HashMap::new(), tank_inst, s_lin, s_near, blank, blank_arr, scene: None, target: None })
    }

    /// A vertex buffer of at least `size` bytes (grown, never shrunk) filled with `data`.
    fn stream(device: &wgpu::Device, queue: &wgpu::Queue, slot: &mut (wgpu::Buffer, u64), data: &[u8]) {
        if data.len() as u64 > slot.1 {
            let size = (data.len() as u64).next_power_of_two();
            *slot = (device.create_buffer(&wgpu::BufferDescriptor { label: None, size, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false }), size);
        }
        if !data.is_empty() {
            queue.write_buffer(&slot.0, 0, data);
        }
    }

    fn target(&mut self, w: u32, h: u32) -> &Target {
        if self.target.as_ref().is_none_or(|t| t.w != w || t.h != h) {
            let d = &self.device;
            let mk = |format, samples, usage| d.create_texture(&wgpu::TextureDescriptor { label: None, size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: samples, dimension: wgpu::TextureDimension::D2, format, usage, view_formats: &[] });
            let ms = mk(FMT, SAMPLES, wgpu::TextureUsages::RENDER_ATTACHMENT).create_view(&Default::default());
            let resolve = mk(FMT, 1, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC);
            let depth = mk(wgpu::TextureFormat::Depth32Float, SAMPLES, wgpu::TextureUsages::RENDER_ATTACHMENT).create_view(&Default::default());
            let bpr = (w * 4).div_ceil(256) * 256;
            let readback = d.create_buffer(&wgpu::BufferDescriptor { label: None, size: (bpr * h) as u64, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
            self.target = Some(Target { w, h, ms, resolve, depth, readback, bpr });
        }
        self.target.as_ref().unwrap()
    }

    /// Bring the per-map GPU state in line with the viewer's current images and objects.
    fn sync(&mut self, v: &Viewer) {
        let map = v.map.as_ref().expect("map");
        let t = &map.t;
        if self.scene.as_ref().is_none_or(|s| s.map_key != map.h16.as_ptr() as usize) {
            let h = texture(&self.device, &self.queue, t.w as u32, t.hh as u32, 1, wgpu::TextureFormat::R32Float, &[bytemuck::cast_slice(&t.h).to_vec()], 4);
            let b = || self.blank.clone();
            self.scene = Some(SceneRes { map_key: map.h16.as_ptr() as usize, h, c: (0, b()), z: (0, b()), o: (0, b()), det: (0, b(), self.blank_arr.clone(), [4.0; 32]), obj: (0, vec![], vec![]), bind: None });
        }
        let (device, queue) = (&self.device, &self.queue);
        let s = self.scene.as_mut().unwrap();
        let mut changed = s.bind.is_none();
        if let Some(l) = &v.map_img {
            if s.c.0 != ptr(&l.rgba) {
                s.c = (ptr(&l.rgba), rgba_texture(device, queue, &l.rgba, true));
                changed = true;
            }
        }
        if let Some((_, l, _)) = &v.zones_img {
            if s.z.0 != ptr(&l.rgba) {
                s.z = (ptr(&l.rgba), rgba_texture(device, queue, &l.rgba, false));
                changed = true;
            }
        }
        let ok = v.overlay.as_ref().map_or(1, |(_, l)| ptr(&l.rgba));
        if s.o.0 != ok {
            s.o = (ok, v.overlay.as_ref().map_or(self.blank.clone(), |(_, l)| rgba_texture(device, queue, &l.rgba, false)));
            changed = true;
        }
        let dk = map.ground.as_ref().map_or(1, ptr);
        if s.det.0 != dk {
            s.det = match map.ground.as_ref().and_then(|g| g.detail.as_ref()) {
                Some(d) => {
                    let n = d.tiles.len().max(1);
                    let side = d.strip.height();
                    let layers: Vec<Vec<Vec<u8>>> = (0..n as u32).map(|l| mips(&image::imageops::crop_imm(&d.strip, l * side, 0, side, side).to_image())).collect();
                    let mut tiles = [4.0f32; 32];
                    for (i, t) in d.tiles.iter().take(32).enumerate() {
                        tiles[i] = *t;
                    }
                    (dk, rgba_texture(device, queue, &d.idx, false), texture_array(device, queue, side, &layers), tiles)
                }
                None => (dk, self.blank.clone(), self.blank_arr.clone(), [4.0; 32]),
            };
            changed = true;
        }
        let objk = map.obj.as_ref().map_or(1, ptr);
        if s.obj.0 != objk {
            let (mut batches, mut trees) = (vec![], vec![]);
            if let Some(o) = &map.obj {
                let mut per: Vec<Vec<usize>> = vec![vec![]; o.models.len()];
                for (i, (m, _)) in o.inst.iter().enumerate() {
                    if let Some(p) = per.get_mut(*m as usize) {
                        p.push(i);
                    }
                }
                for (mi, list) in per.iter().enumerate() {
                    let m = &o.models[mi];
                    if list.is_empty() || m.idx.is_empty() {
                        continue;
                    }
                    let inst: Vec<f32> = list.iter().flat_map(|&i| o.inst[i].1.iter().copied().chain([1.0, 1.0, 1.0, 1.0])).collect();
                    let gpu = Gpu::mesh_on(device, &m.pos, &m.nrm, &m.rgba, Some(&m.idx));
                    batches.push(Batch { mesh: gpu, inst: device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: bytemuck::cast_slice(&inst), usage: wgpu::BufferUsages::VERTEX }), count: list.len() as u32 });
                }
                // trees: one batch per crown shape (rgba[3] of the tree model; older data has 255 there: by height)
                let shape_of = |tm: &crate::data::TreeModel| if tm.rgba[3] <= 3 { tm.rgba[3] as usize } else if tm.height < 4.0 { 3 } else { 0 };
                let mut by: [Vec<f32>; 4] = Default::default();
                for (mi, f) in &o.trees {
                    let Some(tm) = o.tree_models.get(*mi as usize) else { continue };
                    by[shape_of(tm)].extend(f.iter().copied().chain([tm.height, tm.radius, tm.crown, 0.0, tm.rgba[0] as f32 / 255.0, tm.rgba[1] as f32 / 255.0, tm.rgba[2] as f32 / 255.0, 1.0]));
                }
                for (shape, list) in by.iter().enumerate() {
                    if !list.is_empty() {
                        trees.push(TreeBatch { shape, inst: device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: bytemuck::cast_slice(list), usage: wgpu::BufferUsages::VERTEX }), count: (list.len() / 12) as u32 });
                    }
                }
            }
            s.obj = (objk, batches, trees);
        }
        if changed {
            let view = |t: &wgpu::Texture| t.create_view(&Default::default());
            let arr = s.det.2.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2Array), ..Default::default() });
            let (hv, cv, ov, zv, iv) = (view(&s.h), view(&s.c.1), view(&s.o.1), view(&s.z.1), view(&s.det.1));
            s.bind = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.ubuf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&hv) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&cv) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&ov) },
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&zv) },
                    wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&iv) },
                    wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::TextureView(&arr) },
                    wgpu::BindGroupEntry { binding: 7, resource: wgpu::BindingResource::Sampler(&self.s_lin) },
                    wgpu::BindGroupEntry { binding: 8, resource: wgpu::BindingResource::Sampler(&self.s_near) },
                ],
            }));
        }
        // tank models, kept across maps
        for (model, mesh) in &v.tanks {
            if let (Some(m), false) = (mesh, self.tanks.contains_key(model)) {
                let nrm: Vec<i8> = m.nrm.chunks_exact(3).flat_map(|n| [(n[0] * 127.0) as i8, (n[1] * 127.0) as i8, (n[2] * 127.0) as i8, 0]).collect();
                let rgba = vec![255u8; m.pos.len() / 3 * 4];
                let gpu = Gpu::mesh_on(&self.device, &m.pos, &nrm, &rgba, None);
                self.tanks.insert(model.clone(), gpu);
            }
        }
    }

    fn mesh_on(device: &wgpu::Device, pos: &[f32], nrm: &[i8], rgba: &[u8], idx: Option<&[u32]>) -> Mesh {
        let init = |c: &[u8], usage| device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: c, usage });
        Mesh {
            pos: init(bytemuck::cast_slice(pos), wgpu::BufferUsages::VERTEX),
            nrm: init(bytemuck::cast_slice(nrm), wgpu::BufferUsages::VERTEX),
            rgba: init(rgba, wgpu::BufferUsages::VERTEX),
            idx: idx.map(|i| (init(bytemuck::cast_slice(i), wgpu::BufferUsages::INDEX), i.len() as u32)),
            verts: (pos.len() / 3) as u32,
        }
    }
}

/// Box-filtered mip chain of an RGBA image (wgpu does not generate mipmaps).
fn mips(img: &image::RgbaImage) -> Vec<Vec<u8>> {
    let mut out = vec![img.as_raw().clone()];
    let (mut w, mut h) = (img.width(), img.height());
    while w > 1 || h > 1 {
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let src = out.last().unwrap();
        let mut dst = vec![0u8; (nw * nh * 4) as usize];
        for y in 0..nh {
            for x in 0..nw {
                for c in 0..4 {
                    let s = |xx: u32, yy: u32| src[((yy.min(h - 1) * w + xx.min(w - 1)) * 4 + c) as usize] as u32;
                    dst[((y * nw + x) * 4 + c) as usize] = ((s(2 * x, 2 * y) + s(2 * x + 1, 2 * y) + s(2 * x, 2 * y + 1) + s(2 * x + 1, 2 * y + 1)) / 4) as u8;
                }
            }
        }
        out.push(dst);
        (w, h) = (nw, nh);
    }
    out
}

fn texture(device: &wgpu::Device, queue: &wgpu::Queue, w: u32, h: u32, layers: u32, format: wgpu::TextureFormat, levels: &[Vec<u8>], bpp: u32) -> wgpu::Texture {
    let t = device.create_texture(&wgpu::TextureDescriptor { label: None, size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: layers }, mip_level_count: levels.len() as u32, sample_count: 1, dimension: wgpu::TextureDimension::D2, format, usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST, view_formats: &[] });
    for (l, data) in levels.iter().enumerate() {
        let (lw, lh) = ((w >> l).max(1), (h >> l).max(1));
        queue.write_texture(wgpu::TexelCopyTextureInfo { texture: &t, mip_level: l as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All }, data, wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(lw * bpp), rows_per_image: Some(lh) }, wgpu::Extent3d { width: lw, height: lh, depth_or_array_layers: layers });
    }
    t
}

/// +Z-up image (row 0 = top) flipped so texture v grows with z; with_mips for the colour map.
fn rgba_texture(device: &wgpu::Device, queue: &wgpu::Queue, img: &image::RgbaImage, with_mips: bool) -> wgpu::Texture {
    let flipped = image::imageops::flip_vertical(img);
    let levels = if with_mips { mips(&flipped) } else { vec![flipped.into_raw()] };
    texture(device, queue, img.width(), img.height(), 1, wgpu::TextureFormat::Rgba8Unorm, &levels, 4)
}

/// layers[layer][mip] square RGBA images of side `side`.
fn texture_array(device: &wgpu::Device, queue: &wgpu::Queue, side: u32, layers: &[Vec<Vec<u8>>]) -> wgpu::Texture {
    let n = layers.len() as u32;
    let levels = layers[0].len() as u32;
    let t = device.create_texture(&wgpu::TextureDescriptor { label: None, size: wgpu::Extent3d { width: side, height: side, depth_or_array_layers: n }, mip_level_count: levels, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: wgpu::TextureFormat::Rgba8Unorm, usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST, view_formats: &[] });
    for (li, layer) in layers.iter().enumerate() {
        for (m, data) in layer.iter().enumerate() {
            let s = (side >> m).max(1);
            queue.write_texture(wgpu::TexelCopyTextureInfo { texture: &t, mip_level: m as u32, origin: wgpu::Origin3d { x: 0, y: 0, z: li as u32 }, aspect: wgpu::TextureAspect::All }, data, wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(s * 4), rows_per_image: Some(s) }, wgpu::Extent3d { width: s, height: s, depth_or_array_layers: 1 });
        }
    }
    t
}

/// Crown shapes (0 broadleaf, 1 conifer, 2 palm, 3 bush); per vertex x, y, z, trunk flag, normal x, y, z.
fn tree_proxies() -> [Vec<f32>; 4] {
    let lump = |i: f64, j: f64, s: f64| { let v = (i * 12.9898 + j * 78.233 + s * 37.719).sin() * 43758.5453; v - v.floor() };
    let tri = |out: &mut Vec<f32>, a: [f64; 7], b: [f64; 7], c: [f64; 7]| out.extend([a, b, c].iter().flatten().map(|&v| v as f32));
    let lobe = |out: &mut Vec<f32>, cx: f64, cy: f64, cz: f64, rx: f64, ry: f64, seed: f64, segs: usize, rings: usize| {
        let pt = |i: usize, j: usize| {
            let ph = std::f64::consts::PI * i as f64 / rings as f64;
            let th = 2.0 * std::f64::consts::PI * (j % segs) as f64 / segs as f64;
            let k = 1.0 + 0.18 * (lump(i as f64, (j % segs) as f64, seed) - 0.5) * if i > 0 && i < rings { 1.0 } else { 0.0 };
            let (nx, ny, nz) = (ph.sin() * th.cos(), ph.cos(), ph.sin() * th.sin());
            [cx + nx * rx * k, cy + ny * ry * k, cz + nz * rx * k, 0.0, nx, ny, nz]
        };
        for i in 0..rings {
            for j in 0..segs {
                let (a, b, c, d) = (pt(i, j), pt(i, j + 1), pt(i + 1, j), pt(i + 1, j + 1));
                tri(out, a, c, b);
                tri(out, b, c, d);
            }
        }
    };
    let trunk = |out: &mut Vec<f32>| {
        for j in 0..6 {
            let (t0, t1) = (2.0 * std::f64::consts::PI * j as f64 / 6.0, 2.0 * std::f64::consts::PI * (j + 1) as f64 / 6.0);
            let (c0, s0, c1, s1) = (t0.cos(), t0.sin(), t1.cos(), t1.sin());
            let (a, b, c, d) = ([c0, 0.0, s0, 1.0, c0, 0.0, s0], [c1, 0.0, s1, 1.0, c1, 0.0, s1], [c0, 1.0, s0, 1.0, c0, 0.0, s0], [c1, 1.0, s1, 1.0, c1, 0.0, s1]);
            tri(out, a, c, b);
            tri(out, b, c, d);
        }
    };
    let cone = |out: &mut Vec<f32>, y0: f64, y1: f64, r: f64, segs: usize| {
        for j in 0..segs {
            let (t0, t1) = (2.0 * std::f64::consts::PI * j as f64 / segs as f64, 2.0 * std::f64::consts::PI * (j + 1) as f64 / segs as f64);
            let tm = (t0 + t1) / 2.0;
            let n = |t: f64| [t.cos(), 0.55, t.sin()];
            let (n0, n1, nm) = (n(t0), n(t1), n(tm));
            tri(out, [r * t0.cos(), y0, r * t0.sin(), 0.0, n0[0], n0[1], n0[2]], [0.0, y1, 0.0, 0.0, nm[0], nm[1], nm[2]], [r * t1.cos(), y0, r * t1.sin(), 0.0, n1[0], n1[1], n1[2]]);
            tri(out, [r * t0.cos(), y0, r * t0.sin(), 0.0, 0.0, -1.0, 0.0], [r * t1.cos(), y0, r * t1.sin(), 0.0, 0.0, -1.0, 0.0], [0.0, y0, 0.0, 0.0, 0.0, -1.0, 0.0]); // underside
        }
    };
    let (mut broad, mut conifer, mut palm, mut bush) = (vec![], vec![], vec![], vec![]);
    trunk(&mut broad);
    lobe(&mut broad, 0.0, 0.5, 0.0, 0.85, 0.48, 1.0, 8, 5);
    lobe(&mut broad, 0.36, 0.45, 0.22, 0.58, 0.36, 2.0, 8, 5);
    lobe(&mut broad, -0.3, 0.6, -0.25, 0.6, 0.38, 3.0, 8, 5);
    trunk(&mut conifer);
    for k in 0..4 {
        cone(&mut conifer, k as f64 * 0.2, k as f64 * 0.2 + 0.4, 1.0 - k as f64 * 0.22, 10);
    }
    trunk(&mut palm);
    lobe(&mut palm, 0.0, 0.95, 0.0, 0.18, 0.12, 5.0, 6, 4);
    for f in 0..9 {
        // fronds: tapering strips drooping from the top
        let t = 2.0 * std::f64::consts::PI * f as f64 / 9.0 + 0.3 * lump(f as f64, 0.0, 6.0);
        let (dx, dz) = (t.cos(), t.sin());
        let (px_, pz) = (-dz, dx);
        let at = |s: f64| { let w = 0.16 * (1.0 - s); let y = 1.0 - 0.55 * s * s; ([dx * s - px_ * w, y, dz * s - pz * w, 0.0, 0.0, 1.0, 0.0], [dx * s + px_ * w, y, dz * s + pz * w, 0.0, 0.0, 1.0, 0.0]) };
        for k in 0..4 {
            let ((a, b), (c, d)) = (at(k as f64 / 4.0), at((k + 1) as f64 / 4.0));
            tri(&mut palm, a, c, b);
            tri(&mut palm, b, c, d);
        }
    }
    lobe(&mut bush, 0.0, 0.45, 0.0, 0.9, 0.5, 7.0, 8, 5);
    lobe(&mut bush, 0.38, 0.35, 0.25, 0.58, 0.4, 8.0, 8, 5);
    [broad, conifer, palm, bush]
}

// ------------------------------------------------------------------ frame

fn rgb(hex: u32) -> [f32; 3] {
    [((hex >> 16) & 255) as f32 / 255.0, ((hex >> 8) & 255) as f32 / 255.0, (hex & 255) as f32 / 255.0]
}

/// Lines (areas draped on the terrain, A/B/highest poles, sight line, arcs) and the arcs' curtains.
fn build_lines(v: &Viewer) -> (Vec<f32>, Vec<f32>) {
    let map = v.map.as_ref().expect("map");
    let t = &map.t;
    let mut l: Vec<f32> = vec![];
    let seg = |l: &mut Vec<f32>, a: [f64; 3], b: [f64; 3], c: [f32; 3]| {
        l.extend(a.iter().map(|&x| x as f32).chain(c).chain([1.0]));
        l.extend(b.iter().map(|&x| x as f32).chain(c).chain([1.0]));
    };
    for a in v.areas() {
        let shown = match a.kind.as_str() { "battle" => v.sw.on(Sw::Border), "kill" => v.sw.on(Sw::Red), _ => v.sw.on(Sw::Caps) };
        if !shown {
            continue;
        }
        let ring: Vec<[f64; 2]> = match scene::area_shape(&a) {
            scene::Shape::Poly(p) => p.iter().chain(p.first()).copied().collect(),
            scene::Shape::Circle { c, r } => { let r = r.max(3.0); (0..=64).map(|i| { let f = i as f64 * std::f64::consts::PI / 32.0; [c[0] + r * f.sin(), c[1] + r * f.cos()] }).collect() }
        };
        let col = rgb(scene::area_color(&a));
        let mut pts: Vec<Option<[f64; 3]>> = vec![];
        for i in 1..ring.len() {
            let ([x0, z0], [x1, z1]) = (ring[i - 1], ring[i]);
            let n = ((x1 - x0).hypot(z1 - z0) / (2.0 * t.cell)).ceil().max(1.0) as usize;
            for k in if i > 1 { 1 } else { 0 }..=n {
                let (x, z) = (x0 + (x1 - x0) * k as f64 / n as f64, z0 + (z1 - z0) * k as f64 / n as f64);
                pts.push(t.inside(x, z).then(|| [x, t.height_at(x, z) + 0.5, z])); // outlines stop at the terrain's edge
            }
        }
        for w in pts.windows(2) {
            if let [Some(a), Some(b)] = w {
                seg(&mut l, *a, *b, col);
            }
        }
    }
    let high = v.high_shown().map(|(x, z, _)| Pt { x, z });
    for (pt, col) in [(v.a, COL_A), (v.b, COL_BPT), (high, COL_HIGH)] {
        if let Some(p) = pt {
            let g = if col == COL_HIGH { t.height_at(p.x, p.z) } else { map.stand(p.x, p.z) }; // the highest point is bare ground
            seg(&mut l, [p.x, g, p.z], [p.x, g + 25.0, p.z], rgb(col));
        }
    }
    let mut c: Vec<f32> = vec![];
    if let (Some(aim), Some(a), Some(b)) = (&v.aim, v.a, v.b) {
        seg(&mut l, [a.x, aim.y_a, a.z], [b.x, aim.y_b, b.z], rgb(if aim.los.is_none() { 0x58c472 } else { 0xe2574c }));
        for (i, q) in aim.sols.iter().enumerate() {
            let col = rgb(if i > 0 { COL_B } else { COL_A });
            let mut pts: Vec<[f64; 3]> = (0..q.tr.s.len()).step_by(2).take_while(|&k| q.tr.s[k] <= aim.d).map(|k| [a.x + aim.dir.0 * q.tr.s[k], q.tr.y[k], a.z + aim.dir.1 * q.tr.s[k]]).collect();
            pts.push([b.x, q.tr.at(Key::Y, aim.d), b.z]);
            for w in pts.windows(2) {
                seg(&mut l, w[0], w[1], col);
            }
            if v.sw.on(Sw::Arc) {
                // curtain: a translucent wall from the arc down to the ground below it, up to B or where it is blocked
                let end = q.blocked.unwrap_or(aim.d);
                let n = ((end / t.cell.max(2.0)).ceil() as usize).clamp(2, 600);
                let col_at = |k: usize| {
                    let s = end * k as f64 / n as f64;
                    let (x, z) = (a.x + aim.dir.0 * s, a.z + aim.dir.1 * s);
                    let y = q.tr.at(Key::Y, s);
                    ([x, y, z], [x, y.min(map.tr.height_at(x, z)), z])
                };
                let vtx = |c: &mut Vec<f32>, p: [f64; 3], al: f32| c.extend(p.iter().map(|&x| x as f32).chain(col).chain([al]));
                let (mut t0, mut b0) = col_at(0);
                for k in 1..=n {
                    let (t1, b1) = col_at(k);
                    for (p, al) in [(t0, 0.34), (b0, 0.04), (t1, 0.34), (t1, 0.34), (b0, 0.04), (b1, 0.04)] {
                        vtx(&mut c, p, al);
                    }
                    (t0, b0) = (t1, b1);
                }
            }
        }
    }
    (l, c)
}

/// Quadtree over the heightmap, frustum culled against conservative height bounds.
fn collect(t: &Terrain, eye: [f64; 3], pl: &[[f64; 4]; 6], x0: usize, z0: usize, size: usize, out: &mut Vec<f32>, k: f64) {
    let c = t.cell;
    if x0 >= t.w - 1 || z0 >= t.hh - 1 {
        return;
    }
    let (ax, az) = (t.ox + x0 as f64 * c, t.oz + z0 as f64 * c);
    let (bx, bz) = (ax + size as f64 * c, az + size as f64 * c);
    let (ay, by) = (t.lo - size as f64 / P as f64 * c * 4.0 - 2.0, t.hi);
    for p in pl {
        if p[0] * if p[0] > 0.0 { bx } else { ax } + p[1] * if p[1] > 0.0 { by } else { ay } + p[2] * if p[2] > 0.0 { bz } else { az } + p[3] < 0.0 {
            return;
        }
    }
    if size > P {
        let (dx, dy, dz) = ((ax - eye[0]).max(0.0).max(eye[0] - bx), (ay - eye[1]).max(0.0).max(eye[1] - by), (az - eye[2]).max(0.0).max(eye[2] - bz));
        if (dx * dx + dy * dy + dz * dz).sqrt() < size as f64 * c * k {
            let h = size / 2;
            for (ox, oz) in [(0, 0), (h, 0), (0, h), (h, h)] {
                collect(t, eye, pl, x0 + ox, z0 + oz, h, out, k);
            }
            return;
        }
    }
    out.extend([x0 as f32, z0 as f32, (size / P) as f32]);
}
fn frustum(m: &[f64; 16]) -> [[f64; 4]; 6] {
    let row = |i: usize| [m[i], m[4 + i], m[8 + i], m[12 + i]];
    let w = row(3);
    let (r0, r1, r2) = (row(0), row(1), row(2));
    let add = |a: [f64; 4], b: [f64; 4], s: f64| std::array::from_fn(|j| a[j] + s * b[j]);
    // wgpu depth 0..1: near plane is row 2 alone
    [add(w, r0, 1.0), add(w, r0, -1.0), add(w, r1, 1.0), add(w, r1, -1.0), r2, add(w, r2, -1.0)]
}

/// Render the 3D frame when anything it shows changed (called from Viewer::render, which has the window).
pub fn prepare(v: &mut Viewer, window: &mut Window, cx: &mut Context<Viewer>) {
    if v.map.is_none() {
        return;
    }
    v.zone_layer(); // the zone texture is shared with 2D and built on demand
    let scale = window.scale_factor();
    let (w, h) = (f32::from(v.map_bounds().size.width).max(16.0), f32::from(v.map_bounds().size.height).max(16.0));
    let mut st = std::collections::hash_map::DefaultHasher::new();
    let c = &v.v3.cam;
    for f in [c.tx, c.ty, c.tz, c.yaw, c.pitch, c.dist, c.aim_fov, c.zoom, w as f64, h as f64, scale as f64] {
        f.to_bits().hash(&mut st);
    }
    (v.aim_ready(), v.view_mode as u8, v.detail, v.sw_bits(), v.mission, v.mode as u8, v.tanks.values().filter(|m| m.is_some()).count(), v.tank).hash(&mut st);
    (v.map.as_ref().map(ptr), v.map_img.as_ref().map(|l| ptr(&l.rgba)), v.zones_img.as_ref().map(|z| ptr(&z.1.rgba)), v.overlay.as_ref().map(|o| ptr(&o.1.rgba))).hash(&mut st);
    let key = st.finish();
    if key == v.v3.key && !v.v3.dirty && v.v3.frame.is_some() {
        return;
    }
    let mut v3 = std::mem::take(&mut v.v3);
    v3.key = key;
    v3.dirty = false;
    match v3.render(v, w, h, scale) {
        Ok(img) => {
            if let Some(old) = v3.frame.replace(img) {
                window.drop_image(old).ok();
            }
        }
        Err(e) => v3.err = Some(format!("3D 视图不可用: {e:#}")),
    }
    v.v3 = v3;
    let _ = cx;
}

impl View3D {
    fn render(&mut self, v: &Viewer, w: f32, h: f32, scale: f32) -> anyhow::Result<Arc<RenderImage>> {
        if self.gpu.is_none() {
            self.gpu = Some(Gpu::new()?);
        }
        self.camera(v, w, h);
        let map = v.map.as_ref().expect("map");
        let t = &map.t;
        let mut nodes = vec![];
        let root = P.max((t.w.max(t.hh) as f64 - 1.0).log2().ceil().exp2() as usize);
        collect(t, self.eye, &frustum(&self.vp), 0, 0, root, &mut nodes, LODK[v.detail.min(3)]);
        self.n_patches = nodes.len() / 3;
        let (lines, curtain) = build_lines(v);

        let gpu = self.gpu.as_mut().unwrap();
        gpu.sync(v);
        let (pw, ph) = ((w * scale).round() as u32, (h * scale).round() as u32);
        gpu.target(pw, ph);
        Gpu::stream(&gpu.device, &gpu.queue, &mut gpu.nodes, bytemuck::cast_slice(&nodes));
        Gpu::stream(&gpu.device, &gpu.queue, &mut gpu.lines, bytemuck::cast_slice(&lines));
        Gpu::stream(&gpu.device, &gpu.queue, &mut gpu.curtain, bytemuck::cast_slice(&curtain));
        let s = gpu.scene.as_ref().unwrap();
        let ln = (LIGHT[0] * LIGHT[0] + LIGHT[1] * LIGHT[1] + LIGHT[2] * LIGHT[2]).sqrt();
        let zsize = v.zones_img.as_ref().map_or(1.0, |z| z.2);
        let ov = v.overlay.as_ref().map_or([0.0; 4], |(o, _)| [o.x0 as f32, o.z0 as f32, (1.0 / o.size) as f32, 1.0]);
        let u = Uniforms {
            vp: self.vp.map(|x| x as f32),
            eye: [self.eye[0] as f32, self.eye[1] as f32, self.eye[2] as f32, 0.0],
            light: [(LIGHT[0] / ln) as f32, (LIGHT[1] / ln) as f32, (LIGHT[2] / ln) as f32, 0.0],
            sky: [SKY[0], SKY[1], SKY[2], 1.0],
            map: [t.ox as f32, t.oz as f32, t.cell as f32, map.meta.water.unwrap_or(0.0) as f32],
            uv: [(t.ox - t.cell / 2.0) as f32, (t.oz - t.cell / 2.0) as f32, (1.0 / (t.w as f64 * t.cell)) as f32, (1.0 / (t.hh as f64 * t.cell)) as f32],
            zn: [(t.ox - t.cell / 2.0) as f32, (t.z1 + t.cell / 2.0 - zsize) as f32, (1.0 / zsize) as f32, 0.0],
            ov,
            flags: [f32::from(u8::from(v.sw.on(Sw::Grid))), f32::from(u8::from(v.sw.on(Sw::GroundDetail) && s.det.0 != 1)), 0.0, 0.0],
            tiles: s.det.3,
        };
        gpu.queue.write_buffer(&gpu.ubuf, 0, bytemuck::bytes_of(&u));
        // tanks: A = the picked tank (not in the aim view: the camera sits in it) along its driven heading, else
        // facing B; B = the target facing A. Both pitch and roll with the surface under them.
        let mut tank_draws: Vec<(&Mesh, u64)> = vec![];
        if v.sw.on(Sw::Tanks) {
            let model_a = v.index.tanks.get(v.tank).and_then(|t| t.model.clone()).filter(|_| !v.aim_ready());
            let mut inst: Vec<f32> = vec![];
            for (model, p, q, heading, col) in [(model_a, v.a, v.b, v.heading, [0.78, 0.6, 0.3]), (v.b.map(|_| TARGET_MODEL.to_string()), v.b, v.a, None, [0.36, 0.58, 0.8])] {
                let (Some(model), Some(p)) = (model, p) else { continue };
                let Some(mesh) = gpu.tanks.get(&model) else { continue };
                let dir = match heading {
                    Some(h) => (h.sin(), h.cos()),
                    None => {
                        let (dx, dz) = q.map_or((0.0, 1.0), |q| (q.x - p.x, q.z - p.z));
                        let l = dx.hypot(dz).max(1e-9);
                        (dx / l, dz / l)
                    }
                };
                let (f, u, s, base) = scene::hull_pose(map, p, dir);
                let off = inst.len() as u64 * 4;
                inst.extend(f.iter().chain(&u).chain(&s).map(|&c| c as f32));
                inst.extend([p.x as f32, base as f32, p.z as f32, col[0], col[1], col[2], 1.0]);
                tank_draws.push((mesh, off));
            }
            if !inst.is_empty() {
                gpu.queue.write_buffer(&gpu.tank_inst, 0, bytemuck::cast_slice(&inst));
            }
        }
        let target = gpu.target.as_ref().unwrap();
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.ms,
                    depth_slice: None,
                    resolve_target: Some(&target.resolve.create_view(&Default::default())),
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: SKY[0] as f64, g: SKY[1] as f64, b: SKY[2] as f64, a: 1.0 }), store: wgpu::StoreOp::Discard },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: &target.depth, depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }), stencil_ops: None }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            rp.set_bind_group(0, s.bind.as_ref().unwrap(), &[]);
            let p = &gpu.pipes;
            rp.set_pipeline(&p.terrain);
            rp.set_vertex_buffer(0, gpu.grid.0.slice(..));
            rp.set_vertex_buffer(1, gpu.nodes.0.slice(..));
            rp.set_index_buffer(gpu.grid.1.slice(..), wgpu::IndexFormat::Uint16);
            rp.draw_indexed(0..gpu.grid.2, 0, 0..self.n_patches as u32);
            let draw_mesh = |rp: &mut wgpu::RenderPass, m: &Mesh, inst: wgpu::BufferSlice, count: u32| {
                rp.set_vertex_buffer(0, m.pos.slice(..));
                rp.set_vertex_buffer(1, m.nrm.slice(..));
                rp.set_vertex_buffer(2, m.rgba.slice(..));
                rp.set_vertex_buffer(3, inst);
                match &m.idx {
                    Some((ib, n)) => {
                        rp.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                        rp.draw_indexed(0..*n, 0, 0..count);
                    }
                    None => rp.draw(0..m.verts, 0..count),
                }
            };
            rp.set_pipeline(&p.build);
            if v.sw.on(Sw::Build) {
                for b in &s.obj.1 {
                    draw_mesh(&mut rp, &b.mesh, b.inst.slice(..), b.count);
                }
            }
            for (m, off) in &tank_draws {
                draw_mesh(&mut rp, m, gpu.tank_inst.slice(*off..*off + 64), 1);
            }
            if v.sw.on(Sw::Trees) && !s.obj.2.is_empty() {
                rp.set_pipeline(&p.tree);
                for tb in &s.obj.2 {
                    let (vb, n) = &gpu.trees[tb.shape];
                    rp.set_vertex_buffer(0, vb.slice(..));
                    rp.set_vertex_buffer(1, tb.inst.slice(..));
                    rp.draw(0..*n, 0..tb.count);
                }
            }
            if map.meta.water.is_some() && v.sw.on(Sw::Water) {
                rp.set_pipeline(&p.water);
                rp.draw(0..4, 0..1);
            }
            if !curtain.is_empty() {
                rp.set_pipeline(&p.curtain);
                rp.set_vertex_buffer(0, gpu.curtain.0.slice(..));
                rp.draw(0..(curtain.len() / 7) as u32, 0..1);
            }
            if !lines.is_empty() {
                rp.set_vertex_buffer(0, gpu.lines.0.slice(..));
                for pipe in [&p.lines_hidden, &p.lines] {
                    rp.set_pipeline(pipe);
                    rp.draw(0..(lines.len() / 7) as u32, 0..1);
                }
            }
        }
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &target.resolve, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &target.readback, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(target.bpr), rows_per_image: Some(ph) } },
            wgpu::Extent3d { width: pw, height: ph, depth_or_array_layers: 1 },
        );
        gpu.queue.submit([enc.finish()]);
        let slice = target.readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        gpu.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None })?;
        let mut bgra = Vec::with_capacity((pw * ph * 4) as usize);
        {
            let data = slice.get_mapped_range().map_err(|e| anyhow::anyhow!("{e:?}"))?;
            for row in data.chunks_exact(target.bpr as usize) {
                bgra.extend_from_slice(&row[..(pw * 4) as usize]);
            }
        }
        target.readback.unmap();
        let buf = image::RgbaImage::from_raw(pw, ph, bgra).ok_or_else(|| anyhow::anyhow!("frame size"))?;
        Ok(Arc::new(RenderImage::new(vec![image::Frame::new(buf)])))
    }
}

/// Paint the last frame and the overlay (labels, blocked points, shell arcs, aim marks).
pub fn paint(v: &mut Viewer, b: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
    window.paint_quad(gpui::fill(b, color(0x9eb3c7, 1.0)));
    if let Some(e) = &v.v3.err {
        paint::text(window, cx, e, point(b.origin.x + px(12.), b.origin.y + px(40.)), color(0xff8a80, 1.0), 13.0, true);
        return;
    }
    let Some(img) = v.v3.frame.clone() else { return };
    paint::image(window, &img, b);
    let Some(map) = v.map.clone() else { return };
    let (ox, oy) = (f32::from(b.origin.x), f32::from(b.origin.y));
    let v3 = &v.v3;
    let proj = |x: f64, y: f64, z: f64| v3.project(x, y, z).map(|(a, b)| paint::p(ox + a, oy + b));
    let dot = |window: &mut Window, cx: &mut App, p: Option<gpui::Point<Pixels>>, col: u32, label: &str, r: f32| {
        let Some(p) = p else { return };
        paint::dot(window, p, r, Some(color(col, 1.0)), Some((color(0, 1.0), 2.0)));
        paint::text(window, cx, label, point(p.x + px(9.), p.y - px(20.)), color(0xffffff, 1.0), 12.0, true);
    };
    let aim_mode = v.aim_ready();
    if v.sw.on(Sw::Red) && !aim_mode {
        for (i, (_, _, (x, z), on)) in v.red_zones().into_iter().enumerate() {
            if let Some(p) = proj(x, map.stand(x, z) + 3.0, z) {
                let (fill, ink) = if on { (color(0xb01818, 0.95), color(0xffffff, 1.0)) } else { (color(0x3a3f45, 0.85), color(0xb8c0c8, 1.0)) };
                paint::badge(window, cx, p, &(i + 1).to_string(), fill, ink);
            }
        }
    }
    if !aim_mode {
        for (pt, label, col) in [(v.a, "A", COL_A), (v.b, "B", COL_BPT)] {
            if let Some(q) = pt {
                dot(window, cx, proj(q.x, map.stand(q.x, q.z) + 25.0, q.z), col, label, 6.0);
            }
        }
    }
    if let Some((x, z, h)) = v.high_shown() {
        dot(window, cx, proj(x, h + 25.0, z), COL_HIGH, &format!("最高点 {h:.1} m"), 5.0);
    }
    if let (Some(aim), Some(a), Some(bb)) = (&v.aim, v.a, v.b) {
        if v.sw.on(Sw::Arc) {
            draw_arcs(v, &map.tr, aim, a, ox, oy, aim_mode, window, cx);
        }
        for q in &aim.sols {
            if let Some(bl) = q.blocked {
                if let Some(p) = proj(a.x + aim.dir.0 * bl, q.tr.at(Key::Y, bl), a.z + aim.dir.1 * bl) {
                    paint::dot(window, p, 4.0, Some(color(0xe2574c, 1.0)), Some((color(0, 1.0), 2.0)));
                }
            }
        }
        let sight = v.view_mode == crate::app::ViewMode::Sight;
        if aim_mode && sight {
            draw_sight(v, aim, ox, oy, window, cx);
        }
        if aim_mode {
            let (cx_, cy) = (ox + v3.wh.0 / 2.0, oy + v3.wh.1 / 2.0);
            let white = color(0xffffff, 1.0);
            let cross = [[(cx_ - 40.0, cy), (cx_ - 6.0, cy)], [(cx_ + 6.0, cy), (cx_ + 40.0, cy)], [(cx_, cy + 6.0), (cx_, cy + 40.0)]];
            for seg in if sight { &cross[..0] } else { &cross[..] } { // the sight draws its own reticle
                paint::stroke(window, &[paint::p(seg[0].0, seg[0].1), paint::p(seg[1].0, seg[1].1)], 1.0, white, None, false);
            }
            let los_ang = aim.dy.atan2(aim.d);
            for (i, q) in aim.sols.iter().enumerate() {
                let Some(p) = proj(bb.x, aim.y_a + aim.d * q.e.tan(), bb.z) else { continue };
                let col = color(if q.blocked.is_none() { if i > 0 { COL_B } else { COL_A } } else { 0xe2574c }, 1.0);
                paint::stroke(window, &[point(p.x - px(14.), p.y), point(p.x + px(14.), p.y)], 2.0, col, None, false);
                paint::stroke(window, &[point(p.x, p.y - px(5.)), point(p.x, p.y + px(5.))], 2.0, col, None, false);
                let name = if aim.sols.len() > 1 { if i > 0 { "高抛曲射" } else { "低伸直射" } } else { "瞄准点" };
                paint::mono(window, cx, &format!("{name} +{} 密位, 抬高 {:.2} m{}", scene::mil(q.e - los_ang), aim.d * q.e.tan() - aim.dy, if q.blocked.is_none() { "" } else { " [受阻]" }), point(p.x + px(18.), p.y - px(6.)), col, 12.0);
            }
            let head = if sight { format!("炮长瞄准镜 {:.1}×，视场 {:.1}°（滚轮变倍），目标 B 距离 {:.0} m", v3.cam.zoom, sight_fov(v3.cam.zoom), aim.d) } else { format!("从 A 瞄向 B：距离 {:.0} m，视场 {:.1}°（滚轮缩放）", aim.d, v3.cam.aim_fov) };
            paint::mono(window, cx, &format!("{head}{}", if aim.sols.is_empty() { "，当前无可行射击诸元" } else { "" }), paint::p(ox + 10.0, oy + 8.0), color(0xffffff, 1.0), 12.0);
        }
    }
    if matches!(v.view_mode, crate::app::ViewMode::Aim | crate::app::ViewMode::Sight) && !aim_mode {
        paint::mono(window, cx, "瞄准与炮长瞄准镜视角：请先左键放置开火点 A、右键放置目标点 B（当前显示自由环绕视角）。", paint::p(ox + 10.0, oy + 8.0), color(0xffffff, 1.0), 12.0);
    }
}

/// Shell arcs: thick path with a dark edge, faint and dashed where terrain or roofs hide it, a dot every 0.5 s,
/// the apex and the impact (at B or where blocked). Labels in orbit view only.
#[allow(clippy::too_many_arguments)]
fn draw_arcs(v: &Viewer, tr_s: &Terrain, aim: &scene::Aim, a: Pt, ox: f32, oy: f32, aim_mode: bool, window: &mut Window, cx: &mut App) {
    let v3 = &v.v3;
    let proj = |w: [f64; 3]| v3.project(w[0], w[1], w[2]).map(|(x, y)| paint::p(ox + x, oy + y));
    for (i, q) in aim.sols.iter().enumerate() {
        let tr = &q.tr;
        let end = q.blocked.unwrap_or(aim.d);
        let colh = if i > 0 { COL_B } else { COL_A };
        let at = |s: f64| [a.x + aim.dir.0 * s, tr.at(Key::Y, s), a.z + aim.dir.1 * s];
        let n = 160;
        let pts: Vec<(Option<gpui::Point<Pixels>>, bool)> = (0..=n).map(|k| { let w = at(end * k as f64 / n as f64); (proj(w), v3.hidden_from(tr_s, w)) }).collect();
        for pass in [0, 1] {
            let mut k = 1;
            while k <= n {
                let hid = pts[k - 1].1 || pts[k].1;
                let mut run: Vec<gpui::Point<Pixels>> = vec![];
                let flush = |run: &mut Vec<gpui::Point<Pixels>>, window: &mut Window| {
                    let (w, col) = if pass == 1 { (3.0, color(colh, if hid { 0.4 } else { 1.0 })) } else { (6.0, color(0, if hid { 0.24 } else { 0.6 })) };
                    paint::stroke(window, run, w, col, hid.then_some(&[5.0, 6.0][..]), false);
                    run.clear();
                };
                while k <= n && (pts[k - 1].1 || pts[k].1) == hid {
                    match (pts[k - 1].0, pts[k].0) {
                        (Some(p0), Some(p1)) => {
                            if run.is_empty() {
                                run.push(p0);
                            }
                            run.push(p1);
                        }
                        _ => flush(&mut run, window),
                    }
                    k += 1;
                }
                flush(&mut run, window);
            }
        }
        let (mut next, mut top) = (0.5, 0);
        for k in 1..tr.s.len() {
            if tr.s[k] > end {
                break;
            }
            if tr.y[k] > tr.y[top] {
                top = k;
            }
            if tr.t[k] < next {
                continue;
            }
            next += 0.5;
            let w = [a.x + aim.dir.0 * tr.s[k], tr.y[k], a.z + aim.dir.1 * tr.s[k]];
            if let (Some(p), false) = (proj(w), v3.hidden_from(tr_s, w)) {
                paint::dot(window, p, 3.0, Some(color(0xffffff, 1.0)), Some((color(colh, 1.0), 2.0)));
            }
        }
        let name = if aim.sols.len() > 1 { if i > 0 { "高抛曲射" } else { "低伸直射" } } else { "弹道" };
        if let Some(pe) = proj(at(end)) {
            let c = if q.blocked.is_some() { 0xef5a4f } else { colh };
            paint::stroke(window, &paint::circle_pts(pe, 8.0), 2.5, color(c, 1.0), None, true);
            if !aim_mode {
                let txt = match q.blocked { Some(_) => format!("{name} 在 {end:.0} m 处受阻"), None => format!("{name} 命中 {:.2} s, 存速 {:.0} m/s", q.t, q.v) };
                paint::label_box(window, cx, pe, &txt, color(c, 1.0));
            }
        }
        let rise = tr.y[top] - aim.y_a;
        if top > 0 && rise > 2.0 {
            if let Some(pa) = proj([a.x + aim.dir.0 * tr.s[top], tr.y[top], a.z + aim.dir.1 * tr.s[top]]) {
                paint::fill(window, &[point(pa.x, pa.y - px(6.)), point(pa.x + px(5.), pa.y + px(3.)), point(pa.x - px(5.), pa.y + px(3.))], color(colh, 1.0));
                if !aim_mode {
                    paint::label_box(window, cx, pa, &format!("{name}顶点 +{rise:.1} m (距 A {:.0} m)", tr.s[top]), color(colh, 1.0));
                }
            }
        }
    }
}

/// Gunner's sight over the aim camera (looking from A's gun at B): dark eyepiece around a round field, a reticle
/// with a tick every 4 mil (labels every 8), and range marks below the centre: put the mark for a range on a target
/// at that range and the gun is raised for it.
fn draw_sight(v: &Viewer, aim: &scene::Aim, ox: f32, oy: f32, window: &mut Window, cx: &mut App) {
    let v3 = &v.v3;
    let (w, h) = v3.wh;
    let (cx_, cy) = (ox + w / 2.0, oy + h / 2.0);
    let r = w.min(h) * 0.47;
    let k = (h / 2.0) as f64 / (sight_fov(v3.cam.zoom).to_radians() / 2.0).tan(); // px per unit of tan(angle)
    let at = |a: f64| (k * a.tan()) as f32;
    let mil = std::f64::consts::PI / 3200.0;
    // eyepiece: solid quads outside the circle's square (tessellated paths leave seams the scene shows through),
    // and the square with a round hole (lyon fills even-odd) for the corners
    let dark = color(0x050607, 1.0);
    let quad = |window: &mut Window, x0: f32, y0: f32, x1: f32, y1: f32| {
        if x1 > x0 && y1 > y0 {
            window.paint_quad(gpui::fill(Bounds::new(paint::p(x0, y0), gpui::size(px(x1 - x0), px(y1 - y0))), dark));
        }
    };
    let (sx0, sy0, sx1, sy1) = (cx_ - r, cy - r, cx_ + r, cy + r);
    quad(window, ox, oy, ox + w, sy0 + 1.0);
    quad(window, ox, sy1 - 1.0, ox + w, oy + h);
    quad(window, ox, oy, sx0 + 1.0, oy + h);
    quad(window, sx1 - 1.0, oy, ox + w, oy + h);
    let mut pb = gpui::PathBuilder::fill();
    for (i, q) in [(sx0, sy0), (sx1, sy0), (sx1, sy1), (sx0, sy1)].iter().enumerate() {
        if i == 0 { pb.move_to(paint::p(q.0, q.1)) } else { pb.line_to(paint::p(q.0, q.1)) }
    }
    pb.close();
    let ring = paint::circle_pts(paint::p(cx_, cy), r);
    pb.move_to(ring[0]);
    for q in &ring[1..] {
        pb.line_to(*q);
    }
    pb.close();
    if let Ok(path) = pb.build() {
        window.paint_path(path, dark);
    }
    paint::stroke(window, &ring, 3.0, color(0x000000, 1.0), None, true);
    let ink = color(0x0c0c0c, 1.0);
    let line = |window: &mut Window, x0: f32, y0: f32, x1: f32, y1: f32, wd: f32| paint::stroke(window, &[paint::p(x0, y0), paint::p(x1, y1)], wd, ink, None, false);
    line(window, cx_ - r, cy, cx_ - 5.0, cy, 1.5);
    line(window, cx_ + 5.0, cy, cx_ + r, cy, 1.5);
    line(window, cx_, cy + 5.0, cx_, cy + r, 1.5);
    line(window, cx_ - 5.0, cy - 9.0, cx_, cy, 1.5); // small chevron over the centre
    line(window, cx_, cy, cx_ + 5.0, cy - 9.0, 1.5);
    for i in 1..400 {
        let x = at(4.0 * i as f64 * mil);
        if x > r - 12.0 {
            break;
        }
        let big = i % 2 == 0;
        for s in [-1.0, 1.0] {
            line(window, cx_ + s * x, cy - if big { 8.0 } else { 4.0 }, cx_ + s * x, cy, 1.2);
            if big && x > 14.0 {
                paint::text(window, cx, &(i * 4).to_string(), paint::p(cx_ + s * x - 6.0, cy + 3.0), ink, 10.0, false);
            }
        }
    }
    // range marks: below the centre by the drop angle of a level shot (target at muzzle height)
    // shortcut: ignores the slope to the target, fine for the few degrees tank shots have
    if let Some(sh) = v.shell() {
        let max_r = 4000.0;
        let use_exact = sh.rocket.is_some() || sh.v0 < 350.0;
        let tr = (!use_exact).then(|| crate::core::simulate(sh, 0.0, aim.y_a, max_r + 1.0, Default::default()));
        let mut rng = 200.0;
        while rng <= max_r {
            let elev = if let Some(tr) = &tr {
                let y = tr.at(Key::Y, rng);
                if !y.is_finite() {
                    break;
                }
                ((aim.y_a - y) / rng).atan()
            } else {
                let Some(&e) = crate::core::solve_exact(sh, rng, 0.0, aim.y_a, -5.0, 45.0).first() else { break };
                e
            };
            let dy = at(elev);
            if dy > r - 10.0 {
                break;
            }
            let major = (rng as i64) % 400 == 0;
            if dy > 2.0 {
                line(window, cx_ - if major { 16.0 } else { 9.0 }, cy + dy, cx_ - 2.0, cy + dy, 1.2);
                if major {
                    paint::text(window, cx, &format!("{}", rng as i64 / 100), paint::p(cx_ - 34.0, cy + dy - 7.0), ink, 10.0, true);
                }
            }
            rng += 200.0;
        }
    }
}
