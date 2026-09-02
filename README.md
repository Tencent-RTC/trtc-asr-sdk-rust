# TRTC-ASR Rust SDK

基于 TRTC 鉴权体系的语音识别（ASR）Rust SDK，与 [Go SDK](../trtc-asr-sdk-go) 行为对齐，支持实时语音识别（WebSocket）、一句话识别（HTTP）和录音文件识别（异步 HTTP）三种模式。

## 前提条件

1. **获取腾讯云 APPID** — 在 [CAM API 密钥管理](https://console.cloud.tencent.com/cam/capi) 页面查看
2. **创建 TRTC 应用** — 在 [实时音视频控制台](https://console.cloud.tencent.com/trtc/app) 创建应用，获取 `SDKAppID`
3. **获取 SDK 密钥** — 在应用概览页点击「SDK密钥」查看，即用于计算 UserSig 的加密密钥

协议细节（WebSocket 参数、响应字段、说话人分离）与 Go SDK 完全一致，参见 [Go SDK README](../trtc-asr-sdk-go/README.md)。

## 安装

```toml
[dependencies]
trtc-asr-sdk = { path = "../trtc-asr-sdk-rust" }
```

要求：Rust 1.75+（2021 edition）。

## 快速开始

### 实时语音识别

```rust
use std::sync::Arc;
use trtc_asr_sdk::asr::{SpeechRecognizer, SpeechRecognitionListener, SpeechRecognitionResponse};
use trtc_asr_sdk::common::{AsrError, Credential};

struct Printer;
impl SpeechRecognitionListener for Printer {
    fn on_sentence_end(&self, resp: &SpeechRecognitionResponse) {
        println!("[end] {}", resp.result.voice_text_str);
        for seg in &resp.result.speaker_segments {
            let name = if seg.speaker_name.is_empty() {
                format!("spk{}", seg.speaker_id)
            } else {
                seg.speaker_name.clone()
            };
            println!("  [{name}] {}", seg.text);
        }
    }
    fn on_fail(&self, _resp: Option<&SpeechRecognitionResponse>, err: &AsrError) {
        eprintln!("[fail] {err}");
    }
}

let mut credential = Credential::new(app_id, sdk_app_id, "your-sdk-secret-key");
// credential.set_site(trtc_asr_sdk::common::SITE_INTL); // 国际站；须在构造识别器之前调用
let mut recognizer = SpeechRecognizer::new(credential, "16k_zh", Arc::new(Printer));

// 可选配置（全部在 start 前调用）：
// recognizer.set_hotword_list("词1|5,词2|11");
// recognizer.set_speaker_diarization(trtc_asr_sdk::asr::SPEAKER_DIARIZATION_CLUSTER);
// recognizer.set_word_info(1);
// recognizer.set_noise_threshold(1.5);   // VAD 噪声微调（0.0-4.0）

recognizer.start()?;                       // 连接 WebSocket
recognizer.write(&pcm_200ms_chunk)?;       // 发送音频（PCM）
recognizer.stop()?;                        // 发送结束信号并等待最终结果
```

### 一句话识别

```rust
let recognizer = trtc_asr_sdk::asr::SentenceRecognizer::new(credential);
let result = recognizer.recognize_data(&pcm_bytes, "pcm", "16k_zh_en")?;
println!("识别结果: {} ({} ms)", result.result, result.audio_duration);
// 或从 URL：recognizer.recognize_url("https://example.com/a.wav", "wav", "16k_zh_en")
```

### 录音文件识别

```rust
let recognizer = trtc_asr_sdk::asr::FileRecognizer::new(credential);
let task_id = recognizer.create_task_from_data(&pcm_bytes, "pcm", "16k_zh_en")?;
let status = recognizer.wait_for_result(&task_id)?;   // 默认 1s 轮询，10min 超时
println!("识别结果: {}", status.result);
// 或从 URL（≤1GB / ≤12h）：recognizer.create_task_from_url("https://example.com/a.wav", "16k_zh_en")
```

## 设计说明

- **错误模型**：所有 API 返回 `Result<T, AsrError>`，`AsrError.code` 与 Go SDK 错误码一致（1001-1010，服务端错误码原样透传）。
- **生命周期**：`SpeechRecognizer` 单例使用——stopped 后不可重启；回调在 SDK 内部 reader 线程上顺序派发；`stop()` 可在回调中安全调用（通过 reader 线程 ID 检测重入，非终态回调里发送 end 后立即返回，看门狗线程兜底超时强关）。
- **三态参数**：`vad_level` / `noise_threshold` / `filter_empty_result` 用 `Option` 区分「显式传 0」与「不配置」。
- **UserSig**：SDK 内置 TLS sig API v2 兼容实现（HMAC-SHA256 + zlib + 腾讯 base64url 变体），无需外部依赖官方库。
- **并发**：内部读写锁分层（连接锁 + 写锁），reader 线程 100ms 轮询读，保证 `stop()` 可在 `write()` 阻塞时及时收拢。

## 测试

```bash
cargo test
```

74 个测试全部在本机回环 mock 服务器上运行（无需真实凭证/网络）：
- `tests/usersig_test.rs` — UserSig 结构/HMAC/zlib/base64url 往返
- `tests/signature_test.rs` — URL query 构建（含说话人分离、VAD 三态、转义、排序）
- `tests/params_test.rs` — 参数校验（声纹 URL、VAD 范围、枚举）
- `tests/sentence_recognizer_test.rs` — mock HTTP：请求头/query/body 断言、错误路径
- `tests/file_recognizer_test.rs` — mock HTTP：任务创建/查询/轮询/超时
- `tests/speech_recognizer_test.rs` — mock WebSocket：握手鉴权参数、ack 帧不误派 sentence begin、服务端错误/final/终态状态机、回调重入 stop、监听器 panic 恢复、并发写+stop 不死锁

## 示例

```bash
export TRTC_ASR_APP_ID=13xxxxxxxx
export TRTC_ASR_SDK_APP_ID=14xxxxxxxx
export TRTC_ASR_SECRET_KEY=your-sdk-secret-key

cargo run --example realtime_asr -- path/to/audio.pcm
cargo run --example sentence_asr -- path/to/audio.pcm pcm 16k_zh_en
cargo run --example file_asr -- path/to/audio.pcm
```

## License

MIT License
