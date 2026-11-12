# NOTICE

本仓库包含的第三方组件及其许可如下。

## isideload

- 位置：`iloader-main/vendor/isideload/`
- 来源：https://github.com/nab138/isideload
- 许可：MIT
- 版权：© Nicholas Sharp <nab@nabdev.me>
- 本仓库改动：`src/sideload/sideloader.rs` 一处（watchOS App ID 的 App Group 分配），
  见 `patches/0001-isideload-watch-appgroup.patch`

## apple-codesign

- 位置：`apple-codesign/`
- 来源：https://github.com/nab138/apple-crates 的 `apple-codesign-quick/` 子目录
- 上游派生自 https://github.com/indygreg/apple-platform-rs（Gregory Szorc）
- 许可：请遵循上游仓库声明（上游 `apple-platform-rs` 采用 MIT / Apache-2.0 双许可）
- 本仓库改动：仅把 workspace 继承的依赖版本展开为具体版本号，逻辑未改

## iLoader

- 本仓库不包含 iLoader 二进制或源码，仅复用其在本机写入的凭据管理器条目
  （keyring service `io.github.andreysemjonov.iloader`）。
- 来源：https://github.com/AndreySemjonov/iloader

## HermesEnabler

- 本仓库**不包含**、也不再分发 `HermesEnabler.ipa`。
- 请自行从上游发布页获取并核对哈希。

## 本仓库自有部分

- 位置：`hermesfix/`、`scripts/`、`docs/`、`patches/`
- 许可：MIT

```
MIT License

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
