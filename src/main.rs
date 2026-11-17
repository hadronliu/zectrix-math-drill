//! 阶段二 CLI：在终端模拟「ASR 文本」输入，跑通完整业务闭环。
//!
//! 阶段三接入 zectrix-sdk 后，同一套 [`math_drill::app::DrillApp`] 由
//! 设备事件循环驱动，本文件仅作为验证入口。

use math_drill::app::DrillApp;
use math_drill::display::{DisplayIntent, Screen, TextScreen};
use math_drill::question::{Difficulty, QuestionConfig};
use math_drill::rng::Rng;
use math_drill::store::Store;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

struct Args {
    count: usize,
    difficulty: Difficulty,
    db: PathBuf,
    swap_dedup: bool,
    seed: Option<u64>,
    resume: Option<String>,
    help: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        count: 10,
        difficulty: Difficulty::Basic,
        db: PathBuf::from("data/math-drill.db"),
        swap_dedup: true,
        seed: None,
        resume: None,
        help: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--count" | "-n" => {
                a.count = it.next().ok_or("--count 需要数值")?.parse().map_err(|_| "--count 需要整数")?
            }
            "--difficulty" | "-d" => {
                let v = it.next().ok_or("--difficulty 需要值")?;
                a.difficulty =
                    Difficulty::parse(&v).ok_or(format!("未知难度 {v}（entry/basic/advanced）"))?;
            }
            "--db" => a.db = PathBuf::from(it.next().ok_or("--db 需要路径")?),
            "--no-swap-dedup" => a.swap_dedup = false,
            "--seed" => a.seed = Some(it.next().ok_or("--seed 需要数值")?.parse().map_err(|_| "--seed 需要整数")?),
            "--resume" => a.resume = Some(it.next().ok_or("--resume 需要会话 ID")?),
            "--help" | "-h" => a.help = true,
            other => return Err(format!("未知参数 {other}（见 --help）")),
        }
    }
    Ok(a)
}

fn print_help() {
    println!(
        "两位数乘法口算（阶段二：纯业务闭环 CLI）

用法：
  math-drill [选项]

选项：
  -n, --count <N>          题量，默认 10
  -d, --difficulty <级别>  entry | basic | advanced | square，默认 basic
      --db <路径>          SQLite 文件路径，默认 data/math-drill.db
      --no-swap-dedup      不将 23×14 与 14×23 视为同题
      --seed <N>           固定随机种子（复现演示）
      --resume <会话ID>    恢复未完成的练习
  -h, --help

输入约定（模拟 ASR 文本）：
  直接输入识别文本，如：322、三百二十二、答案是三百二十二
  空输入 = 模拟静音；q = 中止练习"
    );
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("错误：{e}");
            std::process::exit(2);
        }
    };
    if args.help {
        print_help();
        return;
    }

    if let Some(parent) = args.db.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    let store = match Store::open(&args.db) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("错误：无法打开数据库 {}：{e}", args.db.display());
            std::process::exit(1);
        }
    };

    let mut app = if let Some(sid) = &args.resume {
        match DrillApp::resume(&store, sid) {
            Ok(a) => {
                println!("已恢复会话 {sid}。");
                a
            }
            Err(e) => {
                eprintln!("错误：{e}");
                std::process::exit(1);
            }
        }
    } else {
        let cfg = QuestionConfig {
            count: args.count,
            difficulty: args.difficulty,
            swap_dedup: args.swap_dedup,
        };
        let mut rng = match args.seed {
            Some(s) => Rng::new(s),
            None => Rng::from_time(),
        };
        match DrillApp::new(cfg, &mut rng, &store) {
            Ok(a) => {
                println!("会话 ID：{}（中断后可用 --resume {} 恢复）", a.session_id(), a.session_id());
                a
            }
            Err(e) => {
                eprintln!("错误：{e}");
                std::process::exit(1);
            }
        }
    };

    let mut screen = TextScreen;
    let total = app.session.total_questions();
    let difficulty = app.session.config.difficulty.as_str();
    screen.show(&DisplayIntent::Welcome {
        count: total,
        difficulty,
    });

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    let mut aborted = false;

    'outer: while let Some(seq) = app.session.current_seq() {
        let q = match app.session.current_question() {
            Some(q) => q.clone(),
            None => break,
        };
        screen.show(&DisplayIntent::Question {
            seq,
            total,
            a: q.a,
            b: q.b,
        });
        app.session.mark_presented();

        loop {
            print!("答案> ");
            let _ = io::stdout().flush();
            let line = match lines.next() {
                Some(Ok(l)) => l,
                Some(Err(e)) => {
                    eprintln!("\n读取输入失败：{e}");
                    aborted = true;
                    break 'outer;
                }
                None => {
                    // EOF：按中止处理，保留进度
                    println!();
                    aborted = true;
                    break 'outer;
                }
            };
            let text = line.trim().to_string();
            if text.eq_ignore_ascii_case("q") {
                aborted = true;
                break 'outer;
            }

            let attempt_id = uuid::Uuid::new_v4().to_string();
            match app.submit(seq, &attempt_id, &text) {
                Ok(math_drill::session::SubmitResult::Judged {
                    correct,
                    parsed,
                    expected,
                    ..
                }) => {
                    screen.show(&DisplayIntent::Judged {
                        correct,
                        parsed,
                        expected,
                    });
                    match app.advance() {
                        Ok(true) => continue 'outer,
                        Ok(false) => break 'outer,
                        Err(e) => {
                            eprintln!("错误：{e}");
                            std::process::exit(1);
                        }
                    }
                }
                Ok(math_drill::session::SubmitResult::RetryNeeded { reason }) => {
                    screen.show(&DisplayIntent::Retry {
                        message: reason.message(),
                    });
                    // 同一题重说；重置计时
                    app.session.mark_presented();
                }
                Ok(math_drill::session::SubmitResult::StaleQuestion { current_seq }) => {
                    screen.show(&DisplayIntent::Retry {
                        message: format!("结果与当前题不符（当前第 {current_seq} 题），已丢弃"),
                    });
                }
                Ok(math_drill::session::SubmitResult::DuplicateAttempt) => {
                    screen.show(&DisplayIntent::Retry {
                        message: "该结果已处理过，已忽略".into(),
                    });
                }
                Ok(math_drill::session::SubmitResult::SessionFinished) => break 'outer,
                Err(e) => {
                    eprintln!("错误：{e}");
                    std::process::exit(1);
                }
            }
        }
    }

    if aborted {
        let _ = app.abort();
        screen.show(&DisplayIntent::Aborted);
    } else {
        let _ = app.finish();
        screen.show(&DisplayIntent::Summary(app.session.summary()));
    }
    println!("记录已保存：{}", args.db.display());
}
