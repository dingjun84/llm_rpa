//! 桌面端 GhostBox（幽灵盒）会话：进程级 Open 一次，重置按钮才 Close/Reset。
//!
//! 设备曲线/速度在 `ghostbox::open_device_guarded` 里于 OpenDevice 成功后设置；
//! 本模块**不**在每次 Move 后 CloseDevice。

use serde::{Deserialize, Serialize};

/// `ghostbox_move_to` 的返回（给界面打日志）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhostboxMoveResult {
    pub x: i32,
    pub y: i32,
    /// MoveMouseTo 的返回码。
    pub code: i32,
    /// 是否复用了已有进程级会话（否则是本次新 Open）。
    pub reused_session: bool,
    pub notice: String,
}

/// `ghostbox_reset_device` 的返回。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhostboxResetResult {
    pub detail: String,
    pub notice: String,
}

/// `ghostbox_right_click` 的返回（给界面打日志）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhostboxRightClickResult {
    /// `PressAndReleaseMouseButton(3)` 的返回码（官方：2=成功，0=失败）。
    pub code: i32,
    /// 是否复用了已有进程级会话（否则是本次新 Open）。
    pub reused_session: bool,
    pub notice: String,
}

/// 把幽灵盒光标移到屏幕绝对坐标 `(x, y)`（Windows + gbilmd64.dll）。
#[tauri::command]
pub fn ghostbox_move_to(x: i32, y: i32) -> Result<GhostboxMoveResult, String> {
    #[cfg(windows)]
    {
        move_to_windows(x, y)
    }
    #[cfg(not(windows))]
    {
        let _ = (x, y);
        Err("幽灵盒 HID 仅支持 Windows（需要 gbilmd64.dll 与已连接的硬件）。".into())
    }
}

/// 在**当前光标位置**触发幽灵盒 HID 右键（`PressAndReleaseMouseButton(3)`）。
///
/// ★ 故意**不**先 `MoveMouseTo`：自检用途是单独验证右键能否弹出菜单
/// （例如企微气泡），鼠标由操作者在倒计时内自己挪到位。
/// ★ 官方按键：1=左 / 2=中 / **3=右**；返回码 **2=成功 / 0=失败**。
#[tauri::command]
pub fn ghostbox_right_click() -> Result<GhostboxRightClickResult, String> {
    #[cfg(windows)]
    {
        right_click_windows()
    }
    #[cfg(not(windows))]
    {
        Err("幽灵盒 HID 仅支持 Windows（需要 gbilmd64.dll 与已连接的硬件）。".into())
    }
}

/// 重置幽灵盒：清掉进程级共享会话（CloseDevice → ResetDevice），并等待 2 秒。
///
/// 与 sigma-drift-viz 自检重置同一约定：调用方（本命令）负责 settle 延时；
/// 重置本身忽略厂商错误，始终可作为逃生舱。
#[tauri::command]
pub fn ghostbox_reset_device() -> Result<GhostboxResetResult, String> {
    #[cfg(windows)]
    {
        reset_windows()
    }
    #[cfg(not(windows))]
    {
        // 非 Windows 也允许点重置：清不了硬件，但给明确说明，避免按钮"没反应"。
        Ok(GhostboxResetResult {
            detail: "noop".into(),
            notice: "当前不是 Windows：没有幽灵盒会话可重置（HID 仅 Windows）。".into(),
        })
    }
}

#[cfg(windows)]
fn move_to_windows(x: i32, y: i32) -> Result<GhostboxMoveResult, String> {
    ghostbox::append_replay_log(&format!("ghostbox_move_to: start target=({x}, {y})"));
    let dll = match resolve_dll(None) {
        Ok(path) => path,
        Err(err) => {
            ghostbox::append_replay_log(&format!("ghostbox_move_to: resolve_dll fail: {err}"));
            return Err(err);
        }
    };
    let reused = ghostbox::shared_device_is_open();
    ghostbox::append_replay_log(&format!(
        "ghostbox_move_to: before open reused_session={reused} dll={}",
        dll.display()
    ));
    let api = match ghostbox::shared_device_session(&dll, ghostbox::OPEN_DEVICE_TIMEOUT) {
        Ok(api) => api,
        Err(err) => {
            ghostbox::append_replay_log(&format!("ghostbox_move_to: open-session fail: {err}"));
            return Err(format!("打开幽灵盒失败：{err}"));
        }
    };
    ghostbox::append_replay_log(&format!(
        "ghostbox_move_to: open-session ok reused_session={reused}; calling MoveMouseTo({x}, {y})"
    ));
    let code = match api.MoveMouseTo(x, y) {
        Ok(code) => code,
        Err(err) => {
            ghostbox::append_replay_log(&format!(
                "ghostbox_move_to: MoveMouseTo({x}, {y}) fail: {err}"
            ));
            return Err(format!("MoveMouseTo({x}, {y}) 失败：{err}"));
        }
    };
    ghostbox::append_replay_log(&format!(
        "ghostbox_move_to: MoveMouseTo({x}, {y}) → code {code}"
    ));
    let session_note = if reused {
        "复用进程级会话"
    } else {
        "新开进程级会话（OpenDevice 一次）"
    };
    let notice = format!("MoveMouseTo({x}, {y}) → code {code}；{session_note}。");
    Ok(GhostboxMoveResult {
        x,
        y,
        code,
        reused_session: reused,
        notice,
    })
}

