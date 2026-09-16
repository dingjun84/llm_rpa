# 企业微信本地视觉自动化

当前仓库处于 **Windows MVP 脚手架** 阶段。真实企业微信操作尚未实现。

## 当前包含

- Rust 自动化状态机与平台/视觉端口；
- 用于单元测试的纯内存模拟平台；
- Tauri + React 的最小桌面界面结构；
- 设计文档与 Windows MVP 接口契约。

## 当前明确不包含

- 企业微信启动、截图、OCR、鼠标键盘或剪贴板的真实实现；
- 真实消息发送、批量任务或定时任务；
- 网络调用、远程 OCR、客户端注入或绕过限制的行为。

## 目录

```text
apps/desktop/              Tauri + React 界面
crates/automation-core/    任务状态机与抽象端口
crates/platform-mock/      仅测试用的模拟实现
docs/                      架构与接口说明
```

在安装 Rust 与 Node.js LTS 后，下一阶段先运行核心状态机测试；通过后再实现 Windows 屏幕捕获和本地 OCR 适配器。
