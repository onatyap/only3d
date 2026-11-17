// only3d viewport shaders.
//
// The scene is rendered off-screen (4x MSAA, sRGB colour target + an R8 selection mask as a
// second target, reversed-Z depth) and then blitted into egui's render pass (blit.wgsl), where
// the selection outline is drawn from the mask.

struct Globals {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    // x = width px, y = height px, z = focal length in px, w = outline radius px
    viewport: vec4<f32>,
    key_dir: vec4<f32>,
    fill_dir: vec4<f32>,
    accent: vec4<f32>,
    outline: vec4<f32>,
    bg_top: vec4<f32>,
    bg_bottom: vec4<f32>,
    // x = feature visibility mask, y = end of the highlighted (top layer) instances,
    // z = blit encodes gamma, w = selection present
    flags: vec4<u32>,
    // background dot grid: rgb + spacing px (0 = off)
    dots: vec4<f32>,
    // overhang highlight: rgb + on/off
    overhang: vec4<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;

struct FragOut {
    @location(0) color: vec4<f32>,
    @location(1) mask: vec4<f32>,
};

fn out(c: vec3<f32>, m: f32) -> FragOut {
    var o: FragOut;
    o.color = vec4<f32>(c, 1.0);
    o.mask = vec4<f32>(m, 0.0, 0.0, 1.0);
    return o;
}

// ------------------------------------------------------------------------------------------
// background gradient

struct BgOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) t: f32,
};

@vertex
fn vs_bg(@builtin(vertex_index) vi: u32) -> BgOut {
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    var o: BgOut;
    o.clip = vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
    o.t = clamp(uv.y, 0.0, 1.0);
    return o;
}

@fragment
fn fs_bg(in: BgOut) -> FragOut {
    // in.t = 0 at the bottom of the screen, 1 at the top
    var c = mix(g.bg_bottom.rgb, g.bg_top.rgb, smoothstep(0.0, 1.0, in.t));
    // dot grid, fixed to the screen
    let sp = g.dots.w;
    if (sp > 1.0) {
        let cell = in.clip.xy - sp * floor(in.clip.xy / sp) - vec2<f32>(sp * 0.5);
        let r = max(sp / 22.0, 0.9);
        let d = length(cell);
        let a = 1.0 - smoothstep(r - 0.6, r + 0.6, d);
        c = mix(c, g.dots.rgb, a);
    }
    return out(c, 0.0);
}

// ------------------------------------------------------------------------------------------
// meshes (world-space positions; flat normals from screen-space derivatives)

struct MeshOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) @interpolate(flat) color: vec4<f32>,
    @location(2) @interpolate(flat) flags: u32,
};

@vertex
fn vs_mesh(@location(0) pos: vec3<f32>, @location(1) color: vec4<f32>, @location(2) flags: u32, @location(3) xf: vec4<f32>, @location(4) pivot: vec2<f32>) -> MeshOut {
    var o: MeshOut;
    // live drag preview: rotate about Z around the pivot, then translate
    let ca = cos(xf.z);
    let sa = sin(xf.z);
    let q = pos.xy - pivot;
    let p = vec3<f32>(pivot.x + q.x * ca - q.y * sa + xf.x, pivot.y + q.x * sa + q.y * ca + xf.y, pos.z);
    o.clip = g.view_proj * vec4<f32>(p, 1.0);
    o.world = p;
    o.color = color;
    o.flags = flags;
    return o;
}

