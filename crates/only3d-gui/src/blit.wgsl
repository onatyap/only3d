// Blit the off-screen scene into egui's render pass and draw the selection outline from the
// mask texture.

struct Globals {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    // w = outline radius in px
    viewport: vec4<f32>,
    key_dir: vec4<f32>,
    fill_dir: vec4<f32>,
    accent: vec4<f32>,
    outline: vec4<f32>,
    bg_top: vec4<f32>,
    bg_bottom: vec4<f32>,
    // z = encode to gamma, w = selection present
    flags: vec4<u32>,
    dots: vec4<f32>,
    overhang: vec4<f32>,
};

@group(0) @binding(0) var scene_tex: texture_2d<f32>;
@group(0) @binding(1) var mask_tex: texture_2d<f32>;
@group(0) @binding(2) var<uniform> g: Globals;

struct BlitOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_blit(@builtin(vertex_index) vi: u32) -> BlitOut {
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    var o: BlitOut;
    o.clip = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    o.uv = uv;
    return o;
}

fn to_gamma(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

@fragment
fn fs_blit(in: BlitOut) -> @location(0) vec4<f32> {
    let dims = vec2<i32>(textureDimensions(scene_tex));
    let p = clamp(vec2<i32>(in.uv * vec2<f32>(dims)), vec2<i32>(0), dims - 1);
    var c = textureLoad(scene_tex, p, 0).rgb;
    if (g.flags.w == 1u) {
        let m = textureLoad(mask_tex, p, 0).r;
        if (m < 0.5) {
            // outline: the strongest selected sample on two rings around the pixel
            let r = g.viewport.w;
            var hit = 0.0;
            for (var k = 0; k < 12; k++) {
                let a = f32(k) * 0.5235988;
                let d = vec2<f32>(cos(a), sin(a));
                let q1 = clamp(p + vec2<i32>(round(d * r)), vec2<i32>(0), dims - 1);
                let q2 = clamp(p + vec2<i32>(round(d * r * 0.5)), vec2<i32>(0), dims - 1);
                hit = max(hit, max(textureLoad(mask_tex, q1, 0).r * 0.85, textureLoad(mask_tex, q2, 0).r));
            }
            c = mix(c, g.outline.rgb, hit);
        }
    }
    if (g.flags.z == 1u) {
        c = to_gamma(c);
    }
    return vec4<f32>(c, 1.0);
}
