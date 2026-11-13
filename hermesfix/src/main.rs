//! hermesfix — 重新签名并安装 HermesEnabler，使 Watch App 获得 App Groups 权限。
//!
//! 与 iLoader 的差异只有一处：`vendor/isideload` 中 `sideloader.rs` 原本对
//! watchOS 的 App ID 直接 `continue`，导致手表 App ID 既没有开通 App Groups
//! 特性位（APG3427HIY），也没有被分配 App Group，最终手表端的
//! `com.apple.security.application-groups` 权限缺失、GroupContainers 为空，
//! 手表 App 报 “An App Group ID may be missing”。
//!
//! 本程序复用 iLoader 已经写入 Windows 凭据管理器的签名私钥、证书、
//! anisette 状态与 Remote Pairing 记录（同一 keyring service、同一
//! machine name），因此不会新建或吊销证书。
//!
//! 安装流程被拆成五段，每段都会发一行 `@@HF_EVENT@@` 结构事件（见 `events`）：
//!
//! ```text
//! 登录 → 签名 → iPhone 安装 → Watch 连接 → Watch 安装
//! ```
//!
//! 手表连不上时不再从 Apple ID 密码重来：签名产物已经落在本地缓存里
//! （`cache`），`--resume-watch` 只重跑后两段。

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use idevice::{
    IdeviceService, provider::IdeviceProvider, services::companion_proxy::CompanionProxy,
};
use isideload::{
    anisette::remote_v3::RemoteV3AnisetteProvider,
    auth::apple_account::{AppleAccount, TwoFactorCallbackParams, TwoFactorCallbackResponse},
    dev::{
        developer_session::DeveloperSession, device_type::DeveloperDeviceType, devices::DevicesApi,
        teams::DeveloperTeam,
    },
    sideload::{
        builder::{MaxCertsBehavior, TeamSelection},
        bundle::Bundle,
        install,
        sideloader::Sideloader,
        watch_install,
    },
    util::{device::IdeviceInfo, keyring_storage::KeyringStorage, storage::SideloadingStorage},
};
use rootcause::prelude::*;
use tracing_subscriber::fmt::MakeWriter;

mod cache;
mod devmode;
mod diag;
mod events;
mod inspect;
mod preflight;
mod probe;

use diag::{Failure, Kind};
use events::Stage;

// 与 iLoader 完全一致：复用同一套证书私钥与 anisette provisioning 状态。
const SERVICE: &str = "io.github.andreysemjonov.iloader";
const MACHINE: &str = "iloader";
const ANISETTE: &str = "https://ani.jkcoxson.com";
const DEFAULT_IPA: &str = "HermesEnabler.ipa";
const LOG_FILE: &str = "hermesfix.log";
const ENGINE: &str = concat!("hermesfix/", env!("CARGO_PKG_VERSION"));

/// 临时性连接问题允许在**单次运行内**自动再试一次；用户可处理的错误不自动重试。
const TRANSIENT_RETRIES: usize = 1;
const TRANSIENT_BACKOFF_SECS: u64 = 4;

fn store() -> Box<dyn SideloadingStorage> {
    Box::new(KeyringStorage::new(SERVICE.to_string()))
}

fn prompt(label: &str) -> String {
    print!("{label}");
    let _ = io::stdout().flush();
    let mut s = String::new();
    let _ = io::stdin().read_line(&mut s);
    s.trim().to_string()
}

#[derive(Clone)]
struct Tee(Arc<Mutex<std::fs::File>>);

impl Write for Tee {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Ok(mut file) = self.0.lock() {
            let _ = file.write_all(buf);
        }
        io::stdout().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Ok(mut file) = self.0.lock() {
            let _ = file.flush();
        }
        io::stdout().flush()
    }
}

struct TeeMake(Arc<Mutex<std::fs::File>>);

impl<'a> MakeWriter<'a> for TeeMake {
    type Writer = Tee;

    fn make_writer(&'a self) -> Self::Writer {
        Tee(self.0.clone())
    }
}

