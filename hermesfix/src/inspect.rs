//! `hermesfix --inspect <ipa>`：只读诊断，不登录、不联网、不碰设备。
//!
//! 存在的理由：watch app 装不上时，第一个要回答的问题不是"签名对不对"，而是
//! "这个 IPA 里的 watch app 到底有没有被解析出来"。如果 `watch_apps` 是空的，
//! 后面既不会给它签名，也不会把它推到手表，而整个流程仍然会打印 [DONE] ——
//! 表现出来就是"iPhone 装好了，手表上什么都没有"。
//!
//! 这个子命令把解包后的结构和解析结果直接打出来，用来区分这两种情况。

use std::path::PathBuf;

use isideload::sideload::{application::Application, bundle::Bundle};

pub fn run(ipa: PathBuf) -> i32 {
    println!("IPA    : {}", ipa.display());
    match std::fs::metadata(&ipa) {
        Ok(m) => println!("Size   : {} bytes", m.len()),
        Err(e) => {
            eprintln!("[FATAL] cannot stat {}: {e}", ipa.display());
            return 2;
        }
    }

    let app = match Application::new(ipa.clone()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[FATAL] cannot open the IPA: {e:?}");
            return 2;
        }
    };

    let root = &app.bundle;
    println!("Bundle : {}", root.bundle_dir.display());
    println!("  id         : {}", root.bundle_identifier().unwrap_or("<missing>"));
    println!("  name       : {}", root.bundle_name().unwrap_or("<missing>"));
    println!(
        "  executable : {}",
        root.app_info
            .get("CFBundleExecutable")
            .and_then(|v| v.as_string())
            .unwrap_or("<missing>")
    );
    println!("  extensions : {}", root.app_extensions().len());
    println!("  frameworks : {}", root.frameworks().len());
    println!("  watch apps : {}", root.watch_apps().len());

    // Watch/ 目录下实际有什么，是解析失败时唯一能看出差别的地方。
    let watch_dir = root.bundle_dir.join("Watch");
    match std::fs::read_dir(&watch_dir) {
        Ok(entries) => {
            println!("  Watch/ entries:");
            for entry in entries.flatten() {
                let meta = entry.metadata();
                println!(
                    "    {}  (dir={:?})",
                    entry.file_name().to_string_lossy(),
                    meta.map(|m| m.is_dir()).ok()
                );
            }
        }
        Err(e) => println!("  Watch/ entries: <cannot read: {e}>"),
    }

    if root.watch_apps().is_empty() {
        println!("\n[!] No watch app was parsed out of this bundle.");
        println!("    Nothing downstream will sign or install it, and the run will still");
        println!("    report [DONE] — which looks exactly like 'the Watch never got the app'.");
        return 1;
    }

    let mut bad = 0;
    for watch in root.watch_apps() {
        println!("\nWatch app: {}", watch.bundle_dir.display());
        report_bundle(watch, "  ", &mut bad);
    }

    if bad > 0 {
        println!("\n[!] {bad} watch bundle(s) look unusable — see the notes above.");
        return 1;
    }

    println!("\n[OK] watch app(s) parsed and structurally usable.");
    0
}

fn report_bundle(bundle: &Bundle, indent: &str, bad: &mut i32) {
    println!("{indent}id           : {}", bundle.bundle_identifier().unwrap_or("<missing>"));
    println!("{indent}name         : {}", bundle.bundle_name().unwrap_or("<missing>"));

    let exe = bundle
        .app_info
        .get("CFBundleExecutable")
        .and_then(|v| v.as_string())
        .unwrap_or("<missing>");
    println!("{indent}executable   : {exe}");

    if let Some(companion) = bundle
        .app_info
        .get("WKCompanionAppBundleIdentifier")
        .and_then(|v| v.as_string())
    {
        println!("{indent}companion    : {companion}");
    } else {
        println!("{indent}companion    : <missing WKCompanionAppBundleIdentifier>");
        *bad += 1;
    }

    // 签名对象就是这个文件：不存在的话 install 一定失败，而且报错通常很含糊。
    let exe_path = bundle.bundle_dir.join(exe);
    match std::fs::metadata(&exe_path) {
        Ok(m) => println!("{indent}exe size     : {} bytes", m.len()),
        Err(e) => {
            println!("{indent}exe size     : <cannot stat {exe}: {e}>");
            *bad += 1;
        }
    }

    let mut files = 0usize;
    let mut dirs = 0usize;
    count(&bundle.bundle_dir, &mut files, &mut dirs);
    println!("{indent}files        : {files} in {dirs} dirs");
}

fn count(dir: &std::path::Path, files: &mut usize, dirs: &mut usize) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        match entry.metadata() {
            Ok(m) if m.is_dir() => {
                *dirs += 1;
                count(&entry.path(), files, dirs);
            }
            Ok(_) => *files += 1,
            Err(_) => {}
        }
    }
}
