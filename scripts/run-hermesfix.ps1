<#
.SYNOPSIS
    重新签名 HermesEnabler，让 Watch App 拿到 com.apple.security.application-groups。

.DESCRIPTION
    复用 iLoader 已写入 Windows 凭据管理器的私钥 / 证书 / anisette 状态
    （同一个 keyring service、同一个 machine name），因此不会新建、也不会吊销证书。

    免费 Apple ID 的 Profile 有效期是 7 天。到期后重跑本脚本即可，
    不要回 iLoader 里撤销证书。

.PARAMETER Ipa
    未签名的 HermesEnabler.ipa。默认在仓库根目录或 .\ipa\ 下查找。

.PARAMETER Email
    预先指定 Apple ID，省去交互输入。

.PARAMETER ExpectedKeyHash
    已签过名的 Apple ID 的 SHA-256(email)。提供时会校验"是否复用了同一张证书"；
    不提供则跳过该校验（首次使用属正常）。

.EXAMPLE
    .\run-hermesfix.ps1

.EXAMPLE
    .\run-hermesfix.ps1 -Ipa D:\dl\HermesEnabler.ipa -Email me@example.com

.NOTES
    计算你账号的哈希（想固定下来时用）：
      $s=[Security.Cryptography.SHA256]::Create()
      ($s.ComputeHash([Text.Encoding]::UTF8.GetBytes("me@example.com".ToLower())) |
        %{ $_.ToString('x2') }) -join ''
#>
[CmdletBinding()]
param(
    [string] $Ipa,
    [string] $Email,
    [string] $ExpectedKeyHash
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$exe = @(
    (Join-Path $root 'bin\hermesfix.exe'),
    (Join-Path $root 'hermesfix\target\release\hermesfix.exe')
) | Where-Object { Test-Path $_ } | Select-Object -First 1

if (-not $Ipa) {
    $Ipa = @(
        (Join-Path $root 'HermesEnabler.ipa'),
        (Join-Path $root 'ipa\HermesEnabler.ipa')
    ) | Where-Object { Test-Path $_ } | Select-Object -First 1
}

Write-Host ''
Write-Host '=== hermesfix: 给 HermesEnabler Watch App 补上 App Groups ===' -ForegroundColor Cyan
Write-Host ''

if (-not $exe) { Write-Host '缺少 hermesfix.exe（可自行编译：scripts\build-windows-gnu.sh）' -ForegroundColor Red; exit 1 }
if (-not $Ipa -or -not (Test-Path $Ipa)) { Write-Host '缺少 HermesEnabler.ipa，用 -Ipa 指定路径' -ForegroundColor Red; exit 1 }

Write-Host "exe: $exe"
Write-Host "ipa: $Ipa"
Write-Host ''

# --- 1. Apple ID：与 iLoader 已存的私钥做校验 --------------------------------
if (-not $Email) {
    Write-Host '必须使用在 iLoader 里登录过的同一个 Apple ID。' -ForegroundColor Yellow
    $Email = Read-Host 'Apple ID (邮箱)'
}
$email = $Email.Trim().ToLower()
if ([string]::IsNullOrWhiteSpace($email)) { Write-Host 'email 不能为空' -ForegroundColor Red; exit 1 }

$hex = $null
if ($ExpectedKeyHash) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    $hex = ($sha.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($email)) |
            ForEach-Object { $_.ToString('x2') }) -join ''
    $sha.Dispose()

    if ($hex -eq $ExpectedKeyHash) {
        Write-Host 'OK：命中已存私钥，将复用现有签名证书。' -ForegroundColor Green
    } else {
        Write-Host '警告：这个 Apple ID 与预期的私钥不匹配。' -ForegroundColor Yellow
        Write-Host '      可能会新建一张证书（免费账号上限 3 张）。' -ForegroundColor Yellow
        Write-Host '      不会吊销任何已有证书。' -ForegroundColor Yellow
        if ((Read-Host '仍要继续？(y/N)') -ne 'y') { Write-Host '已取消'; exit 1 }
    }
} else {
    Write-Host '未提供 -ExpectedKeyHash，跳过"复用同一张证书"校验。' -ForegroundColor DarkGray
}

# --- 2. 密码：掩码输入，只通过环境变量传递 -----------------------------------
$sec  = Read-Host 'Apple ID 密码' -AsSecureString
$bstr = [System.Runtime.InteropServices.Marshal]::SecureStringToBSTR($sec)
$pw   = [System.Runtime.InteropServices.Marshal]::PtrToStringBSTR($bstr)
[System.Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr)

$env:HERMES_EMAIL = $email
$env:HERMES_PW    = $pw

# --- 3. 前置条件提醒 --------------------------------------------------------
Write-Host ''
Write-Host '开始前：iPhone 用数据线连电脑、已解锁、已信任本机；手表与 iPhone 已配对。' -ForegroundColor Yellow
Write-Host ''
Write-Host '两步验证：Apple 默认不会发短信！' -ForegroundColor Yellow
Write-Host '  1. 解锁 iPhone / 手表，等「Apple ID 登录」弹窗。' -ForegroundColor Yellow
Write-Host '  2. 点「允许」，6 位验证码随即显示在那台设备上。' -ForegroundColor Yellow
Write-Host '  什么都没收到时，在验证码提示处输入以下指令之一：' -ForegroundColor Yellow
Write-Host '     push      重发推送通知到受信任设备' -ForegroundColor Yellow
Write-Host '     resend    让 Apple 重新签发验证码' -ForegroundColor Yellow
Write-Host '     sms       改用短信接收' -ForegroundColor Yellow
Write-Host '     sms 2     指定某个受信任号码（会列出编号）' -ForegroundColor Yellow
Write-Host '     abort     放弃退出' -ForegroundColor Yellow
Write-Host ''
if ((Read-Host '按回车开始（输入 n 取消）') -eq 'n') { exit 1 }

# --- 4. 执行 ---------------------------------------------------------------
& $exe $Ipa
$code = $LASTEXITCODE

$env:HERMES_PW    = $null
$env:HERMES_EMAIL = $null

Write-Host ''
Write-Host "exit code: $code" -ForegroundColor Cyan
Write-Host "日志: $root\hermesfix.log" -ForegroundColor Cyan
if ($code -ne 0) {
    Write-Host '失败 —— 先看日志最后 30 行：' -ForegroundColor Red
    Write-Host "  Get-Content '$root\hermesfix.log' -Tail 30" -ForegroundColor DarkGray
} else {
    Write-Host '完成 —— 接着跑 verify_appgroup.py 校验权限，然后到手表上实测。' -ForegroundColor Green
}
Write-Host ''
