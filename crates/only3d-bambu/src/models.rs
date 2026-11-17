//! Printer model codes.
//!
//! SSDP `DevModel.bambu.com` and Bambu Studio's printer profiles use the
//! *model id* (`O1D` = H2D). Source: `resources/printers/<model_id>.json` in
//! github.com/bambulab/BambuStudio (each file's `model_id`, `printer_type`,
//! `display_name`). Older X1 firmware announces the `printer_type` string
//! (`3DPrinter-X1-Carbon`) instead, so both are accepted.

/// Static facts about one printer model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelInfo {
    /// Canonical model id (`O1D`).
    pub code: &'static str,
    /// Other strings printers or tools use for the same model.
    pub aliases: &'static [&'static str],
    /// Marketing name (`Bambu Lab H2D`).
    pub name: &'static str,
    /// Short name (`H2D`).
    pub short: &'static str,
    /// Number of independently heated extruders/nozzles.
    pub nozzles: u8,
    /// Whether `project_file` takes `file:///sdcard/<name>` instead of
    /// `ftp://<name>`. Per ha-bambulab `const.py` `LEGACY_SDCARD_PRINTERS`
    /// (X1/X1C/X1E/P1P/P1S/A1/A1 mini); every newer model, including the H2
    /// family, uses the `ftp://` form.
    pub legacy_sdcard_url: bool,
}

impl ModelInfo {
    pub fn is_dual_nozzle(&self) -> bool {
        self.nozzles >= 2
    }
}

const fn m(
    code: &'static str,
    aliases: &'static [&'static str],
    name: &'static str,
    short: &'static str,
    nozzles: u8,
    legacy_sdcard_url: bool,
) -> ModelInfo {
    ModelInfo {
        code,
        aliases,
        name,
        short,
        nozzles,
        legacy_sdcard_url,
    }
}

/// All known models.
pub const MODELS: &[ModelInfo] = &[
    m("BL-P001", &["3DPrinter-X1-Carbon", "X1C", "X1 Carbon"], "Bambu Lab X1 Carbon", "X1C", 1, true),
    m("BL-P002", &["3DPrinter-X1", "X1"], "Bambu Lab X1", "X1", 1, true),
    m("C13", &["X1E"], "Bambu Lab X1E", "X1E", 1, true),
    m("C11", &["P1P"], "Bambu Lab P1P", "P1P", 1, true),
    m("C12", &["P1S"], "Bambu Lab P1S", "P1S", 1, true),
    m("N1", &["A1MINI", "A1 mini", "A1 Mini"], "Bambu Lab A1 mini", "A1 mini", 1, true),
    m("N2S", &["A1"], "Bambu Lab A1", "A1", 1, true),
    m("N7", &["P2S"], "Bambu Lab P2S", "P2S", 1, false),
    m("N9", &["A2L"], "Bambu Lab A2L", "A2L", 1, false),
    m("N10", &["H3C"], "Bambu Lab H3C", "H3C", 1, false),
    m("N6", &["X2D"], "Bambu Lab X2D", "X2D", 2, false),
    m("O1D", &["H2D"], "Bambu Lab H2D", "H2D", 2, false),
    m("O1E", &["H2DPRO", "H2D Pro"], "Bambu Lab H2D Pro", "H2D Pro", 2, false),
    m("O1S", &["H2S"], "Bambu Lab H2S", "H2S", 1, false),
    // H2C: dual extruder with a hotend rack on the left side.
    m("O1C", &["H2C"], "Bambu Lab H2C", "H2C", 2, false),
    m("O1C2", &[], "Bambu Lab H2C", "H2C", 2, false),
];

/// Look up a model by code, alias, short name or full name (case-insensitive).
pub fn lookup(code_or_name: &str) -> Option<&'static ModelInfo> {
    let q = code_or_name.trim();
    MODELS.iter().find(|mi| {
        mi.code.eq_ignore_ascii_case(q)
            || mi.short.eq_ignore_ascii_case(q)
            || mi.name.eq_ignore_ascii_case(q)
            || mi.aliases.iter().any(|a| a.eq_ignore_ascii_case(q))
    })
}

/// Human name for a model code, e.g. `model_name("O1D") == Some("Bambu Lab H2D")`.
pub fn model_name(code: &str) -> Option<&'static str> {
    lookup(code).map(|mi| mi.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h2d_is_o1d_and_dual_nozzle() {
        let h2d = lookup("O1D").unwrap();
        assert_eq!(h2d.short, "H2D");
        assert!(h2d.is_dual_nozzle());
        assert!(!h2d.legacy_sdcard_url);
        assert_eq!(lookup("h2d").unwrap().code, "O1D");
    }

    #[test]
    fn legacy_codes() {
        assert_eq!(model_name("BL-P001"), Some("Bambu Lab X1 Carbon"));
        assert_eq!(lookup("3DPrinter-X1-Carbon").unwrap().code, "BL-P001");
        assert_eq!(lookup("C12").unwrap().short, "P1S");
        assert_eq!(lookup("N2S").unwrap().short, "A1");
        assert!(lookup("C12").unwrap().legacy_sdcard_url);
        assert!(lookup("nope").is_none());
    }
}
