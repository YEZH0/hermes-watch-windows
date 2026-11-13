//! 把设备与引擎抛出来的错误压成一组稳定、可机读的故障码。
//!
//! 为什么不能只看字符串：`IdeviceError` 已经是类型化的枚举，`PasswordProtected`
//! 和 `DeviceLocked` 是两个不同的变体，靠 `contains("locked")` 分会分错；
//! 而 `rootcause::Report` 会把真正的错误塞在链路深处，最外层只是
//! 「Failed to pair with the Apple Watch through companion proxy」这种话。
//!
//! 所以这里先按**类型**在错误链里 downcast（`SideloadError` / `IdeviceError`），
//! 只有确实只以字符串形式存在的东西（watchOS 的 IXErrorDomain Code=48）才用
//! 一条非常具体的特征串兜底。

use idevice::IdeviceError;
use isideload::SideloadError;
use rootcause::prelude::*;

use crate::events::Stage;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// watchOS 在锁屏时拒绝配对/安装——`PasswordProtected`。
    WatchLocked,
    /// 手表屏幕上还有一个「信任」弹窗没人点。
    WatchPairingPending,
    /// 弹窗里点了「不信任」。
    WatchPairingDenied,
    /// 连不上手表（companion proxy / lockdown 端口没起来）。
    WatchUnreachable,
    /// 手表长时间没有任何回应。
    WatchTimeout,
    /// watchOS 的安装协调器还被上一个进程占用（IXErrorDomain Code=48）。
    WatchInstallBusy,
    /// 这块手表没登记进描述文件。
    WatchNotRegistered,
    /// iPhone 上根本没有配对的手表。
    WatchNotPaired,
    /// IPA 里根本没有手表 App。
    NoWatchApp,
    IphoneLocked,
    IphoneUnreachable,
    /// 设备报「无法验证 App 完整性」。
    AppVerificationFailed,
    AuthFailed,
    CertLimit,
    SigningFailed,
    /// 签名缓存不可用（过期、被改、换设备）。
    ResumeUnavailable,
    Unknown,
}

