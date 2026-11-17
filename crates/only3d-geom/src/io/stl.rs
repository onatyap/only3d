use glam::Vec3;

use crate::{GeomError, Mesh, Result};

/// Read binary or ASCII STL.
pub fn read(bytes: &[u8]) -> Result<Mesh> {
    if bytes.len() >= 84 {
        let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
        if 84 + n * 50 == bytes.len() {
            return read_binary(bytes, n);
        }
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_ascii_lowercase();
    if head.trim_start().starts_with("solid") {
        return read_ascii(bytes);
    }
    if bytes.len() >= 84 {
        // Truncated/padded binary file: read what is there.
        let n = ((bytes.len() - 84) / 50).min(u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize);
        return read_binary(bytes, n);
    }
    Err(GeomError::Parse { format: "stl", message: "file too small".into() })
}

fn read_binary(bytes: &[u8], n: usize) -> Result<Mesh> {
    let f = |o: usize| f32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let mut tris = Vec::with_capacity(n);
    for i in 0..n {
        let o = 84 + i * 50 + 12;
        let v = |k: usize| Vec3::new(f(o + k * 12), f(o + k * 12 + 4), f(o + k * 12 + 8));
        tris.push([v(0), v(1), v(2)]);
    }
    if tris.iter().any(|t| t.iter().any(|v| !v.is_finite())) {
        return Err(GeomError::Parse { format: "stl", message: "non-finite vertex coordinates".into() });
    }
    Ok(Mesh::from_soup(&tris))
}

fn read_ascii(bytes: &[u8]) -> Result<Mesh> {
    let text = String::from_utf8_lossy(bytes);
    let mut tris = Vec::new();
    let mut cur: Vec<Vec3> = Vec::with_capacity(3);
    for line in text.lines() {
        let mut it = line.split_whitespace();
        match it.next() {
            Some("vertex") => {
                let c: Vec<f32> = it.take(3).filter_map(|s| s.parse().ok()).collect();
                if c.len() != 3 {
                    return Err(GeomError::Parse { format: "stl", message: format!("bad vertex line: {line}") });
                }
                cur.push(Vec3::new(c[0], c[1], c[2]));
            }
            Some("endfacet") => {
                if cur.len() >= 3 {
                    for k in 1..cur.len() - 1 {
                        tris.push([cur[0], cur[k], cur[k + 1]]);
                    }
                }
                cur.clear();
            }
            _ => {}
        }
    }
    if tris.is_empty() {
        return Err(GeomError::Parse { format: "stl", message: "no facets found".into() });
    }
    Ok(Mesh::from_soup(&tris))
}

/// Text STL (larger than binary, but readable and diffable).
pub fn write_ascii(m: &Mesh, name: &str) -> String {
    use std::fmt::Write;
    let name: String = name.chars().map(|c| if c.is_whitespace() { '_' } else { c }).collect();
    let mut out = format!("solid {name}\n");
    for i in 0..m.indices.len() {
        let n = m.face_normal(i);
        let _ = writeln!(out, "facet normal {} {} {}\nouter loop", n.x, n.y, n.z);
        for v in m.triangle(i) {
            let _ = writeln!(out, "vertex {} {} {}", v.x, v.y, v.z);
        }
        out.push_str("endloop\nendfacet\n");
    }
    let _ = writeln!(out, "endsolid {name}");
    out
}

pub fn write_binary(m: &Mesh) -> Vec<u8> {
    let mut out = Vec::with_capacity(84 + m.indices.len() * 50);
    let mut header = [0u8; 80];
    let tag = b"only3d binary STL";
    header[..tag.len()].copy_from_slice(tag);
    out.extend_from_slice(&header);
    out.extend_from_slice(&(m.indices.len() as u32).to_le_bytes());
    for i in 0..m.indices.len() {
        let n = m.face_normal(i);
        for v in [n].iter().chain(m.triangle(i).iter()) {
            for c in [v.x, v.y, v.z] {
                out.extend_from_slice(&c.to_le_bytes());
            }
        }
        out.extend_from_slice(&[0, 0]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shapes;

    #[test]
    fn ascii_writes_back_the_same_triangles() {
        let m = shapes::cuboid(Vec3::splat(10.0)).unwrap();
        let back = read(write_ascii(&m, "a cube").as_bytes()).unwrap();
        assert_eq!(back.triangle_count(), m.triangle_count());
    }

    #[test]
    fn roundtrip_binary_and_ascii() {
        let m = shapes::cylinder(5.0, 5.0, 10.0, Some(24), Default::default(), Default::default()).unwrap();
        let back = read(&write_binary(&m)).unwrap();
        assert_eq!(back.triangle_count(), m.triangle_count());
        assert!(back.analyze().watertight);
        let ascii = "solid t\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid t\n";
        assert_eq!(read(ascii.as_bytes()).unwrap().triangle_count(), 1);
    }
}
