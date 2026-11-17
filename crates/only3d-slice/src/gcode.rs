//! Toolpaths → G-code, in the structure Bambu firmware and tools expect (header/config/
//! executable blocks, `; CHANGE_LAYER`, `; FEATURE:`, M73 progress, M991 layer notify), with a
//! motion-aware time estimate.

use std::fmt::Write as _;

use serde::Serialize;

use crate::planner::{Feature, LayerPlan, Path};
use crate::poly::{dist, P};
use crate::settings::SliceSettings;

/// What machine templates need to know about the job.
#[derive(Clone, Debug, Serialize)]
pub struct JobInfo {
    pub total_layers: usize,
    pub max_z: f64,
    pub first_layer_min: [f64; 2],
    pub first_layer_size: [f64; 2],
    pub settings: SliceSettings,
    pub bed_type: String,
    /// Each filament's settings (index = filament id) when the plate uses several; empty for
    /// one filament (then `settings` is it).
    pub filament_settings: Vec<SliceSettings>,
    /// The filament of each path `object` index: the bodies, then one tower index per filament.
    pub object_filament: Vec<usize>,
    /// The job object (group) of each body.
    pub object_group: Vec<usize>,
    /// The filament loaded first, then the new filament of every change, in print order.
    pub filament_sequence: Vec<usize>,
    pub first_layer_filaments: Vec<usize>,
    /// The filaments of each layer in print order.
    pub layer_filaments: Vec<Vec<usize>>,
    /// Filament ids the plate prints with (sorted).
    pub used_filaments: Vec<usize>,
    pub toolchanges: usize,
    pub tower: Option<crate::multi::TowerInfo>,
    pub nozzles: Option<NozzleSetup>,
}

impl JobInfo {
    pub fn is_multi(&self) -> bool {
        !self.filament_settings.is_empty()
    }
    fn filament_of(&self, object: usize) -> usize {
        self.object_filament.get(object).copied().unwrap_or(0)
    }
    fn fs(&self, f: usize) -> &SliceSettings {
        self.filament_settings.get(f).unwrap_or(&self.settings)
    }
}

/// Nozzles of a multi-nozzle machine (H2D: left and right) and how they heat.
#[derive(Clone, Debug, Serialize)]
pub struct NozzleSetup {
    /// Logical extruder of each filament (H2D: 0 = left, 1 = right).
    pub extruder_of: Vec<usize>,
    /// Firmware tool number of each logical extruder (H2D: left = T1, right = T0).
    pub physical: Vec<usize>,
    /// °C per second.
    pub heat_rate: f64,
    pub cool_rate: f64,
    /// A nozzle is pre-heated to this much below its print temperature.
    pub preheat_delta: f64,
    /// Seconds a nozzle switch / a filament change in one nozzle takes, for the estimate.
    pub switch_time_s: f64,
    pub change_time_s: f64,
}

/// One filament change, for the machine's change template.
#[derive(Clone, Debug, Serialize)]
pub struct FilamentChange {
    pub layer: usize,
    pub z: f64,
    pub from: Option<usize>,
    pub to: usize,
    /// 1-based count of this change in the job.
    pub count: usize,
    /// What the machine purges before printing again (mm³): the flush between colours in one
    /// nozzle, 0 for a switch to the other nozzle.
    pub purge_mm3: f64,
    /// Where printing resumes [x, y, z] (the tower).
    pub resume_at: [f64; 3],
    pub tower_center: [f64; 2],
}

/// Machine-specific G-code. The H2D implementation renders Bambu Studio's own templates.
pub trait MachineHooks: Sync {
    fn name(&self) -> &str;
    fn start(&self, job: &JobInfo) -> Result<String, String>;
    fn end(&self, job: &JobInfo) -> Result<String, String>;
    fn layer_change(&self, job: &JobInfo, layer: usize, z: f64) -> Result<String, String>;
    fn timelapse(&self, _job: &JobInfo, _layer: usize, _z: f64) -> Result<String, String> {
        Ok(String::new())
    }
    /// Lines after `; MACHINE_START_GCODE_END` (the filament's start G-code).
    fn filament_start(&self, _job: &JobInfo) -> Result<String, String> {
        Ok("; filament start gcode\n".into())
    }
    /// M201/M203/M204/M205 block.
    fn machine_limits(&self) -> String {
        "M201 X20000 Y20000 Z500 E5000\nM203 X1000 Y1000 Z30 E50\nM204 P20000 R5000 T20000\nM205 X9.00 Y9.00 Z3.00 E2.50\n".into()
    }
    /// Extra `; key = value` lines for the CONFIG block.
    fn config_lines(&self, _job: &JobInfo) -> Vec<(String, String)> {
        Vec::new()
    }
    /// True when `config_lines` is the machine's full config (Bambu printers read it): the CONFIG
    /// block then holds exactly those lines, without only3d's own settings dump.
    fn config_is_complete(&self) -> bool {
        false
    }
    /// G-code that unloads `from` and loads `to` (the machine's change template).
    fn change_filament(&self, _job: &JobInfo, ev: &FilamentChange) -> Result<String, String> {
        Ok(format!("T{}\n", ev.to))
    }
    /// Purge between two filaments in the same nozzle (mm³).
    fn flush_mm3(&self, _job: &JobInfo, _from: usize, _to: usize) -> f64 {
        0.0
    }
}