fn init_logging() {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(LOG_FILE)
        .ok();
    let Some(file) = file else { return };

    // 不引 tracing-subscriber 的 env-filter：它要额外拉 matchers，构建环境里
    // 拿不到。这里只认 RUST_LOG 的级别单词，够本项目用。
    let level = match std::env::var("RUST_LOG").unwrap_or_default().to_ascii_lowercase() {
        l if l.contains("trace") => tracing::Level::TRACE,
        l if l.contains("debug") => tracing::Level::DEBUG,
        l if l.contains("warn") => tracing::Level::WARN,
        l if l.contains("error") => tracing::Level::ERROR,
        _ => tracing::Level::INFO,
    };

    let file = Arc::new(Mutex::new(file));
    let _ = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_target(false)
        .with_writer(TeeMake(file))
        .try_init();
}

#[tokio::main]
async fn main() {
    init_logging();
    let _ = isideload::init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let first = args.first().map(String::as_str);

    let code = match first {
        Some("--inspect") => inspect::run(
            args.get(1)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_IPA)),
        ),
        Some("--probe-watch") => probe_cmd().await,
        Some("--probe-dev-mode") => dev_mode_cmd(false).await,
        Some("--reveal-dev-mode") => dev_mode_cmd(true).await,
        Some("--resume-watch") | Some("--watch-only") => resume_cmd(args.get(1).cloned()).await,
        Some("--purge-cache") => purge_cmd(),
        Some("--help") | Some("-h") => {
            print_help();
            0
        }
        _ => full_cmd(args.first().map(PathBuf::from)).await,
    };

    std::process::exit(code);
}

fn print_help() {
    println!("hermesfix — 给 HermesEnabler 重新签名并把 Watch App 装进手表\n");
    println!("  hermesfix [ipa]            完整流程：登录 → 签名 → iPhone → 手表");
    println!("  hermesfix --resume-watch [缓存id]");
    println!("                             只重跑手表阶段，复用已签名的产物");
    println!("  hermesfix --probe-watch    只检查手表连通/解锁状态，不装东西");
    println!("  hermesfix --probe-dev-mode 只读检测手表开发者模式，不装东西");
    println!("  hermesfix --reveal-dev-mode 请求手表显示「开发者模式」设置入口（不开启）");
    println!("  hermesfix --inspect [ipa]  只读解析 IPA，不登录不联网");
    println!("  hermesfix --purge-cache    删掉本地签名缓存");
    println!("\n退出码：0 成功 / 2 失败 / 3 手表阶段失败（可只重试手表）");
    println!("        --probe-dev-mode 另有：4 未开启 / 5 无法确认");
}

// ---------------------------------------------------------------- 完整流程

