//! SigmaDrift trajectory visualizer (eframe + egui).
//!
//! Mac-first desktop UI that calls `sigma_drift::generate`, draws the path with
//! speed-encoded colors, exposes Config knobs, shows metrics, and can overlay
//! a WindMouse comparison path. Second tab records real mouse trajectories.

mod gb_replay;
mod gb_selftest;
mod hid;

use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};
use hid::{backend_label, HidEvent, HidSession};
use serde::{Deserialize, Serialize};
use sigma_drift::{compute_metrics, windmouse, Config, Metrics, TrajectoryPoint};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

fn main() -> eframe::Result<()> {
    // Native DPI / Retina: leave pixels_per_point unset so eframe follows the
    // OS scale factor. Only constrain a sensible minimum; free resize otherwise.
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([640.0, 480.0])
            .with_title("SigmaDrift Trajectory Visualizer"),
        ..Default::default()
    };
    eframe::run_native(
        "SigmaDrift Trajectory Visualizer",
        options,
        Box::new(|cc| {
            let cjk_font_loaded = install_cjk_font(&cc.egui_ctx);
            let mut app = VizApp::default();
            app.cjk_font_loaded = cjk_font_loaded;
            Ok(Box::new(app))
        }),
    )
}

/// Install a system CJK font so egui can render Chinese labels instead of tofu.
///
/// The font is inserted first in both proportional and monospace families, so it
/// also acts as a fallback for the ASCII/numeric UI text. If no known system font
/// is available, callers use English labels for the small CJK-only UI section.
fn install_cjk_font(ctx: &egui::Context) -> bool {
    const FONT_PATHS: &[&str] = &[
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyh.ttf",
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
        r"C:\Windows\Fonts\msyhbd.ttc",
    ];

    let Some(path) = FONT_PATHS
        .iter()
        .find(|path| std::path::Path::new(path).is_file())
    else {
        eprintln!("No CJK system font found; using ASCII labels where needed");
        return false;
    };

    let Ok(bytes) = std::fs::read(path) else {
        eprintln!("Unable to read CJK system font at {path}; using ASCII labels where needed");
        return false;
    };

    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "macos_cjk".to_owned(),
        egui::FontData::from_owned(bytes).into(),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        if let Some(fonts_for_family) = fonts.families.get_mut(&family) {
            fonts_for_family.push("macos_cjk".to_owned());
        }
    }
    ctx.set_fonts(fonts);
    eprintln!("Using CJK system font: {path}");
    true
}

/// Map normalized speed `t ∈ [0,1]` to a blue→cyan→yellow→red gradient.
fn speed_color(t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    if t < 1.0 / 3.0 {
        let u = t * 3.0;
        Color32::from_rgb(0, (u * 255.0) as u8, 255)
    } else if t < 2.0 / 3.0 {
        let u = (t - 1.0 / 3.0) * 3.0;
        Color32::from_rgb((u * 255.0) as u8, 255, ((1.0 - u) * 255.0) as u8)
    } else {
        let u = (t - 2.0 / 3.0) * 3.0;
        Color32::from_rgb(255, ((1.0 - u) * 255.0) as u8, 0)
    }
}

fn segment_speeds(path: &[TrajectoryPoint]) -> Vec<f64> {
    let mut speeds = vec![0.0; path.len()];
    for i in 1..path.len() {
        let dx = path[i].x - path[i - 1].x;
        let dy = path[i].y - path[i - 1].y;
        let dt = path[i].t - path[i - 1].t;
        speeds[i] = if dt > 0.0 { dx.hypot(dy) / dt } else { 0.0 };
    }
    if path.len() > 1 {
        speeds[0] = speeds[1];
    }
    speeds
}

fn recorded_speeds(points: &[RecordedPoint]) -> Vec<f64> {
    let mut speeds = vec![0.0; points.len()];
    for i in 1..points.len() {
        let dx = points[i].x - points[i - 1].x;
        let dy = points[i].y - points[i - 1].y;
        let dt = points[i].t_ms - points[i - 1].t_ms;
        speeds[i] = if dt > 0.0 { dx.hypot(dy) / dt } else { 0.0 };
    }
    if points.len() > 1 {
        speeds[0] = speeds[1];
    }
    speeds
}

fn peak_speed(speeds: &[f64]) -> f64 {
    speeds.iter().copied().fold(0.0_f64, f64::max).max(1e-9)
}

fn history_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .join("history.json")
}

fn now_local_stamp() -> String {
    // Local wall clock for display/id (box/user zone). Prefer chrono-free formatting.
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Format via libc localtime equivalent: use egui-less simple UTC offset guess.
    // On macOS we can shell out is overkill — use a readable unix-based stamp plus
    // a monotonic counter suffix elsewhere. Prefer `strftime` via `chrono` is not
    // a dep; approximate with seconds for id uniqueness and a human label.
    format!("{secs}")
}

fn human_time_label() -> String {
    // Best-effort local datetime via `date` is avoided; use ISO-ish from system.
    use std::process::Command;
    if let Ok(out) = Command::new("date").args(["+%Y-%m-%d %H:%M:%S"]).output() {
        if out.status.success() {
            return String::from_utf8_lossy(&out.stdout).trim().to_string();
        }
    }
    now_local_stamp()
}

