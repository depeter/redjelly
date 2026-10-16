fn main() {
    slint_build::compile("ui/app.slint").expect("Slint UI failed to compile");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_windows_resources();
    }
}

/// Icon and version info for Explorer, the taskbar and Apps & features.
/// Uses the toolchain's windres (llvm-mingw: x86_64-w64-mingw32-windres).
fn embed_windows_resources() {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let icon = manifest.join("../installer/redjelly.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    println!("cargo:rerun-if-env-changed=WINDRES");

    let version = env!("CARGO_PKG_VERSION");
    let mut parts: Vec<u16> = version.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    parts.resize(4, 0);
    let v = parts.iter().map(u16::to_string).collect::<Vec<_>>().join(",");
    let rc = format!(
        r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {v}
PRODUCTVERSION {v}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "redjelly"
      VALUE "FileDescription", "redjelly - Jellyfin client"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "redjelly"
      VALUE "LegalCopyright", "GPL-3.0-or-later"
      VALUE "OriginalFilename", "redjelly.exe"
      VALUE "ProductName", "redjelly"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = icon.display().to_string().replace('\\', "/"),
    );
    let rc_path = out.join("redjelly.rc");
    let obj = out.join("redjelly-res.o");
    std::fs::write(&rc_path, rc).unwrap();
    let windres = std::env::var("WINDRES").unwrap_or_else(|_| "x86_64-w64-mingw32-windres".into());
    let status = std::process::Command::new(&windres)
        .arg("-i")
        .arg(&rc_path)
        .arg("-o")
        .arg(&obj)
        .arg("--output-format=coff")
        .status()
        .unwrap_or_else(|e| panic!("running {windres}: {e} (set WINDRES)"));
    assert!(status.success(), "{windres} failed");
    println!("cargo:rustc-link-arg-bins={}", obj.display());
}
