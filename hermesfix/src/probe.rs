//! 手表连通性探测：只读，不装东西。
//!
//! 装到手表只有一个入口——companion proxy 转发 lockdown 端口，再用配对记录
//! 跟手表 `start_session`。手表锁屏时设备端会立刻回 `PasswordProtected`，
//! 所以这条路既是安装的必经之路，也是最好的探针：它给出的结论和真正安装时
//! 看到的完全一致，不是猜的。
//!
//! **配对是两件事，别混。** iPhone 要信任这台电脑（USB 上点「信任」），手表
//! 也要信任这台电脑（手表屏幕上点「信任」）。这两份记录互相独立：iPhone 那份
//! 由 usbmuxd 保管，手表那份只有我们自己存。以前每次检测都重新 `pair` 手表，
//! 等于每次都要人去手表上点一遍；现在配对成功就落盘，下次直接建 session。
//!
//! 探测不 uninstall、不碰信任关系、不删配对记录，最坏情况是设备上弹一次
//! 「信任」框——那本身就是需要用户处理的状态生。

use std::time::Duration;

use idevice::{
    Idevice, IdeviceService,
    pairing_file::PairingFile,
    provider::IdeviceProvider,
    services::{companion_proxy::CompanionProxy, lockdown::LockdownClient},
};
use tracing::info;

use crate::diag::Kind;
use crate::pairing;

const WATCH_LOCKDOWN_PORT: u16 = 62078;
const WATCH_LOCKDOWN_SERVICE: &str = "com.apple.mobile.lockdownd";
const CONNECT_ATTEMPTS: usize = 20;
const CONNECT_DELAY: Duration = Duration::from_millis(50);
/// 手表锁屏时会立刻回 PasswordProtected；真卡住通常是手表睡着了。
const PAIR_TIMEOUT: Duration = Duration::from_secs(30);
/// 要人在手表屏幕上点「信任」的那一次，给足时间。
const WATCH_TRUST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub enum WatchState {
    Ready { version: Option<String> },
    Locked,
    PairingPending,
    Denied,
    Unreachable(String),
    Unknown(String),
}

impl WatchState {
    /// 探测结论对应的故障码；连上了就没有故障码。
    pub fn kind(&self) -> Option<Kind> {
        match self {
            WatchState::Ready { .. } => None,
            WatchState::Locked => Some(Kind::WatchLocked),
            WatchState::PairingPending => Some(Kind::WatchPairingPending),
            WatchState::Denied => Some(Kind::WatchPairingDenied),
            WatchState::Unreachable(_) => Some(Kind::WatchUnreachable),
            WatchState::Unknown(_) => Some(Kind::Unknown),
        }
    }

    pub fn detail(&self) -> String {
        match self {
            WatchState::Ready { version } => match version {
                Some(v) => format!("watchOS {v}"),
                None => "已连接".to_string(),
            },
            WatchState::Locked => "watchOS 报告 PasswordProtected（锁屏）".into(),
            WatchState::PairingPending => "手表上还有未处理的配对弹窗".into(),
            WatchState::Denied => "手表拒绝了配对".into(),
            WatchState::Unreachable(why) => why.clone(),
            WatchState::Unknown(why) => why.clone(),
        }
    }
}

/// 走一遍安装用的同一条链路，只到 `start_session` 为止。
pub async fn probe<P: IdeviceProvider>(provider: &P, udid: &str, host_name: &str) -> WatchState {
    // 第一道门：iPhone 得先信任这台电脑。没有记录就现在问，而不是原地报错。
    let phone = match pairing::phone_pairing(provider, udid, host_name).await {
        Ok(file) => file,
        Err(fail) => {
            let state = match fail {
                pairing::PairFail::PhoneLocked => WatchState::Locked,
                _ => WatchState::Unreachable(fail.detail()),
            };
            return state;
        }
    };

    let mut companion = match CompanionProxy::connect(provider).await {
        Ok(service) => service,
        Err(e) => return WatchState::Unreachable(format!("打不开手表 companion proxy：{e}")),
    };

    let mut ports: Vec<u16> = Vec::new();
    let state = match open_session(provider, &mut companion, &phone, host_name, &mut ports).await {
        Ok(mut lockdown) => {
            let version = lockdown
                .get_value(Some("ProductVersion"), None)
                .await
                .ok()
                .and_then(|value| value.as_string().map(str::to_string));
            WatchState::Ready { version }
        }
        Err(state) => state,
    };

    // 关掉转发，别把 iPhone 上的端口一直占着。
    for port in ports.iter().rev() {
        if let Err(e) = companion.stop_forwarding_service_port(*port).await {
            info!("停止手表端口 {port} 转发失败：{e}");
        }
    }

    state
}

