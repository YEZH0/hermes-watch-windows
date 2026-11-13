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
    Idevice, IdeviceError, IdeviceService,
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
/// 建立 session 的等待上限。手表锁屏时会立刻回 PasswordProtected；真卡住
/// 通常是 iPhone 到手表这一段的路没了。
const SESSION_TIMEOUT: Duration = Duration::from_secs(30);
/// 手表这条路出问题，八成是同一件事，话就固定一句，别让用户去猜错误码。
const LINK_DOWN_HINT: &str = "iPhone 送不到手表：Apple Watch 放在充电器上（或戴在手上）、屏幕解锁，iPhone 也保持解锁，两台设备蓝牙连着并在同一 Wi-Fi";
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

    let mut ports: Vec<u16> = Vec::new();
    let state = match open_session(provider, &phone, host_name, &mut ports).await {
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
        if let Err(e) = stop_forward(provider, *port).await {
            info!("停止手表端口 {port} 转发失败：{e}");
        }
    }

    state
}

// ------------------------------------------------------------ companion proxy
//
// iPhone 上的 `com.apple.companion_proxy` **一条连接只答一条命令**：回完就地被设备端
// 关掉，在同一条 socket 上发第二条只会写到已经关闭的管道，报出来还是那句含糊的
// `device socket io failed`。所以每个命令都得新开一条连接——不是浪费，是这套服务
// 本身的规矩（iLoader 的 `watch_install` 也是每条命令一条连接）。
//
// 以前这里复用一个 `CompanionProxy` 对象：先查注册表、再清旧转发、再开新转发，
// 于是第二条命令必然失败，真正要用的转发永远起不来——表现就是「明明都连上了，
// 就是到不了手表」。

/// iPhone 上报的已配对手表 identifier 列表。
pub(crate) async fn companion_registry<P: IdeviceProvider>(
    provider: &P,
) -> Result<Vec<String>, IdeviceError> {
    CompanionProxy::connect(provider)
        .await?
        .get_device_registry()
        .await
}

/// 从手表注册表里读一个字段（设备名等）。
pub(crate) async fn companion_value<P: IdeviceProvider>(
    provider: &P,
    watch_udid: &str,
    key: &str,
) -> Result<plist::Value, IdeviceError> {
    CompanionProxy::connect(provider)
        .await?
        .get_value(watch_udid, key)
        .await
}

/// 把手表上的一个端口经 iPhone 转发出来，返回本机可连的端口。
pub(crate) async fn forward_port<P: IdeviceProvider>(
    provider: &P,
    remote_port: u16,
    service: Option<&str>,
) -> Result<u16, IdeviceError> {
    CompanionProxy::connect(provider)
        .await?
        .start_forwarding_service_port(remote_port, service, None)
        .await
}

pub(crate) async fn stop_forward<P: IdeviceProvider>(
    provider: &P,
    port: u16,
) -> Result<(), IdeviceError> {
    CompanionProxy::connect(provider)
        .await?
        .stop_forwarding_service_port(port)
        .await
}

