//! The inventory database (`~/.only3d/only3d.db`, SQLite, embedded).
//!
//! Every gram is traceable: a spool's remaining weight is the sum of its **ledger**
//! (`spool_events`), and every event caused by a print points at that print. A print points at the
//! exact **project version** (a content-addressed snapshot of the project document) and lists its
//! objects with their estimated grams, so "where did this spool go?" resolves down to objects of
//! a specific design revision.
//!
//! ```text
//! spools ──< spool_events >── prints ──> project_versions ──> projects
//!                               ├──< print_objects   (object id, grams, Bambu identify_id)
//!                               └──< print_filaments (slot → spool, tray)
//! ```

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::ToolError;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS spools (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    material TEXT NOT NULL,
    brand TEXT NOT NULL DEFAULT '',
    sub_brand TEXT,
    color TEXT NOT NULL,
    preset TEXT,
    filament_id TEXT,
    diameter_mm REAL NOT NULL DEFAULT 1.75,
    net_weight_g REAL NOT NULL,
    tare_g REAL,
    cost_per_kg REAL,
    rfid_uuid TEXT UNIQUE,
    location TEXT,
    added_at TEXT NOT NULL,
    archived INTEGER NOT NULL DEFAULT 0,
    notes TEXT NOT NULL DEFAULT '',
    flow_ratio REAL,
    pressure_advance REAL,
    max_vol_speed REAL,
    name_auto INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    path TEXT,
    first_seen TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS project_versions (
    id INTEGER PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    reason TEXT NOT NULL DEFAULT '',
    document TEXT NOT NULL,
    UNIQUE(project_id, hash)
);
CREATE TABLE IF NOT EXISTS prints (
    id TEXT PRIMARY KEY,
    project_version_id INTEGER REFERENCES project_versions(id),
    printer TEXT,
    printer_task_id TEXT,
    job_name TEXT NOT NULL,
    remote_file TEXT,
    gcode_md5 TEXT,
    source TEXT NOT NULL,
    engine TEXT,
    settings TEXT,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    state TEXT NOT NULL,
    est_time_s REAL,
    est_grams REAL,
    notes TEXT NOT NULL DEFAULT '',
    first_layer_at TEXT,
    finished_at TEXT,
    paused_s REAL
);
CREATE UNIQUE INDEX IF NOT EXISTS prints_task ON prints(printer, printer_task_id) WHERE printer_task_id IS NOT NULL;
CREATE TABLE IF NOT EXISTS print_objects (
    print_id TEXT NOT NULL REFERENCES prints(id),
    object_id TEXT NOT NULL,
    object_name TEXT,
    filament_slot INTEGER,
    identify_id INTEGER,
    est_grams REAL,
    status TEXT NOT NULL DEFAULT 'planned',
    PRIMARY KEY (print_id, object_id)
);
CREATE TABLE IF NOT EXISTS print_filaments (
    print_id TEXT NOT NULL REFERENCES prints(id),
    slot INTEGER NOT NULL,
    spool_id TEXT REFERENCES spools(id),
    tray TEXT,
    est_grams REAL,
    PRIMARY KEY (print_id, slot)
);
CREATE TABLE IF NOT EXISTS spool_events (
    id INTEGER PRIMARY KEY,
    spool_id TEXT NOT NULL REFERENCES spools(id),
    at TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('add','print','ams','weigh','manual')),
    delta_g REAL NOT NULL,
    print_id TEXT REFERENCES prints(id),
    note TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS spool_events_spool ON spool_events(spool_id, id);
CREATE INDEX IF NOT EXISTS spool_events_print ON spool_events(print_id);
-- small key/value store, e.g. the highest print number ever handed out
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- fits learned from calibration coupons, per printer and filament: extra mm on holes
CREATE TABLE IF NOT EXISTS fits (
    printer TEXT NOT NULL,
    filament TEXT NOT NULL,
    fit TEXT NOT NULL CHECK (fit IN ('slip', 'press')),
    offset REAL NOT NULL,
    nominal REAL,
    at TEXT NOT NULL,
    note TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (printer, filament, fit)
);
CREATE VIEW IF NOT EXISTS spool_stock AS
    SELECT s.*, COALESCE((SELECT SUM(e.delta_g) FROM spool_events e WHERE e.spool_id = s.id), 0) AS remaining_g
    FROM spools s;
CREATE VIEW IF NOT EXISTS print_usage AS
    SELECT e.print_id, e.spool_id, -SUM(e.delta_g) AS grams
    FROM spool_events e WHERE e.print_id IS NOT NULL GROUP BY e.print_id, e.spool_id;
"#;

pub fn db_path() -> PathBuf {
    crate::config_dir().join("only3d.db")
}

fn dberr(e: rusqlite::Error) -> ToolError {
    ToolError::new("database_error", e.to_string(), "the inventory database could not be read or written")
}

pub struct Db {
    pub conn: Connection,
}

impl Db {
    pub fn open() -> Result<Db, ToolError> {
        Self::open_at(&db_path())
    }

    pub fn open_at(path: &Path) -> Result<Db, ToolError> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| ToolError::new("io_error", e.to_string(), ""))?;
        }
        let conn = Connection::open(path).map_err(dberr)?;
        conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(dberr)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;").map_err(dberr)?;
        conn.execute_batch(SCHEMA).map_err(dberr)?;
        let db = Db { conn };
        db.migrate()?;
        db.import_legacy_json(path)?;
        Ok(db)
    }

    /// Columns added after a database was first made.
    fn migrate(&self) -> Result<(), ToolError> {
        let cols: Vec<String> = self.conn.prepare("PRAGMA table_info(spools)").and_then(|mut st| st.query_map([], |r| r.get::<_, String>(1))?.collect()).map_err(dberr)?;
        for (col, ty) in [("flow_ratio", "REAL"), ("pressure_advance", "REAL"), ("max_vol_speed", "REAL")] {
            if !cols.iter().any(|c| c == col) {
                self.conn.execute_batch(&format!("ALTER TABLE spools ADD COLUMN {col} {ty}")).map_err(dberr)?;
            }
        }
        if !cols.iter().any(|c| c == "name_auto") {
            self.conn.execute_batch("ALTER TABLE spools ADD COLUMN name_auto INTEGER NOT NULL DEFAULT 0").map_err(dberr)?;
            // names earlier versions made up (brand + line + hex) become automatic
            for sp in self.spools(true)? {
                if crate::spool_names::is_old_default(&sp.name, &sp.brand, sp.sub_brand.as_deref(), &sp.material, &sp.color) {
                    self.conn.execute("UPDATE spools SET name_auto=1 WHERE id=?1", [&sp.id]).map_err(dberr)?;
                    self.refresh_auto_name(&sp.id)?;
                }
            }
        }
        let pcols: Vec<String> = self.conn.prepare("PRAGMA table_info(prints)").and_then(|mut st| st.query_map([], |r| r.get::<_, String>(1))?.collect()).map_err(dberr)?;
        for (col, ty) in [("first_layer_at", "TEXT"), ("finished_at", "TEXT"), ("paused_s", "REAL")] {
            if !pcols.iter().any(|c| c == col) {
                self.conn.execute_batch(&format!("ALTER TABLE prints ADD COLUMN {col} {ty}")).map_err(dberr)?;
            }
        }
        Ok(())
    }

    /// A read-only connection for agent SQL queries.
    pub fn open_read_only() -> Result<Connection, ToolError> {
        let p = db_path();
        if !p.exists() {
            Db::open()?;
        }
        Connection::open_with_flags(&p, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(dberr)
    }

    /// One-time import of the earlier `spools.json` inventory (renamed afterwards).
    fn import_legacy_json(&self, db_path: &Path) -> Result<(), ToolError> {
        let json_path = db_path.with_file_name("spools.json");
        let Ok(text) = std::fs::read_to_string(&json_path) else { return Ok(()) };
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let tx = self.conn.unchecked_transaction().map_err(dberr)?;
        for sp in v.get("spools").and_then(|s| s.as_array()).into_iter().flatten() {
            let s = |k: &str| sp.get(k).and_then(|x| x.as_str()).map(String::from);
            let f = |k: &str| sp.get(k).and_then(|x| x.as_f64());
            let Some(id) = s("id") else { continue };
            tx.execute(
                "INSERT OR IGNORE INTO spools (id,name,material,brand,sub_brand,color,preset,filament_id,diameter_mm,net_weight_g,tare_g,cost_per_kg,rfid_uuid,location,added_at,archived,notes)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                params![
                    id,
                    s("name").unwrap_or_default(),
                    s("material").unwrap_or_default(),
                    s("brand").unwrap_or_default(),
                    s("sub_brand"),
                    s("color").unwrap_or_else(|| "#FFFFFF".into()),
                    s("preset"),
                    s("filament_id"),
                    f("diameter_mm").unwrap_or(1.75),
                    f("net_weight_g").unwrap_or(1000.0),
                    f("tare_g"),
                    f("cost_per_kg"),
                    s("rfid_uuid"),
                    s("location"),
                    s("added").unwrap_or_else(crate::spools::now_iso),
                    sp.get("archived").and_then(|x| x.as_bool()).unwrap_or(false) as i64,
                    s("notes").unwrap_or_default(),
                ],
            )
            .map_err(dberr)?;
            for ev in sp.get("history").and_then(|h| h.as_array()).into_iter().flatten() {
                let kind = ev.get("source").and_then(|x| x.as_str()).unwrap_or("manual");
                let kind = if ["add", "print", "ams", "weigh", "manual"].contains(&kind) { kind } else { "manual" };
                tx.execute(
                    "INSERT INTO spool_events (spool_id, at, kind, delta_g, note) VALUES (?1,?2,?3,?4,?5)",
                    params![id, ev.get("at").and_then(|x| x.as_str()).unwrap_or(""), kind, ev.get("delta_g").and_then(|x| x.as_f64()).unwrap_or(0.0), ev.get("note").and_then(|x| x.as_str()).unwrap_or("")],
                )
                .map_err(dberr)?;
            }
        }
        for p in v.get("prints").and_then(|s| s.as_array()).into_iter().flatten() {
            let id = next_print_id(&tx)?;
            tx.execute(
                "INSERT INTO prints (id, job_name, source, started_at, ended_at, state, printer) VALUES (?1,?2,?3,?4,?5,?6,'h2d')",
                params![id, p.get("name").and_then(|x| x.as_str()).unwrap_or(""), p.get("source").and_then(|x| x.as_str()).unwrap_or("printer"), p.get("started").and_then(|x| x.as_str()).unwrap_or(""), p.get("ended").and_then(|x| x.as_str()), p.get("state").and_then(|x| x.as_str()).unwrap_or("UNKNOWN")],
            )
            .map_err(dberr)?;
            for (slot, sp) in p.get("spools").and_then(|s| s.as_array()).into_iter().flatten().enumerate() {
                tx.execute("INSERT OR IGNORE INTO print_filaments (print_id, slot, spool_id) VALUES (?1,?2,?3)", params![id, slot as i64 + 1, sp.as_str()]).map_err(dberr)?;
            }
        }
        tx.commit().map_err(dberr)?;
        let _ = std::fs::rename(&json_path, json_path.with_extension("json.imported"));
        Ok(())
    }

    // -------------------------------------------------------------------------------------
    // spools

    pub fn remaining(&self, spool_id: &str) -> Result<f64, ToolError> {
        self.conn.query_row("SELECT COALESCE(SUM(delta_g),0) FROM spool_events WHERE spool_id=?1", [spool_id], |r| r.get(0)).map_err(dberr)
    }

    /// Append a ledger entry. Returns the new remaining weight.
    pub fn event(&self, spool_id: &str, kind: &str, delta_g: f64, print_id: Option<&str>, note: &str) -> Result<f64, ToolError> {
        let current = self.remaining(spool_id)?;
        // never let the ledger go below zero
        let delta = if current + delta_g < 0.0 { -current } else { delta_g };
        self.conn
            .execute(
                "INSERT INTO spool_events (spool_id, at, kind, delta_g, print_id, note) VALUES (?1,?2,?3,?4,?5,?6)",
                params![spool_id, crate::spools::now_iso(), kind, (delta * 10.0).round() / 10.0, print_id, note],
            )
            .map_err(dberr)?;
        Ok(current + delta)
    }

    pub fn spool_exists(&self, id: &str) -> Result<bool, ToolError> {
        self.conn.query_row("SELECT COUNT(*) FROM spools WHERE id=?1", [id], |r| r.get::<_, i64>(0)).map(|n| n > 0).map_err(dberr)
    }

    pub fn unique_spool_id(&self, base: &str) -> Result<String, ToolError> {
        let base: String = base.to_ascii_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect::<String>().split('-').filter(|x| !x.is_empty()).collect::<Vec<_>>().join("-");
        let base = if base.is_empty() { "spool".to_string() } else { base };
        for i in 1.. {
            let c = format!("{base}-{i}");
            if !self.spool_exists(&c)? {
                return Ok(c);
            }
        }
        unreachable!()
    }

    /// Put a spool at a location, moving whatever was there to the shelf.
    pub fn place(&self, id: &str, loc: &str) -> Result<(), ToolError> {
        self.conn.execute("UPDATE spools SET location='shelf' WHERE location=?1 AND id<>?2", params![loc, id]).map_err(dberr)?;
        self.conn.execute("UPDATE spools SET location=?1 WHERE id=?2", params![loc, id]).map_err(dberr)?;
        Ok(())
    }

    pub fn spools(&self, include_archived: bool) -> Result<Vec<crate::spools::Spool>, ToolError> {
        let mut st = self
            .conn
            .prepare("SELECT s.id,s.name,s.material,s.brand,s.sub_brand,s.color,s.preset,s.filament_id,s.diameter_mm,s.net_weight_g,s.tare_g,s.cost_per_kg,s.rfid_uuid,s.location,s.added_at,s.archived,s.notes,st.remaining_g,s.flow_ratio,s.pressure_advance,s.max_vol_speed,s.name_auto FROM spools s JOIN spool_stock st ON st.id=s.id WHERE (?1 OR s.archived=0) ORDER BY s.archived, s.material, s.id")
            .map_err(dberr)?;
        let rows = st
            .query_map([include_archived], |r| {
                Ok(crate::spools::Spool {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    name_auto: r.get::<_, i64>(21)? != 0,
                    material: r.get(2)?,
                    brand: r.get(3)?,
                    sub_brand: r.get(4)?,
                    color: r.get(5)?,
                    preset: r.get(6)?,
                    filament_id: r.get(7)?,
                    diameter_mm: r.get(8)?,
                    net_weight_g: r.get(9)?,
                    tare_g: r.get(10)?,
                    cost_per_kg: r.get(11)?,
                    rfid_uuid: r.get(12)?,
                    location: r.get(13)?,
                    added: r.get(14)?,
                    archived: r.get::<_, i64>(15)? != 0,
                    notes: r.get(16)?,
                    remaining_g: r.get(17)?,
                    calibration: crate::spools::Calibration { flow_ratio: r.get(18)?, pressure_advance: r.get(19)?, max_volumetric_speed: r.get(20)? },
                    last_used: None,
                    history: Vec::new(),
                })
            })
            .map_err(dberr)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(dberr)
    }

    pub fn spool(&self, id: &str) -> Result<Option<crate::spools::Spool>, ToolError> {
        Ok(self.spools(true)?.into_iter().find(|s| s.id == id))
    }

    pub fn insert_spool(&self, s: &crate::spools::Spool) -> Result<(), ToolError> {
        self.conn
            .execute(
                "INSERT INTO spools (id,name,material,brand,sub_brand,color,preset,filament_id,diameter_mm,net_weight_g,tare_g,cost_per_kg,rfid_uuid,location,added_at,archived,notes,name_auto)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,NULL,?14,0,?15,?16)",
                params![s.id, s.name, s.material, s.brand, s.sub_brand, s.color, s.preset, s.filament_id, s.diameter_mm, s.net_weight_g, s.tare_g, s.cost_per_kg, s.rfid_uuid, s.added, s.notes, s.name_auto as i64],
            )
            .map_err(dberr)?;
        Ok(())
    }

    /// Recompute an automatic name after its brand, line, material or colour changed.
    pub fn refresh_auto_name(&self, id: &str) -> Result<(), ToolError> {
        if let Some(sp) = self.spool(id)?.filter(|s| s.name_auto) {
            let name = sp.auto_name();
            if name != sp.name {
                self.conn.execute("UPDATE spools SET name=?2 WHERE id=?1", params![id, name]).map_err(dberr)?;
            }
        }
        Ok(())
    }

    // -------------------------------------------------------------------------------------
    // projects and prints

    /// Snapshot a project document; returns the version id (existing one if unchanged).
    pub fn project_version(&self, project: &only3d_model::Project, path: Option<&Path>, reason: &str) -> Result<i64, ToolError> {
        let doc = serde_json::to_string(project).unwrap_or_default();
        let hash: String = Sha256::digest(doc.as_bytes()).iter().take(12).map(|b| format!("{b:02x}")).collect();
        self.conn
            .execute(
                "INSERT INTO projects (id, name, path, first_seen) VALUES (?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET name=excluded.name, path=COALESCE(excluded.path, projects.path)",
                params![project.id, project.name, path.map(|p| p.display().to_string()), crate::spools::now_iso()],
            )
            .map_err(dberr)?;
        if let Some(id) = self.conn.query_row("SELECT id FROM project_versions WHERE project_id=?1 AND hash=?2", params![project.id, hash], |r| r.get(0)).optional().map_err(dberr)? {
            return Ok(id);
        }
        self.conn
            .execute("INSERT INTO project_versions (project_id, hash, created_at, reason, document) VALUES (?1,?2,?3,?4,?5)", params![project.id, hash, crate::spools::now_iso(), reason, doc])
            .map_err(dberr)?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Project id previously associated with a file path.
    pub fn project_id_for_path(&self, path: &Path) -> Result<Option<String>, ToolError> {
        self.conn.query_row("SELECT id FROM projects WHERE path=?1", [path.display().to_string()], |r| r.get(0)).optional().map_err(dberr)
    }

    pub fn new_print_id(&self) -> Result<String, ToolError> {
        next_print_id(&self.conn)
    }

    /// Delete a print record with its objects and slots. Refused while the print is running or
    /// once it has charged filament to a spool (the ledger must stay whole).
    pub fn delete_print(&self, id: &str) -> Result<String, ToolError> {
        let row: Option<(String, Option<String>)> = self
            .conn
            .query_row("SELECT state, ended_at FROM prints WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .map_err(dberr)?;
        let Some((state, ended)) = row else {
            return Err(ToolError::new("not_found", format!("no print '{id}'"), "see print_history"));
        };
        if ended.is_none() && matches!(state.as_str(), "RUNNING" | "PAUSE" | "PREPARE" | "SLICING") {
            return Err(ToolError::new("print_active", format!("{id} is {state}"), "wait until it ends, or record its outcome with print_update"));
        }
        let charged: i64 = self.conn.query_row("SELECT COUNT(*) FROM spool_events WHERE print_id=?1", [id], |r| r.get(0)).map_err(dberr)?;
        if charged > 0 {
            return Err(ToolError::new(
                "print_has_usage",
                format!("{id} charged filament to spools ({charged} ledger entr(ies))"),
                "keep it and mark the outcome with print_update; correct grams with spool_use",
            ));
        }
        let tx = self.conn.unchecked_transaction().map_err(dberr)?;
        let n: i64 = id.strip_prefix("P-").and_then(|n| n.parse().ok()).unwrap_or(0);
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('print_id_high', ?1)
             ON CONFLICT(key) DO UPDATE SET value = MAX(CAST(value AS INTEGER), CAST(?1 AS INTEGER))",
            [n.to_string()],
        )
        .map_err(dberr)?;
        tx.execute("DELETE FROM print_objects WHERE print_id=?1", [id]).map_err(dberr)?;
        tx.execute("DELETE FROM print_filaments WHERE print_id=?1", [id]).map_err(dberr)?;
        tx.execute("DELETE FROM prints WHERE id=?1", [id]).map_err(dberr)?;
        tx.commit().map_err(dberr)?;
        Ok(state)
    }

    pub fn running_print(&self, printer: &str) -> Result<Option<(String, String)>, ToolError> {
        self.conn
            .query_row("SELECT id, job_name FROM prints WHERE printer=?1 AND ended_at IS NULL ORDER BY started_at DESC LIMIT 1", [printer], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .map_err(dberr)
    }

    /// Rows of an arbitrary query as JSON objects (used by detail tools).
    pub fn rows(&self, sql: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<Value>, ToolError> {
        query_json(&self.conn, sql, args)
    }
}

fn next_print_id(conn: &Connection) -> Result<String, ToolError> {
    // the larger of the live maximum and the highest number ever used, so ids of deleted
    // records are never handed out again
    let n: i64 = conn
        .query_row(
            "SELECT MAX(COALESCE((SELECT MAX(CAST(SUBSTR(id, 3) AS INTEGER)) FROM prints WHERE id LIKE 'P-%'), 0),
                        COALESCE((SELECT CAST(value AS INTEGER) FROM meta WHERE key='print_id_high'), 0))",
            [],
            |r| r.get(0),
        )
        .map_err(dberr)?;
    Ok(format!("P-{:04}", n + 1))
}

/// Run a query and return rows as JSON objects.
pub fn query_json(conn: &Connection, sql: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<Value>, ToolError> {
    let mut st = conn.prepare(sql).map_err(|e| ToolError::new("bad_sql", e.to_string(), "check table and column names (see inventory_query's description)"))?;
    let names: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
    let rows = st
        .query_map(args, |r| {
            let mut o = serde_json::Map::new();
            for (i, n) in names.iter().enumerate() {
                let v = match r.get_ref(i)? {
                    rusqlite::types::ValueRef::Null => Value::Null,
                    rusqlite::types::ValueRef::Integer(x) => json!(x),
                    rusqlite::types::ValueRef::Real(x) => json!((x * 1000.0).round() / 1000.0),
                    rusqlite::types::ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                    rusqlite::types::ValueRef::Blob(b) => json!(format!("<{} bytes>", b.len())),
                };
                o.insert(n.clone(), v);
            }
            Ok(Value::Object(o))
        })
        .map_err(|e| ToolError::new("bad_sql", e.to_string(), ""))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| ToolError::new("bad_sql", e.to_string(), ""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_databases_gain_the_calibration_columns() {
        let dir = std::env::temp_dir().join(format!("o3d_mig_test_{}_{}", std::process::id(), crate::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.db");
        {
            // a spools table from before calibration
            let c = Connection::open(&path).unwrap();
            c.execute_batch("CREATE TABLE spools (id TEXT PRIMARY KEY, name TEXT NOT NULL, material TEXT NOT NULL, brand TEXT NOT NULL DEFAULT '', sub_brand TEXT, color TEXT NOT NULL, preset TEXT, filament_id TEXT, diameter_mm REAL NOT NULL DEFAULT 1.75, net_weight_g REAL NOT NULL, tare_g REAL, cost_per_kg REAL, rfid_uuid TEXT UNIQUE, location TEXT, added_at TEXT NOT NULL, archived INTEGER NOT NULL DEFAULT 0, notes TEXT NOT NULL DEFAULT '');
                INSERT INTO spools (id, name, material, color, net_weight_g, added_at) VALUES ('pla-1', 'old', 'PLA', '#FFFFFF', 1000, 'x');").unwrap();
        }
        let db = Db::open_at(&path).unwrap();
        db.conn.execute("UPDATE spools SET flow_ratio=0.96, pressure_advance=0.024 WHERE id='pla-1'", []).unwrap();
        let sp = db.spool("pla-1").unwrap().unwrap();
        assert_eq!((sp.calibration.flow_ratio, sp.calibration.pressure_advance, sp.calibration.max_volumetric_speed), (Some(0.96), Some(0.024), None));
        // and the values land in a filament config
        let mut cfg = only3d_profiles::Config::default();
        cfg.insert("filament_flow_ratio", vec!["1".to_string()]);
        sp.calibration.apply(&mut cfg);
        assert_eq!(cfg.get("filament_flow_ratio").and_then(|v| v.first()), Some("0.96"));
        assert!(cfg.get("pressure_advance").is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn made_up_names_become_automatic_and_follow_their_spool() {
        let dir = std::env::temp_dir().join(format!("o3d_names_test_{}_{}", std::process::id(), crate::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.db");
        {
            // spools as an earlier version named them, plus one the user named
            let c = Connection::open(&path).unwrap();
            c.execute_batch("CREATE TABLE spools (id TEXT PRIMARY KEY, name TEXT NOT NULL, material TEXT NOT NULL, brand TEXT NOT NULL DEFAULT '', sub_brand TEXT, color TEXT NOT NULL, preset TEXT, filament_id TEXT, diameter_mm REAL NOT NULL DEFAULT 1.75, net_weight_g REAL NOT NULL, tare_g REAL, cost_per_kg REAL, rfid_uuid TEXT UNIQUE, location TEXT, added_at TEXT NOT NULL, archived INTEGER NOT NULL DEFAULT 0, notes TEXT NOT NULL DEFAULT '');
                INSERT INTO spools (id, name, material, brand, sub_brand, color, filament_id, net_weight_g, added_at) VALUES
                  ('petg-7f7e83-1', 'Bambu PETG Basic #7F7E83', 'PETG', 'Bambu Lab', 'PETG Basic', '#7F7E83', 'GFG00', 1000, 'x'),
                  ('petg-f9c1bd-1', 'Bambu PETG Translucent #F9C1BD', 'PETG', 'Bambu Lab', 'PETG Translucent', '#F9C1BD', 'GFG01', 1000, 'x'),
                  ('petg-ffffff-1', 'Generic PETG #FFFFFF', 'PETG', 'Generic', NULL, '#FFFFFF', NULL, 300, 'x'),
                  ('pla-gift-1', 'Gift from Ana', 'PLA', 'Generic', NULL, '#3F8E43', NULL, 1000, 'x');").unwrap();
        }
        let db = Db::open_at(&path).unwrap();
        let name = |id: &str| db.spool(id).unwrap().unwrap();
        assert_eq!(name("petg-7f7e83-1").name, "Bambu PETG Basic Gray");
        assert_eq!(name("petg-f9c1bd-1").name, "Bambu PETG Translucent Pink");
        assert_eq!(name("petg-ffffff-1").name, "Generic PETG White");
        assert!(name("petg-ffffff-1").name_auto);
        let gift = name("pla-gift-1");
        assert_eq!((gift.name.as_str(), gift.name_auto), ("Gift from Ana", false));
        // an automatic name follows the spool; a typed one stays
        db.conn.execute_batch("UPDATE spools SET brand='Sunlu', color='#111111'").unwrap();
        for id in ["petg-ffffff-1", "pla-gift-1"] {
            db.refresh_auto_name(id).unwrap();
        }
        assert_eq!(name("petg-ffffff-1").name, "Sunlu PETG Black");
        assert_eq!(name("pla-gift-1").name, "Gift from Ana");
        // a new spool registered with an automatic name keeps the flag
        let mut sp = crate::spools::Spool::blank("asa-1", "ASA", "#F57C20", 1000.0);
        sp.brand = "Polymaker".into();
        sp.set_auto_name();
        db.insert_spool(&sp).unwrap();
        let back = name("asa-1");
        assert_eq!((back.name.as_str(), back.name_auto), ("Polymaker ASA Orange", true));
        drop(db);
        // opening again doesn't rename anything
        let db = Db::open_at(&path).unwrap();
        assert_eq!(db.spool("pla-gift-1").unwrap().unwrap().name, "Gift from Ana");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn ledger_versions_and_prints() {
        let dir = std::env::temp_dir().join(format!("o3d_inv_test_{}_{}", std::process::id(), crate::now_ms()));
        let db = Db::open_at(&dir.join("t.db")).unwrap();
        let mut sp = crate::spools::Spool::blank("pla-green-1", "PLA", "#3F8E43", 1000.0);
        sp.name = "green".into();
        db.insert_spool(&sp).unwrap();
        db.event("pla-green-1", "add", 1000.0, None, "new").unwrap();
        let mut p = only3d_model::Project { id: "prj-test".into(), ..Default::default() };
        let v1 = db.project_version(&p, None, "print").unwrap();
        assert_eq!(db.project_version(&p, None, "print").unwrap(), v1, "same content → same version");
        p.name = "changed".into();
        let v2 = db.project_version(&p, None, "print").unwrap();
        assert_ne!(v1, v2);
        let pid = db.new_print_id().unwrap();
        assert_eq!(pid, "P-0001");
        db.conn.execute("INSERT INTO prints (id, project_version_id, job_name, source, started_at, state) VALUES (?1,?2,'job','only3d','x','RUNNING')", params![pid, v2]).unwrap();
        let left = db.event("pla-green-1", "print", -42.5, Some(&pid), "est").unwrap();
        assert!((left - 957.5).abs() < 1e-9);
        // can't go negative
        assert_eq!(db.event("pla-green-1", "manual", -5000.0, None, "").unwrap(), 0.0);
        let usage = db.rows("SELECT grams FROM print_usage WHERE print_id=?1", &[&pid]).unwrap();
        assert_eq!(usage[0]["grams"], json!(42.5));
        assert_eq!(db.new_print_id().unwrap(), "P-0002");

        // a print that charged filament stays; an unused export record can go, and its number
        // is not handed out again
        assert_eq!(db.delete_print(&pid).unwrap_err().code, "print_active");
        db.conn.execute("UPDATE prints SET state='FINISH', ended_at='y' WHERE id=?1", [&pid]).unwrap();
        assert_eq!(db.delete_print(&pid).unwrap_err().code, "print_has_usage");
        db.conn.execute("INSERT INTO prints (id, job_name, source, started_at, state) VALUES ('P-0002','tile','only3d','x','EXPORTED')", []).unwrap();
        db.conn.execute("INSERT INTO print_objects (print_id, object_id) VALUES ('P-0002','o1')", []).unwrap();
        db.conn.execute("INSERT INTO print_filaments (print_id, slot) VALUES ('P-0002', 0)", []).unwrap();
        assert_eq!(db.delete_print("P-0002").unwrap(), "EXPORTED");
        assert_eq!(db.rows("SELECT * FROM print_objects WHERE print_id='P-0002'", &[]).unwrap().len(), 0);
        assert_eq!(db.new_print_id().unwrap(), "P-0003");
        assert_eq!(db.delete_print("P-0002").unwrap_err().code, "not_found");
        let _ = std::fs::remove_dir_all(dir);
    }
}