/// 打开一条可用的手表 lockdown 会话（已 `start_session`）。
///
/// 顺序是：确认 iPhone 认得这台手表 → 转发 62078 → 连上 → 用缓存的配对记录建会话。
/// 没有缓存（或缓存被手表否掉）才真正 `pair` 一次，那一刻手表屏幕上会弹「信任」。
pub(crate) async fn open_session<P: IdeviceProvider>(
    provider: &P,
    companion: &mut CompanionProxy,
    phone: &PairingFile,
    host_name: &str,
    ports: &mut Vec<u16>,
) -> Result<LockdownClient, WatchState> {
    // iPhone 报上来的已配对手表列表。空的意味着手表根本没和这台手机配对，
    // 后面怎么试都白搭，得先把手机上的 Watch App 弄明白。
    let registry = match companion.get_device_registry().await {
        Ok(list) => list,
        Err(e) => {
            info!("读不到 iPhone 上报的手表列表（{e}），先按单台设备继续");
            Vec::new()
        }
    };
    if registry.is_empty() {
        // 有些 iOS 版本 registry 要等第一次转发后才填充，这里不直接判死。
        info!("iPhone 暂时没有上报已配对手表，继续尝试转发");
    } else {
        info!("iPhone 上报了 {} 台已配对手表", registry.len());
    }

    // 上一次异常退出（杀进程、蓝屏、拔线）可能在 iPhone 上留下没回收的转发，
    // 同一个端口再开一次就会撞车，报的却是含糊的 `device socket io failed`。
    // 先清一遍，清不掉也无所谓，至少不会因为自己之前的残留而失败。
    if let Err(e) = companion.stop_forwarding_service_port(WATCH_LOCKDOWN_PORT).await {
        info!("开转发前清理旧的 62078 转发（可以忽略）：{e}");
    }

    let port = companion
        .start_forwarding_service_port(WATCH_LOCKDOWN_PORT, Some(WATCH_LOCKDOWN_SERVICE), None)
        .await
        .map_err(|e| {
            let text = e.to_string();
            if text.contains("device socket io failed") {
                WatchState::Unreachable(
                    "iPhone 送不到手表：把 Apple Watch 解锁、戴在手上，确认手表和 iPhone 蓝牙连着（手表上能看到绿色手机图标），再试一次".into(),
                )
            } else {
                WatchState::Unreachable(format!("手表 lockdown 端口没有转发起来：{text}"))
            }
        })?;
    ports.push(WATCH_LOCKDOWN_PORT);

    // 缓存记录按手表的 identifier 存；拿不到 identifier 时退回用配对里的 HostID，
    // 至少保证同一台电脑不会互相覆盖。
    let watch_key = registry
        .first()
        .cloned()
        .unwrap_or_else(|| phone.host_id.clone());

    // 第一轮：直接用手表自己那份配对记录（正常情况都走这里，手表毫无感知）。
    if let Some(cached) = pairing::watch_cached(&watch_key) {
        let connection = connect_port(provider, port).await.map_err(WatchState::Unreachable)?;
        let mut lockdown = LockdownClient::new(connection);
        match start_session(&mut lockdown, &cached).await {
            Ok(()) => {
                info!("用缓存的手表配对记录建好了 session");
                return Ok(lockdown);
            }
            Err(state) => {
                // 手表重装过、或者用户在手表上「还原」过，旧记录就废了。
                info!("手表不接受缓存的配对记录（{}），改为重新配对", state.detail());
                pairing::forget("watch", &watch_key);
            }
        }
    }

    // 第二轮：手表多半早就信任这台电脑了（iLoader 装过 App 就是这种情况），
    // 那一份身份就是 iPhone 配对记录里的 HostID。直接拿它建 session，
    // 能省掉一次「请去手表上点信任」。成功即存进手表缓存，下次更快。
    {
        let connection = connect_port(provider, port).await.map_err(WatchState::Unreachable)?;
        let mut lockdown = LockdownClient::new(connection);
        match start_session(&mut lockdown, phone).await {
            Ok(()) => {
                info!("手表接受这台电脑已有的配对身份，直接建好了 session");
                match pairing::save_watch(&watch_key, phone) {
                    Ok(()) => info!("已把这份记录存为手表配对缓存"),
                    Err(e) => info!("手表配对记录没存下来：{e}"),
                }
                crate::events::pair("watch", "ok", "", "手表已信任这台电脑");
                return Ok(lockdown);
            }
            Err(state) => {
                // 锁屏和「等信任」是两回事：锁屏不该继续弹配对，直接如实返回。
                if matches!(state, WatchState::Locked) {
                    return Err(state);
                }
                info!("手表不认这台电脑现有的身份（{}），需要一次真正的配对", state.detail());
            }
        }
    }

    // 第二轮：真配对。手表此刻会弹「信任这台电脑吗」，要人解锁手表去点。
    info!("正在请求手表信任这台电脑（手表屏幕上会弹「信任」）");
    crate::events::pair(
        "watch",
        "waiting",
        "",
        "解锁 Apple Watch，屏幕上会弹出「信任这台电脑」，点「信任」",
    );

    let connection = connect_port(provider, port).await.map_err(WatchState::Unreachable)?;
    let mut lockdown = LockdownClient::new(connection);

    let fresh = match tokio::time::timeout(
        WATCH_TRUST_TIMEOUT,
        lockdown.pair(
            phone.host_id.clone(),
            phone.system_buid.clone(),
            Some(host_name),
        ),
    )
    .await
    {
        Err(_) => {
            let state = WatchState::Unreachable(format!(
                "等了 {} 秒没等到手表确认「信任」 —— 手表要解锁、戴在手上、屏幕亮着",
                WATCH_TRUST_TIMEOUT.as_secs()
            ));
            crate::events::pair(
                "watch",
                "failed",
                "WATCH_TRUST_TIMEOUT",
                &state.detail(),
            );
            return Err(state);
        }
        Ok(Ok(file)) => file,
        Ok(Err(e)) => {
            let state = classify_idevice_error(&e);
            crate::events::pair("watch", "failed", state.kind().map(Kind::code).unwrap_or("WATCH_PAIR_FAILED"), &state.detail());
            return Err(state);
        }
    };

    match pairing::save_watch(&watch_key, &fresh) {
        Ok(()) => info!("手表配对成功，记录已缓存（下次不用再点信任）"),
        Err(e) => info!("手表配对成功，但记录没存下来：{e}"),
    }
    crate::events::pair("watch", "ok", "", "手表已信任这台电脑");

    start_session(&mut lockdown, &fresh).await?;
    Ok(lockdown)
}

