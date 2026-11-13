//! 签名产物的本地缓存，用来做「只重试手表」。
//!
//! 一次完整安装里最贵的是登录、申请证书、注册 App ID、下载 profile、重签——
//! 手表没连上就全部白做，用户还得从 Apple ID 密码重新输一遍。签名完成时把
//! 产物原样留一份，失败后就能直接从「安装到手表」继续。
//!
//! 安全边界：
//!
//!   * 只落在 `%LOCALAPPDATA%\HermesWatch\signed\<id>`，不写仓库、不写临时目录；
//!   * 额外用 icacls 去掉继承、只授权当前用户（失败只记日志，不阻断）；
//!   * 内容只有签名后的 .app 与一份 manifest，**没有**密码、验证码、token、
//!     配对文件、私钥；
//!   * 每次复用前校验整棵树的哈希与 profile 到期时间；
//!   * `--purge-cache` 一次删干净，最多保留两份。

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use apple_codesign::ProvisioningProfile;
use plist::Value;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MANIFEST: &str = "manifest.json";
const APP_DIR: &str = "app";
const PROFILE: &str = "embedded.mobileprovision";
/// 免费账号的 profile 只有 7 天。留 10 分钟余量，避免刚装到一半就过期。
const EXPIRY_MARGIN_SECS: i64 = 600;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BundleInfo {
    /// 相对 `app/` 的路径，根 bundle 是 `.`。
    pub path: String,
    pub bundle_id: String,
    pub expires_at: Option<i64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Manifest {
    pub version: u32,
    pub created_at: i64,
    pub engine: String,
    pub ipa_sha256: String,
    pub iphone_udid: String,
    pub watch_udids: Vec<String>,
    pub root_bundle_id: String,
    pub bundles: Vec<BundleInfo>,
    pub tree_digest: String,
    pub files: usize,
    pub iphone_installed: bool,
}

pub struct Saved {
    pub id: String,
    pub dir: PathBuf,
    pub manifest: Manifest,
}

pub fn root() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("HermesWatch").join("signed")
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 把签名后的 app 目录存一份，返回缓存 id。
pub fn save(
    signed_app: &Path,
    engine: &str,
    ipa_sha256: &str,
    iphone_udid: &str,
    watch_udids: &[String],
    root_bundle_id: &str,
) -> Result<Saved, String> {
    let created_at = now();
    let id = cache_id(ipa_sha256, created_at, iphone_udid);
    let base = root();
    let target = base.join(&id);

    std::fs::create_dir_all(&base).map_err(|e| format!("建缓存目录失败：{e}"))?;

    let staging = base.join(format!(".staging-{}-{created_at}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let app_dst = staging.join(APP_DIR);
    copy_tree(signed_app, &app_dst).map_err(|e| format!("复制签名产物失败：{e}"))?;

    let (tree_digest, files) = digest_tree(&app_dst).map_err(|e| format!("算产物哈希失败：{e}"))?;
    let bundles = collect_bundles(&app_dst).map_err(|e| format!("读签名产物失败：{e}"))?;

    let manifest = Manifest {
        version: 1,
        created_at,
        engine: engine.to_string(),
        ipa_sha256: ipa_sha256.to_string(),
        iphone_udid: iphone_udid.to_string(),
        watch_udids: watch_udids.to_vec(),
        root_bundle_id: root_bundle_id.to_string(),
        bundles,
        tree_digest,
        files,
        iphone_installed: false,
    };
    write_manifest(&staging, &manifest)?;

    // 先落 staging 再整体改名：中途崩了也不会留下半份缓存被当成可用。
    let _ = std::fs::remove_dir_all(&target);
    std::fs::rename(&staging, &target).map_err(|e| format!("提交缓存失败：{e}"))?;
    restrict_permissions(&target);
    prune(&id);

    Ok(Saved {
        id,
        dir: target,
        manifest,
    })
}

/// iPhone 装好之后回写标记，重试时用来确认「iPhone 那步不用再做」。
pub fn mark_iphone_installed(dir: &Path) -> Result<(), String> {
    let path = dir.join(MANIFEST);
    let mut manifest = read_manifest(&path)?;
    manifest.iphone_installed = true;
    write_manifest(dir, &manifest)
}

/// 读出最近（或指定）的一份缓存，并做完整校验。
pub fn load(id: Option<&str>) -> Result<Saved, String> {
    let base = root();
    let dir = match id {
        Some(id) => base.join(sanitize(id)),
        None => latest(&base)?,
    };

    let manifest = read_manifest(&dir.join(MANIFEST))?;
    if manifest.version != 1 {
        return Err(format!("缓存格式版本是 {}，本程序只认 1", manifest.version));
    }

    let app = dir.join(APP_DIR);
    if !app.is_dir() {
        return Err("缓存里没有签名产物".into());
    }

    let (digest, files) = digest_tree(&app).map_err(|e| format!("算产物哈希失败：{e}"))?;
    if digest != manifest.tree_digest || files != manifest.files {
        return Err("签名产物被改动过（哈希不一致）".into());
    }

    let deadline = now() + EXPIRY_MARGIN_SECS;
    for bundle in &manifest.bundles {
        if let Some(expiry) = bundle.expires_at
            && expiry <= deadline
        {
            return Err(format!(
                "描述文件将在 {} 分钟后过期（免费账号只有 7 天）",
                (expiry - now()).max(0) / 60
            ));
        }
    }

    Ok(Saved {
        id: dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        dir,
        manifest,
    })
}

/// 删除全部缓存，返回释放的字节数。
pub fn purge() -> Result<u64, String> {
    let base = root();
    if !base.exists() {
        return Ok(0);
    }
    // 只删自己那块，别让一个坏掉的路径把别的东西删了。
    let canon = base.canonicalize().map_err(|e| format!("解析缓存路径失败：{e}"))?;
    if !canon.ends_with("HermesWatch\\signed") && !canon.ends_with("HermesWatch/signed") {
        return Err(format!("缓存路径不像自己的：{}", canon.display()));
    }

    let freed = dir_size(&canon);
    std::fs::remove_dir_all(&canon).map_err(|e| format!("删缓存失败：{e}"))?;
    Ok(freed)
}

// ---------------------------------------------------------------- 内部

fn cache_id(ipa_sha: &str, created_at: i64, udid: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(ipa_sha.as_bytes());
    hasher.update(created_at.to_le_bytes());
    hasher.update(udid.as_bytes());
    hex(&hasher.finalize())[..16].to_string()
}

fn sanitize(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_hexdigit() || *c == '-')
        .collect()
}

fn latest(base: &Path) -> Result<PathBuf, String> {
    let mut best: Option<(SystemTime, PathBuf)> = None;
    let entries = std::fs::read_dir(base).map_err(|_| "还没有可复用的签名产物".to_string())?;
    for entry in entries.flatten() {
        if !entry.path().join(MANIFEST).is_file() {
            continue;
        }
        let stamp = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH);
        if best.as_ref().map(|(t, _)| stamp > *t).unwrap_or(true) {
            best = Some((stamp, entry.path()));
        }
    }
    best.map(|(_, p)| p)
        .ok_or_else(|| "还没有可复用的签名产物".to_string())
}

fn prune(keep: &str) {
    let base = root();
    let Ok(entries) = std::fs::read_dir(&base) else { return };
    let mut dirs: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().join(MANIFEST).is_file())
        .filter(|e| e.file_name().to_string_lossy() != keep)
        .filter_map(|e| {
            let stamp = e.metadata().and_then(|m| m.modified()).ok()?;
            Some((stamp, e.path()))
        })
        .collect();
    dirs.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in dirs.into_iter().skip(1) {
        let _ = std::fs::remove_dir_all(path);
    }
}

