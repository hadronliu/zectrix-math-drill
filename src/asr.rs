//! ASR worker 客户端：JSON-lines 子进程隔离（背单词应用的成熟模式，见 docs/03 决策3）。
//!
//! - Rust 主程序不接触 MLX/模型依赖；worker 崩溃可重启；
//! - 协议见 `workers/asr_worker.py` 头注释；
//! - 锁步式请求-响应（MVP 足够）：读线程 + 通道 + 超时；
//! - worker 的日志全部走 stderr，stdout 仅承载协议行。

use serde::Deserialize;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::Duration;

#[derive(Debug)]
pub enum AsrError {
    /// worker 未就绪或进程死亡。
    WorkerDown(String),
    /// 响应超时。
    Timeout,
    /// 协议错乱（无法解析/字段缺失）。
    Protocol(String),
    /// worker 返回 ok=false。
    Failed(String),
}

impl std::fmt::Display for AsrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AsrError::WorkerDown(e) => write!(f, "语音识别进程不可用：{e}"),
            AsrError::Timeout => write!(f, "语音识别超时"),
            AsrError::Protocol(e) => write!(f, "语音识别协议错误：{e}"),
            AsrError::Failed(e) => write!(f, "语音识别失败：{e}"),
        }
    }
}

impl std::error::Error for AsrError {}

#[derive(Debug, Deserialize)]
struct WorkerLine {
    #[serde(default)]
    ready: Option<bool>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    ok: Option<bool>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

pub struct AsrClient {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<String>,
    next_id: u64,
    restart_config: (PathBuf, PathBuf, Duration),
    needs_restart: bool,
    /// 最近一次识别耗时（毫秒），不含模型加载。
    pub last_latency_ms: Option<u128>,
}

impl AsrClient {
    /// 启动 worker 并等待 `ready`（首次需加载/下载模型，可能耗时数分钟）。
    pub fn spawn(python: &Path, script: &Path, ready_timeout: Duration) -> Result<Self, AsrError> {
        let mut child = Command::new(python)
            .arg("-u")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| AsrError::WorkerDown(format!("无法启动 {python:?}：{e}")))?;

        let stdout = child.stdout.take().ok_or_else(|| {
            let _ = child.kill();
            AsrError::WorkerDown("stdout 不可用".into())
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            let _ = child.kill();
            AsrError::WorkerDown("stdin 不可用".into())
        })?;

        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let mut client = AsrClient {
            child,
            stdin,
            rx,
            next_id: 0,
            restart_config: (python.into(), script.into(), ready_timeout),
            needs_restart: false,
            last_latency_ms: None,
        };
        let start = std::time::Instant::now();
        loop {
            let line = client
                .recv_line(ready_timeout.saturating_sub(start.elapsed()))
                .inspect_err(|_| {
                    let _ = client.child.kill();
                })?;
            let parsed: WorkerLine = serde_json::from_str(&line)
                .map_err(|e| AsrError::Protocol(format!("响应行解析失败：{e}")))?;
            if parsed.ready == Some(true) {
                return Ok(client);
            }
            // ready 之前的其他行（不应出现）丢弃继续等
        }
    }

    fn recv_line(&mut self, timeout: Duration) -> Result<String, AsrError> {
        self.rx.recv_timeout(timeout).map_err(|e| match e {
            RecvTimeoutError::Timeout => AsrError::Timeout,
            RecvTimeoutError::Disconnected => {
                let status = self.child.try_wait().ok().flatten();
                AsrError::WorkerDown(format!("进程已退出（exit {:?}）", status.and_then(|s| s.code())))
            }
        })
    }

