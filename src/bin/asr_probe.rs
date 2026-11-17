//! ASR 链路探针：WAV → 识别文本 → 中文数字解析 → （可选）判题。
//!
//! 用法:
//!   asr-probe [选项] <wav[=期望整数]> ...
//!
//! 每个参数形如 `路径` 或 `路径=期望值`；给出期望值时输出判题结论。

use math_drill::asr::{AsrClient, AsrConfig};
use math_drill::parser::parse_answer;
use std::path::PathBuf;
use std::process::exit;
use std::time::Duration;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut cfg = AsrConfig::default_relative();
    let mut files: Vec<String> = Vec::new();
    let mut it = args.drain(..);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--python" => {
                cfg.python = PathBuf::from(it.next().expect("--python 需要路径"));
            }
            "--script" => {
                cfg.script = PathBuf::from(it.next().expect("--script 需要路径"));
            }
            "--ready-secs" => {
                let s: u64 = it
                    .next()
                    .expect("--ready-secs 需要数值")
                    .parse()
                    .expect("--ready-secs 需要整数");
                cfg.ready_timeout = Duration::from_secs(s);
            }
            "--help" | "-h" => {
                println!("用法: asr-probe [--python 路径] [--script 路径] [--ready-secs N] <wav[=期望整数]> ...");
                return;
            }
            other => files.push(other.to_string()),
        }
    }

    if files.is_empty() {
        eprintln!("错误：至少提供一个 WAV 文件（可带 =期望值 后缀）。见 --help");
        exit(2);
    }

    println!(
        "[asr-probe] 启动 worker：python={:?} script={:?}（首次需加载模型，请耐心等待）",
        cfg.python, cfg.script
    );
    let mut client = match AsrClient::spawn(&cfg.python, &cfg.script, cfg.ready_timeout) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[asr-probe] worker 启动失败：{e}");
            exit(1);
        }
    };
    println!("[asr-probe] worker 就绪。");

    let mut pass = 0usize;
    let mut total = 0usize;
    for f in &files {
        let (path, expected) = match f.split_once('=') {
            Some((p, e)) => (p.to_string(), e.parse::<i64>().ok()),
            None => (f.clone(), None),
        };
        let path = PathBuf::from(&path);
        if !path.exists() {
            println!("\n[文件] {} —— 不存在，跳过", path.display());
            continue;
        }
        print!("\n[文件] {} … ", path.display());
        match client.transcribe(&path, cfg.transcribe_timeout) {
            Ok(text) => {
                let latency = client.last_latency_ms.unwrap_or(0);
                println!("识别文本：{:?}（{latency} ms）", text);
                let parsed = parse_answer(&text);
                println!("        解析结果：{parsed:?}");
                if let Some(exp) = expected {
                    total += 1;
                    match parsed {
                        math_drill::parser::ParsedAnswer::Number(v) if v == exp => {
                            println!("        ✔ 判题：正确（期望 {exp}）");
                            pass += 1;
                        }
                        other => {
                            println!("        ✘ 判题：与期望 {exp} 不符（{other:?}）");
                        }
                    }
                }
            }
            Err(e) => {
                println!("识别失败：{e}");
            }
        }
    }

    if total > 0 {
        println!("\n=== 汇总：{pass}/{total} 通过 ===");
    }
}
