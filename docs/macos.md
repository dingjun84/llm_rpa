# macOS 适配说明

本项目的业务流程（`automation-core`）不依赖操作系统。macOS 通过
`crates/platform-macos` 实现同一组 [`DesktopPlatform`] 端口，与
`platform-windows` 并列。

## 权限（必须手动授予）

真实模式会截屏、移动鼠标、发送按键。首次使用前请到：

**系统设置 → 隐私与安全性**

1. **屏幕录制** — 勾选本应用（或你用来启动它的终端 / Cursor）；
2. **辅助功能** — 同上。

未授权时的典型现象：

| 现象 | 多半缺的权限 |
| --- | --- |
| 截屏失败 / 预览全黑 | 屏幕录制 |
| 鼠标不动、点击无效、无法置前 | 辅助功能 |

改完权限后**重启应用**（部分系统版本要求注销或重启）。

## 配置语义差异

| 配置字段 | Windows | macOS |
| --- | --- | --- |
| `window_class` | Win32 窗口类名（如 `WeWorkWindow`） | **所有者名**（应用显示名，`CGWindowOwnerName`，如 `企业微信` / `微信`） |
| `wecom_exe` | `.exe` 路径 | `.app` 包路径，或包内 `Contents/MacOS/...` 可执行文件 |
| `ocr_command` | `tools/winocr` | `tools/macosocr`（见下） |

界面上的「指认窗口」在 Mac 上会把所有者名写入 `window_class`，一般不必手填。

## 本地 OCR

```bash
cd tools/macosocr
swiftc -O -framework Vision -framework CoreGraphics \
  -o ../../target/debug/macosocr macosocr.swift
```

配置示例：

```jsonc
"ocr_command": "<仓库>/target/debug/macosocr"
```

契约与 `winocr` 相同：PNG → stdin，JSON 数组 → stdout。Vision 提供逐词置信度，
因此 Mac 上 `min_confidence` **有效**（与 Windows.Media.Ocr 固定 1.0 不同）。

也可用任意满足同一契约的本地引擎（例如自建 PaddleOCR 包装）。

## 构建与运行

```bash
# 工作区测试（含 platform-macos 桩；真实系统调用仅在 macOS 链接）
CARGO_INCREMENTAL=0 cargo test -p platform-macos -p automation-core -p platform-mock

# 桌面应用（需在 Mac 上）
cd apps/desktop && npm install && npm run tauri dev
# 或
CARGO_INCREMENTAL=0 cargo build -p desktop --features custom-protocol
```

## 卡死检测

Windows 使用 `IsHungAppWindow`。macOS 没有对等 API：`is_responsive` 仅确认
进程与窗口仍可查询。编排层仍依赖「滚动前后画面指纹」作主判据；两套互补，
Windows 上系统级判据更强一些。

## 全局热键

截图标用的全局热键目前仍仅 Windows（`RegisterHotKey`）。Mac 上请用界面里的
「延时截图」。

## 手动验收清单（Mac）

1. 授予屏幕录制 + 辅助功能，重启应用；
2. 手动启动并登录企业微信 / 微信；
3. 「指认窗口」→ 所有者名与 `.app` 路径写入配置并保存；
4. 「记录窗口尺寸」→「截取目标窗口」完成区域标定；
5. 编译 `macosocr` 并填入 `ocr_command`；
6. 先开「只填不发」跑通：定位联系人 → 输入框有字 → 终态 `Prepared`；
7. 再关「只填不发」，人工确认后发送一条测试消息。
