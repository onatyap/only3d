//! The timelapse videos on a Bambu printer's storage (`printer_files` with `timelapses`): what is
//! there, when each was recorded, its thumbnail, and the print it shows. `print` with
//! `timelapse: true` records one; `printer_file` downloads or deletes one.

use only3d_bambu::RemoteFile;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::inventory::Db;
use crate::printer::{bambu_err, ftps};
use crate::tools::{ToolError, ToolOutput};

/// Where Bambu firmware keeps the videos, and the picture of each beside them.
const FOLDER: &str = "/timelapse";
const THUMBNAILS: &str = "/timelapse/thumbnail";
/// Thumbnails one listing fetches at most: each is a transfer of its own.
pub const MAX_THUMBNAILS: usize = 12;

/// One video on the printer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Timelapse {
    pub name: String,
    /// The path `printer_file` takes.
    pub path: String,
    pub bytes: Option<u64>,
    /// When it was recorded, by the printer's clock ("2026-11-15 12:00:00"), from its name.
    pub recorded: Option<String>,
    /// The file's date as the printer lists it.
    pub modified: Option<String>,
    /// The thumbnail's path, when the printer keeps one.
    pub thumbnail: Option<String>,
    /// The print it shows (P-0007) and its job name: the one that was running at that time.
    pub print: Option<String>,
    pub job: Option<String>,
}

/// A printer's videos, newest first, and the thumbnails asked for.
pub struct Listing {
    pub printer: String,
    pub videos: Vec<Timelapse>,
    /// Video path → JPEG.
    pub thumbnails: Vec<(String, Vec<u8>)>,
}

/// List a printer's timelapses with the newest `thumbnails` pictures. Needs no session, so the
/// window asks from a thread of its own.
pub fn list(printer: Option<&str>, thumbnails: usize) -> Result<Listing, ToolError> {
    let (printer, mut ftp) = ftps(printer)?;
    // a printer that never recorded one has no such folder
    let mut dir = |path: &str| match ftp.list(path) {
        Err(e) if e.code() == "file_not_found" => Ok(Vec::new()),
        r => r.map_err(bambu_err),
    };
    let files = dir(FOLDER)?;
    let pictures = if files.is_empty() { Vec::new() } else { dir(THUMBNAILS).unwrap_or_default() };
    let mut videos = videos(&files, &pictures);
    let prints = prints(&printer);
    for v in &mut videos {
        if let Some(p) = v.recorded.as_deref().and_then(secs).and_then(|t| shown(&prints, t)) {
            (v.print, v.job) = (Some(p.id.clone()), Some(p.job.clone()));
        }
    }
    // a picture that can't be fetched is only a picture: the listing stands without it
    let thumbnails = videos
        .iter()
        .take(thumbnails.min(MAX_THUMBNAILS))
        .filter_map(|v| Some((v.path.clone(), ftp.retrieve(v.thumbnail.as_deref()?).ok()?)))
        .filter(|(_, jpg)| jpg.starts_with(&[0xFF, 0xD8]))
        .collect();
    let _ = ftp.quit();
    Ok(Listing { printer, videos, thumbnails })
}

/// `printer_files` with `timelapses`.
pub(crate) fn tool(printer: Option<&str>, thumbnails: usize) -> Result<ToolOutput, ToolError> {
    let l = list(printer, thumbnails)?;
    let text = if l.videos.is_empty() {
        format!("No timelapses on '{}'. print with timelapse: true records one, kept on the printer's USB drive or card (not those set to internal storage).", l.printer)
    } else {
        let lines: Vec<String> = l.videos.iter().map(line).collect();
        format!("{} timelapse(s) on '{}', newest first (printer_file downloads or deletes one):\n{}", l.videos.len(), l.printer, lines.join("\n"))
    };
    let mut out = ToolOutput::text(text).with_data(json!({"printer": l.printer, "timelapses": l.videos}));
    for (path, jpg) in l.thumbnails {
        out = out.with_jpeg(jpg, path);
    }
    Ok(out)
}

fn line(v: &Timelapse) -> String {
    let mut parts = vec![v.path.clone()];
    parts.extend(v.recorded.clone().or(v.modified.clone()));
    parts.extend(v.bytes.map(|b| format!("{:.1} MB", b as f64 / 1e6)));
    parts.extend(v.print.as_ref().map(|p| format!("{p} {}", v.job.as_deref().unwrap_or("")).trim_end().to_string()));
    parts.join(" · ")
}

