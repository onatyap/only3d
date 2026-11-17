//! How long prints really take. The printer watcher stamps each print's first layer, its pauses
//! and its finish; finished prints then teach two numbers per printer: the start sequence's
//! overhead (heating, levelling, purging) and how much slower (or faster) the printing itself is
//! than the slicer's estimate. Slices show "on the printer ≈ …" once there is data.

use crate::inventory::Db;

/// Watcher-side state for one printer.
#[derive(Default)]
pub struct Tracker {
    paused_since: Option<std::time::Instant>,
}

fn secs(iso: &str) -> Option<f64> {
    // "YYYY-MM-DDTHH:MM:SSZ" → seconds since the epoch (days from a civil date, no crate)
    let b = iso.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let n = |a: usize, l: usize| iso.get(a..a + l)?.parse::<i64>().ok();
    let (y, m, d, hh, mm, ss) = (n(0, 4)?, n(5, 2)?, n(8, 2)?, n(11, 2)?, n(14, 2)?, n(17, 2)?);
    let (y2, m2) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * m2 + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some((days * 86400 + hh * 3600 + mm * 60 + ss) as f64)
}

impl Tracker {
    /// Called by the printer watcher on every status change.
    pub fn observe(&mut self, printer: &str, a: &only3d_bambu::PrinterStatus, b: &only3d_bambu::PrinterStatus) {
        if cfg!(test) {
            return;
        }
        let Ok(db) = Db::open() else { return };
        let Ok(Some((id, _))) = db.running_print(printer) else { return };
        let now = crate::spools::now_iso();
        // the first layer: the start sequence is over
        if b.layer.is_some_and(|l| l >= 1) && a.layer.unwrap_or(0) < 1 {
            let _ = db.conn.execute("UPDATE prints SET first_layer_at=COALESCE(first_layer_at, ?2) WHERE id=?1", rusqlite::params![id, now]);
        }
        let (sa, sb) = (a.gcode_state.as_str(), b.gcode_state.as_str());
        if sb == "PAUSE" && sa != "PAUSE" {
            self.paused_since = Some(std::time::Instant::now());
        }
        if sa == "PAUSE" && sb != "PAUSE" {
            if let Some(t) = self.paused_since.take() {
                let _ = db.conn.execute("UPDATE prints SET paused_s=COALESCE(paused_s, 0) + ?2 WHERE id=?1", rusqlite::params![id, t.elapsed().as_secs_f64()]);
            }
        }
        if sb == "FINISH" && sa != "FINISH" {
            let _ = db.conn.execute("UPDATE prints SET finished_at=COALESCE(finished_at, ?2) WHERE id=?1", rusqlite::params![id, now]);
        }
    }
}

/// What finished prints taught about a printer.
#[derive(Clone, Debug, PartialEq)]
pub struct Correction {
    /// Seconds from sending to the first layer (heating, levelling, purging).
    pub overhead_s: f64,
    /// Real printing time ÷ the slicer's estimate.
    pub ratio: f64,
    /// Prints it is based on.
    pub prints: usize,
}

impl Correction {
    pub fn expected(&self, estimate_s: f64) -> f64 {
        self.overhead_s + estimate_s * self.ratio
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    if !n.is_multiple_of(2) {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// From (sent, first layer, finished, paused s, estimate s) rows of finished prints.
pub fn correction_from(rows: &[(String, String, String, f64, f64)]) -> Option<Correction> {
    let mut over = Vec::new();
    let mut ratio = Vec::new();
    for (sent, first, done, paused, est) in rows {
        let (Some(s0), Some(s1), Some(s2)) = (secs(sent), secs(first), secs(done)) else { continue };
        let printing = s2 - s1 - paused;
        if *est < 30.0 || printing <= 0.0 || s1 < s0 {
            continue;
        }
        over.push(s1 - s0);
        ratio.push(printing / est);
    }
    let n = ratio.len();
    (n > 0).then(|| Correction { overhead_s: median(over), ratio: median(ratio), prints: n })
}

/// The correction for a printer from its last 10 clean finished prints (none in unit tests).
pub fn correction(printer: &str) -> Option<Correction> {
    if cfg!(test) {
        return None;
    }
    let db = Db::open().ok()?;
    let rows = db
        .rows(
            "SELECT started_at, first_layer_at, finished_at, COALESCE(paused_s, 0) AS paused_s, est_time_s FROM prints
             WHERE printer=?1 AND state='FINISH' AND first_layer_at IS NOT NULL AND finished_at IS NOT NULL AND est_time_s IS NOT NULL
             ORDER BY started_at DESC LIMIT 10",
            &[&printer],
        )
        .ok()?;
    let rows: Vec<(String, String, String, f64, f64)> = rows
        .iter()
        .filter_map(|r| Some((r["started_at"].as_str()?.to_string(), r["first_layer_at"].as_str()?.to_string(), r["finished_at"].as_str()?.to_string(), r["paused_s"].as_f64().unwrap_or(0.0), r["est_time_s"].as_f64()?)))
        .collect();
    correction_from(&rows)
}

pub fn fmt_dur(s: f64) -> String {
    let m = (s / 60.0).round() as i64;
    if m >= 60 {
        format!("{}h {:02}m", m / 60, m % 60)
    } else {
        format!("{m} min")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn learns_overhead_and_ratio_from_clean_prints() {
        assert_eq!(secs("2026-11-15T09:14:13Z").unwrap() - secs("2026-11-15T09:00:00Z").unwrap(), 853.0);
        assert_eq!(secs("2026-01-01T00:00:00Z").unwrap() - secs("2025-12-31T23:59:00Z").unwrap(), 60.0);
        // three prints: 9-11 min of start sequence, printing 10 % slower than estimated; the
        // second was paused for 5 minutes, which doesn't count
        let rows = vec![
            ("2026-11-15T09:00:00Z".to_string(), "2026-11-15T09:10:00Z".to_string(), "2026-11-15T10:16:00Z".to_string(), 0.0, 3600.0),
            ("2026-11-15T11:00:00Z".to_string(), "2026-11-15T11:09:00Z".to_string(), "2026-11-15T11:47:00Z".to_string(), 300.0, 1800.0),
            ("2026-11-15T12:00:00Z".to_string(), "2026-11-15T12:11:00Z".to_string(), "2026-11-15T12:44:00Z".to_string(), 0.0, 1800.0),
        ];
        let c = correction_from(&rows).unwrap();
        assert_eq!(c.prints, 3);
        assert!((c.overhead_s - 600.0).abs() < 1.0, "{c:?}");
        assert!((c.ratio - 1.1).abs() < 0.01, "{c:?}");
        assert!((c.expected(3600.0) - (600.0 + 3960.0)).abs() < 1.0);
        assert_eq!(fmt_dur(4560.0), "1h 16m");
        assert!(correction_from(&[]).is_none());
    }
}
