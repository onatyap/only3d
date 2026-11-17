//! Headless software renderer. No GPU, no window: returns an RGBA image / PNG bytes.
//!
//! It exists so that agents (and CI) can *see* models and toolpaths: shaded faces, crease and
//! silhouette outlines (CAD look), an optional print bed with a 10 mm grid, and coloured
//! polylines for toolpath previews.

use glam::{Mat4, Vec3, Vec4Swizzles};
use serde::{Deserialize, Serialize};

use crate::{Aabb, Mesh};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum View {
    #[default]
    Iso,
    Front,
    Back,
    Left,
    Right,
    Top,
    Bottom,
    /// Degrees. Azimuth 0 = looking from +X, -90 = from the front (-Y). Elevation 90 = from above.
    Custom { azimuth: f32, elevation: f32 },
}

impl View {
    pub fn angles(self) -> (f32, f32) {
        match self {
            View::Iso => (-55.0, 30.0),
            View::Front => (-90.0, 0.0),
            View::Back => (90.0, 0.0),
            View::Left => (180.0, 0.0),
            View::Right => (0.0, 0.0),
            View::Top => (-90.0, 90.0),
            View::Bottom => (-90.0, -90.0),
            View::Custom { azimuth, elevation } => (azimuth, elevation.clamp(-90.0, 90.0)),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RenderOptions {
    pub width: u32,
    pub height: u32,
    pub view: View,
    /// Draw the print bed (x size, y size) at Z=0 with origin at (0,0).
    pub bed: Option<[f32; 2]>,
    /// Frame the whole bed instead of just the objects.
    pub fit_bed: bool,
    pub background: [u8; 3],
    pub outline: bool,
    /// Supersampling factor (1..=4) for anti-aliasing.
    pub ssaa: u32,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self { width: 800, height: 600, view: View::Iso, bed: None, fit_bed: false, background: [246, 244, 240], outline: true, ssaa: 2 }
    }
}

pub struct RenderItem<'a> {
    pub mesh: &'a Mesh,
    pub color: [u8; 3],
}

/// A batch of 3D line segments, e.g. one toolpath feature type.
pub struct LineItem<'a> {
    pub segments: &'a [[Vec3; 2]],
    pub color: [u8; 3],
    /// Width in output pixels.
    pub width: f32,
}

pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Image {
    pub fn to_png(&self) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, self.width, self.height);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().expect("png header");
            w.write_image_data(&self.rgba).expect("png data");
        }
        out
    }
}

struct Target {
    w: usize,
    h: usize,
    depth: Vec<f32>,
    color: Vec<[f32; 3]>,
    normal: Vec<Vec3>,
    id: Vec<u32>,
}

const BG: u32 = u32::MAX;
const BED: u32 = u32::MAX - 1;
const LINE: u32 = u32::MAX - 2;

struct Cam {
    view: Mat4,
    scale: f32,
    cx: f32,
    cy: f32,
    w: f32,
    h: f32,
}

impl Cam {
    /// world -> (screen x, screen y, depth: larger = farther)
    fn project(&self, p: Vec3) -> Vec3 {
        let v = (self.view * p.extend(1.0)).xyz();
        Vec3::new((v.x - self.cx) * self.scale + self.w * 0.5, self.h * 0.5 - (v.y - self.cy) * self.scale, -v.z)
    }
}

fn make_cam(bounds: &Aabb, view: View, w: f32, h: f32) -> Cam {
    let (az, el) = view.angles();
    let (az, el) = (az.to_radians(), el.to_radians());
    let dir = Vec3::new(el.cos() * az.cos(), el.cos() * az.sin(), el.sin());
    let center = bounds.center();
    let radius = bounds.size().length().max(1.0);
    let eye = center + dir * radius * 2.0;
    let up = if el.abs() > 1.5 { Vec3::new(-az.cos(), -az.sin(), 0.0) * el.signum() } else { Vec3::Z };
    let viewm = Mat4::look_at_rh(eye, center, up);
    let mut b = Aabb::EMPTY;
    for c in bounds.corners() {
        b.extend((viewm * c.extend(1.0)).xyz());
    }
    let s = b.size();
    let scale = 0.88 * (w / s.x.max(1e-3)).min(h / s.y.max(1e-3));
    let c = b.center();
    Cam { view: viewm, scale, cx: c.x, cy: c.y, w, h }
}

impl Target {
    fn new(w: usize, h: usize, bg: [u8; 3]) -> Self {
        let c = [bg[0] as f32, bg[1] as f32, bg[2] as f32];
        Target { w, h, depth: vec![f32::INFINITY; w * h], color: vec![c; w * h], normal: vec![Vec3::ZERO; w * h], id: vec![BG; w * h] }
    }

