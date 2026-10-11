//! GAME 人声转 MIDI 的**算法与推理**，这一层不认识 HTTP、不认识文件系统布局。
//!
//! 四个子模块，按「离神经网络多远」排开：
//!
//! | 模块 | 是什么 | 进了发布版吗 |
//! |---|---|---|
//! | `algo` | 宿主侧全部纯算法：切片、D3PM 采样环、边界解码、区间↔时值、重采样 | 是 |
//! | `midi` | 最小 SMF 写出器 + 单音化去重叠 | 是 |
//! | `engine` | 把 ONNX 会话和上面两块接起来，跑完一条音频 | 是 |
//! | `fixture` | 读验证用的 golden 向量（裸 `.bin` / `.npy` / `f32` WAV） | 是 |
//!
//! 算法与上游 PyTorch 参考实现逐位一致（golden 向量 21 项全等；真实人声 41 个
//! 音符逐个相同），**每个函数一个字都不许改** —— 那正是它被验证过的样子。
//!
//! 唯一的例外是「喂给图之前按图的能力切一刀」：`algo::MAX_ENCODER_SAMPLES` /
//! `split_oversized`（导出图的 rope 表是冻结到 5000 帧的初始化张量，比它长的输入
//! 会死在图里的 Mul 节点上）。它加在**上游的静音切分之上**，不改任何一条算法规矩。
//!
//! `fixture` 里那个 `read_wav_mono_f32` 是例外：它不只是给验证用的，
//! 推理路径上「ffmpeg 转出来的 44.1 kHz 单声道 f32 WAV」正好也由它读
//! （见 `midi_transcribe::transcribe_blocking`）。
//!
//! # 为什么这里整体 `allow(dead_code)`
//!
//! `algo` 有几项在本应用里确实没人调 —— 不是遗漏，是那些活已经进了 ONNX 图：
//! `decode_gaussian_blurred_probs` / `find_local_extremum` /
//! `decode_soft_boundaries` 的逻辑正是 `estimator.onnx` 内部在做的事
//! （`estimator.onnx` 直接吐 `presence` 与 `scores`，那是已经解好的半音值）。
//! 留着它们有两个用处：
//!
//!   1. **对得上账**：删掉就再也做不了「同一个输入跑两遍都一样」的逐位回归。
//!   2. **导出换了 opset/图结构时要能兜底**：真需要宿主自己解高斯的时候，
//!      公式在这儿现成摆着，不必回头翻 Python。
//!
//! 各函数头顶都写了它是从上游哪个文件的哪个函数来的，别当垃圾代码删。
//! `fixture` 同理：它整套 golden 向量读取器是回归工具，应用只用到其中一个函数。
#![allow(dead_code)]

pub mod algo;
pub mod engine;
pub mod fixture;
pub mod midi;
