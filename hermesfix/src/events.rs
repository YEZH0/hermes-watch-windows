//! 机器可读的阶段与故障事件。
//!
//! 界面不该靠 grep 日志猜进度。日志是给人看的，措辞随时会改，而
//! 「现在卡在哪一步、为什么失败」必须有稳定契约。这里一行一个 JSON：
//!
//! ```text
//! @@HF_EVENT@@ {"v":1,"event":"stage","stage":"watch_connect","state":"start"}
//! @@HF_EVENT@@ {"v":1,"event":"failure","stage":"watch_connect","code":"WATCH_LOCKED",...}
//! ```
//!
//! 前缀足够怪，正常输出撞不上；每行以 `\n` 收尾并立刻 flush，界面按行切分即可。
//! 事件里不放 UDID、不放 Apple ID、不放任何密码——`detail` 是错误链的摘要，
//! 界面显示前还会再过一遍脱敏。

use std::io::Write;

use serde_json::json;

pub const PREFIX: &str = "@@HF_EVENT@@";

/// 一次安装里用户可以观察到的阶段。顺序也是界面进度条的顺序。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Login,
    Sign,
    Iphone,
    WatchConnect,
    WatchInstall,
}

impl Stage {
    pub fn code(self) -> &'static str {
        match self {
            Stage::Login => "login",
            Stage::Sign => "sign",
            Stage::Iphone => "iphone",
            Stage::WatchConnect => "watch_connect",
            Stage::WatchInstall => "watch_install",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Stage::Login => "登录",
            Stage::Sign => "签名",
            Stage::Iphone => "iPhone 安装",
            Stage::WatchConnect => "Watch 连接",
            Stage::WatchInstall => "Watch 安装",
        }
    }

    pub fn is_watch(self) -> bool {
        matches!(self, Stage::WatchConnect | Stage::WatchInstall)
    }
}

fn emit(value: serde_json::Value) {
    println!("{PREFIX} {value}");
    let _ = std::io::stdout().flush();
}

/// 阶段开始 / 结束。
pub fn stage(stage: Stage, state: &str) {
    emit(json!({
        "v": 1,
        "event": "stage",
        "stage": stage.code(),
        "label": stage.label(),
        "state": state,
    }));
}

/// 手表连接探测结果。`code` 为空表示连上了。
pub fn watch_state(code: &str, detail: &str) {
    emit(json!({
        "v": 1,
        "event": "watch",
        "code": code,
        "detail": detail,
    }));
}

/// 结构化的失败。字段是对外契约，改名字要同步改界面。
pub fn failure(f: &crate::diag::Failure) {
    emit(json!({
        "v": 1,
        "event": "failure",
        "stage": f.stage.code(),
        "stage_label": f.stage.label(),
        "code": f.kind.code(),
        "title": f.kind.title(),
        "action": f.kind.action(),
        "recoverable": f.kind.recoverable(),
        "retry": f.kind.retry_scope(),
        "detail": f.detail,
    }));
}

/// 签名产物已经落盘、可以只重试手表阶段。
pub fn resume_available(id: &str, expires_at: Option<i64>) {
    emit(json!({
        "v": 1,
        "event": "resume",
        "available": true,
        "id": id,
        "expires_at": expires_at,
    }));
}

/// 只重试手表时，复用了哪一份签名缓存。
pub fn resume_used(id: &str, age_secs: i64) {
    emit(json!({
        "v": 1,
        "event": "resume",
        "available": true,
        "id": id,
        "age_secs": age_secs,
        "used": true,
    }));
}

/// 全部完成。`resumed` 表示这次只跑了手表阶段。
pub fn done(resumed: bool) {
    emit(json!({
        "v": 1,
        "event": "done",
        "resumed": resumed,
    }));
}
