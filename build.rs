use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const RC_CONTENT: &str = r#"1 ICON "app.ico"
2 ICON "tray_dark.ico"
3 ICON "tray_light.ico"

1 VERSIONINFO
FILEVERSION 0,1,0,0
PRODUCTVERSION 0,1,0,0
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904B0"
        BEGIN
            VALUE "CompanyName", "IT Tools"
            VALUE "FileDescription", "RDP Center"
            VALUE "FileVersion", "0.1.0.0"
            VALUE "InternalName", "RDPCenter"
            VALUE "OriginalFilename", "RDPCenter.exe"
            VALUE "ProductName", "RDP Center"
            VALUE "ProductVersion", "0.1.0.0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0409, 1200
    END
END
"#;

fn main() {
    let icons = ["app.ico", "tray_dark.ico", "tray_light.ico"];
    for name in icons {
        println!("cargo:rerun-if-changed=assets/{name}");
    }

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    for name in icons {
        let src = PathBuf::from("assets").join(name);
        if !src.is_file() {
            println!("cargo:warning=Missing assets/{name}; skipping Windows icon embedding");
            return;
        }
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR not set"));
    for name in icons {
        let src = PathBuf::from("assets").join(name);
        let dst = out_dir.join(name);
        fs::copy(&src, &dst).unwrap_or_else(|e| panic!("failed to copy {name} into OUT_DIR: {e}"));
    }

    let rc_path = out_dir.join("icon.rc");
    fs::write(&rc_path, RC_CONTENT).expect("failed to write icon.rc");

    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_env == "gnu" {
        let obj_path = out_dir.join("icon.o");
        let compiled = ["x86_64-w64-mingw32-windres", "windres"]
            .into_iter()
            .any(|bin| {
                Command::new(bin)
                    .current_dir(&out_dir)
                    .args(["-O", "coff", "-i", "icon.rc", "-o", "icon.o"])
                    .status()
                    .is_ok_and(|s| s.success())
            });
        if compiled {
            println!("cargo:rustc-link-arg-bins={}", obj_path.display());
        } else {
            println!("cargo:warning=windres not available or failed; .exe resources skipped");
        }
    } else if target_env == "msvc" {
        let res_path = out_dir.join("icon.res");
        if Command::new("rc.exe")
            .current_dir(&out_dir)
            .args(["/nologo", "/fo", "icon.res", "icon.rc"])
            .status()
            .is_ok_and(|s| s.success())
        {
            println!("cargo:rustc-link-arg-bins={}", res_path.display());
        } else {
            println!("cargo:warning=rc.exe not available or failed; .exe resources skipped");
        }
    }
}