fn read_manifest(path: &Path) -> Result<Manifest, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("读不到 manifest：{e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("manifest 解析失败：{e}"))
}

fn write_manifest(dir: &Path, manifest: &Manifest) -> Result<(), String> {
    let text = serde_json::to_string_pretty(manifest).map_err(|e| format!("manifest 序列化失败：{e}"))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败：{e}"))?;
    std::fs::write(dir.join(MANIFEST), text).map_err(|e| format!("写 manifest 失败：{e}"))
}

fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

/// 排序后逐文件累积哈希：文件增删改都会变。
fn digest_tree(dir: &Path) -> std::io::Result<(String, usize)> {
    let mut files: BTreeMap<String, PathBuf> = BTreeMap::new();
    collect_files(dir, dir, &mut files)?;

    let mut hasher = Sha256::new();
    for (rel, path) in &files {
        let mut file = std::fs::File::open(path)?;
        let mut file_hash = Sha256::new();
        let mut buf = [0u8; 64 * 1024];
        let mut len = 0u64;
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            len += n as u64;
            file_hash.update(&buf[..n]);
        }
        hasher.update(rel.as_bytes());
        hasher.update(b"\0");
        hasher.update(len.to_le_bytes());
        hasher.update(file_hash.finalize());
    }

    Ok((hex(&hasher.finalize()), files.len()))
}