/// Minimal generic Marlin-style start/end. Fine for previews and generic printers; the H2D
/// uses Bambu's templates instead (see `only3d-studio`).
pub struct GenericHooks;

impl MachineHooks for GenericHooks {
    fn name(&self) -> &str {
        "generic"
    }
    fn start(&self, j: &JobInfo) -> Result<String, String> {
        let s = &j.settings;
        Ok(format!(
            "M140 S{bed}\nM104 S{noz}\nG28\nM190 S{bed}\nM109 S{noz}\nG90\nM83\nG1 Z2 F3000\nG1 X5 Y5 F12000\nG1 Z0.3 F3000\nG1 X5 Y120 E12 F1500\nG1 X5.4 Y120 F3000\nG1 X5.4 Y5 E12 F1500\n",
            bed = s.first_layer_bed_temp,
            noz = s.first_layer_nozzle_temp
        ))
    }
    fn end(&self, j: &JobInfo) -> Result<String, String> {
        Ok(format!("M104 S0\nM140 S0\nM106 S0\nG91\nG1 Z5 F3000\nG90\nG1 Z{:.2} F3000\nM84\n", (j.max_z + 10.0).min(320.0)))
    }
    fn layer_change(&self, _j: &JobInfo, layer: usize, _z: f64) -> Result<String, String> {
        Ok(format!("M73 L{}\n", layer + 1))
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct PrintStats {
    pub print_time_s: f64,
    pub layers: usize,
    pub max_z: f64,
    pub filament_mm: f64,
    pub filament_mm3: f64,
    pub filament_g: f64,
    pub filament_cost: f64,
    pub layer_times_s: Vec<f64>,
    /// Seconds per feature.
    pub feature_time_s: Vec<(String, f64)>,
    pub moves: usize,
    /// Filament grams per job object (index = object order in the job; the brim counts toward
    /// the first object).
    pub object_filament_g: Vec<f64>,
    /// Per filament used: (filament id, mm, grams), the tower included.
    pub filaments: Vec<(usize, f64, f64)>,
    /// Grams the prime tower takes.
    pub tower_g: f64,
    pub toolchanges: usize,
}

/// Trapezoidal move time from `v0` to `v1` with cruise `v` and acceleration `a`.
pub fn move_time(d: f64, v: f64, a: f64, v0: f64, v1: f64) -> f64 {
    if d <= 0.0 {
        return 0.0;
    }
    let (v0, v1) = (v0.min(v), v1.min(v));
    let a = a.max(1.0);
    let da = (v * v - v0 * v0) / (2.0 * a);
    let dd = (v * v - v1 * v1) / (2.0 * a);
    if da + dd <= d {
        (v - v0) / a + (v - v1) / a + (d - da - dd) / v
    } else {
        let vp = ((2.0 * a * d + v0 * v0 + v1 * v1) / 2.0).sqrt();
        ((vp - v0) / a + (vp - v1) / a).max(d / v)
    }
}

const JERK: f64 = 9.0;

/// Junction speed with the classic jerk limit (as Marlin-style planners and Bambu Studio's
/// estimator use): the velocity vector may change by at most `JERK` at a corner.
fn junction(v: f64, a: P, b: P, c: P) -> f64 {
    let u = [b[0] - a[0], b[1] - a[1]];
    let w = [c[0] - b[0], c[1] - b[1]];
    let lu = (u[0] * u[0] + u[1] * u[1]).sqrt();
    let lw = (w[0] * w[0] + w[1] * w[1]).sqrt();
    if lu < 1e-9 || lw < 1e-9 {
        return v;
    }
    let cos = ((u[0] * w[0] + u[1] * w[1]) / (lu * lw)).clamp(-1.0, 1.0);
    let half_sin = ((1.0 - cos) / 2.0).sqrt(); // sin(theta/2)
    if half_sin < 1e-6 {
        return v;
    }
    v.min(JERK / (2.0 * half_sin)).max(JERK.min(v) * 0.5)
}

struct Emitter<'a> {
    /// The loaded filament's settings (flow, retraction).
    s: &'a SliceSettings,
    lines: Vec<(String, f64)>,
    x: f64,
    y: f64,
    z: f64,
    feed: f64,
    accel: f64,
    retracted: bool,
    e_total: f64,
    e_by_object: Vec<f64>,
    feature_time: std::collections::BTreeMap<Feature, f64>,
    moves: usize,
}

fn f3(v: f64) -> String {
    // Bambu style: no trailing zeros, strip leading 0 ("0.4" -> ".4")
    let s = format!("{:.3}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    let s = if s == "-0" { "0".to_string() } else { s };
    if let Some(r) = s.strip_prefix("0.") {
        format!(".{r}")
    } else if let Some(r) = s.strip_prefix("-0.") {
        format!("-.{r}")
    } else {
        s
    }
}

fn f5(v: f64) -> String {
    let s = format!("{:.5}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if let Some(r) = s.strip_prefix("0.") {
        format!(".{r}")
    } else if let Some(r) = s.strip_prefix("-0.") {
        format!("-.{r}")
    } else if s == "-0" {
        "0".into()
    } else {
        s
    }
}

impl<'a> Emitter<'a> {
    fn raw(&mut self, s: impl Into<String>) {
        self.lines.push((s.into(), 0.0));
    }
    fn timed(&mut self, s: String, t: f64) {
        self.lines.push((s, t));
    }
    fn block(&mut self, text: &str) {
        for l in text.lines() {
            self.raw(l.to_string());
        }
    }
    fn set_accel(&mut self, a: f64) {
        if (a - self.accel).abs() > 0.5 {
            self.accel = a;
            self.raw(format!("M204 S{}", a.round() as i64));
        }
    }
    fn retract(&mut self) {
        if !self.retracted && self.s.retract_length > 0.0 {
            let t = self.s.retract_length / self.s.retract_speed;
            self.timed(format!("G1 E-{} F{}", f3(self.s.retract_length), (self.s.retract_speed * 60.0).round()), t);
            self.retracted = true;
        }
    }
    fn unretract(&mut self) {
        if self.retracted {
            let t = self.s.retract_length / self.s.deretract_speed;
            self.timed(format!("G1 E{} F{}", f3(self.s.retract_length), (self.s.deretract_speed * 60.0).round()), t);
            self.retracted = false;
        }
    }
    fn z_to(&mut self, z: f64) {
        if (z - self.z).abs() > 1e-6 {
            let t = (z - self.z).abs() / 20.0;
            self.timed(format!("G1 Z{}", f3(z)), t);
            self.z = z;
        }
    }
    /// Travel to `to`. Short hops inside infill skip retraction.
    fn travel(&mut self, to: P, layer_z: f64, may_skip_retract: bool) {
        let d = dist([self.x, self.y], to);
        if d < 1e-4 {
            return;
        }
        let long = d > self.s.retract_min_travel && !(may_skip_retract && d < 15.0);
        if long {
            self.retract();
        }
        let hop = long && self.s.z_hop > 0.0 && d > 2.0;
        if hop {
            self.z_to(layer_z + self.s.z_hop);
        }
        let v = self.s.travel_speed;
        let t = move_time(d, v, self.s.travel_accel, 0.0, 0.0);
        self.timed(format!("G1 X{} Y{} F{}", f3(to[0]), f3(to[1]), (v * 60.0).round()), t);
        // the modal feed rate is now the travel speed: the next extrusion must restate its own
        self.feed = (v * 60.0).round();
        self.x = to[0];
        self.y = to[1];
        self.moves += 1;
        if hop || (self.z - layer_z).abs() > 1e-6 {
            self.z_to(layer_z);
        }
    }

    fn extrude(&mut self, path: &Path, h: f64, speed_scale: f64) -> f64 {
        let s = self.s;
        let v = (path.speed * speed_scale).max(s.min_print_speed.min(path.speed));
        let e_per_mm = SliceSettings::bead_area(path.width, h) * path.flow * s.flow_ratio / s.filament_area();
        let mut pts = path.pts.clone();
        if path.closed {
            pts.push(pts[0]);
        }
        let feed = (v * 60.0).round();
        if (feed - self.feed).abs() > 0.5 {
            self.raw(format!("G1 F{}", feed));
            self.feed = feed;
        }
        let mut total_t = 0.0;
        let n = pts.len();
        for i in 1..n {
            let (a, b) = (pts[i - 1], pts[i]);
            let d = dist(a, b);
            if d < 1e-5 {
                continue;
            }
            let v0 = if i >= 2 { junction(v, pts[i - 2], a, b) } else { JERK.min(v) };
            let v1 = if i + 1 < n { junction(v, a, b, pts[i + 1]) } else { JERK.min(v) };
            let t = move_time(d, v, self.accel, v0, v1);
            total_t += t;
            let e = d * e_per_mm;
            self.e_total += e;
            if self.e_by_object.len() <= path.object {
                self.e_by_object.resize(path.object + 1, 0.0);
            }
            self.e_by_object[path.object] += e;
            self.timed(format!("G1 X{} Y{} E{}", f3(b[0]), f3(b[1]), f5(e)), t);
            self.moves += 1;
        }
        if let Some(l) = pts.last() {
            self.x = l[0];
            self.y = l[1];
        }
        *self.feature_time.entry(path.feature).or_default() += total_t;
        total_t
    }

    /// Wipe inward along the just-printed loop while retracting.
    fn wipe(&mut self, path: &Path) {
        let s = self.s;
        if !path.closed || s.wipe_distance <= 0.0 || self.retracted || s.retract_length <= 0.0 {
            return;
        }
        let mut remaining = s.wipe_distance;
        let mut cur = [self.x, self.y];
        let mut pts = Vec::new();
        for p in path.pts.iter().skip(1).chain(path.pts.iter()) {
            let d = dist(cur, *p);
            if d >= remaining {
                let t = remaining / d;
                pts.push([cur[0] + (p[0] - cur[0]) * t, cur[1] + (p[1] - cur[1]) * t]);
                break;
            }
            pts.push(*p);
            remaining -= d;
            cur = *p;
            if remaining <= 1e-6 {
                break;
            }
        }
        if pts.is_empty() {
            return;
        }
        let total: f64 = {
            let mut c = [self.x, self.y];
            pts.iter().map(|p| {
                let d = dist(c, *p);
                c = *p;
                d
            }).sum()
        };
        if total < 1e-3 {
            return;
        }
        self.raw("; WIPE_START");
        let wipe_e = s.retract_length * 0.95;
        let mut c = [self.x, self.y];
        for p in pts {
            let d = dist(c, p);
            let e = wipe_e * d / total;
            self.timed(format!("G1 X{} Y{} E-{}", f3(p[0]), f3(p[1]), f5(e)), d / s.travel_speed.min(200.0));
            c = p;
        }
        self.x = c[0];
        self.y = c[1];
        self.raw("; WIPE_END");
        let rest = s.retract_length - wipe_e;
        self.timed(format!("G1 E-{} F{}", f5(rest), (s.retract_speed * 60.0).round()), rest / s.retract_speed);
        self.retracted = true;
    }
}

fn fmt_time(s: f64) -> String {
    let s = s.round() as i64;
    let (d, h, m, sec) = (s / 86400, (s / 3600) % 24, (s / 60) % 60, s % 60);
    let mut out = String::new();
    if d > 0 {
        let _ = write!(out, "{d}d ");
    }
    if h > 0 || d > 0 {
        let _ = write!(out, "{h}h ");
    }
    if m > 0 || h > 0 || d > 0 {
        let _ = write!(out, "{m}m ");
    }
    let _ = write!(out, "{sec}s");
    out
}

/// (extrusion time, travel/other time) of a layer at planned speeds, with the same motion
/// model as the final estimate.
fn layer_time_estimate(l: &LayerPlan, s: &SliceSettings, scale: f64) -> (f64, f64) {
    let (mut te, mut tt) = (0.0, 0.0);
    let mut cur: Option<P> = None;
    for p in &l.paths {
        if p.pts.len() < 2 {
            continue;
        }
        if let Some(c) = cur {
            let d = dist(c, p.start());
            tt += move_time(d, s.travel_speed, s.travel_accel, 0.0, 0.0);
            if d > s.retract_min_travel {
                tt += 2.0 * s.retract_length / s.retract_speed;
            }
        }
        let accel = match p.feature {
            _ if l.index == 0 => s.first_layer_accel,
            Feature::OuterWall => s.outer_wall_accel,
            Feature::TopSurface | Feature::Ironing => s.top_surface_accel,
            _ => s.default_accel,
        };
        let mut pts = p.pts.clone();
        if p.closed {
            pts.push(pts[0]);
        }
        let n = pts.len();
        let v = (p.speed * scale).max(s.min_print_speed.min(p.speed));
        for i in 1..n {
            let v0 = if i >= 2 { junction(v, pts[i - 2], pts[i - 1], pts[i]) } else { JERK.min(v) };
            let v1 = if i + 1 < n { junction(v, pts[i - 1], pts[i], pts[i + 1]) } else { JERK.min(v) };
            te += move_time(dist(pts[i - 1], pts[i]), v, accel, v0, v1);
        }
        cur = Some(p.end());
    }
    (te, tt)
}

/// Fan duty (0..1) for a layer of estimated time `eff` with filament settings `s`.
fn fan_for(s: &SliceSettings, li: usize, eff: f64) -> f64 {
    if (li as u32) < s.fan_off_first_layers {
        0.0
    } else if eff <= s.min_layer_time {
        s.fan_max
    } else if eff >= s.fan_full_below_layer_time {
        s.fan_min
    } else {
        s.fan_max - (s.fan_max - s.fan_min) * (eff - s.min_layer_time) / (s.fan_full_below_layer_time - s.min_layer_time)
    }
}

/// A nozzle switch, for the idle-nozzle cooling and pre-heating pass.
struct Switch {
    /// Line where the change block starts.
    line: usize,
    from_ext: usize,
    to_ext: usize,
    /// The filament the nozzle being switched to prints, and on which layer.
    to: usize,
    layer: usize,
}

/// Emit the full G-code text and statistics.
pub fn emit(layers: &[LayerPlan], job: &JobInfo, hooks: &dyn MachineHooks) -> Result<(String, PrintStats), String> {
    let s = &job.settings;
    let multi = job.is_multi();
    let first_f = job.filament_sequence.first().copied().unwrap_or(0);
    let mut em = Emitter {
        s: job.fs(first_f),
        lines: Vec::with_capacity(200_000),
        x: 0.0,
        y: 0.0,
        z: 0.0,
        feed: 0.0,
        accel: 0.0,
        retracted: false,
        e_total: 0.0,
        e_by_object: Vec::new(),
        feature_time: Default::default(),
        moves: 0,
    };
    let mut layer_times = Vec::with_capacity(layers.len());
    let mut cur_f = first_f;
    let mut count = 0usize;
    let mut switches: Vec<Switch> = Vec::new();
    let ext_of = |f: usize| job.nozzles.as_ref().and_then(|n| n.extruder_of.get(f).copied()).unwrap_or(0);

    em.block(&hooks.machine_limits());
    em.raw("M106 S0");
    em.raw("M106 P2 S0");
    let start = hooks.start(job).map_err(|e| format!("start G-code: {e}"))?;
    if !start.contains("M190") && !start.contains("M140") {
        em.raw(format!("M190 S{} ; set bed temperature and wait for it to be reached", s.first_layer_bed_temp));
    }
    em.raw("; FEATURE: Custom");
    em.block(&start);
    em.raw("; MACHINE_START_GCODE_END");
    let body_start = em.lines.len();
    em.block(&hooks.filament_start(job).map_err(|e| format!("filament start G-code: {e}"))?);
    em.raw("G90");
    em.raw("G21");
    em.raw("M83 ; use relative distances for extrusion");
    em.raw("M981 S1 P20000 ;open spaghetti detector");
    em.z = f64::NAN;

    for (li, layer) in layers.iter().enumerate() {
        let t_layer_start: f64 = em.lines.iter().map(|l| l.1).sum();
        em.raw("; CHANGE_LAYER");
        em.raw(format!("; Z_HEIGHT: {}", f3(layer.z)));
        em.raw(format!("; LAYER_HEIGHT: {}", f3(layer.height)));
        em.retract();
        em.block(&hooks.layer_change(job, li, layer.z).map_err(|e| format!("layer change G-code: {e}"))?);

        // cooling: slow down short layers and choose fan speed
        let (te, tt) = layer_time_estimate(layer, s, 1.0);
        let est = te + tt;
        // slow extrusion down so the layer takes at least min_layer_time (travel stays fast):
        // bisect the speed factor against the same motion model the final estimate uses
        let speed_scale = if li > 0 && est < s.min_layer_time && te > 0.0 {
            let (mut lo, mut hi) = (0.02, 1.0);
            for _ in 0..14 {
                let mid = (lo + hi) / 2.0;
                let (a, b) = layer_time_estimate(layer, s, mid);
                if a + b < s.min_layer_time {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            lo
        } else {
            1.0
        };
        let eff = est.max(if speed_scale < 1.0 { s.min_layer_time } else { 0.0 });
        let mut fan_pwm = (fan_for(em.s, li, eff) * 255.0).round() as i64;
        em.raw(format!("M106 S{fan_pwm}"));
        if li as u32 == em.s.fan_off_first_layers {
            em.raw(format!("M106 P2 S{}", (em.s.aux_fan * 255.0).round() as i64));
        }
        if li == 1 {
            if (em.s.nozzle_temp - em.s.first_layer_nozzle_temp).abs() > 0.5 {
                em.raw(format!("M104 S{}", em.s.nozzle_temp.round()));
            }
            if (s.bed_temp - s.first_layer_bed_temp).abs() > 0.5 {
                em.raw(format!("M140 S{}", s.bed_temp.round()));
            }
        }

        let mut cur_feature: Option<Feature> = None;
        let mut first_move = true;
        for p in &layer.paths {
            if p.pts.len() < 2 {
                continue;
            }
            // another filament: the machine's change sequence, then the new filament's settings
            let f = job.filament_of(p.object);
            if multi && f != cur_f {
                em.retract();
                count += 1;
                let (from_e, to_e) = (ext_of(cur_f), ext_of(f));
                let purge = if from_e == to_e { hooks.flush_mm3(job, cur_f, f) } else { 0.0 };
                let tw = job.tower.as_ref();
                let ev = FilamentChange {
                    layer: li,
                    z: layer.z,
                    from: Some(cur_f),
                    to: f,
                    count,
                    purge_mm3: purge,
                    resume_at: [p.start()[0], p.start()[1], layer.z],
                    tower_center: tw.map(|t| [(t.min[0] + t.max[0]) / 2.0, (t.min[1] + t.max[1]) / 2.0]).unwrap_or([p.start()[0], p.start()[1]]),
                };
                let block = hooks.change_filament(job, &ev).map_err(|e| format!("filament change G-code: {e}"))?;
                if from_e != to_e {
                    switches.push(Switch { line: em.lines.len(), from_ext: from_e, to_ext: to_e, to: f, layer: li });
                }
                em.raw("; filament end gcode ");
                em.raw("; FilamentChangeGCodeStart");
                em.block(&block);
                em.raw("; FilamentChangeGCodeEnd");
                // the change itself takes time the moves don't show
                let n = job.nozzles.as_ref();
                let t = if from_e != to_e { n.map(|n| n.switch_time_s).unwrap_or(10.0) } else { n.map(|n| n.change_time_s).unwrap_or(60.0) + purge / 10.0 };
                if let Some(l) = em.lines.last_mut() {
                    l.1 += t;
                }
                cur_f = f;
                em.s = job.fs(f);
                // the template leaves the nozzle lifted 3 mm, somewhere at the back
                em.z = layer.z + 3.0;
                em.x = 175.0;
                em.y = 265.0;
                em.feed = 0.0;
                em.accel = 0.0;
                em.retracted = false;
                fan_pwm = (fan_for(em.s, li, eff) * 255.0).round() as i64;
                em.raw(format!("M106 S{fan_pwm}"));
                em.raw(format!("M106 P2 S{}", if (li as u32) < em.s.fan_off_first_layers { 0 } else { (em.s.aux_fan * 255.0).round() as i64 }));
                em.raw("; filament start gcode");
                em.retract();
                cur_feature = None;
            }
            if first_move {
                // lift, travel, lower
                if em.z.is_nan() || em.z < layer.z + em.s.z_hop {
                    let zt = layer.z + em.s.z_hop.max(0.2);
                    em.timed(format!("G1 Z{}", f3(zt)), 0.05);
                    em.z = zt;
                }
                em.set_accel(em.s.travel_accel);
                em.travel(p.start(), layer.z, false);
                em.z_to(layer.z);
                first_move = false;
            } else {
                em.set_accel(em.s.travel_accel);
                let infill_hop = matches!(p.feature, Feature::SparseInfill | Feature::SolidInfill) && cur_feature == Some(p.feature);
                em.travel(p.start(), layer.z, infill_hop);
            }
            em.unretract();
            if cur_feature != Some(p.feature) {
                em.raw(format!("; FEATURE: {}", p.feature.bambu_name()));
                em.raw(format!("; LINE_WIDTH: {}", f5(p.width)));
                if p.feature == Feature::Bridge {
                    em.raw("M106 S255");
                } else if cur_feature == Some(Feature::Bridge) {
                    em.raw(format!("M106 S{fan_pwm}"));
                }
                cur_feature = Some(p.feature);
                em.feed = 0.0;
            }
            let accel = match p.feature {
                _ if li == 0 => s.first_layer_accel,
                // gently, like Bambu Studio: a tall thin tower must not wobble
                Feature::PrimeTower => s.first_layer_accel.min(1000.0),
                Feature::OuterWall => s.outer_wall_accel,
                Feature::TopSurface | Feature::Ironing => s.top_surface_accel,
                _ => s.default_accel,
            };
            em.set_accel(accel);
            em.extrude(p, layer.height, speed_scale);
            if p.feature == Feature::OuterWall {
                em.wipe(p);
            }
        }
        let tl = hooks.timelapse(job, li, layer.z).map_err(|e| format!("timelapse G-code: {e}"))?;
        if !tl.is_empty() {
            em.block(&tl);
        }
        let t_layer_end: f64 = em.lines.iter().map(|l| l.1).sum();
        layer_times.push(t_layer_end - t_layer_start);
    }
    em.retract();
    em.raw("; MACHINE_END_GCODE_START");
    em.raw("; filament end gcode ");
    em.block(&hooks.end(job).map_err(|e| format!("end G-code: {e}"))?);

    // the nozzle that is not printing: let it cool while it waits, and heat it again in time
    // for its next turn (like Bambu Studio's "Multi extruder pre cooling / pre heating")
    if let Some(nz) = job.nozzles.as_ref().filter(|_| multi) {
        idle_nozzles(&mut em.lines, &switches, body_start, job, nz, ext_of(first_f));
    }

    // statistics: per filament (its own diameter and density), per job object, the tower
    let total: f64 = em.lines.iter().map(|l| l.1).sum();
    let nb = job.object_group.len();
    let mut fil_mm: std::collections::BTreeMap<usize, f64> = Default::default();
    let mut object_g: Vec<f64> = Vec::new();
    let mut tower_g = 0.0;
    for (o, e) in em.e_by_object.iter().enumerate() {
        let f = if multi { job.filament_of(o) } else { 0 };
        let fs = job.fs(f);
        *fil_mm.entry(f).or_default() += e;
        let g = e * fs.filament_area() * fs.filament_density / 1000.0;
        if o < nb || !multi {
            let gi = job.object_group.get(o).copied().unwrap_or(o);
            if object_g.len() <= gi {
                object_g.resize(gi + 1, 0.0);
            }
            object_g[gi] += g;
        } else {
            tower_g += g;
        }
    }
    let filaments: Vec<(usize, f64, f64)> = fil_mm
        .iter()
        .map(|(f, mm)| {
            let fs = job.fs(*f);
            (*f, *mm, mm * fs.filament_area() * fs.filament_density / 1000.0)
        })
        .collect();
    let mm3: f64 = fil_mm.iter().map(|(f, mm)| mm * job.fs(*f).filament_area()).sum();
    let grams: f64 = filaments.iter().map(|x| x.2).sum();
    let cost: f64 = filaments.iter().map(|(f, _, g)| g / 1000.0 * job.fs(*f).filament_cost_per_kg).sum();
    let stats = PrintStats {
        print_time_s: total,
        layers: layers.len(),
        max_z: job.max_z,
        filament_mm: em.e_total,
        filament_mm3: mm3,
        filament_g: grams,
        filament_cost: cost,
        layer_times_s: layer_times,
        feature_time_s: em.feature_time.iter().map(|(f, t)| (f.bambu_name().to_string(), *t)).collect(),
        moves: em.moves,
        object_filament_g: object_g,
        filaments: filaments.clone(),
        tower_g,
        toolchanges: count,
    };

    // assemble with header, config block and M73 progress
    let mut out = String::with_capacity(em.lines.len() * 24 + 8192);
    out.push_str("; HEADER_BLOCK_START\n");
    let _ = writeln!(out, "; only3d {} (native slicer, machine: {})", env!("CARGO_PKG_VERSION"), hooks.name());
    let _ = writeln!(out, "; model printing time: {}; total estimated time: {}", fmt_time(total), fmt_time(total));
    let _ = writeln!(out, "; total layer number: {}", layers.len());
    if multi {
        let list = |g: &dyn Fn(&(usize, f64, f64)) -> String| filaments.iter().map(g).collect::<Vec<_>>().join(",");
        let _ = writeln!(out, "; total filament length [mm] : {}", list(&|x| format!("{:.2}", x.1)));
        let _ = writeln!(out, "; total filament volume [cm^3] : {}", list(&|x| format!("{:.2}", x.1 * job.fs(x.0).filament_area())));
        let _ = writeln!(out, "; total filament weight [g] : {}", list(&|x| format!("{:.2}", x.2)));
        let _ = writeln!(out, "; filament_density: {}", list(&|x| format!("{}", job.fs(x.0).filament_density)));
        let _ = writeln!(out, "; filament_diameter: {}", list(&|x| format!("{}", job.fs(x.0).filament_diameter)));
        let _ = writeln!(out, "; max_z_height: {:.2}", job.max_z);
        let _ = writeln!(out, "; filament: {}", list(&|x| (x.0 + 1).to_string()));
    } else {
        let _ = writeln!(out, "; total filament length [mm] : {:.2}", em.e_total);
        let _ = writeln!(out, "; total filament volume [cm^3] : {:.2}", mm3);
        let _ = writeln!(out, "; total filament weight [g] : {:.2}", grams);
        let _ = writeln!(out, "; filament_density: {}", s.filament_density);
        let _ = writeln!(out, "; filament_diameter: {}", s.filament_diameter);
        let _ = writeln!(out, "; max_z_height: {:.2}", job.max_z);
        // 1-based ids of the filaments used (one per plate in the native engine), like Bambu Studio
        let _ = writeln!(out, "; filament: 1");
    }
    let _ = writeln!(out, "; support_material_on_wipe_tower: 0");
    out.push_str("; HEADER_BLOCK_END\n\n; CONFIG_BLOCK_START\n");
    if hooks.config_is_complete() {
        // nothing of ours: the block is the machine's full config
    } else if let Ok(serde_json::Value::Object(m)) = serde_json::to_value(s) {
        for (k, v) in m {
            let v = match v {
                serde_json::Value::String(x) => x,
                other => other.to_string(),
            };
            let _ = writeln!(out, "; {k} = {v}");
        }
    }
    for (k, v) in hooks.config_lines(job) {
        let _ = writeln!(out, "; {k} = {v}");
    }
    out.push_str("; CONFIG_BLOCK_END\n\n; EXECUTABLE_BLOCK_START\n");
    let _ = writeln!(out, "M73 P0 R{}", (total / 60.0).floor() as i64);
    let mut acc = 0.0;
    let mut last_p = 0i64;
    let mut last_r = (total / 60.0).floor() as i64;
    for (line, t) in &em.lines {
        acc += t;
        if total > 0.0 {
            let p = ((acc / total) * 100.0).floor() as i64;
            let r = ((total - acc) / 60.0).floor() as i64;
            if p > last_p && p < 100 {
                let _ = writeln!(out, "M73 P{p} R{r}");
                last_p = p;
                last_r = r;
            } else if r < last_r {
                let _ = writeln!(out, "M73 P{last_p} R{r}");
                last_r = r;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("M73 P100 R0\n; EXECUTABLE_BLOCK_END\n");
    Ok((out, stats))
}

/// Insert the idle-nozzle commands: when a nozzle stops printing it may cool (lower the longer
/// it waits), and it is heated again early enough to be at temperature when it is needed. The
/// change template sets the exact print temperature; this only saves waiting and oozing.
fn idle_nozzles(lines: &mut Vec<(String, f64)>, switches: &[Switch], body_start: usize, job: &JobInfo, nz: &NozzleSetup, first_ext: usize) {
    let mut at = vec![0.0f64; lines.len() + 1];
    for (i, l) in lines.iter().enumerate() {
        at[i + 1] = at[i] + l.1;
    }
    let temp_for = |f: usize, layer: usize| {
        let fs = job.fs(f);
        if layer == 0 {
            fs.first_layer_nozzle_temp
        } else {
            fs.nozzle_temp
        }
    };
    // (line, text) to insert; applied from the back
    let mut ins: Vec<(usize, String)> = Vec::new();
    // every idle period: (extruder, line it starts at, its next switch-in)
    let mut idle: Vec<(usize, usize, Option<&Switch>)> = Vec::new();
    let used: Vec<usize> = {
        let mut v: Vec<usize> = job.used_filaments.iter().map(|f| nz.extruder_of.get(*f).copied().unwrap_or(0)).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    for e in used.iter().copied().filter(|e| *e != first_ext) {
        idle.push((e, body_start, switches.iter().find(|s| s.to_ext == e)));
    }
    for (k, sw) in switches.iter().enumerate() {
        idle.push((sw.from_ext, sw.line, switches[k + 1..].iter().find(|s| s.to_ext == sw.from_ext)));
    }
    for (e, start, next) in idle {
        let phys = nz.physical.get(e).copied().unwrap_or(e);
        let t0 = at[start.min(lines.len())];
        let Some(next) = next else {
            // not needed again: let it go cold
            ins.push((start, format!("M104 T{phys} S0 N0 C ;Multi extruder pre cooling")));
            continue;
        };
        let t1 = at[next.line.min(lines.len())];
        let wait = t1 - t0;
        if wait < 45.0 {
            continue;
        }
        let print = temp_for(next.to, next.layer);
        let target = if next.layer == 0 { print } else { print - nz.preheat_delta };
        // cool for about half the wait, so heating back has the other half
        let cool = (print - nz.cool_rate * wait * 0.5).max(25.0).min(target).round();
        let lead = (target - cool) / nz.heat_rate.max(0.5) + 10.0;
        let t_heat = (t1 - lead).max(t0 + wait * 0.5);
        let line = at.partition_point(|t| *t < t_heat).saturating_sub(1).clamp(start + 1, next.line);
        ins.push((start, format!("M400\nM104 T{phys} S{cool:.0} N0 C ;Multi extruder pre cooling")));
        ins.push((line, format!("M632 S0 W\nM104 T{phys} S{target:.0} N0 W ;Multi extruder pre heating\nM633")));
    }
    ins.sort_by_key(|(l, _)| std::cmp::Reverse(*l));
    for (l, text) in ins {
        let l = l.min(lines.len());
        let new: Vec<(String, f64)> = text.lines().map(|t| (t.to_string(), 0.0)).collect();
        lines.splice(l..l, new);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_format_matches_bambu() {
        assert_eq!(f3(0.4), ".4");
        assert_eq!(f3(184.143), "184.143");
        assert_eq!(f3(-0.4), "-.4");
        assert_eq!(f3(20.0), "20");
        assert_eq!(f5(0.68108), ".68108");
    }

    #[test]
    fn lint_catches_out_of_bounds() {
        let g = "; MACHINE_START_GCODE_END\n; Z_HEIGHT: .2\nG1 X10 Y10 F3000\nG1 X20 Y10 E.4\nG1 X400 Y10 E.4\n";
        let r = lint(g, [0.0, 0.0, 350.0, 320.0], 1000.0, 0.2);
        assert!(!r.ok);
        assert!(r.problems[0].contains("outside"), "{:?}", r.problems);
    }

    #[test]
    fn trapezoid() {
        // long move: almost d/v
        let t = move_time(1000.0, 100.0, 10000.0, 0.0, 0.0);
        assert!((t - 10.01).abs() < 0.01, "{t}");
        // short move never reaches cruise
        let t = move_time(1.0, 500.0, 1000.0, 0.0, 0.0);
        assert!((t - 2.0 * (1.0f64 / 1000.0).sqrt()).abs() < 1e-6);
    }
}

/// Independent sanity check of emitted G-code (run before anything is sent to a printer).
#[derive(Clone, Debug, Default, Serialize)]
pub struct LintReport {
    pub ok: bool,
    pub problems: Vec<String>,
    /// Bounding box of extruding moves [xmin, ymin, xmax, ymax] after the start sequence.
    pub extrude_bounds: [f64; 4],
    pub max_feed_mm_s: f64,
    pub max_e_per_mm: f64,
    pub layers: usize,
    pub extrusion_moves: usize,
}

/// Lint the body (after `; MACHINE_START_GCODE_END`): extrusions inside `area` [x0,y0,x1,y1],
/// feed rates within `max_feed`, sane extrusion per mm, Z never goes down between layers.
pub fn lint(gcode: &str, area: [f64; 4], max_feed: f64, max_e_per_mm: f64) -> LintReport {
    let mut r = LintReport { extrude_bounds: [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY], ..Default::default() };
    let (mut x, mut y, mut f) = (f64::NAN, f64::NAN, 0.0f64);
    let mut body = false;
    let mut last_layer_z = f64::NEG_INFINITY;
    let mut in_custom = false;
    for (ln, line) in gcode.lines().enumerate() {
        if line.starts_with("; MACHINE_START_GCODE_END") {
            body = true;
            continue;
        }
        if line.starts_with("; MACHINE_END_GCODE_START") {
            break;
        }
        if !body {
            continue;
        }
        if let Some(z) = line.strip_prefix("; Z_HEIGHT: ") {
            let z: f64 = z.trim().parse().unwrap_or(0.0);
            if z + 1e-6 < last_layer_z && r.problems.len() < 20 {
                r.problems.push(format!("line {}: layer Z {z} below previous {last_layer_z}", ln + 1));
            }
            last_layer_z = z;
            r.layers += 1;
            continue;
        }
        // template blocks (timelapse, wrapping detection) contain their own motion
        if line.contains("SKIPPABLE_START") || line.starts_with(";======== H2D") {
            in_custom = true;
        }
        if line.starts_with("; FEATURE:") || line.contains("SKIPPABLE_END") {
            in_custom = false;
        }
        let code = line.split(';').next().unwrap_or("").trim();
        if !(code.starts_with("G1 ") || code.starts_with("G0 ")) {
            continue;
        }
        let mut nx = x;
        let mut ny = y;
        let mut e = 0.0;
        for t in code.split_whitespace().skip(1) {
            let (k, v) = t.split_at(1);
            let v: f64 = v.parse().unwrap_or(f64::NAN);
            match k {
                "X" => nx = v,
                "Y" => ny = v,
                "E" => e = v,
                "F" => f = v / 60.0,
                _ => {}
            }
        }
        let d = if x.is_nan() || nx.is_nan() { 0.0 } else { ((nx - x).powi(2) + (ny - y).powi(2)).sqrt() };
        if e > 0.0 && d > 1e-6 && !in_custom {
            r.extrusion_moves += 1;
            let b = &mut r.extrude_bounds;
            b[0] = b[0].min(nx.min(x));
            b[1] = b[1].min(ny.min(y));
            b[2] = b[2].max(nx.max(x));
            b[3] = b[3].max(ny.max(y));
            r.max_e_per_mm = r.max_e_per_mm.max(e / d);
            r.max_feed_mm_s = r.max_feed_mm_s.max(f);
            if (nx < area[0] || ny < area[1] || nx > area[2] || ny > area[3]) && r.problems.len() < 20 {
                r.problems.push(format!("line {}: extrusion at ({nx:.2}, {ny:.2}) outside the reachable area", ln + 1));
            }
            if f > max_feed && r.problems.len() < 20 {
                r.problems.push(format!("line {}: extruding at {f:.0} mm/s (limit {max_feed:.0})", ln + 1));
            }
            if e / d > max_e_per_mm && d > 0.2 && r.problems.len() < 20 {
                r.problems.push(format!("line {}: {:.3} mm filament per mm of travel (limit {max_e_per_mm:.3})", ln + 1, e / d));
            }
        }
        x = nx;
        y = ny;
    }
    r.ok = r.problems.is_empty() && r.extrusion_moves > 0;
    if r.extrusion_moves == 0 {
        r.problems.push("no extrusion moves found".into());
    }
    r
}