async fn full_cmd(ipa: Option<PathBuf>) -> i32 {
    let ipa = ipa.unwrap_or_else(|| PathBuf::from(DEFAULT_IPA));
    if !ipa.exists() {
        return fail_kind(
            Stage::Sign,
            Kind::SigningFailed,
            format!("IPA not found: {}", ipa.display()),
        );
    }

    let email = std::env::var("HERMES_EMAIL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| prompt("Apple ID (email): "))
        .trim()
        .to_lowercase();
    if email.is_empty() {
        return fail_kind(Stage::Login, Kind::AuthFailed, "没有提供 Apple ID");
    }

    // 密码只从环境变量或交互式输入读取，绝不写入日志。
    let password = match std::env::var("HERMES_PW") {
        Ok(p) if !p.is_empty() => {
            // edition 2024 起 remove_var 是 unsafe：别的线程可能正在读环境变量。
            // 此刻还没开始登录，也没有别的线程需要 HERMES_PW，先抹掉再说。
            unsafe { std::env::remove_var("HERMES_PW") };
            p
        }
        _ => prompt("Apple ID password: "),
    };
    if password.is_empty() {
        return fail_kind(Stage::Login, Kind::AuthFailed, "没有提供密码");
    }

    println!("\n=== hermesfix: re-sign HermesEnabler with Watch App Groups ===");
    println!("IPA     : {}", ipa.display());
    println!("Account : {email}");
    println!("Anisette: {ANISETTE}\n");

    // ---- 1/5 登录
    events::stage(Stage::Login, "start");
    println!("[1/5] Signing in to Apple ID ...");
    let anisette = match RemoteV3AnisetteProvider::default() {
        Ok(p) => p
            .set_serial_number("0".to_string())
            .set_storage(store())
            .set_url(ANISETTE),
        Err(e) => {
            return fail_kind(Stage::Login, Kind::AuthFailed, format!("anisette 初始化失败：{e:?}"));
        }
    };

    let mut account = match AppleAccount::builder(&email)
        .anisette_provider(anisette)
        .login(&password, two_factor_callback())
        .await
    {
        Ok(a) => a,
        Err(e) => return fail(Stage::Login, &e),
    };
    println!("      login ok");

    let dev_session = match DeveloperSession::from_account(&mut account).await {
        Ok(s) => s,
        Err(e) => return fail(Stage::Login, &e),
    };
    println!("      session ok");
    events::stage(Stage::Login, "done");

    // ---- 2/5 签名
    events::stage(Stage::Sign, "start");
    println!("[2/5] Signing the app (watch App Groups) ...");

    let provider = match phone_provider().await {
        Ok(p) => p,
        Err(e) => return fail(Stage::Iphone, &e),
    };

    let mut sideloader = Sideloader::new(
        dev_session,
        email.clone(),
        TeamSelection::First,
        // 只复用既有证书；证书已满时报错而不是吊销任何证书。
        MaxCertsBehavior::Error,
        MACHINE.to_string(),
        store(),
        // 保留签名产物：手表阶段失败时要靠它续装。
        false,
    );

    let team = match sideloader.get_team().await {
        Ok(t) => t,
        Err(e) => return fail(Stage::Sign, &e),
    };

    let (iphone_udid, watch_udids) = match register_devices(&mut sideloader, &provider, &team).await {
        Ok(pair) => pair,
        Err(e) => return fail(Stage::Sign, &e),
    };

    if watch_udids.is_empty() {
        // 没有配对的手表，后面一定装不上；直接给结论，别让用户等十几分钟。
        return fail_kind(
            Stage::WatchConnect,
            Kind::WatchNotPaired,
            "iPhone 上没有配对的手表",
        );
    }

    let (signed_dir, _special) = match sideloader
        .sign_app(
            ipa.clone(),
            Some(team),
            false,
            None::<fn(f32) -> std::future::Ready<()>>,
        )
        .await
    {
        Ok(result) => result,
        Err(e) => return fail(Stage::Sign, &e),
    };
    println!("      App signed: {}", signed_dir.display());
    events::stage(Stage::Sign, "done");

    // 签名产物先自检：profile 与 bundle、与设备对不对得上。
    let findings = preflight::inspect(&signed_dir, &iphone_udid, &watch_udids);
    if findings.fatal.is_empty() {
        let _ = preflight::report(&findings);
    } else {
        let kind = if findings.watch_device_missing {
            Kind::WatchNotRegistered
        } else {
            Kind::SigningFailed
        };
        return fail_kind(Stage::Sign, kind, findings.fatal.join("；"));
    }

    let bundle = match Bundle::new(signed_dir.clone()) {
        Ok(bundle) => bundle,
        Err(e) => return fail(Stage::Sign, &e),
    };
    if bundle.watch_apps().is_empty() {
        return fail_kind(
            Stage::Sign,
            Kind::NoWatchApp,
            format!("{} 里没有 Watch/ 下的手表 App", signed_dir.display()),
        );
    }
    let root_bundle_id = bundle.bundle_identifier().unwrap_or_default().to_string();

    // 落缓存：手表装失败时可以直接从这儿续装。
    let ipa_sha = cache::sha256_file(&ipa).unwrap_or_default();
    let resume_id = match cache::save(
        &signed_dir,
        ENGINE,
        &ipa_sha,
        &iphone_udid,
        &watch_udids,
        &root_bundle_id,
    ) {
        Ok(saved) => {
            let expires = saved.manifest.bundles.iter().filter_map(|b| b.expires_at).min();
            events::resume_available(&saved.id, expires);
            println!("      signed copy cached as {}", saved.id);
            Some(saved)
        }
        Err(e) => {
            // 缓存失败不该挡安装，但要让用户知道「重试只能从头来」。
            tracing::warn!("签名缓存没能保存：{e}");
            println!("      [!] signed copy not cached: {e}");
            None
        }
    };

    // ---- 3/5 iPhone 安装
    events::stage(Stage::Iphone, "start");
    println!("[3/5] Installing to iPhone ...");
    // 进度回调是 `Fn`，不能用可变捕获；用原子量记「上次打到几了」。
    let last_logged = std::sync::atomic::AtomicU64::new(0);
    let install_result = install::install_app(&provider, &signed_dir, |progress| {
        let last = last_logged.load(std::sync::atomic::Ordering::Relaxed);
        if progress >= last + 10 {
            last_logged.store(progress, std::sync::atomic::Ordering::Relaxed);
            tracing::info!("Installing: {progress}%");
        }
    })
    .await;

    if let Err(e) = install_result {
        return fail(Stage::Iphone, &e);
    }
    println!("      iPhone install ok");

    if let Some(saved) = &resume_id
        && let Err(e) = cache::mark_iphone_installed(&saved.dir)
    {
        tracing::warn!("回写缓存标记失败：{e}");
    }
    events::stage(Stage::Iphone, "done");

    // ---- 4/5 + 5/5 手表
    install_watch_phase(&provider, &bundle, false).await
}

/// 手表两段：先确认连得上（并且没有锁屏），再真正安装。
///
/// 之所以先探测：`install_watch_apps` 内部把「连」和「装」揉在一起，
/// 手表锁着时用户要等它自己超时才看到一句含糊的失败。探测走的是同一条
/// lockdown 链路，锁屏会立刻返回 `PasswordProtected`，两秒内就能给出结论。
async fn install_watch_phase(provider: &impl IdeviceProvider, bundle: &Bundle, resumed: bool) -> i32 {
    events::stage(Stage::WatchConnect, "start");
    println!("[4/5] Connecting to the Apple Watch ...");

    if let Err(failure) = ensure_watch_ready(provider).await {
        return report_failure(failure);
    }
    println!("      watch reachable and unlocked");
    events::stage(Stage::WatchConnect, "done");

    events::stage(Stage::WatchInstall, "start");
    println!("[5/5] Installing the Watch app ...");

    let watch_apps = bundle.watch_apps().to_vec();
    let mut attempt = 0;
    loop {
        attempt += 1;
        match watch_install::install_watch_apps(provider, &watch_apps, MACHINE, |progress| {
            tracing::info!("Installing Apple Watch app: {progress}%");
        })
        .await
        {
            Ok(()) => break,
            Err(e) => {
                let failure = diag::classify(&e, Stage::WatchInstall);
                if attempt <= TRANSIENT_RETRIES && is_transient(failure.kind) {
                    tracing::warn!(
                        "手表安装第 {attempt} 次失败（{}），{} 秒后自动再试一次",
                        failure.kind.code(),
                        TRANSIENT_BACKOFF_SECS
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(TRANSIENT_BACKOFF_SECS)).await;
                    continue;
                }
                return report_failure(failure);
            }
        }
    }

    events::stage(Stage::WatchInstall, "done");
    events::done(resumed);

    println!("\n[DONE] HermesEnabler was signed and installed.");
    println!("       Next: open HermesEnabler on the Watch and use Backup MG first.");
    0
}

/// 探测 + 对临时性故障做一次自动重试。锁屏/弹窗这类要用户动手的不重试。
async fn ensure_watch_ready(provider: &impl IdeviceProvider) -> Result<(), Failure> {
    let mut last = None;
    for attempt in 0..=TRANSIENT_RETRIES {
        let state = probe::probe(provider, MACHINE).await;
        events::watch_state(
            state.kind().map(Kind::code).unwrap_or("READY"),
            &state.detail(),
        );

        match state.kind() {
            None => return Ok(()),
            Some(kind) => {
                let failure = Failure::new(Stage::WatchConnect, kind, state.detail());
                let retryable = is_transient(kind) && attempt < TRANSIENT_RETRIES;
                last = Some(failure);
                if !retryable {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        }
    }
    Err(last.unwrap_or_else(|| {
        Failure::new(Stage::WatchConnect, Kind::Unknown, "手表探测没有得到结论")
    }))
}

fn is_transient(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::WatchUnreachable | Kind::WatchTimeout | Kind::WatchInstallBusy
    )
}

// ---------------------------------------------------------------- 断点续装

async fn resume_cmd(id: Option<String>) -> i32 {
    let saved = match cache::load(id.as_deref()) {
        Ok(saved) => saved,
        Err(e) => return fail_kind(Stage::WatchConnect, Kind::ResumeUnavailable, e),
    };

    if !saved.manifest.iphone_installed {
        return fail_kind(
            Stage::Iphone,
            Kind::ResumeUnavailable,
            "上次停在 iPhone 安装之前，没有可以续用的状态",
        );
    }

    println!("\n=== hermesfix: watch-only retry ===");
    println!("cache   : {}（{} 分钟前签的）", saved.id, (cache::now() - saved.manifest.created_at) / 60);
    println!("bundle  : {}", saved.manifest.root_bundle_id);

    let provider = match phone_provider().await {
        Ok(p) => p,
        Err(e) => return fail(Stage::Iphone, &e),
    };

    let info = match IdeviceInfo::from_device(&provider).await {
        Ok(info) => info,
        Err(e) => return fail(Stage::Iphone, &e),
    };
    if !info.udid.eq_ignore_ascii_case(&saved.manifest.iphone_udid) {
        return fail_kind(
            Stage::Iphone,
            Kind::ResumeUnavailable,
            "现在连的 iPhone 不是签名时那台，缓存的描述文件对它无效",
        );
    }

    // 换了手表同样要拒绝：profile 是按签名时那块表申请的。
    match paired_watches(&provider).await {
        Ok(current) => {
            let expected = &saved.manifest.watch_udids;
            if !expected.is_empty() && &current != expected {
                return fail_kind(
                    Stage::WatchConnect,
                    Kind::ResumeUnavailable,
                    format!(
                        "现在配对的手表（{}）不是签名时那台（{}）",
                        current.join(", "),
                        expected.join(", ")
                    ),
                );
            }
        }
        Err(e) => return fail(Stage::WatchConnect, &e),
    }

    let app_dir = saved.dir.join("app");
    let bundle = match Bundle::new(app_dir) {
        Ok(bundle) => bundle,
        Err(e) => return fail(Stage::Sign, &e),
    };
    if bundle.watch_apps().is_empty() {
        return fail_kind(Stage::Sign, Kind::NoWatchApp, "缓存里没有手表 App");
    }

    events::resume_used(&saved.id, cache::now() - saved.manifest.created_at);
    install_watch_phase(&provider, &bundle, true).await
}

// ---------------------------------------------------------------- 探测 / 清理

async fn probe_cmd() -> i32 {
    let provider = match phone_provider().await {
        Ok(p) => p,
        Err(e) => return fail(Stage::Iphone, &e),
    };

    let state = probe::probe(&provider, MACHINE).await;
    let code = state.kind().map(Kind::code).unwrap_or("READY");
    let detail = state.detail();
    events::watch_state(code, &detail);
    println!("watch: {code} — {detail}");

    match state.kind() {
        None => 0,
        Some(kind) => diag::exit_code(kind),
    }
}

/// 只读检测手表的开发者模式；`reveal = true` 时改成「把设置入口显示出来」。
///
/// 这两件事必须分开：显示入口是手表弹一个设置项，开关仍然要人自己拨。
/// 所以这里既不会因为「入口显示成功」就报 enabled，也不会顺手去 enable。
async fn dev_mode_cmd(reveal: bool) -> i32 {
    let provider = match phone_provider().await {
        Ok(p) => p,
        Err(e) => {
            // 没有 iPhone 就没有结论，但界面仍需要一条结构化结果：
            // 「引擎没说话」和「手表没开」在界面上必须区分开。
            let _ = fail(Stage::Iphone, &e);
            events::dev_mode(
                "unknown",
                "没有找到通过 USB 连接的 iPhone，够不到手表",
                "IPHONE_UNREACHABLE",
                "",
            );
            if reveal {
                events::dev_mode_reveal(false, "没有可用的 iPhone 连接");
            }
            return devmode::DevMode::Unknown(String::new()).exit_code();
        }
    };

    let report = devmode::inspect(&provider, MACHINE, reveal).await;

    let watch_code = report.watch.kind().map(Kind::code).unwrap_or("READY");
    let watch_detail = report.watch.detail();
    events::dev_mode(
        report.dev.code(),
        report.dev.reason(),
        watch_code,
        &watch_detail,
    );

    if reveal {
        events::dev_mode_reveal(
            report.revealed,
            if report.revealed { "" } else { report.dev.reason() },
        );
    }

    let detail = match &report.dev {
        devmode::DevMode::Enabled => "已开启".to_string(),
        other => other.reason().to_string(),
    };
    println!(
        "developer mode: {} — {detail}（手表：{watch_code} {watch_detail}）",
        report.dev.code()
    );
    if reveal {
        println!("{}", if report.revealed {
            "已在手表上请求显示「开发者模式」入口：设置 → 隐私与安全性 → 开发者模式。\
             开关、密码确认和重启都要你自己做——这一步成功了也不等于已经开启。"
        } else {
            "没能让手表显示开发者模式入口，原因见上面的 reason。"
        });
    }

    report.dev.exit_code()
}

fn purge_cmd() -> i32 {
    match cache::purge() {
        Ok(bytes) => {
            println!("已删除本地签名缓存（{:.1} MB）", bytes as f64 / 1_048_576.0);
            0
        }
        Err(e) => {
            println!("[FATAL] {e}");
            2
        }
    }
}

// ---------------------------------------------------------------- 失败汇报

fn fail(stage: Stage, report: &Report) -> i32 {
    report_failure(diag::classify(report, stage))
}

fn fail_kind(stage: Stage, kind: Kind, detail: impl Into<String>) -> i32 {
    report_failure(Failure::new(stage, kind, detail))
}

fn report_failure(failure: Failure) -> i32 {
    eprintln!(
        "\n[FATAL] {} — {}（阶段：{}）",
        failure.kind.code(),
        failure.kind.title(),
        failure.stage.label()
    );
    eprintln!("        {}", failure.kind.action());
    eprintln!("        detail: {}", failure.detail);
    events::failure(&failure);
    diag::exit_code(failure.kind)
}

// ---------------------------------------------------------------- 设备

/// 选 USB 连接的 iPhone。可用 HERMES_UDID 指定。
async fn phone_provider() -> Result<idevice::provider::UsbmuxdProvider, Report> {
    use idevice::usbmuxd::{Connection, UsbmuxdAddr, UsbmuxdConnection};

    let requested = std::env::var("HERMES_UDID").ok().filter(|s| !s.is_empty());
    let mut usbmuxd = UsbmuxdConnection::default()
        .await
        .context("could not reach the Apple Mobile Device service (usbmuxd)")?;
    let devices = usbmuxd
        .get_devices()
        .await
        .context("could not list connected Apple devices")?;

    let mut picked = None;
    for device in devices {
        if !matches!(device.connection_type, Connection::Usb) {
            continue;
        }
        if let Some(want) = requested.as_deref()
            && device.udid != want
        {
            continue;
        }
        picked = Some(device);
        break;
    }

    let device = picked.ok_or_else(|| {
        report!("no USB iPhone found — connect the iPhone by cable, unlock it and tap Trust")
    })?;
    println!("      using iPhone {}", device.udid);

    let addr = UsbmuxdAddr::from_env_var().context("invalid usbmuxd address")?;
    Ok(device.to_provider(addr, MACHINE))
}

/// 把 iPhone 与所有配对的手表登记成开发设备，返回它们的 UDID。
///
/// 顺序很重要：profile 里只会有「已经登记过」的设备，所以必须先登记再签名。
async fn register_devices(
    sideloader: &mut Sideloader,
    provider: &impl IdeviceProvider,
    team: &DeveloperTeam,
) -> Result<(String, Vec<String>), Report> {
    let info = IdeviceInfo::from_device(provider)
        .await
        .context("could not read the iPhone identity")?;

    sideloader
        .get_dev_session()
        .ensure_device_registered(team, &info.name, &info.udid, None)
        .await
        .context("failed to register this iPhone as a development device")?;

    let watches = paired_watches(provider).await?;

    let mut companion = CompanionProxy::connect(provider)
        .await
        .context("failed to open the Apple Watch companion proxy")?;

    for watch_udid in &watches {
        let name = companion
            .get_value(watch_udid, "DeviceName")
            .await
            .ok()
            .and_then(|value| value.as_string().map(str::to_string))
            .unwrap_or_else(|| "Apple Watch".to_string());

        sideloader
            .get_dev_session()
            .ensure_device_registered(team, &name, watch_udid, Some(DeveloperDeviceType::Watchos))
            .await
            .context("failed to register the paired Apple Watch as a development device")?;
    }

    Ok((info.udid, watches))
}

async fn paired_watches(provider: &impl IdeviceProvider) -> Result<Vec<String>, Report> {
    let mut companion = CompanionProxy::connect(provider)
        .await
        .context("failed to open the Apple Watch companion proxy")?;
    // rootcause 的 context 在字面量上会得到 Report<&str>，这里要统一的 Report。
    Ok(companion
        .get_device_registry()
        .await
        .context("failed to list the Apple Watches paired with this iPhone")?)
}

// ---------------------------------------------------------------- 2FA

fn two_factor_callback() -> impl Fn(
    TwoFactorCallbackParams,
) -> std::future::Ready<Result<TwoFactorCallbackResponse, Report>> {
    |params: TwoFactorCallbackParams| {
        if let Some(err) = params.last_error.as_deref() {
            eprintln!("[!] Apple reported: {err}");
        }
        // 始终把可选信息打出来：收不到推送时用户需要知道有哪些号码可发短信。
        eprintln!("    sms supported : {}", params.sms);
        if params.numbers.is_empty() {
            eprintln!("    trusted numbers: <none reported>");
        } else {
            for n in &params.numbers {
                eprintln!(
                    "    trusted number: id={} {} (***{}) push={}",
                    n.id, n.number_with_dial_code, n.last_two_digits, n.push_mode
                );
            }
        }
        if let Some(id) = params.selected_number_id {
            eprintln!("    preselected   : id={id}");
        }
        eprintln!("    input: 6-digit code | push | resend | sms [id] | abort");

        let input = prompt("Two-factor code: ");
        let lower = input.trim().to_ascii_lowercase();
        let response = match lower.as_str() {
            // 空输入视为“再推一次”，比静默重发更符合预期。
            "push" | "device" | "devices" => TwoFactorCallbackResponse::SendToDevices,
            "" | "resend" => TwoFactorCallbackResponse::ResendCode,
            "abort" | "q" | "quit" => TwoFactorCallbackResponse::Abort,
            _ => {
                if let Some(rest) = lower.strip_prefix("sms") {
                    // 支持 "sms" 或 "sms 2"（用上面列出的 id 指定号码）。
                    let id = rest
                        .trim()
                        .parse::<u32>()
                        .ok()
                        .or(params.selected_number_id)
                        .or_else(|| params.numbers.first().map(|n| n.id))
                        .unwrap_or(0);
                    TwoFactorCallbackResponse::SendSms(id)
                } else {
                    TwoFactorCallbackResponse::SubmitCode(
                        input.chars().filter(|c| c.is_ascii_digit()).collect(),
                    )
                }
            }
        };
        std::future::ready(Ok(response))
    }
}
