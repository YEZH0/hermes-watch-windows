//! 签名产物的本地体检。
//!
//! iOS / watchOS 在签名对不上时只回一句 "The integrity of the app could not be
//! verified"，或者 `ApplicationVerificationFailed`，从来不说是哪一条不匹配。
//! 等设备吐这句话时，用户已经等了十几分钟，也不知道该改什么。
//!
//! 这里在把包交给设备之前，把**能离线判定**的项先对一遍：
//!
//!   * 每个 bundle 的 provisioning profile，其 `application-identifier`
//!     必须和这个 bundle 的 CFBundleIdentifier 对得上；
//!   * 开发 profile 的 `ProvisionedDevices` 必须包含目标设备（iPhone / 手表）；
//!   * watch app 的 profile 必须带 `com.apple.security.application-groups`。
//!
//! 判定刻意保守：只有"必然装不上"才算致命，其余只记日志。宁可多打一行，
//! 也不要把本来能用的流程挡下来。

use std::path::{Path, PathBuf};

use apple_codesign::ProvisioningProfile;
use plist::Value;
use rootcause::prelude::*;
use tracing::{info, warn};

const PROFILE_FILE: &str = "embedded.mobileprovision";
const APP_GROUP_ENTITLEMENT: &str = "com.apple.security.application-groups";

/// 体检结论。`fatal` 为空表示没发现"必然失败"的问题。
pub struct Findings {
    pub fatal: Vec<String>,
    pub notes: Vec<String>,
    /// 致命项里是否包含「手表没写进 profile」——上层据此给出更具体的引导。
    pub watch_device_missing: bool,
}

struct Target {
    path: PathBuf,
    watch: bool,
}

/// 体检整个签名产物。`watch_udids` 为空时跳过手表设备比对。
pub fn inspect(app_dir: &Path, iphone_udid: &str, watch_udids: &[String]) -> Findings {
    let mut findings = Findings {
        fatal: Vec::new(),
        notes: Vec::new(),
        watch_device_missing: false,
    };

    let mut targets = Vec::new();
    collect_targets(app_dir, &mut targets);

    if targets.is_empty() {
        findings
            .fatal
            .push(format!("签名产物里没找到任何 .app / .appex（{}）", app_dir.display()));
        return findings;
    }

    for target in &targets {
        check_one(target, iphone_udid, watch_udids, &mut findings);
    }

    findings
}

/// 把结论落到日志；有致命项就返回错误。
pub fn report(findings: &Findings) -> Result<(), Report> {
    for note in &findings.notes {
        info!("[preflight] {note}");
    }
    for fatal in &findings.fatal {
        warn!("[preflight] {fatal}");
    }

    if findings.fatal.is_empty() {
        info!("[preflight] 签名产物自检通过");
        return Ok(());
    }

    let joined = findings.fatal.join("\n  - ");
    bail!(
        "签名产物自检没通过，装上去大概率会报「无法验证此 App 的完整性」：\n  - {joined}"
    )
}

