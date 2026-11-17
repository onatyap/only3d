//! Prints a fully resolved Bambu preset (inherits + include + own keys) as one flat JSON
//! object, usable with `BambuStudio --load-settings` / `--load-filaments`.
//!
//! ```text
//! cargo run -p only3d-profiles --example dump_preset -- machine "Bambu Lab H2D 0.4 nozzle"
//! cargo run -p only3d-profiles --example dump_preset -- filament "Bambu PLA Basic @BBL H2D" --embedded
//! ```

use only3d_profiles::{PresetKind, ProfileStore};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(kind), Some(name)) = (args.first(), args.get(1)) else {
        eprintln!("usage: dump_preset <machine|process|filament> <preset name> [--embedded]");
        std::process::exit(2);
    };
    let kind = match kind.as_str() {
        "machine" => PresetKind::Machine,
        "process" => PresetKind::Process,
        "filament" => PresetKind::Filament,
        other => return Err(format!("unknown preset kind {other}").into()),
    };
    let store = if args.iter().any(|a| a == "--embedded") { ProfileStore::embedded()? } else { ProfileStore::system()? };
    let mut config = store.resolve(kind, name)?;
    // Identity keys Bambu Studio's CLI expects on a flattened (parent-less) preset.
    config.insert("type", kind.as_str());
    config.insert("name", name.as_str());
    config.insert("from", "system");
    match kind {
        PresetKind::Machine => config.insert("printer_settings_id", name.as_str()),
        PresetKind::Process => config.insert("print_settings_id", name.as_str()),
        _ => config.insert("filament_settings_id", vec![name.to_string()]),
    }
    eprintln!("source: {:?}", store.source());
    println!("{}", serde_json::to_string_pretty(&config.to_json())?);
    Ok(())
}