    fn tri(&mut self, a: Vec3, b: Vec3, c: Vec3, col: [f32; 3], n: Vec3, id: u32, bias: f32) {
        let minx = a.x.min(b.x).min(c.x).floor().max(0.0) as i64;
        let maxx = a.x.max(b.x).max(c.x).ceil().min(self.w as f32 - 1.0) as i64;
        let miny = a.y.min(b.y).min(c.y).floor().max(0.0) as i64;
        let maxy = a.y.max(b.y).max(c.y).ceil().min(self.h as f32 - 1.0) as i64;
        if minx > maxx || miny > maxy {
            return;
        }
        let area = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
        if area.abs() < 1e-9 {
            return;
        }
        let inv = 1.0 / area;
        for y in miny..=maxy {
            let py = y as f32 + 0.5;
            for x in minx..=maxx {
                let px = x as f32 + 0.5;
                let w0 = ((b.x - px) * (c.y - py) - (b.y - py) * (c.x - px)) * inv;
                let w1 = ((c.x - px) * (a.y - py) - (c.y - py) * (a.x - px)) * inv;
                let w2 = 1.0 - w0 - w1;
                if w0 < -1e-5 || w1 < -1e-5 || w2 < -1e-5 {
                    continue;
                }
                let z = w0 * a.z + w1 * b.z + w2 * c.z + bias;
                let i = y as usize * self.w + x as usize;
                if z < self.depth[i] {
                    self.depth[i] = z;
                    self.color[i] = col;
                    self.normal[i] = n;
                    self.id[i] = id;
                }
            }
        }
    }

    fn line(&mut self, a: Vec3, b: Vec3, col: [f32; 3], width: f32) {
        let d = b - a;
        let steps = d.x.abs().max(d.y.abs()).ceil().max(1.0) as usize;
        let r = (width * 0.5).max(0.5);
        let ri = r.ceil() as i64;
        for s in 0..=steps {
            let p = a + d * (s as f32 / steps as f32);
            for oy in -ri..=ri {
                for ox in -ri..=ri {
                    if ((ox * ox + oy * oy) as f32) > r * r + 0.5 {
                        continue;
                    }
                    let (x, y) = (p.x as i64 + ox, p.y as i64 + oy);
                    if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 {
                        continue;
                    }
                    let i = y as usize * self.w + x as usize;
                    let z = p.z - 0.02;
                    if z < self.depth[i] {
                        self.depth[i] = z;
                        self.color[i] = col;
                        self.normal[i] = Vec3::Z;
                        self.id[i] = LINE;
                    }
                }
            }
        }
    }
}

fn shade(base: [u8; 3], n_view: Vec3) -> [f32; 3] {
    let l1 = Vec3::new(-0.35, 0.55, 0.76).normalize();
    let l2 = Vec3::new(0.6, -0.2, 0.77).normalize();
    let (n, back) = if n_view.z < 0.0 { (-n_view, true) } else { (n_view, false) };
    let k = 0.38 + 0.52 * n.dot(l1).max(0.0) + 0.18 * n.dot(l2).max(0.0);
    let k = if back { k * 0.6 } else { k };
    [base[0] as f32 * k, base[1] as f32 * k, base[2] as f32 * k]
}

