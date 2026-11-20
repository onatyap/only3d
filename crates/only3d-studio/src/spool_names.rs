//! Automatic spool names: brand + product line + colour name ("Bambu PETG Translucent Pink").
//!
//! Bambu spools take Bambu's own colour names (`profiles/bambu/filament_colour_names.tsv`, from
//! Bambu Studio). Anything else gets the nearest of a short list of common colour names.

use std::sync::OnceLock;

const BAMBU_TABLE: &str = include_str!("../../../profiles/bambu/filament_colour_names.tsv");

struct Named {
    id: &'static str,
    line: &'static str,
    rgb: [u8; 3],
    name: &'static str,
}

fn bambu() -> &'static [Named] {
    static T: OnceLock<Vec<Named>> = OnceLock::new();
    T.get_or_init(|| {
        BAMBU_TABLE
            .lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
            .filter_map(|l| {
                let mut f = l.split('\t');
                let (id, line, hex, name) = (f.next()?, f.next()?, f.next()?, f.next()?);
                Some(Named { id, line, rgb: parse_hex(hex)?, name: name.trim() })
            })
            .collect()
    })
}

/// Common colour names for spools that aren't in Bambu's table.
const COMMON: &[(&str, &str)] = &[
    ("White", "FFFFFF"),
    ("Ivory", "F3EEDB"),
    ("Cream", "F3E5C0"),
    ("Beige", "D8C8A8"),
    ("Tan", "C2A27A"),
    ("Light Gray", "C8C8C8"),
    ("Gray", "8E8E8E"),
    ("Dark Gray", "4A4A4A"),
    ("Black", "111111"),
    ("Red", "D32F2F"),
    ("Burgundy", "7A1F2B"),
    ("Pink", "F4A6C0"),
    ("Pale Pink", "F9D0DA"),
    ("Hot Pink", "EC4E9A"),
    ("Magenta", "C2187C"),
    ("Coral", "F2735E"),
    ("Orange", "F57C20"),
    ("Peach", "F7B98B"),
    ("Yellow", "F7D117"),
    ("Pale Yellow", "F6E79C"),
    ("Gold", "C9A13B"),
    ("Brown", "6E4B2E"),
    ("Dark Brown", "4A2E1E"),
    ("Terracotta", "C2603F"),
    ("Olive", "7A7A2E"),
    ("Lime", "A6D12E"),
    ("Green", "2E9E4A"),
    ("Dark Green", "1F5E35"),
    ("Mint", "A8E0C2"),
    ("Sage Green", "9CAF88"),
    ("Teal", "128A86"),
    ("Turquoise", "3CC8C0"),
    ("Sky Blue", "7CC1EA"),
    ("Baby Blue", "B9D9F2"),
    ("Blue", "1F5FBF"),
    ("Royal Blue", "2A4BD7"),
    ("Navy", "1C2A4A"),
    ("Purple", "6B3FA0"),
    ("Lavender", "B9A6D9"),
    ("Plum", "6E2D5B"),
];

/// Within this CIE76 distance a Bambu colour of the same line counts as the same colour.
const SAME_LINE_DE: f32 = 8.0;

