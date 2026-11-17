# 第三方组件说明（THIRD PARTY NOTICES）

本项目程序代码以 MIT 许可发布。以下第三方组件遵循其各自的上游许可：

| 组件 | 用途 | 许可 |
| --- | --- | --- |
| [zectrix-sdk](https://github.com/itopinion/zectrix-local-push-sdk) | NOTE4 墨水屏连接、推图、按键、录音、播放（feature `device`） | MIT；内嵌原生 Opus 遵循其上游许可（见 SDK 仓库 `LICENSES/Opus-COPYING.txt`） |
| rusqlite / libsqlite3-sys | SQLite 持久化（bundled 静态编译 SQLite） | MIT；SQLite 本体属 Public Domain |
| image | PNG 编码 | MIT OR Apache-2.0 |
| fontdue | 字体栅格化 | MIT OR Apache-2.0 OR Zlib |
| serde / serde_json | JSON 序列化 | MIT OR Apache-2.0 |
| tokio | 异步运行时（feature `device`） | MIT |
| uuid | 尝试标识生成 | Apache-2.0 OR MIT |

## 字体

渲染中文需要系统安装含中文字形的 TTF 字体（默认查找 macOS 的
`Arial Unicode.ttf` 等，可用环境变量 `ZECTRIX_FONT` 指定）。
**苹果系统字体受 Apple 许可约束，不随本仓库分发、不可再分发。**
部署在其他平台时，请自行提供一份允许使用的字体（如 OFL 许可的
Noto Sans SC 的 .ttf 单字体文件），并通过 `ZECTRIX_FONT` 指定
（`.ttc` 字体集合暂不支持）。

## 语音模型

本地语音识别使用 [mlx-community/Qwen3-ASR-1.7B-8bit](https://huggingface.co/mlx-community/Qwen3-ASR-1.7B-8bit)
（经 [mlx-audio](https://github.com/ml-explore/mlx-audio) 加载），
模型遵循其上游许可，首次运行时下载到本机 HuggingFace 缓存，
推理全程在本机进行，音频不出局域网。
