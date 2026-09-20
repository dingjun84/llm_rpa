//! `macosocr`：用 macOS Vision 离线 OCR 实现本项目的本地 OCR 契约。
//!
//! 权威实现是同目录下的 `macosocr.swift`（Vision 框架）。本 Rust 二进制在
//! **非 macOS** 上给出明确错误；在 **macOS** 上提示用 `swiftc` 构建 Swift 工具，
//! 或若 `PATH` / 同目录已有编译好的 `macosocr` 可执行文件则转发。
//!
//! 契约见 `crates/vision/src/ocr.rs`：stdin PNG → stdout JSON 数组。

fn main() {
    #[cfg(target_os = "macos")]
    {
        eprintln!(
            "macosocr (Rust 入口)：请先编译 Swift 实现——\n\
             \n\
             cd tools/macosocr && swiftc -O -framework Vision -framework CoreGraphics \\\n\
               -o ../../target/debug/macosocr macosocr.swift\n\
             \n\
             然后把路径填进配置的 ocr_command。\n\
             契约与 winocr 相同：PNG stdin → JSON stdout。"
        );
        std::process::exit(2);
    }
    #[cfg(not(target_os = "macos"))]
    {
        eprintln!("macosocr: 仅支持 macOS（请用 tools/macosocr/macosocr.swift 在 Mac 上构建）");
        std::process::exit(1);
    }
}