fn stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(s, _)| s)
}

/// The videos among a folder's files, each with its picture, newest first.
fn videos(files: &[RemoteFile], pictures: &[RemoteFile]) -> Vec<Timelapse> {
    let is_video = |n: &str| n.rsplit_once('.').is_some_and(|(s, e)| !s.is_empty() && matches!(e.to_ascii_lowercase().as_str(), "mp4" | "avi"));
    let mut out: Vec<Timelapse> = files
        .iter()
        .filter(|f| !f.is_dir && is_video(&f.name))
        .map(|f| Timelapse {
            path: format!("{FOLDER}/{}", f.name),
            bytes: f.size,
            recorded: recorded(&f.name),
            modified: f.modified.clone(),
            thumbnail: pictures.iter().find(|p| !p.is_dir && stem(&p.name) == stem(&f.name)).map(|p| format!("{THUMBNAILS}/{}", p.name)),
            print: None,
            job: None,
            name: f.name.clone(),
        })
        .collect();
    // by the time in the name; a name without one goes last
    out.sort_by(|a, b| (&b.recorded, &b.name).cmp(&(&a.recorded, &a.name)));
    out
}

/// "video_2026-11-15_12-00-07.mp4" → "2026-11-15 12:00:07".
fn recorded(name: &str) -> Option<String> {
    const SHAPE: &[u8] = b"dddd-dd-dd_dd-dd-dd";
    let fits = |w: &&[u8]| w.iter().zip(SHAPE).all(|(c, s)| if *s == b'd' { c.is_ascii_digit() } else { c == s });
    let at = std::str::from_utf8(name.as_bytes().windows(SHAPE.len()).find(fits)?).ok()?;
    let t = format!("{} {}", &at[..10], at[11..].replace('-', ":"));
    secs(&t).map(|_| t)
}

/// Seconds of a "YYYY-MM-DD HH:MM:SS" clock reading, with no time zone, to compare readings of
/// the same clock.
fn secs(t: &str) -> Option<i64> {
    let n = |r: std::ops::Range<usize>| -> Option<i64> { t.get(r)?.parse().ok() };
    let (y, m, d, h, mi, s) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    if !((1..=12).contains(&m) && (1..=31).contains(&d) && h < 24 && mi < 60 && s < 60) {
        return None;
    }
    // days_from_civil (Howard Hinnant)
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146097 + doe - 719468) * 86400 + h * 3600 + mi * 60 + s)
}

/// A recorded print as a stretch of the local clock.
struct Span {
    id: String,
    job: String,
    from: i64,
    to: i64,
}

/// The prints recorded for a printer, on the local clock (the printer's clock, when both are
/// set to the same time zone: video names carry none). No inventory means no matches.
fn prints(printer: &str) -> Vec<Span> {
    let rows = Db::open().and_then(|db| spans(&db, printer)).unwrap_or_default();
    rows.iter().filter_map(span).collect()
}

fn spans(db: &Db, printer: &str) -> Result<Vec<Value>, ToolError> {
    db.rows(
        "SELECT id, job_name, est_time_s,
                datetime(started_at, 'localtime') AS started, datetime(ended_at, 'localtime') AS ended
         FROM prints WHERE state <> 'EXPORTED' AND printer = ?1 ORDER BY started_at DESC LIMIT 500",
        &[&printer],
    )
}

fn span(r: &Value) -> Option<Span> {
    let from = r["started"].as_str().and_then(secs)?;
    // not seen ending: the estimate, with room for heating, pauses and a slow print
    let guess = from + (r["est_time_s"].as_f64().unwrap_or(0.0) * 1.5) as i64 + 2 * 3600;
    let to = r["ended"].as_str().and_then(secs).unwrap_or(guess);
    Some(Span { id: r["id"].as_str()?.to_string(), job: r["job_name"].as_str().unwrap_or("").to_string(), from, to })
}