/// Render meshes (and optional line batches) to an image.
pub fn render(items: &[RenderItem], lines: &[LineItem], opts: &RenderOptions) -> Image {
    let ss = opts.ssaa.clamp(1, 4) as usize;
    let (w, h) = (opts.width.max(16) as usize * ss, opts.height.max(16) as usize * ss);
    let mut bounds = Aabb::EMPTY;
    for it in items {
        bounds = bounds.union(&it.mesh.bounds());
    }
    for l in lines {
        for s in l.segments {
            bounds.extend(s[0]);
            bounds.extend(s[1]);
        }
    }
    if let Some(bed) = opts.bed {
        let bed_box = Aabb::new(Vec3::ZERO, Vec3::new(bed[0], bed[1], 0.0));
        if opts.fit_bed || bounds.is_empty() {
            bounds = bounds.union(&bed_box);
        }
    }
    if bounds.is_empty() {
        bounds = Aabb::new(Vec3::splat(-10.0), Vec3::splat(10.0));
    }
    let cam = make_cam(&bounds, opts.view, w as f32, h as f32);
    let rot = Mat4::from_mat3(glam::Mat3::from_mat4(cam.view));
    let mut t = Target::new(w, h, opts.background);

    if let Some(bed) = opts.bed {
        let n = rot.transform_vector3(Vec3::Z);
        let c = shade([226, 223, 216], n);
        let p = [Vec3::new(0.0, 0.0, 0.0), Vec3::new(bed[0], 0.0, 0.0), Vec3::new(bed[0], bed[1], 0.0), Vec3::new(0.0, bed[1], 0.0)].map(|v| cam.project(v));
        let bias = bounds.size().length() * 2e-3;
        t.tri(p[0], p[1], p[2], c, n, BED, bias);
        t.tri(p[0], p[2], p[3], c, n, BED, bias);
        let grid = [200.0, 196.0, 188.0];
        let mut x = 0.0;
        while x <= bed[0] + 1e-3 {
            let (a, b) = (cam.project(Vec3::new(x, 0.0, 0.0)), cam.project(Vec3::new(x, bed[1], 0.0)));
            t.line(a + Vec3::Z * bias * 0.5, b + Vec3::Z * bias * 0.5, grid, ss as f32 * 0.8);
            x += 10.0;
        }
        let mut y = 0.0;
        while y <= bed[1] + 1e-3 {
            let (a, b) = (cam.project(Vec3::new(0.0, y, 0.0)), cam.project(Vec3::new(bed[0], y, 0.0)));
            t.line(a + Vec3::Z * bias * 0.5, b + Vec3::Z * bias * 0.5, grid, ss as f32 * 0.8);
            y += 10.0;
        }
        // restore bed id on grid pixels so outlines don't trace the grid
        for i in 0..t.id.len() {
            if t.id[i] == LINE {
                t.id[i] = BED;
            }
        }
    }

    for (oid, it) in items.iter().enumerate() {
        let m = it.mesh;
        let proj: Vec<Vec3> = m.positions.iter().map(|p| cam.project(*p)).collect();
        for (fi, tri) in m.indices.iter().enumerate() {
            let n = m.face_normal(fi);
            let nv = rot.transform_vector3(n);
            let col = shade(it.color, nv);
            t.tri(proj[tri[0] as usize], proj[tri[1] as usize], proj[tri[2] as usize], col, nv, oid as u32, 0.0);
        }
    }
    for l in lines {
        let col = [l.color[0] as f32, l.color[1] as f32, l.color[2] as f32];
        for s in l.segments {
            t.line(cam.project(s[0]), cam.project(s[1]), col, l.width * ss as f32);
        }
    }

    if opts.outline {
        let scene = bounds.size().length() * cam.scale;
        let jump = (scene * 0.02).max(2.0);
        let mut edge = vec![false; w * h];
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let id = t.id[i];
                if id == BG || id == BED || id == LINE {
                    // silhouette against background is drawn from the object side below
                    continue;
                }
                for (dx, dy) in [(1i64, 0i64), (0, 1), (-1, 0), (0, -1)] {
                    let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                    if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                        continue;
                    }
                    let j = ny as usize * w + nx as usize;
                    let other = t.id[j];
                    let is_edge = if other == BG || other == BED {
                        true
                    } else if other == LINE {
                        false
                    } else if other != id {
                        true
                    } else {
                        // silhouette within an object (depth jump) or a sharp crease (normal change)
                        (t.depth[j] - t.depth[i]) * cam.scale > jump || t.normal[i].dot(t.normal[j]) < 0.82
                    };
                    if is_edge {
                        edge[i] = true;
                        break;
                    }
                }
            }
        }
        for i in 0..w * h {
            if edge[i] {
                let c = &mut t.color[i];
                for v in c.iter_mut() {
                    *v *= 0.28;
                }
            }
        }
    }

    // downsample
    let (ow, oh) = (w / ss, h / ss);
    let mut rgba = Vec::with_capacity(ow * oh * 4);
    let n = (ss * ss) as f32;
    for y in 0..oh {
        for x in 0..ow {
            let mut acc = [0.0f32; 3];
            for sy in 0..ss {
                for sx in 0..ss {
                    let c = t.color[(y * ss + sy) * w + x * ss + sx];
                    acc[0] += c[0];
                    acc[1] += c[1];
                    acc[2] += c[2];
                }
            }
            rgba.extend_from_slice(&[
                (acc[0] / n).clamp(0.0, 255.0) as u8,
                (acc[1] / n).clamp(0.0, 255.0) as u8,
                (acc[2] / n).clamp(0.0, 255.0) as u8,
                255,
            ]);
        }
    }
    Image { width: ow as u32, height: oh as u32, rgba }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shapes;

    #[test]
    fn renders_a_png() {
        let m = shapes::rounded_box(Vec3::new(30.0, 20.0, 10.0), 3.0, 4).unwrap();
        let img = render(&[RenderItem { mesh: &m, color: [180, 150, 120] }], &[], &RenderOptions { width: 200, height: 150, bed: Some([350.0, 320.0]), ..Default::default() });
        let png = img.to_png();
        assert_eq!(&png[1..4], b"PNG");
        // something other than background got drawn
        assert!(img.rgba.chunks(4).any(|p| p[0] < 200));
    }
}
