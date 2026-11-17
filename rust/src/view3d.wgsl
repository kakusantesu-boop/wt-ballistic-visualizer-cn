// 3D views: WGSL ports of the GLSL programs in ../../index.html (<script id="view3d">). One bind group for all.

struct U {
    vp: mat4x4<f32>,
    eye: vec4<f32>,
    light: vec4<f32>,   // normalized
    sky: vec4<f32>,
    map: vec4<f32>,     // ox, oz, cell, water level
    uv: vec4<f32>,      // colour map origin x, z and 1 / extent x, z
    zn: vec4<f32>,      // zone layer origin x, z and 1 / size
    ov: vec4<f32>,      // sweep overlay origin x, z, 1 / size, on
    flags: vec4<f32>,   // grid on, ground detail on, clip_inner on
    clip_inner: vec4<f32>, // min_x, min_z, max_x, max_z of inner HM2 box
    tiles: array<vec4<f32>, 8>, // detail texture tile sizes in metres (32)
};
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var tH: texture_2d<f32>;        // heights, row j = z index j
@group(0) @binding(2) var tC: texture_2d<f32>;        // map colours (+Z up flipped: v grows with z)
@group(0) @binding(3) var tO: texture_2d<f32>;        // sweep overlay
@group(0) @binding(4) var tZ: texture_2d<f32>;        // zones
@group(0) @binding(5) var tIdx: texture_2d<f32>;      // detail index per ground texel (255 none)
@group(0) @binding(6) var tDet: texture_2d_array<f32>;
@group(0) @binding(7) var sLin: sampler;
@group(0) @binding(8) var sNear: sampler;

fn fog(col: vec3f, p: vec3f) -> vec3f {
    return mix(col, u.sky.rgb, 0.7 * (1.0 - exp(-distance(p, u.eye.xyz) / 20000.0)));
}
// red zones and outside the map edge, painted on surfaces inside the zone layer extent
fn zones(col: vec3f, p: vec3f) -> vec3f {
    let zuv = (p.xz - u.zn.xy) * u.zn.z;
    let inside = all(zuv >= vec2f(0.0)) && all(zuv <= vec2f(1.0));
    let zn = textureSampleLevel(tZ, sLin, zuv, 0.0);
    return mix(col, zn.rgb, select(0.0, zn.a, inside));
}
fn tile(i: i32) -> f32 {
    return u.tiles[i / 4][i % 4];
}

// ---- terrain: quadtree patches over the heightmap with a skirt ring
struct TOut { @builtin(position) pos: vec4f, @location(0) p: vec3f };
@vertex fn vs_terrain(@location(0) g: vec3f, @location(1) node: vec3f) -> TOut {
    let size = vec2i(textureDimensions(tH));
    let c = clamp(vec2i(node.xy + g.xy * node.z), vec2i(0), size - 1);
    let h = textureLoad(tH, c, 0).r - g.z * (node.z * u.map.z * 4.0 + 2.0);
    var o: TOut;
    o.p = vec3f(u.map.x + f32(c.x) * u.map.z, h, u.map.y + f32(c.y) * u.map.z);
    o.pos = u.vp * vec4f(o.p, 1.0);
    return o;
}
@fragment fn fs_terrain(i: TOut) -> @location(0) vec4f {
    let p = i.p;
    let uv = (p.xz - u.uv.xy) * u.uv.zw;
    var col = textureSample(tC, sLin, uv).rgb;
    // the game's tiled ground detail up close, as a brightness pattern over the colour map
    let id = i32(textureSampleLevel(tIdx, sNear, uv, 0.0).r * 255.0 + 0.5);
    let di = min(id, 31);
    let det = textureSample(tDet, sLin, p.xz / tile(di), di).r * 2.0;
    let near = u.flags.y * clamp(1.0 - distance(p, u.eye.xyz) / 700.0, 0.0, 1.0) * select(0.0, 1.0, id < 32);
    col *= mix(1.0, clamp(1.0 + (det - 1.0) * 1.6, 0.3, 1.8), 0.9 * near);
    var n = normalize(cross(dpdx(p), dpdy(p)));
    if (n.y < 0.0) { n = -n; }
    col *= 0.8 + 0.3 * max(dot(n, u.light.xyz), 0.0);
    col = zones(col, p);
    let q = p.xz / 100.0;
    let gd = abs(fract(q - 0.5) - 0.5) / fwidth(q);
    let g100 = (1.0 - min(min(gd.x, gd.y), 1.0)) * clamp(1.0 - max(fwidth(q.x), fwidth(q.y)) * 1.6, 0.0, 1.0);
    let qk = p.xz / 1000.0;
    let gdk = abs(fract(qk - 0.5) - 0.5) / fwidth(qk);
    let g1k = (1.0 - min(min(gdk.x, gdk.y), 1.0)) * clamp(1.0 - max(fwidth(qk.x), fwidth(qk.y)) * 1.6, 0.0, 1.0);
    col = mix(col, vec3f(1.0), u.flags.x * 0.18 * max(g100, g1k * 0.85));
    let ou = (p.xz - u.ov.xy) * u.ov.z;
    let t = textureSampleLevel(tO, sNear, ou, 0.0);
    let inside = u.ov.w > 0.0 && all(ou >= vec2f(0.0)) && all(ou <= vec2f(1.0));
    col = mix(col, t.rgb, select(0.0, t.a, inside));
    if (u.flags.z > 0.5 && p.x > u.clip_inner.x + 1.0 && p.z > u.clip_inner.y + 1.0 && p.x < u.clip_inner.z - 1.0 && p.z < u.clip_inner.w - 1.0) {
        discard;
    }
    return vec4f(fog(col, p), 1.0);
}