/// The print running at `t`: the latest one to start, with a few minutes' slack at both ends for
/// clocks that differ and for a video named once the print is over.
fn shown(prints: &[Span], t: i64) -> Option<&Span> {
    prints.iter().filter(|p| p.from - 120 <= t && t <= p.to + 600).max_by_key(|p| p.from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, size: u64) -> RemoteFile {
        RemoteFile { name: name.into(), size: Some(size), is_dir: false, modified: Some("Nov 15 12:40".into()) }
    }

    #[test]
    fn lists_videos_newest_first_with_their_pictures() {
        let dir = RemoteFile { is_dir: true, ..file("thumbnail", 4096) };
        let files = [file("video_2026-11-14_09-30-00.mp4", 2_000_000), dir, file("video_2026-11-15_12-00-07.mp4", 14_200_000), file("my clip.avi", 10), file("notes.txt", 1), file(".mp4", 1)];
        let v = videos(&files, &[file("video_2026-11-15_12-00-07.jpg", 900)]);
        assert_eq!(v.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(), ["video_2026-11-15_12-00-07.mp4", "video_2026-11-14_09-30-00.mp4", "my clip.avi"]);
        assert_eq!(v[0].path, "/timelapse/video_2026-11-15_12-00-07.mp4");
        assert_eq!(v[0].recorded.as_deref(), Some("2026-11-15 12:00:07"));
        assert_eq!(v[0].thumbnail.as_deref(), Some("/timelapse/thumbnail/video_2026-11-15_12-00-07.jpg"));
        assert!(v[1].thumbnail.is_none() && v[2].recorded.is_none());
        assert_eq!(line(&v[0]), "/timelapse/video_2026-11-15_12-00-07.mp4 · 2026-11-15 12:00:07 · 14.2 MB");
        assert_eq!(line(&v[2]), "/timelapse/my clip.avi · Nov 15 12:40 · 0.0 MB");
    }

    #[test]
    fn a_name_is_dated_only_by_a_real_time() {
        assert_eq!(recorded("video_2026-13-15_12-00-07.mp4"), None);
        assert_eq!(recorded("vidéo_2026-11-15_12-00-07_p1.avi").as_deref(), Some("2026-11-15 12:00:07"));
        assert_eq!(recorded("2026-11-15.mp4"), None);
        assert_eq!(secs("1970-01-02 00:00:01"), Some(86401));
    }

    #[test]
    fn a_video_belongs_to_the_print_running_then() {
        let row = |id: &str, started: &str, ended: Option<&str>| span(&json!({"id": id, "job_name": "pot", "started": started, "ended": ended, "est_time_s": 3600.0})).unwrap();
        let prints = [row("P-0003", "2026-11-15 14:00:00", None), row("P-0002", "2026-11-15 12:00:00", Some("2026-11-15 13:00:00")), row("P-0001", "2026-11-14 09:00:00", Some("2026-11-14 10:00:00"))];
        let at = |t: &str| shown(&prints, secs(t).unwrap()).map(|p| p.id.as_str());
        assert_eq!(at("2026-11-15 12:06:00"), Some("P-0002"));
        assert_eq!(at("2026-11-15 13:05:00"), Some("P-0002"), "named as it ended");
        assert_eq!(at("2026-11-15 13:30:00"), None, "nothing ran then");
        assert_eq!(at("2026-11-15 15:00:00"), Some("P-0003"), "still running");
        assert_eq!(at("2026-11-16 09:00:00"), None, "never seen ending, but long over");
    }

    #[test]
    fn prints_come_from_the_inventory_for_that_printer_only() {
        let dir = std::env::temp_dir().join(format!("only3d-timelapse-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open_at(&dir.join("inventory.db")).unwrap();
        for (id, printer, state) in [("P-0001", "h2d", "FINISH"), ("P-0002", "x1c", "FINISH"), ("P-0003", "h2d", "EXPORTED")] {
            db.conn.execute("INSERT INTO prints (id, printer, job_name, source, started_at, state) VALUES (?1,?2,'job','only3d','2026-11-20T00:00:00Z',?3)", [id, printer, state]).unwrap();
        }
        let rows = spans(&db, "h2d").unwrap();
        assert_eq!(rows.iter().map(|r| r["id"].as_str().unwrap()).collect::<Vec<_>>(), ["P-0001"]);
        assert!(span(&rows[0]).is_some(), "{rows:?}");
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }
}
