//! 设备端完整语音答题 MVP（阶段三交付，阶段四服务化增强）。
//!
//! 流程：连接 NOTE4 → 推题 → 用户长按录音 → ASR（本地） → 中文数字解析 → 判题
//! → 推结果 + 提示音 → 短按确认下一题 → 总结。
//!
//! 服务模式（阶段四）：连接断开（设备待机/网络抖动/DeviceExit）时**自动重连**，
//! 重连成功后自动恢复当前画面；设备休眠期间持续重试，唤醒即接回。
//!
//! 按键约定（MVP）：上键短按=重显题目；下键短按=中止练习；确认短按=下一题；
//! 确认长按=固件录音手势（不产生 Key 事件）。
//!
//! 用法: drill-device <IP> [选项]
//!   --count N --difficulty entry|basic|advanced --db 路径 --resume 会话ID
//!   --python 路径 --script 路径 --keep-audio（调试：保留录音）

use math_drill::app::DrillApp;
use math_drill::asr::{AsrClient, AsrConfig, AsrError};
use math_drill::question::{Difficulty, QuestionConfig};
use math_drill::render::Renderer;
use math_drill::rng::Rng;
use math_drill::session::SubmitResult;
use math_drill::store::Store;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zectrix_sdk::{AudioClip, Device, Event, ImageFormat, Key, KeyAction, OpenOptions};

