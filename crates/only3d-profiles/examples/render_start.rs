//! Renders the H2D 0.4 start G-code for a single-filament job.
//!
//! ```text
//! cargo run -p only3d-profiles --example render_start -- [--left|--right] [--filament NAME]
//!     [--bed "Textured PEI Plate"] [--bbox MINX,MINY,MAXX,MAXY] [--max-z 20] [--layers 100]
//! ```

use only3d_profiles::job::{self, JobInfo, PrintSetup, H2D_LEFT, H2D_RIGHT};
use only3d_profiles::{BedType, PresetKind, ProfileStore, Template};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let extruder = if args.iter().any(|a| a == "--left") { H2D_LEFT } else { H2D_RIGHT };
    let filament = arg("--filament").unwrap_or_else(|| "Bambu PLA Basic @BBL H2D".into());
    let bed = arg("--bed").unwrap_or_else(|| "Textured PEI Plate".into());
    let bed = BedType::from_name(&bed).ok_or("unknown bed type")?;
    let bbox: Vec<f64> = arg("--bbox")
        .unwrap_or_else(|| "155,140,195,180".into())
        .split(',')
        .map(|s| s.trim().parse())
        .collect::<Result<_, _>>()?;
    let max_z: f64 = arg("--max-z").map_or(Ok(20.0), |s| s.parse())?;
    let layers: usize = arg("--layers").map_or(Ok(100), |s| s.parse())?;

    let store = if args.iter().any(|a| a == "--embedded") { ProfileStore::embedded()? } else { ProfileStore::system()? };
    let machine = store.resolve(PresetKind::Machine, "Bambu Lab H2D 0.4 nozzle")?;
    let process = store.resolve(PresetKind::Process, "0.20mm Standard @BBL H2D")?;
    let fil = store.resolve(PresetKind::Filament, &filament)?;
    let setup = PrintSetup::new(&machine, &process).with_filament(&fil, extruder);
    let job = JobInfo::new(bed, vec![0], [bbox[0], bbox[1]], [bbox[2], bbox[3]], max_z, layers);
    let ctx = job::build_print_context(&setup, &job)?;
    let t = Template::named("machine_start_gcode", machine.get_str("machine_start_gcode").unwrap_or(""))?;
    print!("{}", t.render(&ctx)?);
    Ok(())
}