impl Kind {
    /// 对外契约里的错误码。改名字要同步改界面。
    pub fn code(self) -> &'static str {
        match self {
            Kind::WatchLocked => "WATCH_LOCKED",
            Kind::WatchPairingPending => "WATCH_PAIRING_PENDING",
            Kind::WatchPairingDenied => "WATCH_PAIRING_DENIED",
            Kind::WatchUnreachable => "WATCH_UNREACHABLE",
            Kind::WatchTimeout => "WATCH_TIMEOUT",
            Kind::WatchInstallBusy => "WATCH_INSTALL_BUSY",
            Kind::WatchNotRegistered => "WATCH_NOT_REGISTERED",
            Kind::WatchNotPaired => "WATCH_NOT_PAIRED",
            Kind::NoWatchApp => "WATCH_APP_MISSING",
            Kind::IphoneLocked => "IPHONE_LOCKED",
            Kind::IphoneUnreachable => "IPHONE_UNREACHABLE",
            Kind::AppVerificationFailed => "APP_VERIFICATION_FAILED",
            Kind::AuthFailed => "AUTH_FAILED",
            Kind::CertLimit => "CERT_LIMIT",
            Kind::SigningFailed => "SIGNING_FAILED",
            Kind::ResumeUnavailable => "RESUME_UNAVAILABLE",
            Kind::Unknown => "UNKNOWN",
        }
    }

    /// 用户能不能靠自己的操作解决。false 表示别引导他反复重试。
    pub fn recoverable(self) -> bool {
        !matches!(self, Kind::Unknown)
    }

    /// 修复后能复用签名产物吗：
    /// `watch_only` 只要重跑手表阶段，`full` 得从头签一次，`none` 别重试。
    pub fn retry_scope(self) -> &'static str {
        match self {
            Kind::WatchLocked
            | Kind::WatchPairingPending
            | Kind::WatchNotPaired
            | Kind::WatchUnreachable
            | Kind::WatchTimeout
            | Kind::WatchInstallBusy => "watch_only",
            Kind::Unknown => "none",
            _ => "full",
        }
    }

    /// 一句话说清发生了什么（界面上的标题）。
    pub fn title(self) -> &'static str {
        match self {
            Kind::WatchLocked => "Apple Watch 已锁定，需要先在手表上输入密码",
            Kind::WatchPairingPending => "手表屏幕上还有一个「信任」弹窗没点",
            Kind::WatchPairingDenied => "手表上拒绝了这次配对",
            Kind::WatchUnreachable => "连不上手表",
            Kind::WatchTimeout => "手表一直没有响应",
            Kind::WatchInstallBusy => "手表的安装协调器还被上一次安装占用",
            Kind::WatchNotRegistered => "这块手表没有登记进描述文件",
            Kind::WatchNotPaired => "iPhone 上没有配对的 Apple Watch",
            Kind::NoWatchApp => "这个 IPA 里没有手表 App",
            Kind::IphoneLocked => "iPhone 处于锁定状态",
            Kind::IphoneUnreachable => "没找到通过 USB 连接的 iPhone",
            Kind::AppVerificationFailed => "设备无法验证这个 App 的完整性",
            Kind::AuthFailed => "Apple ID 登录失败",
            Kind::CertLimit => "免费账号的证书数量已满",
            Kind::SigningFailed => "签名阶段失败",
            Kind::ResumeUnavailable => "上次的签名产物已经不能用了",
            Kind::Unknown => "出现了没有识别的错误",
        }
    }

    /// 具体做什么。写清楚「在哪、点什么」，不要写「请检查设备状态」。
    pub fn action(self) -> &'static str {
        match self {
            Kind::WatchLocked => {
                "抬起手表输入解锁密码，保持戴在手上、贴近 iPhone、屏幕别灭。手表解锁后回到这里点「我已处理，重新检查」。"
            }
            Kind::WatchPairingPending => {
                "看手表屏幕，点「信任」。没有弹窗就先把手表解锁，再点「我已处理，重新检查」。"
            }
            Kind::WatchPairingDenied => {
                "手表上点了「不信任」。在 iPhone 的 Watch App 里确认手表仍处于配对状态，然后重新完整安装一次。"
            }
            Kind::WatchUnreachable => {
                "确认手表已解锁、戴在手上、在 iPhone 附近；iPhone 保持解锁并用数据线连着电脑，蓝牙和 Wi-Fi 都打开。"
            }
            Kind::WatchTimeout => {
                "抬腕唤醒手表并解锁，等十秒再重试。手表睡着或走远都会这样。"
            }
            Kind::WatchInstallBusy => {
                "iPhone 刚装完 App，watchOS 还在收尾。等 30 秒再点重试；连续失败就把手表重启一次。"
            }
            Kind::WatchNotRegistered => {
                "签名时没能把这块手表写进描述文件。重新完整安装一次，会为当前配对的手表重新申请 profile。"
            }
            Kind::WatchNotPaired => {
                "先把手表和 iPhone 配对好（iPhone 的 Watch App 里能看到这块表），再点「我已处理，重新检查」。"
            }
            Kind::NoWatchApp => {
                "这个 IPA 不含手表 App，装上去手表上什么都不会出现。换用本仓库自带的 HermesEnabler.ipa。"
            }
            Kind::IphoneLocked => "解锁 iPhone，看到「信任此电脑」就点信任，然后重试。",
            Kind::IphoneUnreachable => {
                "换一根能传数据的线，插紧；解锁 iPhone 并在弹窗里点「信任」，再重试。"
            }
            Kind::AppVerificationFailed => {
                "描述文件与设备对不上，通常是手表或 iPhone 没登记进去。重新完整安装一次。"
            }
            Kind::AuthFailed => {
                "检查邮箱、密码和验证码。连续失败先等十分钟，或换网络再试。"
            }
            Kind::CertLimit => {
                "到 developer.apple.com → Certificates 删掉不用的证书（本工具不会替你吊销），或换一个 Apple ID。"
            }
            Kind::SigningFailed => "导出日志看技术详情；多半是 bundle 结构或描述文件的问题。",
            Kind::ResumeUnavailable => {
                "签名产物过期、被改动，或换了设备。重新完整安装一次即可。"
            }
            Kind::Unknown => "导出脱敏日志后反馈，技术详情里有完整的原始错误链。",
        }
    }
}

