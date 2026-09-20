fn main() {
    tauri_build::build();

    // `tauri_build::build()` 只会给应用二进制嵌入清单，测试目标拿不到。
    //
    // 而 tauri 的依赖链（muda）会导入 comctl32 v6 才有的 TaskDialogIndirect，
    // 缺少 v6 声明时加载器会绑定 System32 下的 comctl32 v5，
    // 测试进程还没进 main 就 STATUS_ENTRYPOINT_NOT_FOUND (0xC0000139)。
    //
    // 这里单独给测试目标补上清单；细节见 test-manifest.xml。
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test-manifest.xml");
        println!("cargo:rerun-if-changed=test-manifest.xml");
        // `/MANIFEST:EMBED` 要显式给出：rustc 自己可能已经传过 `/MANIFEST:NO`，
        // 而 cargo 的 link-arg 排在最后，才能保证覆盖生效。
        println!("cargo:rustc-link-arg-tests=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-tests=/MANIFESTINPUT:{}", manifest.display());
    }
}
