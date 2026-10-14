# vhidlab — 虚拟 HID 客户端

对接自研 KMDF+VHF 驱动（`\\.\VhidLab`）。相对移动/滚轮限 ±127，按键 id 与 GhostBox 一致（1 左 / 2 中 / 3 右）。

绝对 `move_to`：三次贝塞尔 + `GetCursorPos` 闭环校正（GhostBox 的曲线在厂商 DLL 内，本 crate 软件实现）。

仅 Windows。驱动工程见同级仓库 `vhid-lab`。