// ---- lines (areas, poles, sight line, arcs) and arc curtains, pulled to the eye: no z-fight with terrain
override ALPHA: f32 = 1.0;
struct LOut { @builtin(position) pos: vec4f, @location(0) c: vec4f };
@vertex fn vs_lines(@location(0) p: vec3f, @location(1) c: vec4f) -> LOut {
    var o: LOut;
    o.pos = u.vp * vec4f(p + (u.eye.xyz - p) * 0.003, 1.0);
    o.c = vec4f(c.rgb, c.a * ALPHA);
    return o;
}
@fragment fn fs_lines(i: LOut) -> @location(0) vec4f {
    return i.c;
}

// ---- objects and tank models: one mesh instanced by placements (rows of a Dagor tm) and a tint
struct BOut { @builtin(position) pos: vec4f, @location(0) p: vec3f, @location(1) n: vec3f, @location(2) c: vec3f };
@vertex fn vs_build(@location(0) p: vec3f, @location(1) n: vec4f, @location(6) c: vec4f,
                    @location(2) ax: vec3f, @location(3) ay: vec3f, @location(4) az: vec3f, @location(5) at: vec3f, @location(7) tint: vec4f) -> BOut {
    var o: BOut;
    o.p = p.x * ax + p.y * ay + p.z * az + at;
    o.n = n.x * ax + n.y * ay + n.z * az;
    o.c = c.rgb * tint.rgb;
    o.pos = u.vp * vec4f(o.p, 1.0);
    return o;
}
@fragment fn fs_build(i: BOut) -> @location(0) vec4f {
    let n = i.n / max(length(i.n), 1e-6);
    let col = zones(i.c * (0.5 + 0.6 * abs(dot(n, u.light.xyz))), i.p);
    return vec4f(fog(col, i.p), 1.0);
}

// ---- trees: a proxy per crown shape, instanced per tree with a random turn and a slight colour change
fn hash(p: vec2f) -> f32 { return fract(sin(dot(p, vec2f(12.9898, 78.233))) * 43758.5453); }
struct TrOut { @builtin(position) pos: vec4f, @location(0) p: vec3f, @location(1) n: vec3f, @location(2) c: vec3f, @location(3) y: f32 };
@vertex fn vs_tree(@location(0) ap: vec4f, @location(4) an: vec3f, @location(1) ai: vec4f, @location(2) sz: vec4f, @location(3) ac: vec4f) -> TrOut {
    let h = sz.x * ai.w;
    let r = sz.y * ai.w;
    let cb = sz.z * ai.w;
    let a = hash(ai.xz) * 6.2831853;
    let m = mat2x2f(cos(a), sin(a), -sin(a), cos(a));
    var o: TrOut;
    var q: vec3f;
    if (ap.w > 0.5) {
        let tr = max(0.12, 0.07 * r);
        q = vec3f(ap.x * tr, ap.y * (cb + 0.25 * (h - cb)), ap.z * tr);
        o.c = vec3f(0.30, 0.22, 0.15);
        o.y = 1.0;
    } else {
        q = vec3f(ap.x * r, cb + ap.y * (h - cb), ap.z * r);
        o.c = ac.rgb * (0.9 + 0.4 * hash(ai.zx + 7.0));
        o.y = ap.y;
    }
    let qxz = m * q.xz;
    let nxz = m * an.xz;
    o.n = vec3f(nxz.x, an.y, nxz.y);
    o.p = ai.xyz + vec3f(qxz.x, q.y, qxz.y);
    o.pos = u.vp * vec4f(o.p, 1.0);
    return o;
}
fn h3(p: vec3f) -> f32 { return fract(sin(dot(p, vec3f(127.1, 311.7, 74.7))) * 43758.5453); }
fn noise3(p: vec3f) -> f32 {
    let i = floor(p);
    var f = fract(p);
    f = f * f * (3.0 - 2.0 * f);
    return mix(mix(mix(h3(i), h3(i + vec3f(1, 0, 0)), f.x), mix(h3(i + vec3f(0, 1, 0)), h3(i + vec3f(1, 1, 0)), f.x), f.y),
               mix(mix(h3(i + vec3f(0, 0, 1)), h3(i + vec3f(1, 0, 1)), f.x), mix(h3(i + vec3f(0, 1, 1)), h3(i + vec3f(1, 1, 1)), f.x), f.y), f.z);
}
@fragment fn fs_tree(i: TrOut) -> @location(0) vec4f {
    let n = i.n / max(length(i.n), 1e-6);
    let lit = 0.45 + 0.6 * max(dot(n, u.light.xyz), 0.0) + 0.12 * n.y;
    let leaf = 0.6 * noise3(i.p * 1.6) + 0.4 * noise3(i.p * 4.5); // clumps of leaves, darker gaps
    let col = zones(i.c * lit * (0.6 + 0.6 * leaf) * (0.62 + 0.38 * i.y), i.p);
    return vec4f(fog(col, i.p), 1.0);
}