pub struct Failure {
    pub stage: Stage,
    pub kind: Kind,
    pub detail: String,
}

impl Failure {
    pub fn new(stage: Stage, kind: Kind, detail: impl Into<String>) -> Self {
        Self {
            stage,
            kind,
            detail: clamp(&detail.into(), 600),
        }
    }
}

/// 进程退出码：0 成功，2 硬失败，3 手表阶段失败但可以只重试手表。
pub fn exit_code(kind: Kind) -> i32 {
    if kind.retry_scope() == "watch_only" {
        3
    } else {
        2
    }
}

/// 分类一个 `rootcause::Report`。
pub fn classify(report: &Report, stage: Stage) -> Failure {
    let detail = format!("{report:?}");

    if let Some(kind) = typed(report, stage) {
        return Failure::new(stage, kind, detail);
    }

    // 只给确实没有类型的那几条留特征串，且必须足够具体。
    if detail.contains("IXErrorDomain Code=48") {
        return Failure::new(stage, Kind::WatchInstallBusy, detail);
    }
    if detail.contains("ApplicationVerificationFailed") {
        return Failure::new(stage, Kind::AppVerificationFailed, detail);
    }

    Failure::new(stage, Kind::Unknown, detail)
}

fn typed(report: &Report, stage: Stage) -> Option<Kind> {
    for node in report.iter_reports() {
        if let Some(err) = node.downcast_current_context::<IdeviceError>()
            && let Some(kind) = from_idevice(err, stage)
        {
            return Some(kind);
        }

        if let Some(err) = node.downcast_current_context::<SideloadError>() {
            match err {
                SideloadError::IdeviceError(inner) => {
                    if let Some(kind) = from_idevice(inner, stage) {
                        return Some(kind);
                    }
                }
                SideloadError::AuthWithMessage(_, _) => return Some(Kind::AuthFailed),
                SideloadError::AnisetteNotProvisioned => return Some(Kind::AuthFailed),
                SideloadError::DeveloperError(_, message) => {
                    return Some(developer_kind(message));
                }
                SideloadError::InvalidBundle(_) => return Some(Kind::SigningFailed),
                SideloadError::PlistParseError(_) => return Some(Kind::SigningFailed),
            }
        }
    }
    None
}

fn from_idevice(err: &IdeviceError, stage: Stage) -> Option<Kind> {
    let watch = stage.is_watch();
    Some(match err {
        // watchOS 的「需要输入密码」就是这一个变体；iPhone 阶段同一个变体是锁屏。
        IdeviceError::PasswordProtected | IdeviceError::DeviceLocked => {
            if watch {
                Kind::WatchLocked
            } else {
                Kind::IphoneLocked
            }
        }
        IdeviceError::PairingDialogResponsePending => Kind::WatchPairingPending,
        IdeviceError::UserDeniedPairing => Kind::WatchPairingDenied,
        IdeviceError::Timeout => {
            if watch {
                Kind::WatchTimeout
            } else {
                Kind::IphoneUnreachable
            }
        }
        IdeviceError::NoEstablishedConnection
        | IdeviceError::DeviceNotFound
        | IdeviceError::NotFound
        | IdeviceError::ServiceNotFound
        | IdeviceError::Socket(_) => {
            if watch {
                Kind::WatchUnreachable
            } else {
                Kind::IphoneUnreachable
            }
        }
        IdeviceError::Usbmuxd(_) => Kind::IphoneUnreachable,
        IdeviceError::ApplicationVerificationFailed(_) => Kind::AppVerificationFailed,
        IdeviceError::UnexpectedResponse(message) => {
            // 少数路径把设备回的原始字符串塞进这里，值本身还是那两个老面孔。
            if message.contains("PasswordProtected") {
                if watch {
                    Kind::WatchLocked
                } else {
                    Kind::IphoneLocked
                }
            } else if message.contains("PairingDialogResponsePending") {
                Kind::WatchPairingPending
            } else if message.contains("UserDeniedPairing") {
                Kind::WatchPairingDenied
            } else {
                return None;
            }
        }
        _ => return None,
    })
}

