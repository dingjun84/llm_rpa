# ghostbox — 幽灵盒子 SDK 绑定与 HID 回放

通过厂商 `gbilmd64.dll` 做硬件键鼠回放（相对 HID 轨迹 → `MoveMouseRelative`，可选终点 `MoveMouseTo`）。

## 依赖

- 产品：**GHOSTBOX**（变色龙系列）硬件已插入并被系统识别
- DLL：`gbilmd64.dll`（64 位）必须放在 **可执行文件同目录**，或通过 CLI `--dll` 指定绝对路径
- 参考 DEMO（只读，勿改）：`../rpa/src/ghostboxm.rs`

## 重要警告

OpenDevice() 前严格按 CloseDevice -> ResetDevice -> OpenDevice 顺序调用；前两个调用错误会被忽略，OpenDevice 仍使用超时保护。

## JSON 回放格式

**A. 原生格式**

```json
{
  "final_pos": [500, 400],
  "snap_final": true,
  "steps": [
    { "t_ms": 0, "dx": 1, "dy": 0, "buttons": 0 },
    { "t_ms": 16, "dx": 2, "dy": 1, "buttons": 0 }
  ]
}
```

**B. sigma-drift-viz 录制片段**（含 `hid_events`；像素终点取 `end`）

```json
{
  "id": "...",
  "name": "...",
  "start": [0, 0],
  "end": [500, 400],
  "points": [],
  "events": [],
  "hid_events": [
    { "t_ms": 0.0, "dx": 1, "dy": 0, "buttons": 0 }
  ]
}
```

也支持 viz 历史数组（`Recording[]`）：自动选取第一条非空 `hid_events` 的记录。

`buttons`：bit0=左键→SDK 1，bit1=右键→SDK 2，bit2=中键→SDK 3。

## 构建与运行

```bat
:: 先激活工具链（见仓库 scripts/rust-env.sh）
cargo build -p ghostbox-play --release

:: 构建后把 DLL 拷到 target\release\（本仓库脚本/说明会做）
copy /Y ..\rpa\target\release\gbilmd64.dll target\release\

cargo run -p ghostbox-play --release -- --file tools\ghostbox-play\examples\sample_sequence.json
```

库 API 入口：`ghostbox::replay_hid_sequence`、`ghostbox::open_device_guarded`。