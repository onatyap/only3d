//! File import/export. `load` / `save` dispatch on the extension.

pub mod obj;
pub mod step;
pub mod stl;
pub mod threemf;

use std::path::Path;

use crate::{GeomError, Mesh, Result};

/// One imported object (3MF files can hold several, with names and colours).
#[derive(Clone, Debug)]
pub struct ImportedObject {
    pub name: String,
    /// The whole object (every normal part merged).
    pub mesh: Mesh,
    pub color: Option<[u8; 3]>,
    /// The object's parts when it is made of several (slicer projects: each can have its own
    /// filament). Empty for single-body objects.
    pub parts: Vec<ImportedPart>,
}

/// One part of a multi-part object, in the same coordinates as the whole object.
#[derive(Clone, Debug)]
pub struct ImportedPart {
    /// The part's object id inside the 3MF (how slicer metadata refers to it).
    pub id: u32,
    pub name: String,
    pub mesh: Mesh,
    /// The slicer's filament slot for this part (1-based), when the file says.
    pub filament: Option<u32>,
}

impl ImportedObject {
    pub fn new(name: impl Into<String>, mesh: Mesh) -> Self {
        ImportedObject { name: name.into(), mesh, color: None, parts: Vec::new() }
    }
}

fn ext(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

pub(crate) fn io_err(path: &Path, e: std::io::Error) -> GeomError {
    GeomError::Io { path: path.display().to_string(), source: e }
}

/// Load every object in a file.
pub fn load(path: &Path) -> Result<Vec<ImportedObject>> {
    let bytes = std::fs::read(path).map_err(|e| io_err(path, e))?;
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("object").to_string();
    match ext(path).as_str() {
        "stl" => Ok(vec![ImportedObject::new(stem, stl::read(&bytes)?)]),
        "obj" => Ok(vec![ImportedObject::new(stem, obj::read(&bytes)?)]),
        "3mf" => threemf::read(&bytes),
        "step" | "stp" => step::read(&bytes),
        other => Err(GeomError::UnsupportedFormat(other.to_string())),
    }
}

/// Load a file and merge all its objects into one mesh.
pub fn load_merged(path: &Path) -> Result<Mesh> {
    let mut m = Mesh::new();
    for o in load(path)? {
        m.append(&o.mesh);
    }
    Ok(m)
}

/// Save objects; STL/OBJ merge them, 3MF keeps them separate.
pub fn save(path: &Path, objects: &[ImportedObject]) -> Result<()> {
    let bytes = match ext(path).as_str() {
        "stl" => {
            let mut m = Mesh::new();
            objects.iter().for_each(|o| m.append(&o.mesh));
            stl::write_binary(&m)
        }
        "obj" => {
            let mut m = Mesh::new();
            objects.iter().for_each(|o| m.append(&o.mesh));
            obj::write(&m).into_bytes()
        }
        "3mf" => threemf::write(objects)?,
        other => return Err(GeomError::UnsupportedFormat(other.to_string())),
    };
    std::fs::write(path, bytes).map_err(|e| io_err(path, e))
}