fn collect_files(root_dir: &Path, dir: &Path, out: &mut BTreeMap<String, PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_files(root_dir, &path, out)?;
        } else {
            let rel = path
                .strip_prefix(root_dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(rel, path);
        }
    }
    Ok(())
}

/// 找出每个会单独签名的 bundle，记下它的 profile 到期时间。
fn collect_bundles(app: &Path) -> Result<Vec<BundleInfo>, String> {
    let mut out = Vec::new();
    let mut dirs = vec![(app.to_path_buf(), ".".to_string())];

    // Watch/ 与 PlugIns/ 下的 .app/.appex 各自带一份 profile。
    for folder in ["Watch", "PlugIns"] {
        let Ok(entries) = std::fs::read_dir(app.join(folder)) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let ext = path.extension().map(|e| e.to_string_lossy().to_string());
            if matches!(ext.as_deref(), Some("app") | Some("appex")) {
                let rel = format!("{folder}/{}", path.file_name().unwrap_or_default().to_string_lossy());
                dirs.push((path, rel));
            }
        }
    }

    for (dir, rel) in dirs {
        let Some(bundle_id) = read_string(&dir.join("Info.plist"), "CFBundleIdentifier") else {
            continue;
        };
        let expires_at = std::fs::read(dir.join(PROFILE))
            .ok()
            .and_then(|bytes| ProvisioningProfile::parse(&bytes).ok())
            .and_then(|profile| {
                profile
                    .plist()
                    .get("ExpirationDate")
                    .and_then(Value::as_date)
                    .map(|date| {
                        let time: SystemTime = date.to_owned().into();
                        time.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
                    })
            });
        out.push(BundleInfo {
            path: rel,
            bundle_id,
            expires_at,
        });
    }

    Ok(out)
}

fn read_string(plist: &Path, key: &str) -> Option<String> {
    let value = Value::from_file(plist).ok()?;
    value
        .as_dictionary()?
        .get(key)?
        .as_string()
        .map(str::to_string)
}

fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            total += dir_size(&entry.path());
        } else {
            total += meta.len();
        }
    }
    total
}

