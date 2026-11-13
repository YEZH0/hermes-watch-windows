//! 配对记录：iPhone 一份、手表一份。
//!
//! 装到手表这件事有两道「信任」关卡，缺任何一道都走不通：
//!
//! 1. **iPhone 信任这台电脑。** 以前只做「去 usbmuxd 目录里读，读不到就报错」，
//!    所以从没在这台机器上配过对的 iPhone 直接卡死在第一步——界面只会说
//!    「拿不到配对记录」，用户完全不知道该去手机上点「信任」。
//!    现在读不到就主动发一次 Pair，把那句系统弹窗问出来。
//! 2. **手表信任这台电脑。** 这一份记录 iPhone 上**没有**，只在手表自己那边生效，
//!    而且必须由一次真正成功的 `pair` 换回来。以前每次检测都重新 `pair` 一遍，
//!    手表每次都得弹一次信任框——不弹就干等 30 秒超时。现在配对成功即落盘，
//!    下次直接 `start_session`，手表什么都不用做。
//!
//! 两份记录都只写本机 `%LOCALAPPDATA%\HermesWatch\pairing`，用 icacls 收到当前用户，
//! 不进日志、不上传。删掉这个目录等于「忘记信任」，下次会重新问一遍。

use std::path::PathBuf;
use std::time::Duration;

use idevice::{
    IdeviceError,
    pairing_file::PairingFile,
    provider::IdeviceProvider,
    services::lockdown::LockdownClient,
};
use tracing::{info, warn};

/// iPhone lockdownd 的 USB 端口。
const PHONE_LOCKDOWN_PORT: u16 = 62078;
/// 用户在手机上点「信任」要多久：给足 60 秒，别催。
const USER_CONFIRM_TIMEOUT: Duration = Duration::from_secs(60);

const PHONE_PREFIX: &str = "phone";
const WATCH_PREFIX: &str = "watch";
const HOST_ID_FILE: &str = "host-id.txt";

/// 配对失败的原因。这些东西必须分开讲，用户才知道手该往哪儿伸。
#[derive(Debug)]
pub enum PairFail {
    /// 弹窗已经发出去了，等人在手机上点「信任」，等到超时也没点。
    UserDidNotConfirm,
    /// 手机锁着，lockdownd 直接拒了。
    PhoneLocked,
    /// 用户在手机上点了「不信任」。
    Denied,
    /// 连都连不上 / 其他。
    Other(String),
}

impl PairFail {
    pub fn code(&self) -> &'static str {
        match self {
            PairFail::UserDidNotConfirm => "PHONE_TRUST_TIMEOUT",
            PairFail::PhoneLocked => "PHONE_LOCKED",
            PairFail::Denied => "PHONE_TRUST_DENIED",
            PairFail::Other(_) => "PHONE_PAIR_FAILED",
        }
    }

    pub fn detail(&self) -> String {
        match self {
            PairFail::UserDidNotConfirm => format!(
                "已经请求 iPhone 信任这台电脑，等了 {} 秒没等到「信任」 —— \
                 解锁 iPhone，看到「要信任此电脑吗」点「信任」并输入锁屏密码",
                USER_CONFIRM_TIMEOUT.as_secs()
            ),
            PairFail::PhoneLocked => "iPhone 处于锁屏状态：先解锁，再重试".into(),
            PairFail::Denied => "iPhone 上点了「不信任」：拔掉数据线重插，这次点「信任」".into(),
            PairFail::Other(why) => why.clone(),
        }
    }
}

fn base_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("HermesWatch")
        .join("pairing")
}

fn file_for(prefix: &str, id: &str) -> PathBuf {
    // UDID / 手表 identifier 都是十六进制加连字符，还是过一遍白名单，
    // 免得外部环境变量把 `..` 之类的东西拼进路径。
    let safe: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    base_dir().join(format!("{prefix}-{safe}.plist"))
}

/// 读一份缓存下来的配对记录。文件坏了就当没有。
pub fn load(prefix: &str, id: &str) -> Option<PairingFile> {
    let path = file_for(prefix, id);
    if !path.is_file() {
        return None;
    }
    match PairingFile::read_from_file(&path) {
        Ok(file) => Some(file),
        Err(e) => {
            warn!("配对缓存 {} 读不出来（{e}），当作没有", path.display());
            None
        }
    }
}

pub fn save(prefix: &str, id: &str, file: &PairingFile) -> Result<(), String> {
    let dir = base_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("建配对缓存目录失败：{e}"))?;
    let bytes = file
        .clone()
        .serialize()
        .map_err(|e| format!("序列化配对记录失败：{e}"))?;
    let path = file_for(prefix, id);
    std::fs::write(&path, bytes).map_err(|e| format!("写配对记录失败：{e}"))?;
    crate::cache::restrict_permissions(&dir);
    Ok(())
}

/// 忘掉某一台设备的配对记录。`--forget-pairing` 用得上。
pub fn forget(prefix: &str, id: &str) -> bool {
    let path = file_for(prefix, id);
    std::fs::remove_file(path).is_ok()
}

pub fn phone_cached(udid: &str) -> Option<PairingFile> {
    load(PHONE_PREFIX, udid)
}

pub fn watch_cached(identifier: &str) -> Option<PairingFile> {
    load(WATCH_PREFIX, identifier)
}

pub fn save_watch(identifier: &str, file: &PairingFile) -> Result<(), String> {
    save(WATCH_PREFIX, identifier, file)
}

pub fn save_phone(udid: &str, file: &PairingFile) -> Result<(), String> {
    save(PHONE_PREFIX, udid, file)
}

