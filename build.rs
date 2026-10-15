use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-changed=build.rs");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let icon = manifest_dir.join("assets").join("app.ico");
    if !icon.is_file() {
        println!("cargo:warning=缺少 assets/app.ico，跳过 exe 图标");
        return;
    }

    let Some(rc_exe) = find_rc_exe() else {
        println!("cargo:warning=未找到 Windows SDK 的 rc.exe，跳过 exe 图标");
        return;
    };

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let rc_file = out_dir.join("app.rc");
    let res_file = out_dir.join("app.res");
    // rc.exe 会按 C 转义规则解析引号内的字符串，路径中的反斜杠必须写成双反斜杠
    let icon_literal = icon.display().to_string().replace('\\', "\\\\");
    // 1 为图标资源 ID，仅嵌入图标，ID 任意且与其它资源不冲突即可
    fs::write(&rc_file, format!("1 ICON \"{icon_literal}\"\n")).expect("写入 app.rc 失败");

    let status = Command::new(&rc_exe)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res_file)
        .arg(&rc_file)
        .status()
        .expect("执行 rc.exe 失败");
    if !status.success() {
        panic!("rc.exe 编译资源失败（{status}）");
    }

    println!("cargo:rustc-link-arg={}", res_file.display());
}

/// 查找 Windows SDK 中的 rc.exe：优先取版本号最高的一份，其次回退到 PATH。
fn find_rc_exe() -> Option<PathBuf> {
    let mut sdk_candidates = Vec::new();
    for key in ["ProgramFiles(x86)", "ProgramFiles"] {
        let Ok(root) = env::var(key) else { continue };
        let bin = Path::new(&root).join("Windows Kits").join("10").join("bin");
        let Ok(entries) = fs::read_dir(&bin) else {
            continue;
        };
        for entry in entries.flatten() {
            let candidate = entry.path().join("x64").join("rc.exe");
            if candidate.is_file() {
                sdk_candidates.push(candidate);
            }
        }
    }
    sdk_candidates.sort();
    if let Some(rc) = sdk_candidates.pop() {
        return Some(rc);
    }

    let path = env::var_os("PATH")?;
    env::split_paths(&path).map(|dir| dir.join("rc.exe")).find(|p| p.is_file())
}