// ---- water: one quad over the map at the level's water_level; depth of the bare ground below sets colour and opacity
struct WOut { @builtin(position) pos: vec4f, @location(0) p: vec3f };
@vertex fn vs_water(@builtin(vertex_index) vi: u32) -> WOut {
    let size = vec2f(textureDimensions(tH));
    let c = vec2f(f32(vi & 1u), f32(vi >> 1u));
    var o: WOut;
    o.p = vec3f(u.map.x + c.x * (size.x - 1.0) * u.map.z, u.map.w, u.map.y + c.y * (size.y - 1.0) * u.map.z);
    o.pos = u.vp * vec4f(o.p, 1.0);
    return o;
}
fn h2(p: vec2f) -> f32 { return fract(sin(dot(p, vec2f(127.1, 311.7))) * 43758.5453); }
fn vn(p: vec2f) -> f32 {
    let i = floor(p);
    var f = fract(p);
    f = f * f * (3.0 - 2.0 * f);
    return mix(mix(h2(i), h2(i + vec2f(1, 0)), f.x), mix(h2(i + vec2f(0, 1)), h2(i + vec2f(1, 1)), f.x), f.y);
}
fn waves(p: vec2f) -> f32 { return vn(p * 0.18) + 0.5 * vn(p * 0.47 + 3.1) + 0.25 * vn(p * 1.3 + 7.7); }
@fragment fn fs_water(i: WOut) -> @location(0) vec4f {
    let p = i.p;
    let size = vec2i(textureDimensions(tH));
    let c = clamp(vec2i(floor((p.xz - u.map.xy) / u.map.z + 0.5)), vec2i(0), size - 1);
    let depth = p.y - textureLoad(tH, c, 0).r;
    let w0 = waves(p.xz);
    let e = 0.6;
    let n = normalize(vec3f((w0 - waves(p.xz + vec2f(e, 0.0))) * 0.35, 1.0, (w0 - waves(p.xz + vec2f(0.0, e))) * 0.35));
    let v = normalize(u.eye.xyz - p);
    let fr = 0.03 + 0.97 * pow(1.0 - max(dot(n, v), 0.0), 5.0);
    var col = mix(vec3f(0.20, 0.45, 0.52), vec3f(0.05, 0.18, 0.28), clamp(depth / 12.0, 0.0, 1.0));
    col = mix(col, u.sky.rgb, fr * 0.85);
    col += vec3f(1.0, 0.95, 0.85) * pow(max(dot(reflect(-u.light.xyz, n), v), 0.0), 160.0) * 0.9;
    col = mix(col, vec3f(0.85, 0.9, 0.92), 0.45 * smoothstep(0.45, 0.0, depth)); // shore line
    let a = max(clamp(0.5 + depth * 0.1, 0.5, 0.92), fr);
    col = zones(col, p);
    if (depth <= 0.0) { discard; }
    if (u.flags.z > 0.5 && p.x >= u.clip_inner.x && p.z >= u.clip_inner.y && p.x <= u.clip_inner.z && p.z <= u.clip_inner.w) {
        discard;
    }
    return vec4f(fog(col, p), a);
}
