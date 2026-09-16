# 更新日志

本文件记录 TRTC-ASR Rust SDK 的所有重要变更。

格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循[语义化版本](https://semver.org/lang/zh-CN/)。

## [未发布]

## [1.2.5] - 2026-09-16

### 文档

- v3 已全量开放，去掉示例、注释与 changelog 中「需为 SDKAppID 开启灰度开关」
  的使用前提；公开注释与测试只描述客户可见的 `auth` 字段
  （`sdkappid` / `usersig` / 离线 `request_id`）。

## [1.2.4] - 2026-09-15

### 文档

- 中英文 README 与其余五个语言 SDK 对齐：前提条件与凭证获取补上「v3 以
  `SDKAppID` 为唯一客户维度、不再需要腾讯云 `AppID`」；FAQ「错误码怎么看」补上
  v3 数字错误码清单与 SDK 本地 10xx 区间说明；配置项表补三态字段、热词维度、
  词级/字级时间与上下文字段说明，并标注 v3 不再携带 v2 的 `customization_id`
  / `replace_text_id`；示例清单补齐 v2 的三个示例；在线时序图补多句 index
  递增说明。

## [1.2.3] - 2026-09-15

### 修复

- **Linux 上实时识别写音频挂死**：`write()` 会永久阻塞在连接锁上，进程不退出。
  `tungstenite` 的读和发送都需要 `&mut WebSocket`，因此二者共用 `Shared::ws`
  一把锁；读线程虽然有 100ms 的读超时（锁的持有时长是有界的），但释放后会
  立刻重新加锁，留给写方的窗口只有几微秒。`std::sync::Mutex` 在 Linux 是
  futex 实现、允许插队（barging），读线程因此可以持续抢占并饿死写方；macOS
  的 pthread 互斥量会把锁交接给等待者，所以同一份代码在 macOS 上不暴露。
  隔离实验（两线程跑同一份代码）：写方 5 秒内获取锁 macOS 61768 次 /
  Linux 6 次，最长等待 11.5 秒。
  现在写方在加锁前通过 `Shared::writer_ticket` 登记，读线程在每轮读之前
  让出套接字（`yield_to_pending_writers`）。两个方向都有界：写方最多等一次
  在途读加一轮竞争（约 2 × 100ms），读线程最多让路 100ms，因此密集写入也
  不会反过来饿死读线程、让连接无人处理。无写入时只有一次原子读，零开销。
  `stop()` 沿用原有的 end-signal 交接（更强的确定性让路），未改动。
  v2 与 v3 共用同一套 `Shared` 与读循环，一并生效。

## [1.2.2] - 2026-09-14

### 变更

- 示例与 README 的默认引擎由 `16k_zh_en` 改为 `bigmodel`（推荐用法）；
  `engine_model_type` 为必填项，示例不再提供默认值（缺失时打印用法并以
  退出码 2 结束）；`language` 未显式指定时仅 `bigmodel` 默认补 `zh`，
  其他引擎保持服务端自动检测；参数表与引擎列表同步更新。
- 补齐 v3 的 `FileRecognizer::create_task_from_data_with_options` 与
  `SentenceRecognizer::recognize_data_with_options`，与 v2 及其他语言 SDK
  的 `*_with_options` 形态保持一致。

### 修复

- **v3 离线接口（一句话 / 录音文件）TLS 握手失败**：`SentenceRecognizer` /
  `FileRecognizer` 每次请求都新建 `ureq::Agent`，对生产端点握手时报
  `received corrupt message of type InvalidContentType`，导致 v3 离线接口
  在真实网络下完全不可用（v2 复用 agent 所以正常，问题此前被 mock 测试
  掩盖）。现在两个客户端各持有一个复用的 agent（`set_timeout` 重建），
  与 v2 的形态一致。根因机制未完全定位，但 2026-09-14 在生产端点对比验证
  过「复用 agent 成功 / 每请求新建失败」，请勿改回 per-request agent。
- **录音文件识别（v2）词级时间戳恒为 0**：`describe_task_status` 返回的
  `ResultDetail[].Words[]` 里时间偏移字段名是 `StartTime` / `EndTime`，
  SDK 此前按 `OffsetStartMs` / `OffsetEndMs` 解析，取不到值，导致
  `SentenceWords::offset_start_ms` / `offset_end_ms` 全部为 0。现在
  `StartTime` / `EndTime` 为主名，`serde` 的 `alias` 保留
  `OffsetStartMs` / `OffsetEndMs` 作为回退兼容。

## [1.2.1] - 2026-09-10

### 变更

- README 鉴权章节按**在线（流式）/ 离线（HTTP）**拆分，各自给出 `auth` 字段表，
  并补充 UserSig 规则：identifier 绑定（在线 `voice_id` / 离线 `request_id`）、
  自动签名有效期 86400 秒且每条连接 / 每次请求重新生成、调用 `credential.set_user_sig()`
  传入固定签名后不再刷新、签名与站点（`credential.set_site(SITE_INTL)`）绑定。
  中英文 README 同步。

## [1.2.0] - 2026-09-09

### 新增

- 新增 v3 协议客户端，位于独立模块 `trtc_asr_sdk::asr::v3`，与 v2/v1 客户端（`asr` 顶层导出）完全解耦，两者可独立选用、互不影响：
  - `v3::SpeechRecognizer`：WebSocket `/asr/v3`，URL 仅携带 `voice_id`，鉴权与识别参数通过首帧 JSON（`{"type":"start","auth":{...},"params":{...}}`）下发；`start()` 同步等待服务端 ack，鉴权失败（4002）/参数非法（4001）等错误同步返回
  - `v3::SentenceRecognizer`：`POST /v3/transcribe`，body 为 `{auth, params}` 分块，请求/响应均为 snake_case 扁平结构（无 `Response` 外壳）
  - `v3::FileRecognizer`：`POST /v3/create_transcription` + `/v3/describe_transcription`，任务 ID 为 `transcription_id`（与 v1 `RecTaskId` 不通用）
  - `v3::new_credential(sdk_app_id, secret_key)`：v3 不再需要腾讯云 AppID
  - 服务端数字错误码（4xxx/5xxx）直接作为 `AsrError.code` 返回，与 SDK 本地错误码（10xx）区间不冲突；离线错误一律以响应 body 的 `code` 为准（鉴权失败也是 HTTP 200）
  - `needvad`/`convert_num_mode` 显式传 0 会真正下发（v2 query 传参会吞掉 0 值）；说话人分离的 `speaker_roles` 元素序列化为 snake_case（`role_name`/`audio_url`），声纹 ID 列表为 `voiceprint_ids`
  - v3 新增能力：`word_with_space`、`context`（识别上下文 text/terms/general）；录音文件支持 `audio_urls` 分布式录音
  - 下行消息结构与 v2 完全一致，listener trait（`asr::SpeechRecognitionListener`）两版共用
- 新增 `examples/v3_realtime_asr.rs` / `v3_sentence_asr.rs` / `v3_file_asr.rs` 示例
- README 改为只承载 v3 协议文档；v2 / v1 协议与客户端说明移至 `docs/v2_protocol.md`

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
