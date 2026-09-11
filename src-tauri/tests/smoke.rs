//! 冒烟集成测试：链接主 lib 的最小用例。
//!
//! 顺带作为 manifest 修复的金丝雀——windows-gnu 下测试二进制若拿不到
//! Common-Controls manifest（见 build.rs），会启动即 STATUS_ENTRYPOINT_NOT_FOUND。

#[test]
fn links_lib_and_smoke_passes() {
    // 引用主 lib 的公开项，保证集成目标与 lib 保持可编译关系
    let _ = harness_agent_lib::run;
}
