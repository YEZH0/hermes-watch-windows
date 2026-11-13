//! Apple Watch 开发者模式：只读检测，以及「把设置入口显示出来」。
//!
//! 走的是安装时用的同一条链路（USB iPhone → companion proxy → 手表 lockdown），
//! 所以这里的结论和真正安装时看到的一致，不是另开一条道猜出来的。
//!
//! 这一模块**不**开启开发者模式、不重启手表、不改任何系统设置：
//! `action 0` 只是请求手表把「开发者模式」这一项显示在设置里，开关还得人自己拨，
//! 拨完还要在手表上确认并重启。所以「显示入口成功」永远不等于「已经开启」。

use std::time::Duration;

use idevice::{
    IdeviceService,
    pairing_file::PairingFile,
    provider::IdeviceProvider,
    services::{
        amfi::AmfiClient, companion_proxy::CompanionProxy, lockdown::LockdownClient,
    },
};
use tracing::info;

use crate::probe::{self, WatchState};

/// 手表上的 AMFI 服务：`action 0` 显示开发者模式入口，`action 3` 读开关状态。
const AMFI_SERVICE: &str = "com.apple.amfi.lockdown";
const AMFI_TIMEOUT: Duration = Duration::from_secs(12);

/// 开发者模式的三态。`Unknown` 一定要带原因——界面要能说清「为什么不知道」。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DevMode {
    Enabled,
    Disabled,
    Unknown(String),
}

impl DevMode {
    /// 对外契约里的取值，界面按这个分支，改名字要同步改界面。
    pub fn code(&self) -> &'static str {
        match self {
            DevMode::Enabled => "enabled",
            DevMode::Disabled => "disabled",
            DevMode::Unknown(_) => "unknown",
        }
    }

    pub fn reason(&self) -> &str {
        match self {
            DevMode::Enabled => "",
            DevMode::Disabled => "手表明确回复 developer_mode = false",
            DevMode::Unknown(why) => why,
        }
    }

    /// 退出码：0 已开启 / 4 明确未开启 / 5 无法确认。
    pub fn exit_code(&self) -> i32 {
        match self {
            DevMode::Enabled => 0,
            DevMode::Disabled => 4,
            DevMode::Unknown(_) => 5,
        }
    }
}

#[derive(Debug)]
pub struct Report {
    /// 连接层结论：Ready / Locked / Unreachable / PairingPending …
    pub watch: WatchState,
    pub dev: DevMode,
    /// 这次调用有没有真的把「开发者模式」入口显示出来。
    pub revealed: bool,
}

impl Report {
    fn skip(watch: WatchState, why: impl Into<String>) -> Self {
        Self {
            watch,
            dev: DevMode::Unknown(why.into()),
            revealed: false,
        }
    }
}

/// 检测（`reveal = false`）或显示设置入口（`reveal = true`）。
///
/// `reveal` 模式下如果发现已经开启，就不再打扰手表，直接返回已开启。
pub async fn inspect<P: IdeviceProvider>(
    provider: &P,
    udid: &str,
    host_name: &str,
    reveal: bool,
) -> Report {
    // 第一道门：iPhone 得先信任这台电脑。没有记录就现在问，而不是原地报错。
    let phone = match crate::pairing::phone_pairing(provider, udid, host_name).await {
        Ok(file) => file,
        Err(fail) => {
            let state = match fail {
                crate::pairing::PairFail::PhoneLocked => WatchState::Locked,
                _ => WatchState::Unreachable(fail.detail()),
            };
            return Report::skip(
                state,
                format!("iPhone 还没信任这台电脑（{}）", fail.code()),
            );
        }
    };

    let mut companion = match CompanionProxy::connect(provider).await {
        Ok(service) => service,
        Err(e) => {
            return Report::skip(
                WatchState::Unreachable(format!("打不开手表 companion proxy：{e}")),
                "连不上 iPhone 上的 companion proxy 服务",
            );
        }
    };

    let mut ports: Vec<u16> = Vec::new();
    let report = run(provider, &phone, host_name, reveal, &mut companion, &mut ports).await;

    // 转发出来的端口要收回去，别让 iPhone 一直挂着手表的连接。
    for port in ports.iter().rev() {
        if let Err(e) = companion.stop_forwarding_service_port(*port).await {
            info!("停止手表端口 {port} 转发失败：{e}");
        }
    }

    report
}