// ---------------------------------------------------------------------------
// Record data model
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RecordedPoint {
    x: f64,
    y: f64,
    t_ms: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum RecordedEvent {
    Move {
        x: f64,
        y: f64,
        t_ms: f64,
    },
    Click {
        x: f64,
        y: f64,
        t_ms: f64,
        button: String,
    },
}

#[derive(Clone, Debug)]
struct ClickEndpoint {
    label: &'static str,
    x: f64,
    y: f64,
    t_ms: f64,
    button: Option<String>,
    /// True when derived from start/end markers rather than a Click event.
    from_marker: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Recording {
    id: String,
    name: String,
    created_at: String,
    start: [f64; 2],
    end: [f64; 2],
    points: Vec<RecordedPoint>,
    events: Vec<RecordedEvent>,
    /// Coalesced IOHID (Mac) / Raw Input (Win) mouse samples.
    /// Shape: `{ t_ms, dx, dy, buttons }` — see `hid::HidEvent`.
    #[serde(default)]
    hid_events: Vec<HidEvent>,
}

impl Recording {
    fn duration_ms(&self) -> f64 {
        self.points
            .last()
            .map(|p| p.t_ms)
            .or_else(|| {
                self.events.last().map(|e| match e {
                    RecordedEvent::Move { t_ms, .. } | RecordedEvent::Click { t_ms, .. } => *t_ms,
                })
            })
            .unwrap_or(0.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AppTab {
    Generate,
    Record,
    GhostBox,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RecordPhase {
    Idle,
    Recording,
}

enum DragTarget {
    None,
    Start,
    End,
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

struct VizApp {
    tab: AppTab,

    // ---- Generate tab ----
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    seed_text: String,
    use_seed: bool,
    cfg: Config,
    sigma_path: Vec<TrajectoryPoint>,
    sigma_speeds: Vec<f64>,
    wind_path: Vec<TrajectoryPoint>,
    wind_speeds: Vec<f64>,
    metrics: Metrics,
    show_windmouse: bool,
    show_points: bool,
    show_start_end: bool,
    /// When true, stretch-fit path bounds into the canvas (legacy).
    /// When false (default), 1:1 world→screen: canvas.min + (x, y), Y down.
    fit_path_to_canvas: bool,
    point_radius: f32,
    line_width: f32,
    /// Last central-panel canvas size in logical points (for Fit ends).
    last_canvas_size: Vec2,
    animating: bool,
    anim_index: usize,
    anim_speed: f32,
    anim_accum: f32,
    status: String,

    // ---- Record tab ----
    rec_start: [f64; 2],
    rec_end: [f64; 2],
    rec_phase: RecordPhase,
    rec_started_at: Option<Instant>,
    /// Live buffer while recording (also used after stop before append).
    rec_live_points: Vec<RecordedPoint>,
    rec_live_events: Vec<RecordedEvent>,
    /// Currently viewed / played recording (loaded from history or just finished).
    rec_current: Option<Recording>,
    rec_speeds: Vec<f64>,
    rec_history: Vec<Recording>,
    rec_selected_id: Option<String>,
    rec_drag: DragTarget,
    rec_animating: bool,
    rec_anim_index: usize,
    /// Wall-clock playback origin when using timestamps.
    rec_anim_wall_start: Option<Instant>,
    /// Prefer timestamp-based playback when true.
    rec_anim_use_timestamps: bool,
    rec_anim_speed: f32,
    rec_anim_accum: f32,
    rec_show_points: bool,
    rec_rename_buf: String,
    rec_last_canvas: Rect,
    rec_status: String,
    rec_id_counter: u64,
    /// Active HID capture session while `RecordPhase::Recording`.
    hid_session: Option<HidSession>,
    /// Last HID backend status / error (shown on Record tab).
    rec_hid_status: String,
    /// Whether a CJK-capable system font was installed into egui.
    cjk_font_loaded: bool,
    /// GhostBox replay: snap to recording end via MoveMouseTo after relative path.
    gb_snap_final: bool,
    /// GhostBox replay: skip per-step timeline waits (coalesced relative still emitted).
    gb_fast: bool,
    /// Background GhostBox HID replay status (shared with worker thread).
    gb_replay: Arc<Mutex<gb_replay::GbReplayShared>>,
    /// Background GhostBox absolute-move self-test status.
    gb_selftest: Arc<Mutex<gb_selftest::GbSelfTestShared>>,
}

impl Default for VizApp {
    fn default() -> Self {
        let mut app = Self {
            tab: AppTab::Generate,
            x0: 80.0,
            y0: 80.0,
            x1: 520.0,
            y1: 380.0,
            seed_text: "42".into(),
            use_seed: true,
            cfg: Config::default(),
            sigma_path: Vec::new(),
            sigma_speeds: Vec::new(),
            wind_path: Vec::new(),
            wind_speeds: Vec::new(),
            metrics: Metrics::default(),
            show_windmouse: false,
            show_points: true,
            show_start_end: true,
            fit_path_to_canvas: false,
            point_radius: 2.5,
            line_width: 2.0,
            last_canvas_size: Vec2::ZERO,
            animating: false,
            anim_index: 0,
            anim_speed: 40.0,
            anim_accum: 0.0,
            status: "Click Generate to sample a trajectory.".into(),

            rec_start: [80.0, 80.0],
            rec_end: [520.0, 380.0],
            rec_phase: RecordPhase::Idle,
            rec_started_at: None,
            rec_live_points: Vec::new(),
            rec_live_events: Vec::new(),
            rec_current: None,
            rec_speeds: Vec::new(),
            rec_history: Vec::new(),
            rec_selected_id: None,
            rec_drag: DragTarget::None,
            rec_animating: false,
            rec_anim_index: 0,
            rec_anim_wall_start: None,
            rec_anim_use_timestamps: true,
            rec_anim_speed: 40.0,
            rec_anim_accum: 0.0,
            rec_show_points: true,
            rec_rename_buf: String::new(),
            rec_last_canvas: Rect::NOTHING,
            rec_status: "Drag start (green) / end (red), then click Start to record.".into(),
            rec_id_counter: 0,
            hid_session: None,
            rec_hid_status: format!("{} — idle", backend_label()),
            cjk_font_loaded: false,
            gb_snap_final: true,
            gb_fast: true,
            gb_replay: Arc::new(Mutex::new(gb_replay::GbReplayShared::default())),
            gb_selftest: Arc::new(Mutex::new(gb_selftest::GbSelfTestShared::default())),
        };
        app.regenerate();
        app.load_history();
        app
    }
}

impl VizApp {
    // -----------------------------------------------------------------------
    // Generate helpers
    // -----------------------------------------------------------------------

    fn parse_seed(&self) -> Option<u64> {
        if !self.use_seed {
            return None;
        }
        match self.seed_text.trim().parse::<u64>() {
            Ok(0) => Some(0),
            Ok(s) => Some(s),
            Err(_) => None,
        }
    }

    fn regenerate(&mut self) {
        let seed = self.parse_seed();
        if self.use_seed && seed.is_none() {
            self.status = format!("Invalid seed: {:?}", self.seed_text);
            return;
        }

        self.sigma_path =
            sigma_drift::generate(self.x0, self.y0, self.x1, self.y1, &self.cfg, seed);
        self.sigma_speeds = segment_speeds(&self.sigma_path);

        if self.show_windmouse {
            self.wind_path = windmouse::generate(self.x0, self.y0, self.x1, self.y1, seed);
            self.wind_speeds = segment_speeds(&self.wind_path);
        } else {
            self.wind_path.clear();
            self.wind_speeds.clear();
        }

        let dist = (self.x1 - self.x0).hypot(self.y1 - self.y0);
        self.metrics = compute_metrics(
            &self.sigma_path,
            self.x1,
            self.y1,
            self.cfg.target_width,
            dist,
        );

        self.animating = false;
        self.anim_index = self.sigma_path.len();
        self.anim_accum = 0.0;
        self.status = format!(
            "Generated {} SigmaDrift points (seed={}).",
            self.sigma_path.len(),
            seed.map(|s| s.to_string())
                .unwrap_or_else(|| "entropy".into())
        );
    }

    fn reset_defaults(&mut self) {
        self.x0 = 80.0;
        self.y0 = 80.0;
        self.x1 = 520.0;
        self.y1 = 380.0;
        self.seed_text = "42".into();
        self.use_seed = true;
        self.cfg = Config::default();
        self.show_windmouse = false;
        self.show_points = true;
        self.show_start_end = true;
        self.fit_path_to_canvas = false;
        self.point_radius = 2.5;
        self.line_width = 2.0;
        self.anim_speed = 40.0;
        self.regenerate();
        self.status = "Reset to defaults.".into();
    }

    fn start_animation(&mut self) {
        if self.sigma_path.is_empty() {
            self.regenerate();
        }
        self.animating = true;
        self.anim_index = 1.min(self.sigma_path.len());
        self.anim_accum = 0.0;
        self.status = "Animating…".into();
    }

    fn export_csv(&mut self) {
        if self.sigma_path.is_empty() {
            self.status = "Nothing to export — generate first.".into();
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .set_file_name("sigma_drift_trajectory.csv")
            .add_filter("CSV", &["csv"])
            .save_file()
        else {
            self.status = "Export cancelled.".into();
            return;
        };

        let mut out = String::from("index,x,y,t_ms,speed_px_per_ms\n");
        for (i, p) in self.sigma_path.iter().enumerate() {
            let spd = self.sigma_speeds.get(i).copied().unwrap_or(0.0);
            out.push_str(&format!("{},{},{},{},{}\n", i, p.x, p.y, p.t, spd));
        }
        match std::fs::write(&path, out) {
            Ok(()) => {
                self.status = format!(
                    "Exported {} points to {}.",
                    self.sigma_path.len(),
                    path.display()
                )
            }
            Err(e) => self.status = format!("Export failed: {e}"),
        }
    }

    fn visible_count(&self) -> usize {
        if self.animating {
            self.anim_index.clamp(0, self.sigma_path.len())
        } else {
            self.sigma_path.len()
        }
    }

    fn world_bounds(&self) -> Rect {
        let mut min_x = self.x0.min(self.x1);
        let mut max_x = self.x0.max(self.x1);
        let mut min_y = self.y0.min(self.y1);
        let mut max_y = self.y0.max(self.y1);
        for p in self.sigma_path.iter().chain(self.wind_path.iter()) {
            min_x = min_x.min(p.x);
            max_x = max_x.max(p.x);
            min_y = min_y.min(p.y);
            max_y = max_y.max(p.y);
        }
        let pad = 40.0;
        Rect::from_min_max(
            Pos2::new((min_x - pad) as f32, (min_y - pad) as f32),
            Pos2::new((max_x + pad) as f32, (max_y + pad) as f32),
        )
    }

    fn world_to_screen(&self, world: Pos2, bounds: Rect, canvas: Rect) -> Pos2 {
        if self.fit_path_to_canvas {
            let nx = if bounds.width() > 0.0 {
                (world.x - bounds.min.x) / bounds.width()
            } else {
                0.5
            };
            let ny = if bounds.height() > 0.0 {
                (world.y - bounds.min.y) / bounds.height()
            } else {
                0.5
            };
            Pos2::new(
                canvas.min.x + nx * canvas.width(),
                canvas.min.y + (1.0 - ny) * canvas.height(),
            )
        } else {
            Pos2::new(canvas.min.x + world.x, canvas.min.y + world.y)
        }
    }

    fn fit_ends_into_canvas(&mut self) {
        let size = self.last_canvas_size;
        if size.x < 8.0 || size.y < 8.0 {
            self.status = "Canvas size unknown — wait for a frame, then retry.".into();
            return;
        }
        let pad = 40.0_f64;
        let w = size.x as f64;
        let h = size.y as f64;
        let pad_x = pad.min(w * 0.25);
        let pad_y = pad.min(h * 0.25);
        self.x0 = pad_x;
        self.y0 = pad_y;
        self.x1 = (w - pad_x).max(pad_x + 1.0);
        self.y1 = (h - pad_y).max(pad_y + 1.0);
        self.status = format!(
            "Fit ends into canvas ({:.0}×{:.0} pt): ({:.0},{:.0}) → ({:.0},{:.0}).",
            w, h, self.x0, self.y0, self.x1, self.y1
        );
    }

    // -----------------------------------------------------------------------
    // Record helpers
    // -----------------------------------------------------------------------

    fn load_history(&mut self) {
        let path = history_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Vec<Recording>>(&text) {
                Ok(list) => {
                    self.rec_id_counter = list
                        .iter()
                        .filter_map(|r| {
                            r.id.strip_prefix("rec_")
                                .and_then(|s| s.parse::<u64>().ok())
                        })
                        .max()
                        .unwrap_or(0);
                    self.rec_history = list;
                    self.rec_status = format!(
                        "Loaded {} recording(s) from {}.",
                        self.rec_history.len(),
                        path.display()
                    );
                }
                Err(e) => {
                    self.rec_status = format!("History parse error: {e}");
                }
            },
            Err(_) => {
                // No file yet — fine.
            }
        }
    }

    fn save_history(&mut self) {
        let path = history_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(&self.rec_history) {
            Ok(text) => {
                if let Err(e) = std::fs::write(&path, text) {
                    self.rec_status = format!("Failed to save history: {e}");
                }
            }
            Err(e) => self.rec_status = format!("Failed to serialize history: {e}"),
        }
    }

    fn rec_elapsed_ms(&self) -> f64 {
        self.rec_started_at
            .map(|t| t.elapsed().as_secs_f64() * 1000.0)
            .unwrap_or(0.0)
    }

    fn start_recording(&mut self) {
        if self.rec_phase == RecordPhase::Recording {
            return;
        }
        self.rec_animating = false;
        self.rec_phase = RecordPhase::Recording;
        self.rec_live_points.clear();
        self.rec_live_events.clear();
        self.rec_current = None;
        self.rec_speeds.clear();
        self.rec_drag = DragTarget::None;

        // Drop any stale session, then start IOHID (Mac) / Raw Input (Win).
        // Prefer HID epoch Instant so pixel t_ms and HID t_ms share one origin.
        self.hid_session = None;
        match HidSession::start() {
            Ok(session) => {
                self.rec_started_at = Some(session.epoch());
                self.rec_hid_status = session.status_message().to_string();
                self.hid_session = Some(session);
            }
            Err(e) => {
                self.rec_started_at = Some(Instant::now());
                self.rec_hid_status = e;
            }
        }

        // Seed with start marker position at t=0.
        let t = 0.0;
        self.rec_live_points.push(RecordedPoint {
            x: self.rec_start[0],
            y: self.rec_start[1],
            t_ms: t,
        });
        self.rec_live_events.push(RecordedEvent::Move {
            x: self.rec_start[0],
            y: self.rec_start[1],
            t_ms: t,
        });
        self.rec_status = format!(
            "REC — move the mouse, then click End. ({})",
            self.rec_hid_status
        );
    }

    fn stop_recording(&mut self) {
        if self.rec_phase != RecordPhase::Recording {
            return;
        }
        let t = self.rec_elapsed_ms();
        // Snap a final sample at end marker if last point is far, else keep last pointer.
        // Always push end marker as final intended target sample.
        self.rec_live_points.push(RecordedPoint {
            x: self.rec_end[0],
            y: self.rec_end[1],
            t_ms: t,
        });
        self.rec_live_events.push(RecordedEvent::Move {
            x: self.rec_end[0],
            y: self.rec_end[1],
            t_ms: t,
        });

        // Slice HID to [0, end_ts] (inclusive — keeps click edges on the boundary).
        let hid_events = if let Some(session) = self.hid_session.take() {
            let ev = session.stop_and_slice(0.0, t);
            self.rec_hid_status = format!("{} — captured {} events", backend_label(), ev.len());
            ev
        } else {
            if !self.rec_hid_status.contains("IOHIDManagerOpen")
                && !self.rec_hid_status.contains("unavailable")
                && !self.rec_hid_status.contains("failed")
                && !self.rec_hid_status.contains("timed out")
                && !self.rec_hid_status.contains("RegisterRawInput")
            {
                self.rec_hid_status = format!("{} — no session", backend_label());
            }
            Vec::new()
        };

        self.rec_phase = RecordPhase::Idle;
        self.rec_started_at = None;
        self.rec_id_counter = self.rec_id_counter.saturating_add(1);
        let id = format!("rec_{}", self.rec_id_counter);
        let created = human_time_label();
        let name = format!("Recording {}", self.rec_id_counter);
        let hid_n = hid_events.len();
        let recording = Recording {
            id: id.clone(),
            name: name.clone(),
            created_at: created,
            start: self.rec_start,
            end: self.rec_end,
            points: self.rec_live_points.clone(),
            events: self.rec_live_events.clone(),
            hid_events,
        };
        self.rec_speeds = recorded_speeds(&recording.points);
        let n = recording.points.len();
        let dur = recording.duration_ms();
        self.rec_history.push(recording.clone());
        self.rec_current = Some(recording);
        self.rec_selected_id = Some(id);
        self.rec_rename_buf = name;
        self.rec_anim_index = n;
        self.save_history();
        self.rec_status = format!(
            "Recorded {n} points + {hid_n} HID over {dur:.0} ms. Saved. ({})",
            self.rec_hid_status
        );
    }

    fn load_recording(&mut self, id: &str) {
        if let Some(rec) = self.rec_history.iter().find(|r| r.id == id).cloned() {
            self.rec_start = rec.start;
            self.rec_end = rec.end;
            self.rec_speeds = recorded_speeds(&rec.points);
            self.rec_rename_buf = rec.name.clone();
            self.rec_selected_id = Some(rec.id.clone());
            self.rec_animating = false;
            self.rec_anim_index = rec.points.len();
            self.rec_anim_wall_start = None;
            let n = rec.points.len();
            let dur = rec.duration_ms();
            self.rec_status = format!("Loaded '{}' — {n} pts, {dur:.0} ms.", rec.name);
            self.rec_current = Some(rec);
        }
    }

    fn delete_selected_recording(&mut self) {
        let Some(id) = self.rec_selected_id.clone() else {
            self.rec_status = "No recording selected.".into();
            return;
        };
        self.rec_history.retain(|r| r.id != id);
        if self.rec_current.as_ref().map(|r| r.id.as_str()) == Some(id.as_str()) {
            self.rec_current = None;
            self.rec_speeds.clear();
        }
        self.rec_selected_id = None;
        self.rec_rename_buf.clear();
        self.save_history();
        self.rec_status = format!("Deleted {id}.");
    }

    fn rename_selected_recording(&mut self) {
        let Some(id) = self.rec_selected_id.clone() else {
            return;
        };
        let name = self.rec_rename_buf.trim().to_string();
        if name.is_empty() {
            self.rec_status = "Name cannot be empty.".into();
            return;
        }
        if let Some(r) = self.rec_history.iter_mut().find(|r| r.id == id) {
            r.name = name.clone();
        }
        if let Some(r) = self.rec_current.as_mut() {
            if r.id == id {
                r.name = name;
            }
        }
        self.save_history();
        self.rec_status = "Renamed.".into();
    }

    fn start_rec_playback(&mut self) {
        let Some(rec) = self.rec_current.as_ref() else {
            self.rec_status = "Nothing to play — record or select from history.".into();
            return;
        };
        if rec.points.is_empty() {
            self.rec_status = "Recording has no points.".into();
            return;
        }
        self.rec_animating = true;
        self.rec_anim_index = 1.min(rec.points.len());
        self.rec_anim_accum = 0.0;
        self.rec_anim_wall_start = Some(Instant::now());
        self.rec_status = "Playing back…".into();
    }

    fn start_ghostbox_replay(&mut self) {
        let Some(rec) = self.rec_current.as_ref() else {
            self.rec_status = if self.cjk_font_loaded {
                "没有可回放的录制 — 请先录制或从历史选择。".into()
            } else {
                "Nothing to replay — record or select from history.".into()
            };
            return;
        };
        if rec.hid_events.is_empty() {
            self.rec_status = if self.cjk_font_loaded {
                "hid_events 为空 — GhostBox 回放需要 Raw Input 录制的相对轨迹。".into()
            } else {
                "hid_events empty — GhostBox replay needs Raw Input relative samples.".into()
            };
            return;
        }

        {
            let g = self.gb_replay.lock().unwrap_or_else(|e| e.into_inner());
            if g.busy {
                self.rec_status = g.status.clone();
                return;
            }
        }

        // Stop pixel animation so canvas isn't competing for attention.
        self.rec_animating = false;
        self.rec_anim_wall_start = None;

        let snap = self.gb_snap_final;
        let fast = self.gb_fast;
        let n = rec.hid_events.len();
        gb_replay::spawn_hid_replay(
            rec.hid_events.clone(),
            rec.start,
            rec.end,
            snap,
            fast,
            Arc::clone(&self.gb_replay),
        );
        self.rec_status = if self.cjk_font_loaded {
            format!("幽灵盒回放已启动（{n} 个 HID 步，snap_final={snap}，fast={fast}）…")
        } else {
            format!("GhostBox replay started ({n} HID steps, snap_final={snap}, fast={fast})…")
        };
    }

    fn rec_visible_count(&self) -> usize {
        let n = self
            .rec_current
            .as_ref()
            .map(|r| r.points.len())
            .unwrap_or_else(|| self.rec_live_points.len());
        if self.rec_phase == RecordPhase::Recording {
            self.rec_live_points.len()
        } else if self.rec_animating {
            self.rec_anim_index.clamp(0, n)
        } else {
            n
        }
    }

    fn rec_points_view(&self) -> &[RecordedPoint] {
        if self.rec_phase == RecordPhase::Recording {
            &self.rec_live_points
        } else if let Some(rec) = self.rec_current.as_ref() {
            &rec.points
        } else {
            &self.rec_live_points
        }
    }

    fn rec_events_view(&self) -> &[RecordedEvent] {
        if self.rec_phase == RecordPhase::Recording {
            &self.rec_live_events
        } else if let Some(rec) = self.rec_current.as_ref() {
            &rec.events
        } else {
            &self.rec_live_events
        }
    }

    fn canvas_to_world(canvas: Rect, screen: Pos2) -> [f64; 2] {
        [
            (screen.x - canvas.min.x) as f64,
            (screen.y - canvas.min.y) as f64,
        ]
    }

    fn world_to_canvas(canvas: Rect, world: [f64; 2]) -> Pos2 {
        Pos2::new(
            canvas.min.x + world[0] as f32,
            canvas.min.y + world[1] as f32,
        )
    }

    // -----------------------------------------------------------------------
    // Generate canvas / panels (unchanged behavior)
    // -----------------------------------------------------------------------

    fn draw_generate_canvas(&mut self, ui: &mut egui::Ui) {
        let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::hover());
        let canvas = response.rect;
        self.last_canvas_size = canvas.size();

        painter.rect_filled(canvas, 4.0, Color32::from_gray(24));
        painter.rect_stroke(
            canvas,
            4.0,
            Stroke::new(1.0_f32, Color32::from_gray(60)),
            StrokeKind::Outside,
        );

        let painter = painter.with_clip_rect(canvas);

        let bounds = self.world_bounds();
        let peak = peak_speed(&self.sigma_speeds);
        let ppp = ui.ctx().pixels_per_point();

        {
            let grid_col = Color32::from_gray(40);
            for i in 1..8 {
                let t = i as f32 / 8.0;
                let x = canvas.min.x + t * canvas.width();
                let y = canvas.min.y + t * canvas.height();
                painter.line_segment(
                    [Pos2::new(x, canvas.min.y), Pos2::new(x, canvas.max.y)],
                    Stroke::new(1.0_f32, grid_col),
                );
                painter.line_segment(
                    [Pos2::new(canvas.min.x, y), Pos2::new(canvas.max.x, y)],
                    Stroke::new(1.0_f32, grid_col),
                );
            }
        }

        if self.show_windmouse && self.wind_path.len() >= 2 {
            let peak_w = peak_speed(&self.wind_speeds);
            for i in 1..self.wind_path.len() {
                let a = self.world_to_screen(
                    Pos2::new(
                        self.wind_path[i - 1].x as f32,
                        self.wind_path[i - 1].y as f32,
                    ),
                    bounds,
                    canvas,
                );
                let b = self.world_to_screen(
                    Pos2::new(self.wind_path[i].x as f32, self.wind_path[i].y as f32),
                    bounds,
                    canvas,
                );
                let t = (self.wind_speeds[i] / peak_w) as f32;
                let mut c = speed_color(t);
                c = Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 90);
                painter.line_segment([a, b], Stroke::new(1.5_f32, c));
            }
        }

        let n = self.visible_count();
        if n >= 2 {
            for i in 1..n {
                let a = self.world_to_screen(
                    Pos2::new(
                        self.sigma_path[i - 1].x as f32,
                        self.sigma_path[i - 1].y as f32,
                    ),
                    bounds,
                    canvas,
                );
                let b = self.world_to_screen(
                    Pos2::new(self.sigma_path[i].x as f32, self.sigma_path[i].y as f32),
                    bounds,
                    canvas,
                );
                let t = (self.sigma_speeds[i] / peak) as f32;
                painter.line_segment([a, b], Stroke::new(self.line_width, speed_color(t)));
            }
        }

        if self.show_points && n > 0 {
            for i in 0..n {
                let p = self.world_to_screen(
                    Pos2::new(self.sigma_path[i].x as f32, self.sigma_path[i].y as f32),
                    bounds,
                    canvas,
                );
                let t = (self.sigma_speeds[i] / peak) as f32;
                painter.circle_filled(p, self.point_radius, speed_color(t));
            }
        }

        if self.show_start_end {
            let s = self.world_to_screen(Pos2::new(self.x0 as f32, self.y0 as f32), bounds, canvas);
            let e = self.world_to_screen(Pos2::new(self.x1 as f32, self.y1 as f32), bounds, canvas);
            painter.circle_stroke(
                s,
                7.0,
                Stroke::new(2.0_f32, Color32::from_rgb(80, 220, 120)),
            );
            painter.circle_filled(s, 3.0, Color32::from_rgb(80, 220, 120));
            painter.circle_stroke(
                e,
                7.0,
                Stroke::new(2.0_f32, Color32::from_rgb(255, 100, 100)),
            );
            painter.circle_filled(e, 3.0, Color32::from_rgb(255, 100, 100));
            painter.line_segment(
                [s, e],
                Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(200, 200, 200, 60)),
            );
        }

        {
            let legend = Rect::from_min_size(
                Pos2::new(canvas.max.x - 140.0, canvas.min.y + 12.0),
                Vec2::new(120.0, 14.0),
            );
            let steps = 64;
            for i in 0..steps {
                let t = i as f32 / (steps - 1) as f32;
                let x0 = legend.min.x + t * legend.width();
                let x1 = legend.min.x + ((i + 1) as f32 / steps as f32) * legend.width();
                painter.rect_filled(
                    Rect::from_min_max(Pos2::new(x0, legend.min.y), Pos2::new(x1, legend.max.y)),
                    0.0,
                    speed_color(t),
                );
            }
            painter.text(
                Pos2::new(legend.min.x, legend.max.y + 2.0),
                egui::Align2::LEFT_TOP,
                "slow",
                egui::FontId::proportional(11.0),
                Color32::LIGHT_GRAY,
            );
            painter.text(
                Pos2::new(legend.max.x, legend.max.y + 2.0),
                egui::Align2::RIGHT_TOP,
                "fast",
                egui::FontId::proportional(11.0),
                Color32::LIGHT_GRAY,
            );
        }

        {
            let lw = canvas.width();
            let lh = canvas.height();
            let pw = lw * ppp;
            let ph = lh * ppp;
            let mode = if self.fit_path_to_canvas {
                "fit"
            } else {
                "1:1"
            };
            let label = format!(
                "{:.0}×{:.0} pt · {:.0}×{:.0} px @{:.1}  [{mode}]",
                lw, lh, pw, ph, ppp
            );
            painter.text(
                Pos2::new(canvas.min.x + 10.0, canvas.min.y + 8.0),
                egui::Align2::LEFT_TOP,
                label,
                egui::FontId::monospace(12.0),
                Color32::from_rgb(180, 220, 255),
            );
        }

        if let Some(pos) = response.hover_pos() {
            if canvas.contains(pos) && n > 0 {
                let mut best_i = 0usize;
                let mut best_d = f32::MAX;
                for i in 0..n {
                    let p = self.world_to_screen(
                        Pos2::new(self.sigma_path[i].x as f32, self.sigma_path[i].y as f32),
                        bounds,
                        canvas,
                    );
                    let d = p.distance(pos);
                    if d < best_d {
                        best_d = d;
                        best_i = i;
                    }
                }
                if best_d < 12.0 {
                    let pt = &self.sigma_path[best_i];
                    let spd = self.sigma_speeds[best_i];
                    egui::show_tooltip_at_pointer(
                        ui.ctx(),
                        ui.layer_id(),
                        egui::Id::new("pt_tip"),
                        |ui| {
                            ui.label(format!(
                                "#{}  ({:.1}, {:.1})  t={:.1} ms  v={:.4} px/ms",
                                best_i, pt.x, pt.y, pt.t, spd
                            ));
                        },
                    );
                }
            }
        }
    }

    fn config_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Path");
        egui::Grid::new("endpoints")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Start X");
                ui.add(egui::DragValue::new(&mut self.x0).speed(1.0));
                ui.end_row();
                ui.label("Start Y");
                ui.add(egui::DragValue::new(&mut self.y0).speed(1.0));
                ui.end_row();
                ui.label("End X");
                ui.add(egui::DragValue::new(&mut self.x1).speed(1.0));
                ui.end_row();
                ui.label("End Y");
                ui.add(egui::DragValue::new(&mut self.y1).speed(1.0));
                ui.end_row();
            });

        if ui.button("Fit ends into canvas").clicked() {
            self.fit_ends_into_canvas();
        }

        ui.horizontal(|ui| {
            ui.checkbox(&mut self.use_seed, "Seed");
            ui.add_enabled(
                self.use_seed,
                egui::TextEdit::singleline(&mut self.seed_text).desired_width(100.0),
            );
        });

        ui.separator();
        ui.heading("Fitts / target");
        slider_f64(ui, "fitts_a", &mut self.cfg.fitts_a, 0.0..=200.0);
        slider_f64(ui, "fitts_b", &mut self.cfg.fitts_b, 0.0..=400.0);
        slider_f64(ui, "target_width", &mut self.cfg.target_width, 1.0..=80.0);

        ui.separator();
        ui.heading("Primary / reach");
        slider_f64(
            ui,
            "undershoot_min",
            &mut self.cfg.undershoot_min,
            0.5..=1.0,
        );
        slider_f64(
            ui,
            "undershoot_max",
            &mut self.cfg.undershoot_max,
            0.5..=1.05,
        );
        slider_f64(
            ui,
            "peak_time_ratio",
            &mut self.cfg.peak_time_ratio,
            0.1..=0.7,
        );
        slider_f64(
            ui,
            "primary_sigma_min",
            &mut self.cfg.primary_sigma_min,
            0.05..=0.5,
        );
        slider_f64(
            ui,
            "primary_sigma_max",
            &mut self.cfg.primary_sigma_max,
            0.05..=0.6,
        );

        ui.separator();
        ui.heading("Corrections");
        slider_f64(
            ui,
            "overshoot_prob",
            &mut self.cfg.overshoot_prob,
            0.0..=1.0,
        );
        slider_f64(ui, "overshoot_min", &mut self.cfg.overshoot_min, 1.0..=1.3);
        slider_f64(ui, "overshoot_max", &mut self.cfg.overshoot_max, 1.0..=1.4);
        slider_f64(
            ui,
            "correction_sigma_min",
            &mut self.cfg.correction_sigma_min,
            0.05..=0.4,
        );
        slider_f64(
            ui,
            "correction_sigma_max",
            &mut self.cfg.correction_sigma_max,
            0.05..=0.5,
        );
        slider_f64(
            ui,
            "second_correction_prob",
            &mut self.cfg.second_correction_prob,
            0.0..=1.0,
        );

        ui.separator();
        ui.heading("Curvature / OU / tremor");
        slider_f64(
            ui,
            "curvature_scale",
            &mut self.cfg.curvature_scale,
            0.0..=0.15,
        );
        slider_f64(ui, "ou_theta", &mut self.cfg.ou_theta, 0.1..=15.0);
        slider_f64(ui, "ou_sigma", &mut self.cfg.ou_sigma, 0.0..=8.0);
        slider_f64(
            ui,
            "tremor_freq_min",
            &mut self.cfg.tremor_freq_min,
            1.0..=20.0,
        );
        slider_f64(
            ui,
            "tremor_freq_max",
            &mut self.cfg.tremor_freq_max,
            1.0..=25.0,
        );
        slider_f64(
            ui,
            "tremor_amp_min",
            &mut self.cfg.tremor_amp_min,
            0.0..=3.0,
        );
        slider_f64(
            ui,
            "tremor_amp_max",
            &mut self.cfg.tremor_amp_max,
            0.0..=5.0,
        );
        slider_f64(ui, "sdn_k", &mut self.cfg.sdn_k, 0.0..=0.3);

        ui.separator();
        ui.heading("Sampling");
        slider_f64(
            ui,
            "sample_dt_mean",
            &mut self.cfg.sample_dt_mean,
            1.0..=30.0,
        );
        slider_f64(ui, "gamma_shape", &mut self.cfg.gamma_shape, 0.5..=12.0);

        ui.separator();
        ui.heading("Display");
        ui.checkbox(&mut self.fit_path_to_canvas, "Fit path to canvas");
        ui.label(
            RichText::new(
                "Off (default) = 1:1 world→screen; enlarging the window reveals more pixels. On = stretch-fit.",
            )
            .small()
            .weak(),
        );
        ui.checkbox(&mut self.show_windmouse, "Overlay WindMouse");
        ui.checkbox(&mut self.show_points, "Show sample markers");
        ui.checkbox(&mut self.show_start_end, "Show start/end");
        ui.add(egui::Slider::new(&mut self.point_radius, 1.0..=6.0).text("point radius"));
        ui.add(egui::Slider::new(&mut self.line_width, 0.5..=5.0).text("line width"));
        ui.add(egui::Slider::new(&mut self.anim_speed, 5.0..=200.0).text("anim pts/s"));
    }

    fn metrics_panel(&self, ui: &mut egui::Ui) {
        ui.heading("Metrics (SigmaDrift)");
        let m = &self.metrics;
        egui::Grid::new("metrics")
            .num_columns(2)
            .spacing([12.0, 4.0])
            .striped(true)
            .show(ui, |ui| {
                metric_row(ui, "movement_time", format!("{:.2} ms", m.movement_time));
                metric_row(ui, "path_length", format!("{:.2} px", m.path_length));
                metric_row(
                    ui,
                    "straight_distance",
                    format!("{:.2} px", m.straight_distance),
                );
                metric_row(ui, "path_efficiency", format!("{:.4}", m.path_efficiency));
                metric_row(ui, "peak_speed", format!("{:.4} px/ms", m.peak_speed));
                metric_row(ui, "time_to_peak", format!("{:.2} ms", m.time_to_peak));
                metric_row(ui, "num_submovements", format!("{}", m.num_submovements));
                metric_row(ui, "endpoint_error", format!("{:.3} px", m.endpoint_error));
                metric_row(
                    ui,
                    "fitts_predicted_mt",
                    format!("{:.2} ms", m.fitts_predicted_mt),
                );
                metric_row(ui, "num_samples", format!("{}", self.sigma_path.len()));
            });

        if self.show_windmouse && !self.wind_path.is_empty() {
            ui.separator();
            ui.label(RichText::new(format!("WindMouse samples: {}", self.wind_path.len())).weak());
        }
    }

    // -----------------------------------------------------------------------
    // Record UI
    // -----------------------------------------------------------------------

    fn record_side_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Record");
        ui.label(RichText::new(&self.rec_status).weak());
        ui.label(RichText::new(backend_label()).small().weak());
        ui.label(RichText::new(&self.rec_hid_status).small().weak());
        ui.separator();

        let recording = self.rec_phase == RecordPhase::Recording;

        egui::Grid::new("rec_endpoints")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Start X");
                ui.add_enabled(
                    !recording,
                    egui::DragValue::new(&mut self.rec_start[0]).speed(1.0),
                );
                ui.end_row();
                ui.label("Start Y");
                ui.add_enabled(
                    !recording,
                    egui::DragValue::new(&mut self.rec_start[1]).speed(1.0),
                );
                ui.end_row();
                ui.label("End X");
                ui.add_enabled(
                    !recording,
                    egui::DragValue::new(&mut self.rec_end[0]).speed(1.0),
                );
                ui.end_row();
                ui.label("End Y");
                ui.add_enabled(
                    !recording,
                    egui::DragValue::new(&mut self.rec_end[1]).speed(1.0),
                );
                ui.end_row();
            });

        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !recording,
                    egui::Button::new(RichText::new("Start").strong()),
                )
                .on_hover_text("Begin recording pointer samples")
                .clicked()
            {
                self.start_recording();
            }
            if ui
                .add_enabled(recording, egui::Button::new(RichText::new("End").strong()))
                .on_hover_text("Stop recording and save")
                .clicked()
            {
                self.stop_recording();
            }
            if recording {
                ui.label(
                    RichText::new("● REC")
                        .color(Color32::from_rgb(255, 60, 60))
                        .strong(),
                );
                ui.label(format!("{:.0} ms", self.rec_elapsed_ms()));
            }
        });

        ui.separator();
        ui.heading("Playback");
        ui.checkbox(&mut self.rec_anim_use_timestamps, "Honor sample timestamps");
        ui.add(egui::Slider::new(&mut self.rec_anim_speed, 5.0..=200.0).text("fallback pts/s"));
        ui.checkbox(&mut self.rec_show_points, "Show sample markers");
        ui.horizontal(|ui| {
            if ui.button("Play").clicked() {
                self.start_rec_playback();
            }
            if ui.button("Stop play").clicked() {
                self.rec_animating = false;
                self.rec_anim_wall_start = None;
                if let Some(rec) = self.rec_current.as_ref() {
                    self.rec_anim_index = rec.points.len();
                }
                self.rec_status = "Playback stopped.".into();
            }
        });

        // GhostBox hardware HID relative replay (Raw Input hid_events → MoveMouseRelative).
        let hid_n = self
            .rec_current
            .as_ref()
            .map(|r| r.hid_events.len())
            .unwrap_or(0);
        let gb_busy = self.gb_replay.lock().map(|g| g.busy).unwrap_or(false);
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.gb_snap_final, "snap_final");
            ui.checkbox(&mut self.gb_fast, "fast")
                .on_hover_text("Skip per-step timeline waits; still emit coalesced relative motion.");
            #[cfg(windows)]
            {
                let enabled = hid_n > 0 && !gb_busy;
                let label = gb_replay::button_label(self.cjk_font_loaded);
                let resp = ui
                    .add_enabled(enabled, egui::Button::new(label))
                    .on_hover_text(gb_replay::disabled_hint(self.cjk_font_loaded));
                if resp.clicked() {
                    self.start_ghostbox_replay();
                }
            }
            #[cfg(not(windows))]
            {
                let label = gb_replay::button_label(self.cjk_font_loaded);
                ui.add_enabled(false, egui::Button::new(label))
                    .on_hover_text("GhostBox replay is Windows-only (gbilmd64.dll).");
            }
        });
        {
            let gb_status = self
                .gb_replay
                .lock()
                .map(|g| g.status.clone())
                .unwrap_or_default();
            if !gb_status.is_empty() {
                ui.label(RichText::new(&gb_status).small());
            }
            #[cfg(not(windows))]
            {
                ui.label(
                    RichText::new("GhostBox HID replay: disabled on this OS (Windows-only).")
                        .small()
                        .weak(),
                );
            }
        }
        if gb_busy {
            // Keep egui responsive / status refreshing while worker runs.
            ui.ctx().request_repaint();
        }

        ui.separator();
        ui.heading("History");
        ui.label(
            RichText::new(format!("File: {}", history_path().display()))
                .small()
                .weak(),
        );

        let mut load_id: Option<String> = None;
        egui::ScrollArea::vertical()
            .max_height(180.0)
            .show(ui, |ui| {
                if self.rec_history.is_empty() {
                    ui.label(RichText::new("No recordings yet.").weak());
                }
                for rec in self.rec_history.iter().rev() {
                    let selected = self.rec_selected_id.as_deref() == Some(rec.id.as_str());
                    let label = format!(
                        "{}  ·  {} pts  ·  {} HID  ·  {:.0} ms\n{}",
                        rec.name,
                        rec.points.len(),
                        rec.hid_events.len(),
                        rec.duration_ms(),
                        rec.created_at
                    );
                    if ui.selectable_label(selected, label).clicked() {
                        load_id = Some(rec.id.clone());
                    }
                }
            });
        if let Some(id) = load_id {
            if self.rec_phase != RecordPhase::Recording {
                self.load_recording(&id);
            }
        }

        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.rec_rename_buf)
                    .desired_width(140.0)
                    .hint_text("rename…"),
            );
            if ui.button("Rename").clicked() {
                self.rename_selected_recording();
            }
            if ui.button("Delete").clicked() {
                self.delete_selected_recording();
            }
        });

        if let Some(rec) = self.rec_current.as_ref() {
            ui.separator();
            ui.label(format!(
                "Current: {} — {} points, {:.0} ms, {} click events, {} HID",
                rec.name,
                rec.points.len(),
                rec.duration_ms(),
                rec.events
                    .iter()
                    .filter(|e| matches!(e, RecordedEvent::Click { .. }))
                    .count(),
                rec.hid_events.len()
            ));
        }

        self.record_segment_panel(ui);
    }

    /// Start/end click endpoints for the current recording (Click events, else markers).
    fn rec_click_endpoints(&self) -> Option<(ClickEndpoint, ClickEndpoint)> {
        let rec = self.rec_current.as_ref()?;
        let clicks: Vec<(f64, f64, f64, String)> = rec
            .events
            .iter()
            .filter_map(|e| match e {
                RecordedEvent::Click { x, y, t_ms, button } => {
                    Some((*x, *y, *t_ms, button.clone()))
                }
                _ => None,
            })
            .collect();

        let start = if let Some((x, y, t_ms, button)) = clicks.first() {
            ClickEndpoint {
                label: if self.cjk_font_loaded {
                    "开始点击"
                } else {
                    "Start click"
                },
                x: *x,
                y: *y,
                t_ms: *t_ms,
                button: Some(button.clone()),
                from_marker: false,
            }
        } else {
            let t_ms = rec.points.first().map(|p| p.t_ms).unwrap_or(0.0);
            ClickEndpoint {
                label: if self.cjk_font_loaded {
                    "开始点击"
                } else {
                    "Start click"
                },
                x: rec.start[0],
                y: rec.start[1],
                t_ms,
                button: None,
                from_marker: true,
            }
        };

        let end = if clicks.len() >= 2 {
            let (x, y, t_ms, button) = &clicks[clicks.len() - 1];
            ClickEndpoint {
                label: if self.cjk_font_loaded {
                    "结束点击"
                } else {
                    "End click"
                },
                x: *x,
                y: *y,
                t_ms: *t_ms,
                button: Some(button.clone()),
                from_marker: false,
            }
        } else if clicks.len() == 1 {
            // Single recorded click = start; end falls back to marker.
            let t_ms = rec.points.last().map(|p| p.t_ms).unwrap_or(start.t_ms);
            ClickEndpoint {
                label: if self.cjk_font_loaded {
                    "结束点击"
                } else {
                    "End click"
                },
                x: rec.end[0],
                y: rec.end[1],
                t_ms,
                button: None,
                from_marker: true,
            }
        } else {
            let t_ms = rec.points.last().map(|p| p.t_ms).unwrap_or(0.0);
            ClickEndpoint {
                label: if self.cjk_font_loaded {
                    "结束点击"
                } else {
                    "End click"
                },
                x: rec.end[0],
                y: rec.end[1],
                t_ms,
                button: None,
                from_marker: true,
            }
        };

        Some((start, end))
    }

    /// Scrollable segment table + start/end click info (left-bottom, under history).
    fn record_segment_panel(&self, ui: &mut egui::Ui) {
        ui.separator();
        ui.heading(if self.cjk_font_loaded {
            "段详情"
        } else {
            "Segment details"
        });

        let Some(rec) = self.rec_current.as_ref() else {
            ui.label(
                RichText::new(if self.cjk_font_loaded {
                    "选择历史记录（或完成一次录制）以查看段详情。"
                } else {
                    "Select a recording (or finish one) to view segment details."
                })
                .weak()
                .small(),
            );
            return;
        };

        // ---- Start / end click info ----
        ui.label(
            RichText::new(if self.cjk_font_loaded {
                "点击端点"
            } else {
                "Click endpoints"
            })
            .strong(),
        );
        if let Some((start, end)) = self.rec_click_endpoints() {
            for ep in [&start, &end] {
                let src = if ep.from_marker {
                    if self.cjk_font_loaded {
                        "标记"
                    } else {
                        "marker"
                    }
                } else if self.cjk_font_loaded {
                    "点击事件"
                } else {
                    "click event"
                };
                let btn = ep
                    .button
                    .as_deref()
                    .map(|b| format!("  button={b}"))
                    .unwrap_or_default();
                ui.monospace(format!(
                    "{} [{}]: t={:.1} ms  x={:.2}  y={:.2}{}",
                    ep.label, src, ep.t_ms, ep.x, ep.y, btn
                ));
            }
        }

        ui.add_space(4.0);
        ui.label(
            RichText::new(if self.cjk_font_loaded {
                "HID 采样"
            } else {
                "HID samples"
            })
            .strong(),
        );
        if rec.hid_events.is_empty() {
            ui.label(
                RichText::new(if self.cjk_font_loaded {
                    "无 HID 样本（未捕获、权限失败或后端不可用）"
                } else {
                    "No HID samples (not captured, permission denied, or backend unavailable)"
                })
                .weak()
                .small(),
            );
        } else {
            let hid_summary = if self.cjk_font_loaded {
                format!("已保存 {} 条 · {}", rec.hid_events.len(), backend_label())
            } else {
                format!(
                    "Saved {} samples · {}",
                    rec.hid_events.len(),
                    backend_label()
                )
            };
            ui.label(RichText::new(hid_summary).small());
            egui::ScrollArea::vertical()
                .id_salt("rec_hid_preview")
                .max_height(100.0)
                .show(ui, |ui| {
                    let preview_n = rec.hid_events.len().min(32);
                    for (i, ev) in rec.hid_events.iter().take(preview_n).enumerate() {
                        ui.monospace(format!(
                            "#{i}  t={:.2}  dx={}  dy={}  buttons={:#04x}",
                            ev.t_ms, ev.dx, ev.dy, ev.buttons
                        ));
                    }
                    if rec.hid_events.len() > preview_n {
                        ui.label(
                            RichText::new(format!(
                                "… +{} more (see history.json)",
                                rec.hid_events.len() - preview_n
                            ))
                            .weak()
                            .small(),
                        );
                    }
                });
        }

        ui.add_space(4.0);
        let segment_summary = if self.cjk_font_loaded {
            format!(
                "连续采样段 ({} → {} 共 {} 段)",
                0,
                rec.points.len().saturating_sub(1),
                rec.points.len().saturating_sub(1)
            )
        } else {
            format!(
                "Continuous sample segments ({} → {}; {} total)",
                0,
                rec.points.len().saturating_sub(1),
                rec.points.len().saturating_sub(1)
            )
        };
        ui.label(RichText::new(segment_summary).strong());

        egui::ScrollArea::vertical()
            .id_salt("rec_segment_table")
            .max_height(240.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("rec_seg_grid")
                    .striped(true)
                    .num_columns(4)
                    .spacing([12.0, 2.0])
                    .min_col_width(48.0)
                    .show(ui, |ui| {
                        ui.label(RichText::new("i→i+1").strong().small());
                        ui.label(RichText::new("Δt (ms)").strong().small());
                        ui.label(RichText::new("Δx").strong().small());
                        ui.label(RichText::new("Δy").strong().small());
                        ui.end_row();

                        if rec.points.len() < 2 {
                            ui.label(
                                RichText::new(if self.cjk_font_loaded {
                                    "无段（点数 < 2）"
                                } else {
                                    "No segments (fewer than 2 points)"
                                })
                                .weak()
                                .small(),
                            );
                            ui.end_row();
                            return;
                        }
                        for i in 0..rec.points.len() - 1 {
                            let a = &rec.points[i];
                            let b = &rec.points[i + 1];
                            let dt = b.t_ms - a.t_ms;
                            let dx = b.x - a.x;
                            let dy = b.y - a.y;
                            ui.monospace(format!("{i}→{}", i + 1));
                            ui.monospace(format!("{dt:.2}"));
                            ui.monospace(format!("{dx:.2}"));
                            ui.monospace(format!("{dy:.2}"));
                            ui.end_row();
                        }
                    });
            });
    }

    fn draw_record_canvas(&mut self, ui: &mut egui::Ui) {
        let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let canvas = response.rect;
        self.rec_last_canvas = canvas;
        self.last_canvas_size = canvas.size();

        painter.rect_filled(canvas, 4.0, Color32::from_gray(24));
        painter.rect_stroke(
            canvas,
            4.0,
            Stroke::new(1.0_f32, Color32::from_gray(60)),
            StrokeKind::Outside,
        );
        let painter = painter.with_clip_rect(canvas);

        // Grid
        {
            let grid_col = Color32::from_gray(40);
            for i in 1..8 {
                let t = i as f32 / 8.0;
                let x = canvas.min.x + t * canvas.width();
                let y = canvas.min.y + t * canvas.height();
                painter.line_segment(
                    [Pos2::new(x, canvas.min.y), Pos2::new(x, canvas.max.y)],
                    Stroke::new(1.0_f32, grid_col),
                );
                painter.line_segment(
                    [Pos2::new(canvas.min.x, y), Pos2::new(canvas.max.x, y)],
                    Stroke::new(1.0_f32, grid_col),
                );
            }
        }

        let recording = self.rec_phase == RecordPhase::Recording;
        let start_screen = Self::world_to_canvas(canvas, self.rec_start);
        let end_screen = Self::world_to_canvas(canvas, self.rec_end);

        // Marker hit testing / dragging (disabled while recording, except end click to stop).
        let pointer = response.interact_pointer_pos();
        let marker_r = 12.0_f32;

        if recording {
            // Sample pointer each frame (high-rate).
            if let Some(pos) = ui.ctx().pointer_latest_pos() {
                let w = Self::canvas_to_world(canvas, pos);
                let t = self.rec_elapsed_ms();
                let should_push = self
                    .rec_live_points
                    .last()
                    .map(|p| (p.x - w[0]).hypot(p.y - w[1]) > 0.25 || (t - p.t_ms) >= 8.0)
                    .unwrap_or(true);
                if should_push {
                    self.rec_live_points.push(RecordedPoint {
                        x: w[0],
                        y: w[1],
                        t_ms: t,
                    });
                    self.rec_live_events.push(RecordedEvent::Move {
                        x: w[0],
                        y: w[1],
                        t_ms: t,
                    });
                }
            }

            // Click events (primary) while recording — but treat click on end marker as stop.
            let clicked_end = response.clicked()
                && pointer
                    .map(|p| p.distance(end_screen) <= marker_r + 4.0)
                    .unwrap_or(false);
            if clicked_end {
                self.stop_recording();
            } else if response.clicked() {
                if let Some(pos) = pointer {
                    let w = Self::canvas_to_world(canvas, pos);
                    let t = self.rec_elapsed_ms();
                    self.rec_live_events.push(RecordedEvent::Click {
                        x: w[0],
                        y: w[1],
                        t_ms: t,
                        button: "primary".into(),
                    });
                    // Also ensure a point exists at click.
                    self.rec_live_points.push(RecordedPoint {
                        x: w[0],
                        y: w[1],
                        t_ms: t,
                    });
                }
            }

            ui.ctx().request_repaint();
        } else {
            // Idle: drag markers; click start marker to begin recording.
            if response.drag_started() {
                if let Some(pos) = pointer {
                    let ds = pos.distance(start_screen);
                    let de = pos.distance(end_screen);
                    if ds <= marker_r && ds <= de {
                        self.rec_drag = DragTarget::Start;
                    } else if de <= marker_r {
                        self.rec_drag = DragTarget::End;
                    } else {
                        self.rec_drag = DragTarget::None;
                    }
                }
            }
            if response.dragged() {
                if let Some(pos) = pointer {
                    let w = Self::canvas_to_world(canvas, pos);
                    match self.rec_drag {
                        DragTarget::Start => self.rec_start = w,
                        DragTarget::End => self.rec_end = w,
                        DragTarget::None => {}
                    }
                }
            }
            if response.drag_stopped() {
                self.rec_drag = DragTarget::None;
            }
            if response.clicked() && !response.dragged() {
                if let Some(pos) = pointer {
                    if pos.distance(start_screen) <= marker_r + 4.0 {
                        self.start_recording();
                    }
                }
            }
        }

        // Draw trajectory
        let points = self.rec_points_view().to_vec();
        let events = self.rec_events_view().to_vec();
        let n = self.rec_visible_count().min(points.len());
        let speeds = if self.rec_phase == RecordPhase::Recording {
            recorded_speeds(&points)
        } else if !self.rec_speeds.is_empty() {
            self.rec_speeds.clone()
        } else {
            recorded_speeds(&points)
        };
        let peak = peak_speed(&speeds);

        if n >= 2 {
            for i in 1..n {
                let a = Self::world_to_canvas(canvas, [points[i - 1].x, points[i - 1].y]);
                let b = Self::world_to_canvas(canvas, [points[i].x, points[i].y]);
                let t = speeds.get(i).copied().unwrap_or(0.0) / peak;
                painter.line_segment([a, b], Stroke::new(self.line_width, speed_color(t as f32)));
            }
        }
        if self.rec_show_points && n > 0 {
            for i in 0..n {
                let p = Self::world_to_canvas(canvas, [points[i].x, points[i].y]);
                let t = speeds.get(i).copied().unwrap_or(0.0) / peak;
                painter.circle_filled(p, self.point_radius, speed_color(t as f32));
            }
        }

        // Click markers (rings) — only those within visible time prefix.
        let t_cut = if n > 0 {
            points[n - 1].t_ms
        } else {
            f64::INFINITY
        };
        for ev in &events {
            if let RecordedEvent::Click { x, y, t_ms, .. } = ev {
                if *t_ms <= t_cut + 0.5 {
                    let p = Self::world_to_canvas(canvas, [*x, *y]);
                    painter.circle_stroke(
                        p,
                        9.0,
                        Stroke::new(2.0_f32, Color32::from_rgb(255, 220, 80)),
                    );
                    painter.circle_stroke(
                        p,
                        5.0,
                        Stroke::new(1.5_f32, Color32::from_rgb(255, 180, 40)),
                    );
                }
            }
        }

        // Guide line + markers
        painter.line_segment(
            [start_screen, end_screen],
            Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(200, 200, 200, 60)),
        );
        // Start (green)
        painter.circle_filled(start_screen, 5.0, Color32::from_rgb(80, 220, 120));
        painter.circle_stroke(
            start_screen,
            marker_r,
            Stroke::new(2.5_f32, Color32::from_rgb(80, 220, 120)),
        );
        painter.text(
            start_screen + Vec2::new(0.0, -marker_r - 4.0),
            egui::Align2::CENTER_BOTTOM,
            "START",
            egui::FontId::proportional(11.0),
            Color32::from_rgb(80, 220, 120),
        );
        // End (red)
        painter.circle_filled(end_screen, 5.0, Color32::from_rgb(255, 100, 100));
        painter.circle_stroke(
            end_screen,
            marker_r,
            Stroke::new(2.5_f32, Color32::from_rgb(255, 100, 100)),
        );
        painter.text(
            end_screen + Vec2::new(0.0, -marker_r - 4.0),
            egui::Align2::CENTER_BOTTOM,
            "END",
            egui::FontId::proportional(11.0),
            Color32::from_rgb(255, 100, 100),
        );

        // REC overlay
        if recording {
            painter.text(
                Pos2::new(canvas.min.x + 12.0, canvas.min.y + 10.0),
                egui::Align2::LEFT_TOP,
                format!(
                    "● REC  {:.0} ms  {} samples",
                    self.rec_elapsed_ms(),
                    points.len()
                ),
                egui::FontId::monospace(14.0),
                Color32::from_rgb(255, 70, 70),
            );
        } else {
            let ppp = ui.ctx().pixels_per_point();
            let label = format!(
                "{:.0}×{:.0} pt · 1:1 canvas coords",
                canvas.width(),
                canvas.height()
            );
            let _ = ppp;
            painter.text(
                Pos2::new(canvas.min.x + 10.0, canvas.min.y + 8.0),
                egui::Align2::LEFT_TOP,
                label,
                egui::FontId::monospace(12.0),
                Color32::from_rgb(180, 220, 255),
            );
        }

        // Speed legend
        {
            let legend = Rect::from_min_size(
                Pos2::new(canvas.max.x - 140.0, canvas.min.y + 12.0),
                Vec2::new(120.0, 14.0),
            );
            let steps = 64;
            for i in 0..steps {
                let t = i as f32 / (steps - 1) as f32;
                let x0 = legend.min.x + t * legend.width();
                let x1 = legend.min.x + ((i + 1) as f32 / steps as f32) * legend.width();
                painter.rect_filled(
                    Rect::from_min_max(Pos2::new(x0, legend.min.y), Pos2::new(x1, legend.max.y)),
                    0.0,
                    speed_color(t),
                );
            }
            painter.text(
                Pos2::new(legend.min.x, legend.max.y + 2.0),
                egui::Align2::LEFT_TOP,
                "slow",
                egui::FontId::proportional(11.0),
                Color32::LIGHT_GRAY,
            );
            painter.text(
                Pos2::new(legend.max.x, legend.max.y + 2.0),
                egui::Align2::RIGHT_TOP,
                "fast",
                egui::FontId::proportional(11.0),
                Color32::LIGHT_GRAY,
            );
        }
    }

    fn tick_rec_animation(&mut self, ctx: &egui::Context) {
        if !self.rec_animating {
            return;
        }
        let n = match self.rec_current.as_ref() {
            Some(rec) => rec.points.len(),
            None => {
                self.rec_animating = false;
                return;
            }
        };
        if n == 0 {
            self.rec_animating = false;
            return;
        }

        if self.rec_anim_use_timestamps {
            let elapsed_ms = self
                .rec_anim_wall_start
                .map(|t| t.elapsed().as_secs_f64() * 1000.0)
                .unwrap_or(0.0);
            let mut idx = 1usize;
            if let Some(rec) = self.rec_current.as_ref() {
                while idx < n && rec.points[idx].t_ms <= elapsed_ms {
                    idx += 1;
                }
            }
            self.rec_anim_index = idx;
            if idx >= n {
                self.rec_animating = false;
                self.rec_anim_wall_start = None;
                self.rec_status = "Playback complete.".into();
            } else {
                ctx.request_repaint();
            }
        } else {
            let dt = ctx.input(|i| i.stable_dt);
            self.rec_anim_accum += dt * self.rec_anim_speed;
            while self.rec_anim_accum >= 1.0 {
                self.rec_anim_accum -= 1.0;
                if self.rec_anim_index < n {
                    self.rec_anim_index += 1;
                } else {
                    self.rec_animating = false;
                    self.rec_status = "Playback complete.".into();
                    break;
                }
            }
            ctx.request_repaint();
        }
    }
}