fn check_one(
    target: &Target,
    iphone_udid: &str,
    watch_udids: &[String],
    findings: &mut Findings,
) {
    let name = target
        .path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| target.path.display().to_string());

    let bundle_id = match read_bundle_id(&target.path) {
        Ok(id) => id,
        Err(e) => {
            findings.fatal.push(format!("{name}: 读不到 CFBundleIdentifier（{e}）"));
            return;
        }
    };

    let profile_path = target.path.join(PROFILE_FILE);
    let bytes = match std::fs::read(&profile_path) {
        Ok(bytes) => bytes,
        Err(e) => {
            findings
                .fatal
                .push(format!("{name}: 缺少 {PROFILE_FILE}（{e}）"));
            return;
        }
    };

    let profile = match ProvisioningProfile::parse(&bytes) {
        Ok(profile) => profile,
        Err(e) => {
            findings
                .fatal
                .push(format!("{name}: profile 解析失败（{e}）"));
            return;
        }
    };

    // 1. application-identifier 必须指向这个 bundle：前缀是 TeamID，后面是 bundle id。
    //    这一条错了，设备一定报完整性验证失败。
    match profile
        .entitlements()
        .get("application-identifier")
        .and_then(Value::as_string)
    {
        Some(app_id) => {
            let suffix = app_id.split_once('.').map(|(_, rest)| rest).unwrap_or(app_id);
            if suffix != bundle_id {
                findings.fatal.push(format!(
                    "{name}: profile 是给 {suffix} 的，但 bundle 的 id 是 {bundle_id}"
                ));
            }
        }
        None => findings
            .fatal
            .push(format!("{name}: profile 的 entitlements 里没有 application-identifier")),
    }

    // 2. 开发 profile 会列出允许安装的设备；免费账号的 profile 一定有这个字段。
    match profile.plist().get("ProvisionedDevices").and_then(Value::as_array) {
        Some(devices) => {
            let allowed: Vec<&str> = devices.iter().filter_map(Value::as_string).collect();

            if allowed.is_empty() {
                findings
                    .notes
                    .push(format!("{name}: profile 的 ProvisionedDevices 是空的"));
            } else if target.watch {
                let hit = watch_udids
                    .iter()
                    .any(|w| allowed.iter().any(|d| d.eq_ignore_ascii_case(w)));
                if !watch_udids.is_empty() && !hit {
                    findings.watch_device_missing = true;
                    findings.fatal.push(format!(
                        "{name}: profile 里没有这台手表（{}），只有 {} 台设备",
                        watch_udids.join(", "),
                        allowed.len()
                    ));
                }
            } else if !allowed.iter().any(|d| d.eq_ignore_ascii_case(iphone_udid)) {
                findings.fatal.push(format!(
                    "{name}: profile 里没有这台 iPhone（{iphone_udid}），只有 {} 台设备",
                    allowed.len()
                ));
            }
        }
        None => findings
            .notes
            .push(format!("{name}: profile 没有 ProvisionedDevices")),
    }

    // 3. watch app 拿不到 App Group，手表上照样报 "An App Group ID may be missing"。
    if target.watch {
        match profile.entitlements().get(APP_GROUP_ENTITLEMENT) {
            Some(Value::Array(groups)) => {
                let groups: Vec<&str> = groups.iter().filter_map(Value::as_string).collect();
                if groups.is_empty() {
                    findings.fatal.push(format!(
                        "{name}: entitlements 里的 {APP_GROUP_ENTITLEMENT} 是空数组"
                    ));
                } else {
                    info!("[preflight] {name}: app groups = {groups:?}");
                }
            }
            _ => findings.fatal.push(format!(
                "{name}: entitlements 里没有 {APP_GROUP_ENTITLEMENT}"
            )),
        }
    }

    // 4. profile 到期时间：免费账号 7 天，过期后同样只报一句看不懂的错。
    if let Some(date) = profile.plist().get("ExpirationDate").and_then(Value::as_date) {
        findings.notes.push(format!("{name}: profile 到期 {date:?}"));
    }

    // 5. 签名对象必须存在且非空。
    for exe in executables(&target.path) {
        match std::fs::metadata(&exe) {
            Ok(m) if m.len() > 0 => {}
            Ok(_) => findings
                .fatal
                .push(format!("{name}: 可执行文件 {} 是空的", exe.display())),
            Err(e) => findings
                .fatal
                .push(format!("{name}: 找不到可执行文件 {}（{e}）", exe.display())),
        }
    }
}

fn read_bundle_id(app: &Path) -> Result<String, String> {
    let info = app.join("Info.plist");
    let value = Value::from_file(&info).map_err(|e| format!("{}: {e}", info.display()))?;
    let dict = value
        .as_dictionary()
        .ok_or_else(|| format!("{} 不是字典", info.display()))?;
    dict.get("CFBundleIdentifier")
        .and_then(Value::as_string)
        .map(str::to_string)
        .ok_or_else(|| format!("{} 里没有 CFBundleIdentifier", info.display()))
}

fn executables(app: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(value) = Value::from_file(app.join("Info.plist")) else {
        return out;
    };
    let Some(dict) = value.as_dictionary() else {
        return out;
    };
    if let Some(exe) = dict.get("CFBundleExecutable").and_then(Value::as_string) {
        out.push(app.join(exe));
    }
    out
}

/// 收集 root bundle 与所有会单独签名、单独装到设备上的嵌套 bundle。
fn collect_targets(dir: &Path, out: &mut Vec<Target>) {
    out.push(Target {
        path: dir.to_path_buf(),
        watch: false,
    });

    for folder in ["Watch", "PlugIns"] {
        let Ok(entries) = std::fs::read_dir(dir.join(folder)) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if !is_dir {
                continue;
            }
            let ext = path.extension().map(|e| e.to_string_lossy().to_string());
            if !matches!(ext.as_deref(), Some("app") | Some("appex")) {
                continue;
            }
            let watch = folder == "Watch";
            out.push(Target {
                path: path.clone(),
                watch,
            });
            collect_nested(&path, watch, out);
        }
    }
}

fn collect_nested(dir: &Path, watch: bool, out: &mut Vec<Target>) {
    let Ok(entries) = std::fs::read_dir(dir.join("PlugIns")) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e == "appex").unwrap_or(false) {
            out.push(Target {
                path,
                watch,
            });
        }
    }
}
