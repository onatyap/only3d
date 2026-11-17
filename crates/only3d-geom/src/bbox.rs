use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};

/// Axis-aligned bounding box in millimetres.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Default for Aabb {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl Aabb {
    pub const EMPTY: Aabb = Aabb { min: Vec3::splat(f32::INFINITY), max: Vec3::splat(f32::NEG_INFINITY) };

    pub fn new(min: Vec3, max: Vec3) -> Self {
        Self { min, max }
    }

    pub fn from_points<'a>(pts: impl IntoIterator<Item = &'a Vec3>) -> Self {
        let mut b = Self::EMPTY;
        for p in pts {
            b.extend(*p);
        }
        b
    }

    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y || self.min.z > self.max.z
    }

    pub fn extend(&mut self, p: Vec3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }

    pub fn union(&self, o: &Aabb) -> Aabb {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        Aabb { min: self.min.min(o.min), max: self.max.max(o.max) }
    }

    pub fn size(&self) -> Vec3 {
        if self.is_empty() {
            Vec3::ZERO
        } else {
            self.max - self.min
        }
    }

    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    pub fn corners(&self) -> [Vec3; 8] {
        let (a, b) = (self.min, self.max);
        [
            Vec3::new(a.x, a.y, a.z),
            Vec3::new(b.x, a.y, a.z),
            Vec3::new(b.x, b.y, a.z),
            Vec3::new(a.x, b.y, a.z),
            Vec3::new(a.x, a.y, b.z),
            Vec3::new(b.x, a.y, b.z),
            Vec3::new(b.x, b.y, b.z),
            Vec3::new(a.x, b.y, b.z),
        ]
    }

    pub fn transformed(&self, m: &Mat4) -> Aabb {
        if self.is_empty() {
            return *self;
        }
        Aabb::from_points(self.corners().iter().map(|c| m.transform_point3(*c)).collect::<Vec<_>>().iter())
    }

    pub fn overlaps(&self, o: &Aabb) -> bool {
        !(self.max.x < o.min.x
            || self.min.x > o.max.x
            || self.max.y < o.min.y
            || self.min.y > o.max.y
            || self.max.z < o.min.z
            || self.min.z > o.max.z)
    }

    pub fn contains(&self, o: &Aabb) -> bool {
        o.min.cmpge(self.min).all() && o.max.cmple(self.max).all()
    }
}