#[cfg(windows)]
fn right_click_windows() -> Result<GhostboxRightClickResult, String> {
    ghostbox::append_replay_log(
        "ghostbox_right_click: start PressAndReleaseMouseButton(3=右); no MoveMouseTo",
    );
    let dll = match resolve_dll(None) {
        Ok(path) => path,
        Err(err) => {
            ghostbox::append_replay_log(&format!("ghostbox_right_click: resolve_dll fail: {err}"));
            return Err(err);
        }
    };
    let reused = ghostbox::shared_device_is_open();
    ghostbox::append_replay_log(&format!(
        "ghostbox_right_click: before open reused_session={reused} dll={}",
        dll.display()
    ));
    let api = match ghostbox::shared_device_session(&dll, ghostbox::OPEN_DEVICE_TIMEOUT) {
        Ok(api) => api,
        Err(err) => {
            ghostbox::append_replay_log(&format!("ghostbox_right_click: open-session fail: {err}"));
            return Err(format!("打开幽灵盒失败：{err}"));
        }
    };
    ghostbox::append_replay_log(&format!(
        "ghostbox_right_click: open-session ok reused_session={reused}; PressAndReleaseMouseButton({})",
        ghostbox::MOUSE_BUTTON_RIGHT
    ));
    let code = match api.PressAndReleaseMouseButton(ghostbox::MOUSE_BUTTON_RIGHT) {
        Ok(code) => code,
        Err(err) => {
            ghostbox::append_replay_log(&format!(
                "ghostbox_right_click: PressAndReleaseMouseButton({}) fail: {err}",
                ghostbox::MOUSE_BUTTON_RIGHT
            ));
            return Err(format!(
                "PressAndReleaseMouseButton({}) 调用失败：{err}",
                ghostbox::MOUSE_BUTTON_RIGHT
            ));
        }
    };
    ghostbox::append_replay_log(&format!(
        "ghostbox_right_click: PressAndReleaseMouseButton({}) → code {code} (2=成功/0=失败)",
        ghostbox::MOUSE_BUTTON_RIGHT
    ));
    if let Err(err) =
        ghostbox::ensure_mouse_button_ok("PressAndReleaseMouseButton(3=右)", code)
    {
        let session_note = if reused {
            "复用进程级会话"
        } else {
            "新开进程级会话"
        };
        let notice = format!(
            "HID 右键失败：PressAndReleaseMouseButton(3) → code {code}（期望 2=成功）；{session_note}；{err}"
        );
        ghostbox::append_replay_log(&format!("ghostbox_right_click: {notice}"));
        return Err(notice);
    }
    let session_note = if reused {
        "复用进程级会话"
    } else {
        "新开进程级会话（OpenDevice 一次）"
    };
    let notice = format!(
        "HID 右键成功：PressAndReleaseMouseButton(3) → code {code}（2=成功）；{session_note}（未 MoveMouseTo，用当前光标位置）。"
    );
    Ok(GhostboxRightClickResult {
        code,
        reused_session: reused,
        notice,
    })
}

#[cfg(windows)]
fn reset_windows() -> Result<GhostboxResetResult, String> {
    ghostbox::append_replay_log("ghostbox_reset_device: begin");
    let dll = match resolve_dll(None) {
        Ok(path) => path,
        Err(err) => {
            ghostbox::append_replay_log(&format!("ghostbox_reset_device: resolve_dll fail: {err}"));
            return Ok(GhostboxResetResult {
                detail: err.clone(),
                notice: format!("重置跳过（找不到 DLL）：{err}"),
            });
        }
    };
    let detail = ghostbox::reset_shared_device_session(&dll);
    std::thread::sleep(std::time::Duration::from_secs(2));
    let notice = format!("幽灵盒已重置（等待 2 秒后）：{detail}");
    ghostbox::append_replay_log(&format!("ghostbox_reset_device: done {detail}"));
    Ok(GhostboxResetResult { detail, notice })
}

/// 解析 `gbilmd64.dll`：显式路径 → 可执行文件旁 → 当前工作目录（与 ghostbox-play 一致）。
#[cfg(windows)]
fn resolve_dll(explicit: Option<&std::path::Path>) -> Result<std::path::PathBuf, String> {
    use std::path::PathBuf;

    if let Some(p) = explicit {
        if p.is_file() {
            return Ok(p.to_path_buf());
        }
        return Err(format!("DLL 不存在：{}", p.display()));
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
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
    Err(format!(
        "找不到 gbilmd64.dll（已试：{}）",
        candidates
            .iter()
            .map(|p: &PathBuf| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}
