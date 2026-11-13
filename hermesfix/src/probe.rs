//! 手表连通性探测：只读，不装东西。
//!
//! 装到手表目前只有一个入口——companion proxy 转发 lockdown 端口，再用
//! iPhone 的配对记录跟手表 `pair`。手表锁屏时 `pair` 会立刻返回
//! `PasswordProtected`，所以这条路既是安装的必经之路，也是最好的探针：
//! 它给出的结论和真正安装时看到的完全一致，不是猜的。
//!
//! 探测不 uninstall、不碰信任关系、不改配对记录，最坏情况是手表上弹一个
//! 「信任」框——那本身就是需要用户处理的状态。

use std::time::Duration;

use idevice::{
    Idevice, IdeviceService,
    provider::IdeviceProvider,
    services::{companion_proxy::CompanionProxy, lockdown::LockdownClient},
};
use tracing::info;

use crate::diag::Kind;

const WATCH_LOCKDOWN_PORT: u16 = 62078;
const WATCH_LOCKDOWN_SERVICE: &str = "com.apple.mobile.lockdownd";
const CONNECT_ATTEMPTS: usize = 20;
const CONNECT_DELAY: Duration = Duration::from_millis(50);
/// 手表锁屏时会立刻回 PasswordProtected；真卡住通常是手表睡着了。
const PAIR_TIMEOUT: Duration = Duration::from_secs(30);

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
pub async fn probe(provider: &impl IdeviceProvider, host_name: &str) -> WatchState {
    let pairing = match provider.get_pairing_file().await {
        Ok(file) => file,
        Err(e) => return WatchState::Unreachable(format!("拿不到 iPhone 的配对记录：{e}")),
    };

    let mut companion = match CompanionProxy::connect(provider).await {
        Ok(service) => service,
        Err(e) => return WatchState::Unreachable(format!("打不开手表 companion proxy：{e}")),
    };

    let port = match companion
        .start_forwarding_service_port(WATCH_LOCKDOWN_PORT, Some(WATCH_LOCKDOWN_SERVICE), None)
        .await
    {
        Ok(port) => port,
        Err(e) => return WatchState::Unreachable(format!("手表 lockdown 端口没有转发起来：{e}")),
    };

    let state = probe_lockdown(
        provider,
        port,
        host_name,
        pairing.host_id.clone(),
        pairing.system_buid.clone(),
    )
    .await;

    // 关掉转发，别把 iPhone 上的端口一直占着。
    if let Ok(mut companion) = CompanionProxy::connect(provider).await
        && let Err(e) = companion
            .stop_forwarding_service_port(WATCH_LOCKDOWN_PORT)
            .await
    {
        info!("停止手表 lockdown 转发失败：{e}");
    }

    state
}

async fn probe_lockdown(
    provider: &impl IdeviceProvider,
    port: u16,
    host_name: &str,
    host_id: String,
    system_buid: String,
) -> WatchState {
    let connection = match connect(provider, port).await {
        Ok(connection) => connection,
        Err(e) => return WatchState::Unreachable(e),
    };

    let mut lockdown = LockdownClient::new(connection);

    let pairing = match tokio::time::timeout(
        PAIR_TIMEOUT,
        lockdown.pair(host_id, system_buid, Some(host_name)),
    )
    .await
    {
        Err(_) => {
            return WatchState::Unreachable(format!(
                "等了 {} 秒没有得到手表的回应（多半是手表锁着或睡着了）",
                PAIR_TIMEOUT.as_secs()
            ));
        }
        Ok(Ok(pairing)) => pairing,
        Ok(Err(e)) => return classify_device_error(&e),
    };

    match tokio::time::timeout(PAIR_TIMEOUT, lockdown.start_session(&pairing)).await {
        Err(_) => WatchState::Unreachable("手表接受了配对，但建立 session 超时".into()),
        Ok(Ok(_)) => {
            let version = lockdown
                .get_value(Some("ProductVersion"), None)
                .await
                .ok()
                .and_then(|value| value.as_string().map(str::to_string));
            WatchState::Ready { version }
        }
        Ok(Err(e)) => classify_device_error(&e),
    }
}

fn classify_device_error(err: &idevice::IdeviceError) -> WatchState {
    use idevice::IdeviceError;
    match err {
        IdeviceError::PasswordProtected | IdeviceError::DeviceLocked => WatchState::Locked,
        IdeviceError::PairingDialogResponsePending => WatchState::PairingPending,
        IdeviceError::UserDeniedPairing => WatchState::Denied,
        IdeviceError::Timeout => WatchState::Unreachable("手表没有回应".into()),
        other => WatchState::Unknown(other.to_string()),
    }
}

async fn connect(provider: &impl IdeviceProvider, port: u16) -> Result<Idevice, String> {
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