    /// 转写一个 WAV 文件，返回识别文本。
    pub fn transcribe(&mut self, wav: &Path, timeout: Duration) -> Result<String, AsrError> {
        if self.needs_restart {
            let (python, script, timeout) = self.restart_config.clone();
            let _ = self.child.kill();
            let _ = self.child.wait();
            *self = Self::spawn(&python, &script, timeout)?;
        }
        self.needs_restart = true;
        self.last_latency_ms = None;
        self.next_id += 1;
        let id = format!("asr-{}", self.next_id);
        let req = serde_json::json!({ "id": id, "path": wav.to_string_lossy() });
        let start = std::time::Instant::now();
        writeln!(self.stdin, "{req}")
            .map_err(|e| AsrError::WorkerDown(format!("写入失败：{e}")))?;
        self.stdin.flush().ok();

        loop {
            let line = self.recv_line(timeout.saturating_sub(start.elapsed()))?;
            let parsed: WorkerLine = serde_json::from_str(&line)
                .map_err(|e| AsrError::Protocol(format!("响应行解析失败：{e}")))?;
            if parsed.id.as_deref() != Some(id.as_str()) {
                continue; // 串扰响应丢弃（锁步下不应发生）
            }
            self.needs_restart = false;
            self.last_latency_ms = Some(start.elapsed().as_millis());
            if parsed.ok == Some(true) {
                return parsed
                    .text
                    .ok_or_else(|| AsrError::Protocol("响应缺少 text 字段".into()));
            }
            return Err(AsrError::Failed(
                parsed.error.unwrap_or_else(|| "未知错误".into()),
            ));
        }
    }
}

impl Drop for AsrClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 默认 worker 配置：项目旁 venv + workers/asr_worker.py。
#[derive(Debug, Clone)]
pub struct AsrConfig {
    pub python: PathBuf,
    pub script: PathBuf,
    /// 首次 ready 等待（模型加载/下载）。
    pub ready_timeout: Duration,
    /// 单次识别超时。
    pub transcribe_timeout: Duration,
}

impl AsrConfig {
    /// 以 crate 目录为基准推导默认路径。
    pub fn default_relative() -> Self {
        AsrConfig {
            python: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../asr-env/bin/python"),
            script: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("workers/asr_worker.py"),
            ready_timeout: Duration::from_secs(600),
            transcribe_timeout: Duration::from_secs(30),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 不依赖真实模型：用 python3 模拟 worker 验证协议往返。
    #[test]
    fn protocol_roundtrip_with_stub_worker() {
        let dir = std::env::temp_dir().join("math-drill-asr-test");
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("stub_worker.py");
        std::fs::write(
            &script,
            r#"
import sys, json
print(json.dumps({"ready": True}), flush=True)
for line in sys.stdin:
    req = json.loads(line)
    print(json.dumps({"id": req["id"], "ok": True, "text": "三百二十二"}), flush=True)
"#,
        )
        .unwrap();
        let python = if Path::new("/usr/bin/python3").exists() {
            PathBuf::from("/usr/bin/python3")
        } else {
            PathBuf::from("python3")
        };
        let mut client =
            AsrClient::spawn(&python, &script, Duration::from_secs(10)).unwrap();
        let text = client
            .transcribe(Path::new("/tmp/whatever.wav"), Duration::from_secs(5))
            .unwrap();
        assert_eq!(text, "三百二十二");
        assert!(client.last_latency_ms.is_some());
    }

    #[test]
    fn timeout_and_dead_worker_are_reported() {
        let dir = std::env::temp_dir().join("math-drill-asr-test2");
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("sleepy_worker.py");
        std::fs::write(
            &script,
            r#"
import sys, json, time
print(json.dumps({"ready": True}), flush=True)
for line in sys.stdin:
    time.sleep(10)  # 永不响应
"#,
        )
        .unwrap();
        let mut client =
            AsrClient::spawn(Path::new("python3"), &script, Duration::from_secs(10)).unwrap();
        let err = client
            .transcribe(Path::new("/tmp/x.wav"), Duration::from_millis(300))
            .unwrap_err();
        assert!(matches!(err, AsrError::Timeout), "{err:?}");
    }
}