@fragment
fn fs_mesh(in: MeshOut) -> FragOut {
    let albedo = in.color.rgb;
    if ((in.flags & 1u) != 0u) {
        // unlit (plate, strips)
        return out(albedo, 0.0);
    }
    var n = normalize(cross(dpdxFine(in.world), dpdyFine(in.world)));
    let v = normalize(g.eye.xyz - in.world);
    if (dot(n, v) < 0.0) {
        n = -n;
    }
    let selected = (in.flags & 2u) != 0u;
    // pointed at from elsewhere (an activity row): a lighter tint than selection
    let hovered = (in.flags & 4u) != 0u;
    // the anchor for arranging: a stronger tint and rim than the rest of the selection
    let anchor = (in.flags & 8u) != 0u;
    var base = albedo;
    if (anchor) {
        base = mix(albedo, g.accent.rgb, 0.4);
    } else if (selected) {
        base = mix(albedo, g.accent.rgb, 0.22);
    } else if (hovered) {
        base = mix(albedo, g.accent.rgb, 0.14);
    }
    // overhangs steeper than 45° (not the faces resting on the plate)
    if (g.overhang.w > 0.5 && n.z < -0.7071 && in.world.z > 0.05) {
        base = mix(base, g.overhang.rgb, 0.85);
    }
    // warm hemisphere ambient + key + fill
    let hemi = mix(vec3<f32>(0.20, 0.19, 0.18), vec3<f32>(0.42, 0.41, 0.39), n.z * 0.5 + 0.5);
    let kd = max(dot(n, g.key_dir.xyz), 0.0);
    let fd = max(dot(n, g.fill_dir.xyz), 0.0);
    var c = base * (hemi + vec3<f32>(1.0, 0.97, 0.92) * kd * 0.78 + vec3<f32>(0.80, 0.86, 0.95) * fd * 0.26);
    // satin specular
    let h = normalize(g.key_dir.xyz + v);
    c += vec3<f32>(1.0, 0.98, 0.94) * pow(max(dot(n, h), 0.0), 48.0) * 0.10;
    // rim
    let rim = pow(1.0 - max(dot(n, v), 0.0), 3.0);
    if (anchor) {
        c += g.accent.rgb * rim * 1.1;
    } else if (selected) {
        c += g.accent.rgb * rim * 0.55;
    } else if (hovered) {
        c += g.accent.rgb * rim * 0.4;
    } else {
        c += vec3<f32>(1.0, 0.97, 0.92) * rim * 0.12;
    }
    return out(c, select(0.0, 1.0, selected));
}

// ------------------------------------------------------------------------------------------
// ribbons: toolpaths and plate grid lines, one instance (4-vertex strip) per segment

struct LineOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) @interpolate(flat) color: vec3<f32>,
    @location(1) side: f32,
    @location(2) @interpolate(flat) shade: f32,
};

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    return c * (c * (c * 0.305306011 + 0.682171111) + 0.012522878);
}

@vertex
fn vs_line(
    @builtin(vertex_index) vi: u32,
    @builtin(instance_index) ii: u32,
    @location(0) a: vec3<f32>,
    @location(1) b: vec3<f32>,
    @location(2) color: vec4<f32>,
    @location(3) width: f32,
) -> LineOut {
    var o: LineOut;
    // alpha byte carries the feature index (255 = always visible)
    let feature = u32(color.a * 255.0 + 0.5);
    if (feature < 32u && ((g.flags.x >> feature) & 1u) == 0u) {
        o.clip = vec4<f32>(2.0, 2.0, 2.0, 1.0);
        return o;
    }
    // strip corners: (0,-1) (0,1) (1,-1) (1,1)
    let t = f32(vi >> 1u);
    let side = f32(vi & 1u) * 2.0 - 1.0;
    var ca = g.view_proj * vec4<f32>(a, 1.0);
    var cb = g.view_proj * vec4<f32>(b, 1.0);
    // clip against a near plane in w so the screen-space maths stays valid
    let eps = 1e-3;
    if (ca.w < eps && cb.w < eps) {
        o.clip = vec4<f32>(2.0, 2.0, 2.0, 1.0);
        return o;
    }
    if (ca.w < eps) {
        ca = mix(ca, cb, (eps - ca.w) / (cb.w - ca.w));
    } else if (cb.w < eps) {
        cb = mix(cb, ca, (eps - cb.w) / (ca.w - cb.w));
    }
    let half_vp = g.viewport.xy * 0.5;
    var dir = cb.xy / cb.w * half_vp - ca.xy / ca.w * half_vp;
    let len = length(dir);
    dir = select(vec2<f32>(1.0, 0.0), dir / len, len > 1e-5);
    let p = select(ca, cb, t > 0.5);
    var wpx = -width;
    if (width > 0.0) {
        wpx = max(width * g.viewport.z / p.w, 1.25);
    }
    let half_w = wpx * 0.5;
    let off_px = vec2<f32>(-dir.y, dir.x) * side * half_w + dir * ((t * 2.0 - 1.0) * half_w);
    o.clip = vec4<f32>(p.xy + off_px / half_vp * p.w, p.z, p.w);
    var rgb = srgb_to_linear(color.rgb);
    if (width > 0.0 && ii < g.flags.y) {
        rgb = min(rgb * 1.12 + vec3<f32>(0.03), vec3<f32>(1.0));
    }
    o.color = rgb;
    o.side = side;
    o.shade = select(0.0, 1.0, width > 0.0);
    return o;
}

@fragment
fn fs_line(in: LineOut) -> FragOut {
    // fake tube lighting across the ribbon
    let s = clamp(in.side, -1.0, 1.0);
    let tube = 0.55 + 0.45 * sqrt(max(1.0 - s * s, 0.0)) + 0.08 * (-s);
    return out(in.color * mix(1.0, tube, in.shade), 0.0);
}