/// 打开一条可用的手表 lockdown 会话（已 `start_session`）。
///
/// 顺序是：确认 iPhone 认得这台手表 → 转发 62078 → 连上 → 用缓存的配对记录建会话。
/// 没有缓存（或缓存被手表否掉）才真正 `pair` 一次，那一刻手表屏幕上会弹「信任」。
pub(crate) async fn open_session<P: IdeviceProvider>(
    provider: &P,
    phone: &PairingFile,
    host_name: &str,
    ports: &mut Vec<u16>,
) -> Result<LockdownClient, WatchState> {
    // iPhone 报上来的已配对手表列表。空的意味着手表根本没和这台手机配对，
    // 后面怎么试都白搭，得先把手机上的 Watch App 弄明白。
    let registry = match companion_registry(provider).await {
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

    // 缓存记录按手表的 identifier 存；拿不到 identifier 时退回用配对里的 HostID，
    // 至少保证同一台电脑不会互相覆盖。
    let watch_key = registry
        .first()
        .cloned()
        .unwrap_or_else(|| phone.host_id.clone());

    // 第一轮：直接用手表自己那份配对记录（正常情况都走这里，手表毫无感知）。
    if let Some(cached) = pairing::watch_cached(&watch_key) {
        match try_session(provider, ports, &cached).await {
            Ok(lockdown) => {
                info!("用缓存的手表配对记录建好了 session");
                return Ok(lockdown);
            }
            Err(SessionFail::NotTrusted) => {
                // 手表重装过、或者用户在手表上「还原」过，旧记录就废了。
                info!("手表不认缓存的那份记录，改为重新配对");
                pairing::forget("watch", &watch_key);
            }
            Err(fail) => return Err(fail.into_state()),
        }
    }

    // 第二轮：手表多半早就信任这台电脑了（iLoader 装过 App 就是这种情况），
    // 那一份身份就是 iPhone 配对记录里的 HostID。直接拿它建 session，
    // 能省掉一次「请去手表上点信任」。成功即存进手表缓存，下次更快。
    match try_session(provider, ports, phone).await {
        Ok(lockdown) => {
            info!("手表接受这台电脑已有的配对身份，直接建好了 session");
            match pairing::save_watch(&watch_key, phone) {
                Ok(()) => info!("已把这份记录存为手表配对缓存"),
                Err(e) => info!("手表配对记录没存下来：{e}"),
            }
            crate::events::pair("watch", "ok", "", "手表已信任这台电脑");
            return Ok(lockdown);
        }
        Err(SessionFail::NotTrusted) => {
            info!("手表不认这台电脑现有的身份，需要一次真正的配对");
        }
        Err(fail) => return Err(fail.into_state()),
    }

    // 第三轮：真配对。手表此刻会弹「信任这台电脑吗」，要人解锁手表去点。
    info!("正在请求手表信任这台电脑（手表屏幕上会弹「信任」）");
    crate::events::pair(
        "watch",
        "waiting",
        "",
        "解锁 Apple Watch，屏幕上会弹出「信任这台电脑」，点「信任」",
    );

    let mut lockdown = connect_watch(provider, ports).await?;

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
                "等了 {} 秒没等到手表确认「信任」 —— {LINK_DOWN_HINT}",
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

    start_session(&mut lockdown, &fresh).await.map_err(|fail| match fail {
        SessionFail::NotTrusted => {
            WatchState::Unknown("手表刚接受配对，转头又不认这份身份".into())
        }
        other => other.into_state(),
    })?;
    Ok(lockdown)
}

/// 转发一次手表 lockdown 端口并连上去。
///
/// **每轮都要重新转发。** 转发出来的那个端口一次只服一条连接：上一条卡住的连接
/// 会把它堵死，第二条照样能握手，但写进去的数据永远没有下文，报出来的还是
/// `device socket io failed`——看起来像「手表连不上」，其实只是端口没换。
async fn connect_watch<P: IdeviceProvider>(
    provider: &P,
    ports: &mut Vec<u16>,
) -> Result<LockdownClient, WatchState> {
    if let Err(e) = stop_forward(provider, WATCH_LOCKDOWN_PORT).await {
        info!("清理旧的 62078 转发（可以忽略）：{e}");
    }

    let port = forward_port(provider, WATCH_LOCKDOWN_PORT, Some(WATCH_LOCKDOWN_SERVICE))
        .await
        .map_err(|e| {
            WatchState::Unreachable(format!("手表 lockdown 端口没有转发起来：{e} —— {LINK_DOWN_HINT}"))
        })?;
    // 每轮转发的是同一个远端端口，收尾只需要关一次。
    if !ports.contains(&WATCH_LOCKDOWN_PORT) {
        ports.push(WATCH_LOCKDOWN_PORT);
    }

    let connection = connect_port(provider, port)
        .await
        .map_err(|e| WatchState::Unreachable(format!("转发端口 {port} 连不上：{e}")))?;
    Ok(LockdownClient::new(connection))
}

/// 一次建会话的失败原因。
///
/// 三种必须分开：`InvalidHostID` 才是「这台电脑在手表那儿没有身份」，值得再真配
/// 一次；一句都没答多半是手表刚醒/链路刚抖，换条转发再问一次还有救；其余是
/// 手表明确答复了（锁屏、弹窗被拒……），重试只会浪费用户的时间。
enum SessionFail {
    NotTrusted,
    Silent(WatchState),
    Fatal(WatchState),
}

impl SessionFail {
    fn into_state(self) -> WatchState {
        match self {
            SessionFail::NotTrusted => {
                WatchState::Unknown("手表不认这台电脑的身份，也不接受新的配对".into())
            }
            SessionFail::Silent(state) | SessionFail::Fatal(state) => state,
        }
    }
}

/// 建会话，一句都没答时换一条转发再问一次。
///
/// 手表侧刚被蓝牙唤醒的第一个请求经常丢，重试一次就能过；不重试的话用户看到
/// 的就是「连不上」，而实际上再问一句就好了。
async fn try_session<P: IdeviceProvider>(
    provider: &P,
    ports: &mut Vec<u16>,
    pairing: &PairingFile,
) -> Result<LockdownClient, SessionFail> {
    match attempt_session(provider, ports, pairing).await {
        Err(SessionFail::Silent(state)) => {
            info!("手表一句都没答（{}），换一条转发再问一次", state.detail());
            attempt_session(provider, ports, pairing).await
        }
        other => other,
    }
}

async fn attempt_session<P: IdeviceProvider>(
    provider: &P,
    ports: &mut Vec<u16>,
    pairing: &PairingFile,
) -> Result<LockdownClient, SessionFail> {
    let mut lockdown = connect_watch(provider, ports)
        .await
        .map_err(SessionFail::Fatal)?;
    start_session(&mut lockdown, pairing).await?;
    Ok(lockdown)
}

async fn start_session(
    lockdown: &mut LockdownClient,
    pairing: &PairingFile,
) -> Result<(), SessionFail> {
    match tokio::time::timeout(SESSION_TIMEOUT, lockdown.start_session(pairing)).await {
        Err(_) => Err(SessionFail::Silent(WatchState::Unreachable(format!(
            "手表 {} 秒没有回话 —— {LINK_DOWN_HINT}",
            SESSION_TIMEOUT.as_secs()
        )))),
        Ok(Ok(_)) => Ok(()),
        Ok(Err(idevice::IdeviceError::InvalidHostID)) => Err(SessionFail::NotTrusted),
        // Socket 在这条路上几乎只有一种含义：写到手表那条管子时对方已经关了。
        Ok(Err(idevice::IdeviceError::Socket(_))) => {
            Err(SessionFail::Silent(WatchState::Unreachable(LINK_DOWN_HINT.into())))
        }
        Ok(Err(e)) => Err(SessionFail::Fatal(classify_idevice_error(&e))),
    }
}

pub(crate) fn classify_idevice_error(err: &idevice::IdeviceError) -> WatchState {
    use idevice::IdeviceError;
    match err {
        IdeviceError::PasswordProtected | IdeviceError::DeviceLocked => WatchState::Locked,
        IdeviceError::PairingDialogResponsePending => WatchState::PairingPending,
        IdeviceError::UserDeniedPairing => WatchState::Denied,
        IdeviceError::Timeout => WatchState::Unreachable(LINK_DOWN_HINT.into()),
        // Socket 在 idevice 里就是「读写这条 socket 出错了」，具体原因被吞掉了。
        // 对手表来说几乎只有一种情况：iPhone 到手表这一段的路没了。
        IdeviceError::Socket(_) => WatchState::Unreachable(LINK_DOWN_HINT.into()),
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