/// 去掉继承、只留当前用户。失败不阻断——缓存本身就是本地临时产物，
/// 但会把结果记进日志，让「权限没收紧」这件事可见。
pub(crate) fn restrict_permissions(dir: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use std::process::Command;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let Ok(user) = std::env::var("USERNAME") else { return };
        if user.trim().is_empty() {
            return;
        }

        let status = Command::new("icacls")
            .arg(dir)
            .arg("/inheritance:r")
            .arg("/grant:r")
            .arg(format!("{user}:(OI)(CI)F"))
            .arg("/T")
            .arg("/C")
            .arg("/Q")
            .creation_flags(CREATE_NO_WINDOW)
            .status();

        match status {
            Ok(s) if s.success() => tracing::info!("[cache] 已把 {} 的权限收紧到当前用户", dir.display()),
            Ok(s) => tracing::warn!("[cache] icacls 退出码 {:?}，缓存目录仍是默认权限", s.code()),
            Err(e) => tracing::warn!("[cache] 无法调用 icacls（{e}），缓存目录仍是默认权限"),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
    }
}

/// IPA 的 SHA-256，写进 manifest 用来对应「这份缓存是从哪个包签出来的」。
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("读不到文件：{e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| format!("读文件失败：{e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hf-cache-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_app(dir: &Path) {
        std::fs::create_dir_all(dir.join("Watch/Demo.app")).unwrap();
        std::fs::write(dir.join("Demo"), b"root-executable").unwrap();
        std::fs::write(dir.join("Info.plist"), plist_with("com.demo.app")).unwrap();
        std::fs::write(dir.join("Watch/Demo.app/DemoWatch"), b"watch").unwrap();
        // 手表 bundle 也要有自己的 Info.plist，否则 collect_bundles 认不出来
        std::fs::write(
            dir.join("Watch/Demo.app/Info.plist"),
            plist_with("com.demo.app.watchkitapp"),
        )
        .unwrap();
    }

    fn plist_with(bundle_id: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>CFBundleIdentifier</key><string>{bundle_id}</string></dict></plist>"#
        )
    }

    #[test]
    fn digest_changes_when_a_file_changes() {
        let a = scratch("digest-a");
        fake_app(&a);
        let (first, count) = digest_tree(&a).unwrap();
        assert_eq!(count, 4);

        std::fs::write(a.join("Demo"), b"root-executable-changed").unwrap();
        let (second, _) = digest_tree(&a).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn digest_changes_when_a_file_is_added() {
        let a = scratch("digest-b");
        fake_app(&a);
        let (first, _) = digest_tree(&a).unwrap();
        std::fs::write(a.join("extra"), b"x").unwrap();
        let (second, count) = digest_tree(&a).unwrap();
        assert_ne!(first, second);
        assert_eq!(count, 5);
    }

    #[test]
    fn bundles_are_found_including_the_watch_one() {
        let a = scratch("bundles");
        fake_app(&a);
        let bundles = collect_bundles(&a).unwrap();
        assert_eq!(bundles.len(), 2);
        assert!(
            bundles
                .iter()
                .any(|b| b.bundle_id == "com.demo.app" && b.path == ".")
        );
        assert!(
            bundles
                .iter()
                .any(|b| b.bundle_id == "com.demo.app.watchkitapp" && b.path.starts_with("Watch/"))
        );
        // 没有 profile 时到期时间是 None，不能瞎猜一个日期出来。
        assert!(bundles.iter().all(|b| b.expires_at.is_none()));
    }

    #[test]
    fn cache_id_is_filesystem_safe() {
        let id = cache_id(&"a".repeat(64), 1_700_000_000, "00008110-001234567890001E");
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn sanitize_strips_path_traversal() {
        // 目的是「结果不能当路径用」，不是某个固定字符串。
        for evil in ["../../etc/passwd", "..\\..\\windows\\system32", "a/../../b"] {
            let clean = sanitize(evil);
            assert!(!clean.contains('.'), "{evil} → {clean} 里还有点号");
            assert!(!clean.contains('/'), "{evil} → {clean} 里还有斜杠");
            assert!(!clean.contains('\\'), "{evil} → {clean} 里还有反斜杠");
        }
        assert_eq!(sanitize("abc123"), "abc123");
    }
}