async fn run<P: IdeviceProvider>(
    provider: &P,
    phone: &PairingFile,
    host_name: &str,
    reveal: bool,
    companion: &mut CompanionProxy,
    ports: &mut Vec<u16>,
) -> Report {
    // 第二道门：手表也要信任这台电脑。缓存命中时手表毫无感知；没有缓存才弹「信任」。
    let mut lockdown = match probe::open_session(provider, companion, phone, host_name, ports).await {
        Ok(lockdown) => lockdown,
        Err(state) => {
            let why = match &state {
                WatchState::Locked => "手表处于锁屏状态，先解锁再检测".to_string(),
                WatchState::PairingPending => "手表上还有没点的「信任」弹窗".to_string(),
                WatchState::Denied => "手表拒绝了这次配对".to_string(),
                // 其余情况把引擎那边的原话带上——「手表没有回应」这种笼统说法
                // 帮不上忙，用户需要知道到底是转发没起来，还是设备没连上。
                other => other.detail(),
            };
            return Report::skip(state, why);
        }
    };

    let watch = WatchState::Ready {
        version: lockdown
            .get_value(Some("ProductVersion"), None)
            .await
            .ok()
            .and_then(|value| value.as_string().map(str::to_string)),
    };

    match open_amfi(provider, companion, &mut lockdown, ports).await {
        Ok(mut amfi) => {
            let current = match ask_status(&mut amfi).await {
                Ok(true) => DevMode::Enabled,
                Ok(false) => DevMode::Disabled,
                Err(why) => DevMode::Unknown(why),
            };

            if !reveal || current == DevMode::Enabled {
                return Report { watch, dev: current, revealed: false };
            }

            match tokio::time::timeout(AMFI_TIMEOUT, amfi.reveal_developer_mode_option_in_ui()).await
            {
                Err(_) => Report {
                    watch,
                    dev: DevMode::Unknown("请求手表显示开发者模式入口超时".into()),
                    revealed: false,
                },
                Ok(Ok(())) => Report { watch, dev: current, revealed: true },
                Ok(Err(e)) => Report {
                    watch,
                    dev: DevMode::Unknown(format!("手表没有接受显示入口的请求：{e}")),
                    revealed: false,
                },
            }
        }
        // 服务打不开不是「未开启」，是「这台手表/这个 watchOS 我们问不出来」。
        Err(why) => Report {
            watch,
            dev: DevMode::Unknown(why),
            revealed: false,
        },
    }
}

/// 打开手表上的 AMFI 服务。它和 lockdown 一样，端口要经过 iPhone 转发一次。
async fn open_amfi<P: IdeviceProvider>(
    provider: &P,
    companion: &mut CompanionProxy,
    lockdown: &mut LockdownClient,
    ports: &mut Vec<u16>,
) -> Result<AmfiClient, String> {
    let (remote_port, _ssl) = lockdown
        .start_service(AMFI_SERVICE)
        .await
        .map_err(|e| format!("手表没有暴露 {AMFI_SERVICE} 服务：{e}"))?;

    let local_port = companion
        .start_forwarding_service_port(remote_port, None, None)
        .await
        .map_err(|e| format!("AMFI 服务端口没有转发起来：{e}"))?;
    ports.push(remote_port);

    let connection = probe::connect_port(provider, local_port)
        .await
        .map_err(|e| format!("连不上转发出来的 AMFI 端口：{e}"))?;

    Ok(AmfiClient::new(connection))
}

async fn ask_status(amfi: &mut AmfiClient) -> Result<bool, String> {
    match tokio::time::timeout(AMFI_TIMEOUT, amfi.get_developer_mode_status()).await {
        Err(_) => Err("等待手表的开发者模式状态超时".into()),
        Ok(Ok(enabled)) => Ok(enabled),
        Ok(Err(e)) => Err(format!("手表没有给出开发者模式状态：{e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable_contract() {
        assert_eq!(DevMode::Enabled.code(), "enabled");
        assert_eq!(DevMode::Disabled.code(), "disabled");
        assert_eq!(DevMode::Unknown("x".into()).code(), "unknown");
    }

    #[test]
    fn exit_codes_never_collide_with_install_results() {
        // 0/2/3 已经被安装流程占用（2 失败、3 手表可仅重试），这里只用 4/5。
        assert_eq!(DevMode::Enabled.exit_code(), 0);
        assert_eq!(DevMode::Disabled.exit_code(), 4);
        assert_eq!(DevMode::Unknown("x".into()).exit_code(), 5);
    }

    #[test]
    fn unknown_always_carries_a_reason() {
        let dev = DevMode::Unknown("amfi 服务打不开".into());
        assert_eq!(dev.reason(), "amfi 服务打不开");
        // 说「不知道」的时候必须能解释为什么，界面上不能只甩一句「无法确认」。
        assert!(!dev.reason().is_empty());
    }

    #[test]
    fn skip_marks_unknown_not_disabled() {
        let report = Report::skip(WatchState::Locked, "手表锁屏");
        // 锁屏不等于「开发者模式没开」——这两件事混一起就会让用户去翻错的地方。
        assert_eq!(report.dev.code(), "unknown");
        assert!(!report.revealed);
    }
}
