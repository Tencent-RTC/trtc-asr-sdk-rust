# 更新日志

本文件记录 TRTC-ASR Rust SDK 的所有重要变更。

格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循[语义化版本](https://semver.org/lang/zh-CN/)。

## [未发布]

### 变更

- 仓库迁移至 `github.com/Tencent-RTC/trtc-asr-sdk-rust`，功能与 API 无任何变化。
  旧仓库保留 `v1.0.0` 并归档，不再更新。

### 修复

- 修复实时识别 `stop()` 与 reader 轮询对同一 WebSocket mutex 的竞争：
  `stop()` 进入 stopping 后，reader 会在下一次 polling read 返回时让出 mutex，
  等 end 信号写出后再恢复读取。避免 reader 反复抢锁而延迟 end/final，导致
  终端回调丢失并使生命周期测试在 GitHub Ubuntu runner 偶发 exit 101。
  让锁期间若 `stop()` 异常中断，reader 会在兜底超时后自行恢复轮询，不会挂死。
- 并发生命周期测试的等待预算保持放宽（`stop_timeout` 1s→2s、同步等待 10s），
  与上述修复叠加，在 CPU 饥饿环境下留出足够余量。

## [1.0.0] - 2026-09-02

首个正式版本。

### 新增

- Credential 可通过 `set_site` 选择国内站（默认，`asr.cloud-rtc.com`）或国际站（`asr-intl.cloud-rtc.com`），三个识别器共用（须在构造识别器之前设置）
- 实时语音识别（WebSocket），支持流式写入与优雅停止
- 一句话识别（HTTP）
- 录音文件识别（异步 HTTP，CreateRecTask + DescribeTaskStatus）
- 说话人分离：匿名聚类与声纹角色认证两种模式
- VAD 调优、热词、自定义语言模型、脏词/语气词/标点过滤等识别参数
- 所有请求上报 SDK 自身标识（`platform` / `sdk_lang` / `sdk_type` / `version`），
  便于服务端按语言、版本、平台定位客户问题
- MIT LICENSE
- GitHub Actions CI：Linux/macOS 上的 build、test、clippy，外加 `cargo package`
  校验（`Cargo.toml` 排除了 `tests/`，需确认剩余内容仍可打包）