async fn start_session(lockdown: &mut LockdownClient, pairing: &PairingFile) -> Result<(), WatchState> {
    match tokio::time::timeout(PAIR_TIMEOUT, lockdown.start_session(pairing)).await {
        Err(_) => Err(WatchState::Unreachable(
            "手表接受了配对，但建立 session 超时".into(),
        )),
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(classify_idevice_error(&e)),
    }
}

pub(crate) fn classify_idevice_error(err: &idevice::IdeviceError) -> WatchState {
    use idevice::IdeviceError;
    match err {
        IdeviceError::PasswordProtected | IdeviceError::DeviceLocked => WatchState::Locked,
        IdeviceError::PairingDialogResponsePending => WatchState::PairingPending,
        IdeviceError::UserDeniedPairing => WatchState::Denied,
        IdeviceError::Timeout => WatchState::Unreachable("手表没有回应".into()),
        other => WatchState::Unknown(other.to_string()),
    }
}

pub(crate) async fn connect_port(provider: &impl IdeviceProvider, port: u16) -> Result<Idevice, String> {
    let mut last = String::new();
    for attempt in 1..=CONNECT_ATTEMPTS {
        match provider.connect(port).await {
            Ok(connection) => {
                if attempt > 1 {
                    info!("手表转发端口 {port} 第 {attempt} 次连上了");
                }
                return Ok(connection);
            }
            Err(e) => {
                last = e.to_string();
                if attempt < CONNECT_ATTEMPTS {
                    tokio::time::sleep(CONNECT_DELAY).await;
                }
            }
        }
    }
    Err(format!(
        "手表 lockdown 端口连不上（试了 {CONNECT_ATTEMPTS} 次）：{last}"
    ))
}
