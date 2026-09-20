fn main() {
    // 探针没有测试目标，所以不需要主应用 `build.rs` 里那段
    // "给测试目标补 comctl32 v6 清单" 的处理——`tauri_build::build()`
    // 已经把清单嵌进应用二进制了。
    tauri_build::build();
}
