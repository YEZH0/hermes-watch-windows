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

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use isideload::{
    anisette::remote_v3::RemoteV3AnisetteProvider,
    auth::apple_account::{AppleAccount, TwoFactorCallbackParams, TwoFactorCallbackResponse},
    dev::developer_session::DeveloperSession,
    sideload::{
        builder::{MaxCertsBehavior, TeamSelection},
        sideloader::Sideloader,
    },
    util::{keyring_storage::KeyringStorage, storage::SideloadingStorage},
};
use tracing_subscriber::fmt::MakeWriter;

// 与 iLoader 完全一致：复用同一套证书私钥与 anisette provisioning 状态。
const SERVICE: &str = "io.github.andreysemjonov.iloader";
const MACHINE: &str = "iloader";
const ANISETTE: &str = "https://ani.jkcoxson.com";
const DEFAULT_IPA: &str = "HermesEnabler.ipa";
const LOG_FILE: &str = "hermesfix.log";

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

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("\n[FATAL] {msg}");
    std::process::exit(2);
}

#[derive(Clone)]
struct Tee(Arc<Mutex<std::fs::File>>);

impl Write for Tee {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _ = io::stdout().write_all(buf);
        if let Ok(mut f) = self.0.lock() {
            let _ = f.write_all(buf);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        let _ = io::stdout().flush();
        if let Ok(mut f) = self.0.lock() {
            let _ = f.flush();
        }
        Ok(())
    }
}

#[derive(Clone)]
struct TeeMake(Arc<Mutex<std::fs::File>>);

impl<'a> MakeWriter<'a> for TeeMake {
    type Writer = Tee;
    fn make_writer(&'a self) -> Tee {
        Tee(self.0.clone())
    }
}

fn init_logging() {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(LOG_FILE)
        .ok()
        .map(|f| Arc::new(Mutex::new(f)));
    let Some(file) = file else { return };
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .with_target(false)
        .with_writer(TeeMake(file))
        .try_init();
}

#[tokio::main]
async fn main() {
    init_logging();
    let _ = isideload::init();

    let mut args = std::env::args().skip(1);
    let ipa = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_IPA));
    if !ipa.exists() {
        die(format!("IPA not found: {}", ipa.display()));
    }

    let email = std::env::var("HERMES_EMAIL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| prompt("Apple ID (email): "))
        .trim()
        .to_lowercase();
    if email.is_empty() {
        die("Apple ID is required");
    }

    // 密码只从环境变量或交互式输入读取，绝不写入日志。
    let password = match std::env::var("HERMES_PW") {
        Ok(p) if !p.is_empty() => {
            let _ = std::env::remove_var("HERMES_PW");
            p
        }
        _ => prompt("Apple ID password: "),
    };
    if password.is_empty() {
        die("password is required");
    }

    println!("\n=== hermesfix: re-sign HermesEnabler with Watch App Groups ===");
    println!("IPA    : {}", ipa.display());
    println!("Account: {email}");
    println!("Anisette: {ANISETTE}\n");

    println!("[1/4] Signing in to Apple ID ...");
    let anisette = match RemoteV3AnisetteProvider::default() {
        Ok(p) => p
            .set_serial_number("0".to_string())
            .set_storage(store())
            .set_url(ANISETTE),
        Err(e) => die(format!("anisette init failed: {e:?}")),
    };

    let mut account = match AppleAccount::builder(&email)
        .anisette_provider(anisette)
        .login(&password, move |params: TwoFactorCallbackParams| async move {
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
            match lower.as_str() {
                // 空输入视为“再推一次”，比静默重发更符合预期。
                "push" | "device" | "devices" => {
                    return Ok(TwoFactorCallbackResponse::SendToDevices)
                }
                "" | "resend" => return Ok(TwoFactorCallbackResponse::ResendCode),
                "abort" | "q" | "quit" => return Ok(TwoFactorCallbackResponse::Abort),
                _ => {}
            }
            if let Some(rest) = lower.strip_prefix("sms") {
                // 支持 "sms" 或 "sms 2"（用上面列出的 id 指定号码）。
                let id = rest
                    .trim()
                    .parse::<u32>()
                    .ok()
                    .or(params.selected_number_id)
                    .or_else(|| params.numbers.first().map(|n| n.id))
                    .unwrap_or(0);
                return Ok(TwoFactorCallbackResponse::SendSms(id));
            }
            Ok(TwoFactorCallbackResponse::SubmitCode(
                input.chars().filter(|c| c.is_ascii_digit()).collect(),
            ))
        })
        .await
    {
        Ok(a) => a,
        Err(e) => die(format!("login failed: {e:?}")),
    };
    println!("      login ok");

    println!("[2/4] Creating developer session ...");
    let dev_session = match DeveloperSession::from_account(&mut account).await {
        Ok(s) => s,
        Err(e) => die(format!("developer session failed: {e:?}")),
    };
    println!("      session ok");

    println!("[3/4] Locating the USB iPhone ...");
    let provider = match phone_provider().await {
        Ok(p) => p,
        Err(e) => die(format!("no usable iPhone: {e:?}")),
    };

    let mut sideloader = Sideloader::new(
        dev_session,
        email.clone(),
        TeamSelection::First,
        // 只复用既有证书；证书已满时报错而不是吊销任何证书。
        MaxCertsBehavior::Error,
        MACHINE.to_string(),
        store(),
        false, // 保留签名后的产物，便于事后核对权限
    );

    println!("[4/4] Signing and installing to iPhone + Apple Watch ...");
    println!("      (keep the iPhone unlocked and connected until this finishes)");
    let result = sideloader
        .install_app(
            &provider,
            ipa.clone(),
            false,
            None::<fn(f32) -> std::future::Ready<()>>,
        )
        .await;

    match result {
        Ok(special) => {
            println!("\n[DONE] HermesEnabler was signed and installed.");
            if let Some(s) = special {
                println!("       special app: {s:?}");
            }
            println!("       Next: open HermesEnabler on the Watch and use Backup MG first.");
        }
        Err(e) => die(format!("sign/install failed: {e:?}")),
    }
}

/// 选择 USB 连接的 iPhone。可用 HERMES_UDID 指定。
async fn phone_provider() -> Result<idevice::provider::UsbmuxdProvider, rootcause::Report> {
    use idevice::usbmuxd::{Connection, UsbmuxdAddr, UsbmuxdConnection};
    use rootcause::prelude::*;

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
        if let Some(want) = requested.as_deref() {
            if device.udid != want {
                continue;
            }
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
