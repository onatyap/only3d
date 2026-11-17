//! Lists the variables each G-code template of a machine preset references.
//!
//! ```text
//! cargo run -p only3d-profiles --example list_variables -- ["Bambu Lab H2D 0.4 nozzle"] [--embedded]
//! ```

use only3d_profiles::{PresetKind, ProfileStore, Template, GCODE_TEMPLATE_KEYS};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let name = args.iter().find(|a| !a.starts_with("--")).cloned().unwrap_or_else(|| "Bambu Lab H2D 0.4 nozzle".into());
    let store = if args.iter().any(|a| a == "--embedded") { ProfileStore::embedded()? } else { ProfileStore::system()? };
    let machine = store.resolve(PresetKind::Machine, &name)?;
    for key in GCODE_TEMPLATE_KEYS {
        let Some(src) = machine.get_str(key).filter(|s| !s.trim().is_empty()) else { continue };
        let t = Template::named(*key, src)?;
        println!("{key}: {}", t.required_variables().join(" "));
    }
    Ok(())
}