fn developer_kind(message: &str) -> Kind {
    let lower = message.to_ascii_lowercase();
    if lower.contains("certificate")
        && (lower.contains("maximum") || lower.contains("limit") || lower.contains("exceed"))
    {
        Kind::CertLimit
    } else {
        Kind::SigningFailed
    }
}

fn clamp(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 复刻引擎里真实的包装方式：底层类型化错误 + 一层人话 context。
    fn wrapped(err: IdeviceError) -> Report {
        Report::new(SideloadError::IdeviceError(err))
            .context("Failed to pair with the Apple Watch through companion proxy")
            .into()
    }

    #[test]
    fn password_protected_on_watch_is_locked() {
        let failure = classify(&wrapped(IdeviceError::PasswordProtected), Stage::WatchConnect);
        assert_eq!(failure.kind, Kind::WatchLocked);
        assert_eq!(failure.kind.code(), "WATCH_LOCKED");
        assert_eq!(failure.kind.retry_scope(), "watch_only");
        assert!(failure.detail.contains("PasswordProtected"), "原始错误要留在 detail 里");
    }

    #[test]
    fn password_protected_on_iphone_is_not_a_watch_problem() {
        let failure = classify(&wrapped(IdeviceError::PasswordProtected), Stage::Iphone);
        assert_eq!(failure.kind, Kind::IphoneLocked);
    }

    #[test]
    fn pairing_dialog_and_denial_are_distinct() {
        assert_eq!(
            classify(&wrapped(IdeviceError::PairingDialogResponsePending), Stage::WatchConnect).kind,
            Kind::WatchPairingPending
        );
        assert_eq!(
            classify(&wrapped(IdeviceError::UserDeniedPairing), Stage::WatchConnect).kind,
            Kind::WatchPairingDenied
        );
    }

    #[test]
    fn transport_errors_split_by_stage() {
        assert_eq!(
            classify(&wrapped(IdeviceError::NoEstablishedConnection), Stage::WatchConnect).kind,
            Kind::WatchUnreachable
        );
        assert_eq!(
            classify(&wrapped(IdeviceError::NoEstablishedConnection), Stage::Iphone).kind,
            Kind::IphoneUnreachable
        );
        assert_eq!(
            classify(&wrapped(IdeviceError::Timeout), Stage::WatchInstall).kind,
            Kind::WatchTimeout
        );
    }

    #[test]
    fn bare_idevice_error_without_context_still_classifies() {
        let report: Report = Report::new(IdeviceError::PasswordProtected).into();
        assert_eq!(classify(&report, Stage::WatchInstall).kind, Kind::WatchLocked);
    }

    #[test]
    fn coordinator_conflict_is_recognised_from_the_only_place_it_exists() {
        let report: Report = report!("watchOS refused: Error Domain=IXErrorDomain Code=48 \"busy\"");
        let failure = classify(&report, Stage::WatchInstall);
        assert_eq!(failure.kind, Kind::WatchInstallBusy);
        assert_eq!(failure.kind.retry_scope(), "watch_only");
    }

    #[test]
    fn auth_failures_are_not_reported_as_watch_problems() {
        let report: Report =
            Report::new(SideloadError::AuthWithMessage(-20101, "bad password".into())).into();
        assert_eq!(classify(&report, Stage::Login).kind, Kind::AuthFailed);
    }

    #[test]
    fn cert_limit_is_recognised_from_the_developer_message() {
        let report: Report = Report::new(SideloadError::DeveloperError(
            -1,
            "Your account already has the maximum number of certificates".into(),
        ))
        .into();
        assert_eq!(classify(&report, Stage::Sign).kind, Kind::CertLimit);
    }

    #[test]
    fn unknown_stays_unknown_and_does_not_invite_retry() {
        let report: Report = report!("something nobody has seen before");
        let failure = classify(&report, Stage::WatchInstall);
        assert_eq!(failure.kind, Kind::Unknown);
        assert!(!failure.kind.recoverable());
        assert_eq!(failure.kind.retry_scope(), "none");
        assert_eq!(exit_code(failure.kind), 2);
    }

    #[test]
    fn recoverable_watch_failures_exit_with_three() {
        assert_eq!(exit_code(Kind::WatchLocked), 3);
        assert_eq!(exit_code(Kind::WatchNotRegistered), 2);
    }
}