fn slider_f64(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
) {
    ui.horizontal(|ui| {
        ui.set_min_width(160.0);
        ui.label(label);
        ui.add(
            egui::Slider::new(value, range)
                .clamping(egui::SliderClamping::Never)
                .min_decimals(2)
                .max_decimals(4),
        );
    });
}

fn metric_row(ui: &mut egui::Ui, name: &str, value: String) {
    ui.label(RichText::new(name).strong());
    ui.monospace(value);
    ui.end_row();
}

impl VizApp {
    fn ghostbox_selftest_panel(&mut self, ui: &mut egui::Ui) {
        let (busy, status) = self
            .gb_selftest
            .lock()
            .map(|state| (state.busy, state.status.clone()))
            .unwrap_or_else(|poisoned| {
                let state = poisoned.into_inner();
                (state.busy, state.status.clone())
            });
        ui.heading(if self.cjk_font_loaded {
            "GhostBox / 幽灵盒自检"
        } else {
            "GhostBox self-test"
        });
        ui.label(if self.cjk_font_loaded {
            "确认 MoveMouseTo 与 GetMouseX/GetMouseY 可用。"
        } else {
            "Confirm MoveMouseTo with three absolute screen moves."
        });
        ui.add_space(8.0);
        let button_label = if self.cjk_font_loaded {
            "随机移到 3 点"
        } else {
            "Move to 3 random points"
        };
        if ui
            .add_enabled(!busy, egui::Button::new(button_label))
            .clicked()
        {
            gb_selftest::spawn_self_test(Arc::clone(&self.gb_selftest));
        }
        let reset_label = if self.cjk_font_loaded {
            "复位 / Reset"
        } else {
            "Reset"
        };
        // Intentionally not gated by `busy`: this is the escape hatch for a stuck worker.
        if ui
            .button(reset_label)
            .on_hover_text("Clear busy state and call CloseDevice (best effort)")
            .clicked()
        {
            gb_selftest::reset_self_test(Arc::clone(&self.gb_selftest));
        }
        ui.add_space(8.0);
        ui.label(
            RichText::new(if self.cjk_font_loaded {
                "状态："
            } else {
                "Status:"
            })
            .strong(),
        );
        ui.label(if status.is_empty() {
            if self.cjk_font_loaded {
                "尚未运行。"
            } else {
                "Not run yet."
            }
        } else {
            &status
        });
        if busy {
            ui.ctx().request_repaint();
        }
    }
}
impl eframe::App for VizApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Generate animation tick
        if self.tab == AppTab::Generate && self.animating {
            let dt = ctx.input(|i| i.stable_dt);
            self.anim_accum += dt * self.anim_speed;
            while self.anim_accum >= 1.0 {
                self.anim_accum -= 1.0;
                if self.anim_index < self.sigma_path.len() {
                    self.anim_index += 1;
                } else {
                    self.animating = false;
                    self.status = "Animation complete.".into();
                    break;
                }
            }
            ctx.request_repaint();
        }

        if self.tab == AppTab::Record {
            self.tick_rec_animation(ctx);
            // Sync GhostBox worker status into the Record status line.
            if let Ok(mut g) = self.gb_replay.lock() {
                if !g.status.is_empty() {
                    self.rec_status = g.status.clone();
                    // Clear terminal messages after one sync so we don't stomp later UI status.
                    if !g.busy
                        && (g.status.starts_with("GhostBox done:")
                            || g.status.starts_with("GhostBox error:")
                            || g.status.contains("Windows-only"))
                    {
                        g.status.clear();
                    }
                }
                if g.busy {
                    ctx.request_repaint();
                }
            }
        }

        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("SigmaDrift Viz");
                ui.separator();
                ui.selectable_value(&mut self.tab, AppTab::Generate, "Generate");
                ui.selectable_value(&mut self.tab, AppTab::Record, "Record");
                ui.selectable_value(
                    &mut self.tab,
                    AppTab::GhostBox,
                    if self.cjk_font_loaded {
                        "幽灵盒自检"
                    } else {
                        "GhostBox"
                    },
                );
            });
        });

        match self.tab {
            AppTab::Generate => {
                egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new("Generate").strong()).clicked() {
                            self.regenerate();
                        }
                        if ui.button("Animate").clicked() {
                            self.start_animation();
                        }
                        if ui.button("Reset").clicked() {
                            self.reset_defaults();
                        }
                        if ui.button("Export CSV").clicked() {
                            self.export_csv();
                        }
                        ui.separator();
                        ui.label(RichText::new(&self.status).weak());
                    });
                });

                egui::SidePanel::left("config")
                    .default_width(320.0)
                    .resizable(true)
                    .show(ctx, |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            self.config_panel(ui);
                        });
                    });

                egui::SidePanel::right("metrics")
                    .default_width(260.0)
                    .resizable(true)
                    .show(ctx, |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            self.metrics_panel(ui);
                            ui.separator();
                            ui.label(
                                RichText::new(
                                    "Color: blue=slow → cyan → yellow → red=fast\n\
                                     (per-segment speed = |Δpos|/Δt; markers use that segment's speed).",
                                )
                                .small()
                                .weak(),
                            );
                        });
                    });

                egui::CentralPanel::default().show(ctx, |ui| {
                    self.draw_generate_canvas(ui);
                });
            }
            AppTab::Record => {
                egui::SidePanel::left("record_panel")
                    .default_width(320.0)
                    .resizable(true)
                    .show(ctx, |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            self.record_side_panel(ui);
                        });
                    });

                egui::CentralPanel::default().show(ctx, |ui| {
                    self.draw_record_canvas(ui);
                });
            }
            AppTab::GhostBox => {
                egui::CentralPanel::default().show(ctx, |ui| {
                    self.ghostbox_selftest_panel(ui);
                });
            }
        }
    }
}
