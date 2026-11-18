//! The printer registry (profiles/printers.json, published as docs/printers.md): how far only3d
//! is tested on each printer. `print` reads it to warn before a print on a printer nobody has
//! printed on with only3d yet.

use serde::Deserialize;

const REGISTRY: &str = include_str!("../../../profiles/printers.json");

#[derive(Deserialize)]
struct Registry {
    printers: Vec<Printer>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Printer {
    /// The model as printers report it ("Bambu Lab X1 Carbon").
    pub name: String,
    pub slice: Step,
    pub export: Step,
    pub print: Step,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Step {
    /// "tested", "checked", "untested" or "no".
    pub status: String,
    #[serde(default)]
    pub note: String,
}

/// The registry's entry for printer model `model`.
pub fn find(model: &str) -> Option<Printer> {
    let reg: Registry = serde_json::from_str(REGISTRY).ok()?;
    reg.printers.into_iter().find(|p| p.name.eq_ignore_ascii_case(model))
}

/// What to tell whoever approves a print on `model` when only3d hasn't printed on one yet.
pub fn print_note(model: &str) -> Option<String> {
    let how = match find(model) {
        Some(p) if p.print.status == "tested" => return None,
        Some(p) => format!("its G-code is {} against Bambu Studio's", if p.slice.status == "checked" { "checked" } else { "not yet compared" }),
        None => "it isn't in only3d's printer list".into(),
    };
    Some(format!(
        "Untested printer: nobody has printed on a {model} with only3d yet ({how}). Watch the first layer, and please report how it went (docs/printers.md)."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_reads_and_warns_only_where_nobody_printed() {
        let h2d = find("Bambu Lab H2D").expect("the H2D is listed");
        assert_eq!(h2d.print.status, "tested");
        assert!(print_note("Bambu Lab H2D").is_none());
        let note = print_note("Bambu Lab Q9").unwrap();
        assert!(note.contains("Untested printer") && note.contains("Q9"), "{note}");
    }
}
