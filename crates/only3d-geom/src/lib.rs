#![allow(clippy::too_many_arguments, clippy::type_complexity, clippy::needless_range_loop, clippy::unnecessary_sort_by)]
//! only3d geometry core.
//!
//! Everything here is plain data + pure functions so it can be shared by the GUI, the
//! slicer, the CLI and agents without hidden state:
//! * [`Mesh`]: indexed triangle mesh (f32, millimetres, Z up) with analysis and repair.
//! * [`shapes`]: watertight primitive generators (rounded/chamfered boxes, cylinders, ...).
//! * [`io`]: STL / OBJ / 3MF import and export.
//! * [`raster`]: a dependency-free software renderer producing PNGs, so headless agents
//!   can *look* at what they built.

pub mod bbox;
pub mod faces;
pub mod hull;
pub mod io;
pub mod mesh;
pub mod raster;
pub mod repair;
pub mod shapes;
pub mod text;

pub use bbox::Aabb;
pub use glam::{DVec2, DVec3, Mat3, Mat4, Quat, Vec2, Vec3};
pub use mesh::{Mesh, MeshReport};

/// Geometry errors. Every variant carries enough context for an agent to act on it.
#[derive(Debug, thiserror::Error)]
pub enum GeomError {
    #[error("io error on {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("could not parse {format}: {message}")]
    Parse { format: &'static str, message: String },
    #[error("unsupported file type '{0}' (supported: stl, obj, 3mf, step)")]
    UnsupportedFormat(String),
    #[error("invalid shape parameters: {0}")]
    InvalidShape(String),
}

pub type Result<T> = std::result::Result<T, GeomError>;
