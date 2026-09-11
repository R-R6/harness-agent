fn main() {
    tauri_build::build();

    // windows-gnu 工具链：tauri 只给 bin 目标嵌 Common-Controls manifest
    // （embed-resource v3 走 cargo:rustc-link-arg-bins），测试二进制没有
    // RT_MANIFEST 时 mingw crt 会链入默认基础 manifest，comctl32 绑到 v5，
    // tao 导入的 TaskDialogIndirect 在 v5 里不存在 → cargo test 的测试进程
    // 启动即 STATUS_ENTRYPOINT_NOT_FOUND。这里给测试目标补嵌同一份 manifest，
    // 让 comctl32 走 v6 SxS 程序集。msvc 由 tauri 自己的 /MANIFESTINPUT 处理，
    // 无需重复。
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu")
        && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
    {
        let manifest = std::env::var("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR")
            .replace('\\', "/")
            + "/windows-app-manifest.xml";
        let rc = std::env::var("OUT_DIR").expect("OUT_DIR") + "/test-manifest.rc";
        std::fs::write(&rc, format!("1 24 \"{manifest}\"")).expect("写 test-manifest.rc 失败");
        // compile_for_everything（全目标 rustc-link-arg）：compile()/compile_for_tests()
        // 都只覆盖 [[test]] 声明目标，lib 单元测试二进制拿不到资源。
        // manifest 内容与 tauri 给 bin 嵌的一致，重复无功能影响。
        let _ = embed_resource::compile_for_everything(&rc, embed_resource::NONE);
    }
}
