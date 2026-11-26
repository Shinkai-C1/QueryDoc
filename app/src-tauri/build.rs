fn main() {
    embed_test_manifest_for_windows();
    tauri_build::build()
}

/// Windows 下 cargo test 的测试进程没有应用清单（manifest），
/// 加载器绑定 comctl32 v5（无 TaskDialogIndirect），进程启动即
/// STATUS_ENTRYPOINT_NOT_FOUND（0xc0000139，tauri-apps/tauri#13419）。
/// 真实 app 由 tauri-build 嵌入清单不受影响，故仅给测试目标补清单。
#[cfg(windows)]
fn embed_test_manifest_for_windows() {
    let manifest = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
</assembly>
"#;
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("test.manifest");
    std::fs::write(&out, manifest).unwrap();
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo::rustc-link-arg-tests=/MANIFEST:EMBED");
        println!(
            "cargo::rustc-link-arg-tests=/MANIFESTINPUT:{}",
            out.display()
        );
    }
}

#[cfg(not(windows))]
fn embed_test_manifest_for_windows() {}