/// 拿 iPhone 的配对记录：usbmuxd 里有就用，本机缓存里有就用，
/// 都没有就**主动发一次 Pair** —— 这一步会在 iPhone 上弹「要信任此电脑吗」。
pub async fn phone_pairing<P: IdeviceProvider>(
    provider: &P,
    udid: &str,
    host_name: &str,
) -> Result<PairingFile, PairFail> {
    if let Ok(file) = provider.get_pairing_file().await {
        return Ok(file);
    }

    if let Some(file) = phone_cached(udid) {
        info!("usbmuxd 里没有配对记录，用本机缓存的那一份");
        crate::events::pair("phone", "ok", "", "用的是上次配对留下的记录");
        return Ok(file);
    }

    // 这一刻起是「等人在手机上点信任」，界面要马上把这句话显示出来，
    // 不能等 60 秒后超时了才说。
    info!("iPhone 从未与本机配对，正在请求信任（iPhone 上会弹「要信任此电脑吗」）");
    crate::events::pair(
        "phone",
        "waiting",
        "",
        "已请求 iPhone 信任这台电脑：解锁 iPhone，在「要信任此电脑吗」弹窗里点「信任」，必要时输入锁屏密码",
    );

    let file = match pair_phone(provider, udid, host_name).await {
        Ok(file) => file,
        Err(fail) => {
            crate::events::pair("phone", "failed", fail.code(), &fail.detail());
            return Err(fail);
        }
    };

    match save_phone(udid, &file) {
        Ok(()) => info!("iPhone 配对成功，记录已缓存"),
        Err(e) => warn!("iPhone 配对成功，但记录没存下来：{e}"),
    }
    crate::events::pair("phone", "ok", "", "iPhone 已信任这台电脑");
    Ok(file)
}

/// 直接对 iPhone 发一次 Pair。成功返回新记录（调用方负责落盘）。
async fn pair_phone<P: IdeviceProvider>(
    provider: &P,
    udid: &str,
    host_name: &str,
) -> Result<PairingFile, PairFail> {
    let connection = provider
        .connect(PHONE_LOCKDOWN_PORT)
        .await
        .map_err(|e| PairFail::Other(format!("连不上 iPhone 的 lockdownd：{e}")))?;

    let mut lockdown = LockdownClient::new(connection);

    // host_id 必须是「这台电脑」的稳定身份：同一个 host_id 才会被 iPhone 认成
    // 同一台机器，换一个就等于新电脑，又得点一次信任。
    let host_id = host_id();
    // BUID 只是给配对记录填个字段，设备端不校验（见 idevice 的 pair 注释）。
    let buid = udid.replace('-', "");

    match tokio::time::timeout(
        USER_CONFIRM_TIMEOUT,
        lockdown.pair(host_id, buid, Some(host_name)),
    )
    .await
    {
        Err(_) => Err(PairFail::UserDidNotConfirm),
        Ok(Ok(file)) => Ok(file),
        Ok(Err(e)) => Err(match e {
            IdeviceError::PasswordProtected | IdeviceError::DeviceLocked => PairFail::PhoneLocked,
            IdeviceError::UserDeniedPairing => PairFail::Denied,
            other => PairFail::Other(format!("iPhone 配对失败：{other}")),
        }),
    }
}

/// 本机的主机标识：第一次生成后固定下来，存 `host-id.txt`。
fn host_id() -> String {
    let path = base_dir().join(HOST_ID_FILE);
    if let Ok(text) = std::fs::read_to_string(&path) {
        let trimmed = text.trim().to_string();
        if trimmed.len() == 36 {
            return trimmed;
        }
    }

    let id = new_uuid();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match std::fs::write(&path, &id) {
        Ok(()) => {
            if let Some(dir) = path.parent() {
                crate::cache::restrict_permissions(dir);
            }
            info!("已生成新的主机标识 {id}");
        }
        Err(e) => warn!("主机标识存不下来（{e}），这次先临时用 {id}"),
    }
    id
}

/// 格式化一个 UUIDv4 风格的字符串。不引 uuid 依赖，配对只需要「稳定且够随机」。
fn new_uuid() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut state = nanos ^ ((std::process::id() as u64) << 40) ^ 0x9E37_79B9_7F4A_7C15;

    let mut bytes = [0u8; 16];
    for byte in bytes.iter_mut() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *byte = state as u8;
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant 10xx

    let hex = |slice: &[u8]| {
        slice
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<String>()
    };
    format!(
        "{}-{}-{}-{}-{}",
        hex(&bytes[0..4]),
        hex(&bytes[4..6]),
        hex(&bytes[6..8]),
        hex(&bytes[8..10]),
        hex(&bytes[10..16])
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_has_the_right_shape() {
        let id = new_uuid();
        assert_eq!(id.len(), 36, "{id}");
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.iter().map(|p| p.len()).collect::<Vec<_>>(), vec![8, 4, 4, 4, 12]);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
        assert!(id.starts_with(|c: char| c.is_ascii_hexdigit()));
    }

    #[test]
    fn two_uuids_differ() {
        assert_ne!(new_uuid(), new_uuid());
    }

    #[test]
    fn ids_are_stripped_to_a_safe_charset() {
        // 外部传进来的标识不能把路径跳出缓存目录。
        let path = file_for("watch", "../../etc/passwd");
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(!name.contains(".."), "{name}");
        assert!(!name.contains('/'), "{name}");
        assert!(name.starts_with("watch-"), "{name}");
    }

    #[test]
    fn fail_reasons_are_not_empty() {
        for fail in [
            PairFail::UserDidNotConfirm,
            PairFail::PhoneLocked,
            PairFail::Denied,
            PairFail::Other("x".into()),
        ] {
            assert!(!fail.detail().trim().is_empty());
            assert!(!fail.code().is_empty());
        }
    }
}