/// 常驻连接下推卡：Busy 时退避重试（实测显示完成后 <3s 内 Busy，docs/01 §3.2）。
async fn show_retry(device: &Device, png: &[u8]) -> Result<(), zectrix_sdk::Error> {
    const FORMAT: ImageFormat = ImageFormat::Gray16;
    let mut attempt = 0;
    loop {
        match device.show_with_format(png, FORMAT).await {
            Ok(()) => return Ok(()),
            Err(zectrix_sdk::Error::Busy) if attempt < 3 => {
                attempt += 1;
                eprintln!("[show] 设备 Busy，2.5s 后重试（第 {attempt} 次）");
                tokio::time::sleep(Duration::from_millis(2500)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// 合成提示音：音列 + 10ms 淡入淡出。
fn synth(freqs_ms: &[(f32, usize)]) -> AudioClip {
    const RATE: usize = 16000;
    let fade = RATE / 100; // 10ms
    let mut samples = Vec::new();
    for &(f, dur_ms) in freqs_ms {
        let n = RATE * dur_ms / 1000;
        for i in 0..n {
            let env = (i.min(n - i)).min(fade) as f32 / fade as f32;
            let v = (i as f32 * f * std::f32::consts::TAU / RATE as f32).sin() * 11000.0 * env;
            samples.push(v as i16);
        }
        samples.extend([0i16; RATE / 50]); // 20ms 静音间隔
    }
    AudioClip { samples }
}

fn correct_sound() -> AudioClip {
    synth(&[(880.0, 120), (1174.7, 190)])
}
fn wrong_sound() -> AudioClip {
    synth(&[(233.1, 340)])
}
fn done_sound() -> AudioClip {
    synth(&[(659.3, 130), (880.0, 130), (1174.7, 260)])
}

/// PCM16 写 WAV（与 examples/recorder.rs 相同格式）。
fn wav_bytes(samples: &[i16]) -> Vec<u8> {
    let n = (samples.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + n as usize);
    b.extend(b"RIFF");
    b.extend((36 + n).to_le_bytes());
    b.extend(b"WAVEfmt ");
    b.extend(16u32.to_le_bytes());
    b.extend(1u16.to_le_bytes()); // PCM
    b.extend(1u16.to_le_bytes()); // mono
    b.extend(16000u32.to_le_bytes());
    b.extend(32000u32.to_le_bytes());
    b.extend(2u16.to_le_bytes());
    b.extend(16u16.to_le_bytes());
    b.extend(b"data");
    b.extend(n.to_le_bytes());
    for s in samples {
        b.extend(s.to_le_bytes());
    }
    b
}

#[derive(Debug, Clone, PartialEq)]
enum Ui {
    Question,
    Result,
}

/// 当前应显示的画面（重连后自动恢复用）。
#[derive(Clone)]
enum Screen {
    Question,
    Result { correct: bool, parsed: i64, expected: i64 },
    Retry(String),
    Summary,
    Aborted,
}

struct Args {
    ip: String,
    count: usize,
    difficulty: Difficulty,
    db: PathBuf,
    keep_audio: bool,
    resume: Option<String>,
    cfg: AsrConfig,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mut ip = std::env::var("ZECTRIX_IP").ok();
    let mut count = 10;
    let mut difficulty = Difficulty::Basic;
    let mut db = PathBuf::from("data/drill-device.db");
    let mut keep_audio = false;
    let mut resume = None;
    let mut cfg = AsrConfig::default_relative();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--count" => count = it.next().ok_or("--count 需要数值")?.parse().map_err(|_| "--count 需要整数")?,
            "--difficulty" => {
                let v = it.next().ok_or("--difficulty 需要值")?;
                difficulty = Difficulty::parse(&v).ok_or(format!("未知难度 {v}"))?;
            }
            "--resume" => resume = Some(it.next().ok_or("--resume 需要会话 ID")?),
            "--db" => db = PathBuf::from(it.next().ok_or("--db 需要路径")?),
            "--python" => cfg.python = PathBuf::from(it.next().ok_or("--python 需要路径")?),
            "--script" => cfg.script = PathBuf::from(it.next().ok_or("--script 需要路径")?),
            "--keep-audio" => keep_audio = true,
            other if ip.is_none() && !other.starts_with("--") => ip = Some(other.into()),
            other => return Err(format!("未知参数 {other}")),
        }
    }
    Ok(Args {
        ip: ip.ok_or("用法: drill-device <IP> [选项]（或设置 ZECTRIX_IP）")?,
        count,
        difficulty,
        db,
        keep_audio,
        resume,
        cfg,
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = parse_args().map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    // 1. 先启动 ASR worker（模型加载最慢，期间并行做别的准备）
    eprintln!(
        "[drill] 启动本地语音识别（首次需加载模型）：{:?}",
        args.cfg.script
    );
    let asr = {
        let cfg = args.cfg.clone();
        tokio::task::spawn_blocking(move || AsrClient::spawn(&cfg.python, &cfg.script, cfg.ready_timeout))
            .await
            .map_err(|e| format!("ASR 任务失败：{e}"))?
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?
    };
    let asr = Arc::new(Mutex::new(asr));
    eprintln!("[drill] 语音识别就绪");

    // 2. 渲染器、存储、会话
    let renderer = Arc::new(Renderer::new()?);
    if let Some(parent) = args.db.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    let store = Store::open(&args.db)?;
    let mut app = match &args.resume {
        Some(sid) => {
            let a = DrillApp::resume(&store, sid)?;
            println!("[drill] 已恢复会话 {sid}");
            a
        }
        None => {
            let qcfg = QuestionConfig {
                count: args.count,
                difficulty: args.difficulty,
                swap_dedup: true,
            };
            let a = DrillApp::new(qcfg, &mut Rng::from_time(), &store)?;
            println!("[drill] 新会话 {}", a.session_id());
            a
        }
    };
    let session_id = app.session_id().to_string();
    let total = app.session.total_questions();

    // 渲染当前画面的 PNG（供推送与重连恢复共用）。
    let render_screen = |renderer: &Renderer, app: &DrillApp, screen: &Screen| -> Result<Vec<u8>, String> {
        match screen {
            Screen::Question => {
                let q = app.session.current_question().ok_or("无当前题")?;
                renderer.question_card(q.seq, total, q.a, q.b).encode_png()
            }
            Screen::Result { correct, parsed, expected } => {
                renderer.result_card(*correct, *parsed, *expected).encode_png()
            }
            Screen::Retry(m) => renderer.retry_card(m).encode_png(),
            Screen::Summary => {
                let s = app.session.summary();
                renderer
                    .summary_card(
                        s.first_attempt_correct,
                        s.questions_with_first_answer,
                        s.corrections,
                        s.unparseable_attempts,
                    )
                    .encode_png()
            }
            Screen::Aborted => renderer.aborted_card().encode_png(),
        }
    };

    // 3. 服务主循环：断线自动重连，画面自动恢复
    let mut ui = Ui::Question;
    let mut screen = Screen::Question;
    let mut done = false;

    'service: loop {
        // —— 连接（首次 3×5s；重连无限次 × 10s，设备唤醒即接回）——
        let device = Arc::new(loop {
            match Device::open(OpenOptions {
                target: Some(args.ip.clone()),
                client_name: Some("乘法口算".into()),
            })
            .await
            {
                Ok(d) => break d,
                Err(e) => {
                    eprintln!("[drill] 连接失败：{e}；10s 后重试（唤醒设备即可自动接回）");
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            }
        });
        eprintln!("[drill] 已连接设备 {:?}", device.summary);

        // 恢复/首推当前画面
        match show_retry(&device, &render_screen(&renderer, &app, &screen)?).await {
            Ok(()) => {}
            // 连接在推卡时又断开：回到外层重连
            Err(zectrix_sdk::Error::Disconnected) | Err(zectrix_sdk::Error::ConsumerTooSlow) => {
                continue 'service;
            }
            Err(e) => return Err(e.into()),
        }
        if matches!(screen, Screen::Question) {
            app.session.mark_presented();
        }

        // —— 事件循环 ——
        loop {
            let event = match device.next_event().await {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("[drill] 事件流错误：{e}；转入重连");
                    break;
                }
            };
            let Some(event) = event else {
                eprintln!("[drill] 事件流结束；转入重连");
                break;
            };
            match event {
                Event::Disconnected { reason } => {
                    // 设备待机/网络抖动/DeviceExit：保留会话，重连接管（§11.1）
                    eprintln!("[drill] 连接断开（{reason:?}）；10s 后自动重连，画面将自动恢复");
                    break;
                }
                Event::Key { key, action } => match (key, action) {
                    (Key::Up, KeyAction::Short) if ui == Ui::Question => {
                        screen = Screen::Question;
                        if let Err(e) = show_retry(&device, &render_screen(&renderer, &app, &screen)?).await {
                            if matches!(e, zectrix_sdk::Error::Disconnected) {
                                break;
                            }
                            return Err(e.into());
                        }
                        app.session.mark_presented();
                    }
                    (Key::Down, KeyAction::Short) => {
                        app.abort()?;
                        screen = Screen::Aborted;
                        if let Err(e) = show_retry(&device, &render_screen(&renderer, &app, &screen)?).await {
                            if !matches!(e, zectrix_sdk::Error::Disconnected) {
                                return Err(e.into());
                            }
                        }
                        println!("[drill] 已中止。会话 {session_id} 已保存，可 --resume {session_id} 继续。");
                        done = true;
                        break;
                    }
                    (Key::Confirm, KeyAction::Short) if ui == Ui::Result => {
                        match app.advance()? {
                            true => {
                                screen = Screen::Question;
                                if let Err(e) =
                                    show_retry(&device, &render_screen(&renderer, &app, &screen)?).await
                                {
                                    if matches!(e, zectrix_sdk::Error::Disconnected) {
                                        break;
                                    }
                                    return Err(e.into());
                                }
                                app.session.mark_presented();
                                ui = Ui::Question;
                            }
                            false => {
                                screen = Screen::Summary;
                                if let Err(e) =
                                    show_retry(&device, &render_screen(&renderer, &app, &screen)?).await
                                {
                                    if matches!(e, zectrix_sdk::Error::Disconnected) {
                                        break;
                                    }
                                    return Err(e.into());
                                }
                                device.play(&done_sound(), 70).await.ok();
                                app.finish()?;
                                let s = app.session.summary();
                                println!("[drill] 练习完成！会话 {session_id}");
                                println!(
                                    "[drill] 首答正确 {}/{}，订正 {} 次，未识别 {} 次",
                                    s.first_attempt_correct,
                                    s.questions_with_first_answer,
                                    s.corrections,
                                    s.unparseable_attempts
                                );
                                done = true;
                                break;
                            }
                        }
                    }
                    _ => {}
                },
                Event::RecordingStarted => { /* 固件负责「正在聆听」UI */ }
                Event::RecordingFinished { audio, reason } => {
                    if ui != Ui::Question {
                        eprintln!("[drill] 丢弃非题目状态的录音（{reason:?}），不跨题提交");
                        continue;
                    }
                    let Some(clip) = audio else {
                        // 零帧：没有录到声音，不判错（§11.1）
                        screen = Screen::Retry("没有听到声音，请重说".into());
                        if let Err(e) =
                            show_retry(&device, &render_screen(&renderer, &app, &screen)?).await
                        {
                            if matches!(e, zectrix_sdk::Error::Disconnected) {
                                break;
                            }
                            return Err(e.into());
                        }
                        continue;
                    };
                    let seq = app.session.current_seq().expect("Question 态必有当前题");
                    let tmp = std::env::temp_dir().join(format!(
                        "drill-{}-{}.wav",
                        session_id,
                        uuid::Uuid::new_v4()
                    ));
                    std::fs::write(&tmp, wav_bytes(&clip.samples))?;
                    eprintln!(
                        "[drill] 收到录音 {:.2}s，识别中…",
                        clip.samples.len() as f32 / 16000.0
                    );

                    let asr = asr.clone();
                    let path = tmp.clone();
                    let transcribed = tokio::task::spawn_blocking(move || {
                        asr.lock()
                            .expect("ASR 锁")
                            .transcribe(&path, Duration::from_secs(30))
                    })
                    .await;
                    if !args.keep_audio {
                        let _ = std::fs::remove_file(&tmp); // §11.2 默认不留原始音频
                    }
                    let text = match transcribed {
                        Ok(Ok(t)) => t,
                        other => {
                            let msg = match &other {
                                Ok(Ok(_)) => unreachable!("成功路径已在上分支处理"),
                                Ok(Err(AsrError::Timeout)) => {
                                    screen = Screen::Retry("识别超时，请重说".into());
                                    if let Err(e) = show_retry(
                                        &device,
                                        &render_screen(&renderer, &app, &screen)?,
                                    )
                                    .await
                                    {
                                        if matches!(e, zectrix_sdk::Error::Disconnected) {
                                            break;
                                        }
                                        return Err(e.into());
                                    }
                                    continue;
                                }
                                Ok(Err(e)) => e.to_string(),
                                Err(join) => format!("识别任务异常：{join}"),
                            };
                            eprintln!("[drill] ASR 失败：{msg}");
                            screen = Screen::Retry("识别失败，请重说".into());
                            if let Err(e) =
                                show_retry(&device, &render_screen(&renderer, &app, &screen)?).await
                            {
                                if matches!(e, zectrix_sdk::Error::Disconnected) {
                                    break;
                                }
                                return Err(e.into());
                            }
                            continue;
                        }
                    };
                    println!("[drill] 识别文本：{text:?}");

                    let attempt_id = uuid::Uuid::new_v4().to_string();
                    match app.submit(seq, &attempt_id, &text)? {
                        SubmitResult::Judged {
                            correct,
                            parsed,
                            expected,
                            ..
                        } => {
                            screen = Screen::Result { correct, parsed, expected };
                            if let Err(e) =
                                show_retry(&device, &render_screen(&renderer, &app, &screen)?).await
                            {
                                if matches!(e, zectrix_sdk::Error::Disconnected) {
                                    break;
                                }
                                return Err(e.into());
                            }
                            let sound = if correct { correct_sound() } else { wrong_sound() };
                            device.play(&sound, 70).await.ok();
                            ui = Ui::Result;
                        }
                        SubmitResult::RetryNeeded { reason } => {
                            let msg = reason.message();
                            println!("[drill] 需要重说：{msg}");
                            screen = Screen::Retry(msg);
                            if let Err(e) =
                                show_retry(&device, &render_screen(&renderer, &app, &screen)?).await
                            {
                                if matches!(e, zectrix_sdk::Error::Disconnected) {
                                    break;
                                }
                                return Err(e.into());
                            }
                        }
                        SubmitResult::StaleQuestion { current_seq } => {
                            eprintln!("[drill] 迟到结果已丢弃（当前第 {current_seq} 题）");
                        }
                        SubmitResult::DuplicateAttempt => {
                            eprintln!("[drill] 重复尝试已忽略");
                        }
                        SubmitResult::SessionFinished => {
                            done = true;
                            break;
                        }
                    }
                }
            }
        }

        // —— 会话结束处理 ——
        device.close().await.ok();
        if done {
            break 'service;
        }
        eprintln!("[drill] 10s 后重连…");
        tokio::time::sleep(Duration::from_secs(10)).await;
    }

    Ok(())
}