fn parse_hex(h: &str) -> Option<[u8; 3]> {
    let h = h.trim().trim_start_matches('#');
    if h.len() < 6 || !h.is_char_boundary(6) {
        return None;
    }
    let v = u32::from_str_radix(&h[..6], 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

fn lab(c: [u8; 3]) -> [f32; 3] {
    let lin = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (lin(c[0]), lin(c[1]), lin(c[2]));
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
    let f = |t: f32| if t > 0.008856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn distance(a: [u8; 3], b: [u8; 3]) -> f32 {
    let (a, b) = (lab(a), lab(b));
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// How far apart two hex colours look (CIE76 ΔE; about 2 is barely visible, 12 is plainly another
/// shade). `None` when either isn't a colour.
pub fn colour_distance(a: &str, b: &str) -> Option<f32> {
    Some(distance(parse_hex(a)?, parse_hex(b)?))
}

/// A Bambu Lab spool: by brand, or by a Bambu filament id (GFA00...).
pub fn is_bambu(brand: &str, filament_id: Option<&str>) -> bool {
    brand.to_ascii_lowercase().contains("bambu") || filament_id.is_some_and(|f| f.starts_with("GF"))
}

/// The colour's name: Bambu's for a Bambu spool of that line, else the nearest common name.
pub fn colour_name(color: &str, brand: &str, line: &str, filament_id: Option<&str>) -> String {
    let Some(rgb) = parse_hex(color) else { return color.trim().to_string() };
    if is_bambu(brand, filament_id) {
        let by_id: Vec<&Named> = filament_id.map(|f| bambu().iter().filter(|n| n.id.eq_ignore_ascii_case(f)).collect()).unwrap_or_default();
        let cands: Vec<&Named> = if by_id.is_empty() {
            let basic = format!("{line} Basic");
            bambu().iter().filter(|n| n.line.eq_ignore_ascii_case(line) || n.line.eq_ignore_ascii_case(&basic)).collect()
        } else {
            by_id
        };
        if let Some(n) = cands.iter().find(|n| n.rgb == rgb) {
            return n.name.to_string();
        }
        if let Some((n, d)) = cands.iter().map(|n| (n, distance(n.rgb, rgb))).min_by(|a, b| a.1.total_cmp(&b.1)) {
            if d <= SAME_LINE_DE {
                return n.name.to_string();
            }
        }
    }
    COMMON
        .iter()
        .filter_map(|(name, hex)| Some((*name, distance(parse_hex(hex)?, rgb))))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(n, _)| n.to_string())
        .unwrap_or_else(|| color.trim().to_string())
}

/// "Bambu PETG Translucent Pink": brand ("Bambu Lab" → "Bambu"), the product line (else the
/// material), and the colour name without the words the line already says.
pub fn auto_name(brand: &str, sub_brand: Option<&str>, material: &str, color: &str, filament_id: Option<&str>) -> String {
    let brand_raw = brand.trim();
    let brand_short = if brand_raw.eq_ignore_ascii_case("bambu lab") || brand_raw.eq_ignore_ascii_case("bambulab") { "Bambu" } else { brand_raw };
    let material = material.trim();
    let mut line = sub_brand.map(str::trim).filter(|s| !s.is_empty()).unwrap_or(material).to_string();
    let lower = |s: &str| s.to_lowercase();
    if !material.is_empty() && !lower(&line).contains(&lower(material)) {
        line = format!("{material} {line}");
    }
    // a line that already starts with the brand ("Bambu PLA Basic") keeps it once
    for b in [brand_raw, brand_short] {
        let n = b.len();
        if n > 0 && line.len() > n && line.is_char_boundary(n) && line[..n].eq_ignore_ascii_case(b) && line[n..].starts_with(' ') {
            line = line[n..].trim().to_string();
            break;
        }
    }
    let colour = colour_name(color, brand_raw, &line, filament_id);
    let line_words: Vec<String> = line.split_whitespace().map(lower).collect();
    let colour: Vec<&str> = colour.split_whitespace().filter(|w| !line_words.contains(&lower(w))).collect();
    [brand_short.to_string(), line, colour.join(" ")].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
}

/// Names the earlier versions gave by default (brand + line + hex), which become automatic.
pub fn is_old_default(name: &str, brand: &str, sub_brand: Option<&str>, material: &str, color: &str) -> bool {
    let line = sub_brand.filter(|s| !s.is_empty()).unwrap_or(material);
    let name = name.trim();
    name.is_empty() || name.eq_ignore_ascii_case(&format!("{brand} {line} {color}")) || name.eq_ignore_ascii_case(&format!("Bambu {line} {color}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_read_like_the_spool_label() {
        assert_eq!(auto_name("Bambu Lab", Some("PETG Basic"), "PETG", "#7F7E83", Some("GFG00")), "Bambu PETG Basic Gray");
        // "Translucent Pink" in a PETG Translucent line says Translucent once
        assert_eq!(auto_name("Bambu Lab", Some("PETG Translucent"), "PETG", "#F9C1BD", Some("GFG01")), "Bambu PETG Translucent Pink");
        assert_eq!(auto_name("Generic", None, "PETG", "#FFFFFF", None), "Generic PETG White");
        // Bambu's own names, also without a filament id (found by line)
        assert_eq!(auto_name("Bambu Lab", Some("PLA Basic"), "PLA", "#00AE42", None), "Bambu PLA Basic Bambu Green");
        // close to a Bambu colour of the line: that name; otherwise the nearest common name
        assert_eq!(auto_name("Bambu Lab", Some("PETG Basic"), "PETG", "#7E7E84", Some("GFG00")), "Bambu PETG Basic Gray");
        assert_eq!(auto_name("Sunlu", Some("Silk+"), "PLA", "#C9A23C", None), "Sunlu PLA Silk+ Gold");
        // a generic spool never takes a Bambu product colour name
        assert_eq!(colour_name("#00AE42", "Generic", "PLA", None), "Green");
        // no brand: just line and colour
        assert_eq!(auto_name("", None, "ASA", "#111111", None), "ASA Black");
        // a line that repeats the brand says it once; Bambu calls matte black "Charcoal"
        assert_eq!(auto_name("Bambu Lab", Some("Bambu PLA Matte"), "PLA", "#000000", None), "Bambu PLA Matte Charcoal");
    }

    #[test]
    fn old_default_names_are_recognised() {
        assert!(is_old_default("Bambu PETG Basic #7F7E83", "Bambu Lab", Some("PETG Basic"), "PETG", "#7F7E83"));
        assert!(is_old_default("Generic PETG #FFFFFF", "Generic", None, "PETG", "#FFFFFF"));
        assert!(is_old_default("", "Generic", None, "PETG", "#FFFFFF"));
        assert!(!is_old_default("Gift from Ana", "Generic", None, "PETG", "#FFFFFF"));
        assert!(bambu().len() > 300, "the colour table is embedded");
    }
}
