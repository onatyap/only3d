use std::fmt::Write;

use glam::Vec3;

use crate::{GeomError, Mesh, Result};

pub fn read(bytes: &[u8]) -> Result<Mesh> {
    let text = String::from_utf8_lossy(bytes);
    let mut pos = Vec::new();
    let mut idx = Vec::new();
    for (ln, line) in text.lines().enumerate() {
        let mut it = line.split_whitespace();
        match it.next() {
            Some("v") => {
                let c: Vec<f32> = it.take(3).filter_map(|s| s.parse().ok()).collect();
                if c.len() != 3 {
                    return Err(GeomError::Parse { format: "obj", message: format!("line {}: bad vertex", ln + 1) });
                }
                pos.push(Vec3::new(c[0], c[1], c[2]));
            }
            Some("f") => {
                let mut f = Vec::new();
                for tok in it {
                    let v: i64 = tok.split('/').next().and_then(|s| s.parse().ok()).ok_or_else(|| GeomError::Parse {
                        format: "obj",
                        message: format!("line {}: bad face index '{tok}'", ln + 1),
                    })?;
                    let i = if v < 0 { pos.len() as i64 + v } else { v - 1 };
                    if i < 0 || i as usize >= pos.len() {
                        return Err(GeomError::Parse { format: "obj", message: format!("line {}: index out of range", ln + 1) });
                    }
                    f.push(i as u32);
                }
                for k in 1..f.len().saturating_sub(1) {
                    idx.push([f[0], f[k], f[k + 1]]);
                }
            }
            _ => {}
        }
    }
    let mut m = Mesh::from_parts(pos, idx);
    m.weld(1e-5);
    Ok(m)
}

pub fn write(m: &Mesh) -> String {
    let mut s = String::with_capacity(m.positions.len() * 30 + m.indices.len() * 20);
    s.push_str("# only3d OBJ\n");
    for p in &m.positions {
        let _ = writeln!(s, "v {} {} {}", p.x, p.y, p.z);
    }
    for t in &m.indices {
        let _ = writeln!(s, "f {} {} {}", t[0] + 1, t[1] + 1, t[2] + 1);
    }
    s
}
