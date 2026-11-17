//! 极趣墨水屏两位数乘法口算 —— 纯业务闭环（阶段二）。
//!
//! 模块划分对应开发方案 §9：
//! - [`question`]：题目引擎（§9.1）
//! - [`parser`]：中文数字解析（§8.3/§8.4）
//! - [`session`]：会话与状态管理、判题与训练记录（§9.2/§9.5）
//! - [`store`]：SQLite 持久化（§10）
//! - [`display`]：设备显示适配层接口（§9.6，阶段二仅文本渲染）
//!
//! 本 crate 不依赖设备与 ASR：语音答案以「ASR 文本」形式输入。

pub mod app;
pub mod asr;
pub mod display;
pub mod parser;
pub mod question;
pub mod render;
pub mod rng;
pub mod session;
pub mod store;
