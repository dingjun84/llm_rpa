//! Replay a HID relative mouse sequence through GhostBox (`gbilmd64.dll`).

use clap::Parser;
use ghostbox::{
    open_device_guarded, replay_hid_sequence, GBMAPI, GhostboxError, HidStep, ReplayRequest,
    OPEN_DEVICE_TIMEOUT,
};
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(name = "ghostbox-play", about = "Replay HID sequences via GhostBox hardware SDK")]
struct Args {
    /// Path to JSON sequence (native ReplayRequest, viz Recording, or Recording[])
    #[arg(long)]
    file: PathBuf,

    /// Path to gbilmd64.dll (default: beside this exe, then cwd)
    #[arg(long)]
    dll: Option<PathBuf>,

    /// Skip MoveMouseTo(final_pos) even if present in JSON
    #[arg(long, default_value_t = false)]
    no_snap: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let text = std::fs::read_to_string(&args.file)?;
    let mut req = load_replay_request(&text)?;
    if args.no_snap {
        req.snap_final = false;
    }

    println!(
        "loaded {} steps from {} (start_pos={:?}, move_to_start={}, final_pos={:?}, snap_final={}, fast={})",
        req.steps.len(),
        args.file.display(),
        req.start_pos,
        req.move_to_start,
        req.final_pos,
        req.snap_final,
        req.fast
    );

    let dll = resolve_dll(args.dll.as_deref())?;
    println!("loading DLL: {}", dll.display());
    let api = Arc::new(GBMAPI::load(dll.as_os_str())?);

    match api.SDKType() {
        Ok(t) => println!("SDKType: {t}"),
        Err(e) => eprintln!("SDKType: {e}"),
    }
    match api.SDKVersion() {
        Ok(v) => println!("SDKVersion: {v}"),
        Err(e) => eprintln!("SDKVersion: {e}"),
    }


    println!("preparing device (CloseDevice -> ResetDevice -> OpenDevice)");
    println!("OpenDevice (timeout {} ms)…", OPEN_DEVICE_TIMEOUT.as_millis());
    let open_code = open_device_guarded(&api, OPEN_DEVICE_TIMEOUT)?;
    println!("OpenDevice -> {open_code}");

    let report = match replay_hid_sequence(&api, &req) {
        Ok(r) => r,
        Err(e) => {
            let _ = api.CloseDevice();
            return Err(e.into());
        }
    };

    let close = api.CloseDevice()?;
    println!("CloseDevice -> {close}");

    println!(
        "done: steps={}, coalesced={}, move_calls={}, button_events={}, duration_ms={}, mouse=({}, {}), start={}, snapped={}",
        report.steps_in,
        report.steps_coalesced,
        report.move_calls,
        report.button_events,
        report.duration_ms,
        report.mouse_x,
        report.mouse_y,
        report.moved_to_start,
        report.snapped_final
    );
    Ok(())
}

fn resolve_dll(explicit: Option<&Path>) -> Result<PathBuf, GhostboxError> {
    if let Some(p) = explicit {
        if p.is_file() {
            return Ok(p.to_path_buf());
        }
        return Err(GhostboxError::Message(format!(
            "DLL not found: {}",
            p.display()
        )));
    }

    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("gbilmd64.dll"));
        }
    }
    candidates.push(PathBuf::from("gbilmd64.dll"));

    for c in &candidates {
        if c.is_file() {
            return Ok(c.clone());
        }
    }
    Err(GhostboxError::Message(format!(
        "gbilmd64.dll not found beside exe or in cwd; tried: {}",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

/// Accept native ReplayRequest, viz Recording, Recording[], or `{hid_events:…}`.
fn load_replay_request(text: &str) -> Result<ReplayRequest, Box<dyn std::error::Error>> {
    let value: Value = serde_json::from_str(text)?;

    if let Some(req) = try_native(&value) {
        return Ok(req);
    }

    if value.is_array() {
        let arr = value.as_array().unwrap();
        for item in arr {
            if let Some(req) = try_viz_recording(item) {
                return Ok(req);
            }
        }
        return Err("JSON array has no recording with non-empty hid_events".into());
    }

    if let Some(req) = try_viz_recording(&value) {
        return Ok(req);
    }

    Err(
        "unrecognized JSON: expected ReplayRequest, viz Recording, or Recording[]".into(),
    )
}

fn try_native(value: &Value) -> Option<ReplayRequest> {
    let obj = value.as_object()?;
    if !obj.contains_key("steps") {
        return None;
    }
    serde_json::from_value::<ReplayRequest>(value.clone()).ok()
}

#[derive(Debug, Deserialize)]
struct VizHidEvent {
    t_ms: f64,
    dx: i32,
    dy: i32,
    #[serde(default)]
    buttons: u8,
}

#[derive(Debug, Deserialize)]
struct VizRecordingLoose {
    #[serde(default)]
    hid_events: Vec<VizHidEvent>,
    #[serde(default)]
    start: Option<[f64; 2]>,
    #[serde(default)]
    start_pos: Option<[f64; 2]>,
    #[serde(default)]
    end: Option<[f64; 2]>,
    #[serde(default)]
    end_pos: Option<[f64; 2]>,
    #[serde(default)]
    final_pos: Option<VizFinalPos>,
    #[serde(default)]
    snap_final: Option<bool>,
    #[serde(default)]
    fast: Option<bool>,
    #[serde(default)]
    move_to_start: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum VizFinalPos {
    Pair([f64; 2]),
    Tuple((i32, i32)),
}

fn try_viz_recording(value: &Value) -> Option<ReplayRequest> {
    let rec: VizRecordingLoose = serde_json::from_value(value.clone()).ok()?;
    if rec.hid_events.is_empty() {
        return None;
    }
    let steps: Vec<HidStep> = rec
        .hid_events
        .into_iter()
        .map(|e| HidStep {
            t_ms: e.t_ms.max(0.0).round() as u64,
            dx: e.dx,
            dy: e.dy,
            buttons: e.buttons,
        })
        .collect();

    let final_pos = rec
        .final_pos
        .map(|p| match p {
            VizFinalPos::Pair(a) => (a[0].round() as i32, a[1].round() as i32),
            VizFinalPos::Tuple(t) => t,
        })
        .or_else(|| rec.end.map(|a| (a[0].round() as i32, a[1].round() as i32)))
        .or_else(|| rec.end_pos.map(|a| (a[0].round() as i32, a[1].round() as i32)));

    let start_pos = rec
        .start
        .or(rec.start_pos)
        .map(|a| (a[0].round() as i32, a[1].round() as i32));

    Some(ReplayRequest {
        steps,
        start_pos,
        move_to_start: rec.move_to_start.unwrap_or(true),
        final_pos,
        snap_final: rec.snap_final.unwrap_or(true),
        fast: rec.fast.unwrap_or(true),
    })
}