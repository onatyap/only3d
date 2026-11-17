//! Orbit camera (Z up) with reversed-Z infinite perspective.

use glam::{Mat4, Vec2, Vec3, Vec4Swizzles};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ViewPreset {
    Iso,
    Top,
    Bottom,
    Front,
    Back,
    Left,
    Right,
}

impl ViewPreset {
    pub fn label(self) -> &'static str {
        match self {
            ViewPreset::Iso => "Iso",
            ViewPreset::Top => "Top",
            ViewPreset::Bottom => "Bottom",
            ViewPreset::Front => "Front",
            ViewPreset::Back => "Back",
            ViewPreset::Left => "Left",
            ViewPreset::Right => "Right",
        }
    }
    /// (yaw, pitch) in degrees. Yaw 0 looks from +X, -90 from the front (-Y).
    fn angles(self) -> (f32, f32) {
        match self {
            ViewPreset::Iso => (-112.0, 34.0),
            ViewPreset::Top => (-90.0, 89.9),
            ViewPreset::Bottom => (-90.0, -89.5),
            ViewPreset::Front => (-90.0, 4.0),
            ViewPreset::Back => (90.0, 4.0),
            ViewPreset::Left => (180.0, 4.0),
            ViewPreset::Right => (0.0, 4.0),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct OrbitCamera {
    pub target: Vec3,
    /// radians, around Z
    pub yaw: f32,
    /// radians, elevation above the XY plane
    pub pitch: f32,
    pub dist: f32,
    pub fov_y: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        let mut c = OrbitCamera { target: Vec3::new(175.0, 160.0, 0.0), yaw: 0.0, pitch: 0.0, dist: 600.0, fov_y: 32f32.to_radians() };
        c.set_preset(ViewPreset::Iso);
        c
    }
}

impl OrbitCamera {
    pub fn set_preset(&mut self, p: ViewPreset) {
        let (y, pi) = p.angles();
        self.yaw = y.to_radians();
        self.pitch = pi.to_radians();
    }

    pub fn dir_from_target(&self) -> Vec3 {
        Vec3::new(self.pitch.cos() * self.yaw.cos(), self.pitch.cos() * self.yaw.sin(), self.pitch.sin())
    }

    pub fn eye(&self) -> Vec3 {
        self.target + self.dir_from_target() * self.dist
    }

    /// Camera basis: (right, up, back) in world space.
    pub fn basis(&self) -> (Vec3, Vec3, Vec3) {
        let back = self.dir_from_target();
        let right = Vec3::Z.cross(back).try_normalize().unwrap_or(Vec3::X);
        let up = back.cross(right).normalize();
        (right, up, back)
    }

    pub fn view(&self) -> Mat4 {
        let (_, up, _) = self.basis();
        Mat4::look_at_rh(self.eye(), self.target, up)
    }

    pub fn near(&self) -> f32 {
        (self.dist * 0.002).clamp(0.02, 5.0)
    }

    pub fn proj(&self, aspect: f32) -> Mat4 {
        Mat4::perspective_infinite_reverse_rh(self.fov_y, aspect.max(1e-3), self.near())
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        self.proj(aspect) * self.view()
    }

    /// Focal length in pixels for a viewport `h` pixels tall.
    pub fn focal_px(&self, h: f32) -> f32 {
        h * 0.5 / (self.fov_y * 0.5).tan()
    }

    pub fn orbit(&mut self, d: Vec2) {
        self.yaw -= d.x * 0.0085;
        self.pitch = (self.pitch + d.y * 0.0085).clamp(-89.5f32.to_radians(), 89.9f32.to_radians());
    }

    /// Pan by a screen delta (points) in a viewport `h` points tall.
    pub fn pan(&mut self, d: Vec2, h: f32) {
        let (right, up, _) = self.basis();
        let k = 2.0 * self.dist * (self.fov_y * 0.5).tan() / h.max(1.0);
        self.target += (-right * d.x + up * d.y) * k;
    }

    /// Dolly by `factor` (<1 zooms in) while keeping `anchor` fixed on screen.
    pub fn zoom_about(&mut self, factor: f32, anchor: Vec3) {
        let factor = factor.clamp(0.2, 5.0);
        let new_dist = (self.dist * factor).clamp(2.0, 20000.0);
        let f = new_dist / self.dist;
        let eye = self.eye();
        let new_eye = anchor + (eye - anchor) * f;
        let new_target = anchor + (self.target - anchor) * f;
        self.target = new_target;
        self.dist = (new_eye - new_target).length();
    }

    /// Frame a bounding box (its bounding sphere, scaled by `margin`).
    pub fn frame(&mut self, min: Vec3, max: Vec3, aspect: f32, margin: f32) {
        let c = (min + max) * 0.5;
        let r = ((max - min).length() * 0.5).max(5.0);
        let half_y = self.fov_y * 0.5;
        let half_x = (half_y.tan() * aspect.max(0.2)).atan();
        let fit = half_y.min(half_x).max(1e-3);
        self.target = c;
        self.dist = r / fit.sin() * margin;
    }

    /// World-space ray through a point in the viewport (`p` relative to its top-left, in points).
    pub fn ray(&self, p: Vec2, size: Vec2) -> (Vec3, Vec3) {
        let ndc = Vec2::new(p.x / size.x * 2.0 - 1.0, 1.0 - p.y / size.y * 2.0);
        let inv = self.view_proj(size.x / size.y).inverse();
        // reversed Z: depth 1 = near plane, ~0 = far
        let near = inv * glam::Vec4::new(ndc.x, ndc.y, 1.0, 1.0);
        let far = inv * glam::Vec4::new(ndc.x, ndc.y, 0.1, 1.0);
        let a = near.xyz() / near.w;
        let b = far.xyz() / far.w;
        (a, (b - a).normalize_or(-self.dir_from_target()))
    }

    /// Project a world point to viewport points (None if behind the camera).
    pub fn project(&self, p: Vec3, size: Vec2) -> Option<Vec2> {
        let c = self.view_proj(size.x / size.y) * p.extend(1.0);
        if c.w <= 1e-4 {
            return None;
        }
        let n = c.xy() / c.w;
        Some(Vec2::new((n.x * 0.5 + 0.5) * size.x, (0.5 - n.y * 0.5) * size.y))
    }
}
