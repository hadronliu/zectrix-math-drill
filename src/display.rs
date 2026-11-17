//! 设备显示适配层接口（开发方案 §9.6）。
//!
//! 逻辑层只提交 [`DisplayIntent`]，不依赖具体渲染实现：
//! 阶段二用 [`TextScreen`]（终端）；阶段三/四实现 `Screen` trait，
//! 渲染 400×300 PNG 后经 zectrix-sdk 推送，逻辑层不变。

use crate::session::SessionSummary;

/// 显示意图。
#[derive(Debug, Clone)]
pub enum DisplayIntent {
    /// 练习开始。
    Welcome {
        count: usize,
        difficulty: &'static str,
    },
    /// 题目页（大字号题面 + 进度）。
    Question {
        seq: usize,
        total: usize,
        a: u32,
        b: u32,
    },
    /// 判题结果页。
    Judged {
        correct: bool,
        parsed: i64,
        expected: i64,
    },
    /// 重说提示（不判错）。
    Retry { message: String },
    /// 中止。
    Aborted,
    /// 练习总结页。
    Summary(SessionSummary),
}

/// 屏幕适配接口。
pub trait Screen {
    fn show(&mut self, intent: &DisplayIntent);
}

/// 终端文本渲染（阶段二）。
pub struct TextScreen;

impl Screen for TextScreen {
    fn show(&mut self, intent: &DisplayIntent) {
        match intent {
            DisplayIntent::Welcome { count, difficulty } => {
                println!("=== 两位数乘法口算 ===");
                println!("难度 {difficulty}，共 {count} 题。");
                println!("本阶段为纯业务闭环：请以文本模拟 ASR 识别结果（直接输入数字或中文读法）。");
                println!("空输入 = 模拟静音；输入 q 中止练习。");
            }
            DisplayIntent::Question { seq, total, a, b } => {
                println!();
                println!("—— 第 {seq}/{total} 题 ——");
                println!();
                println!("        {a} × {b} = ?");
                println!();
            }
            DisplayIntent::Judged {
                correct,
                parsed,
                expected,
            } => {
                if *correct {
                    println!("✔ 正确！{parsed} ✓");
                } else {
                    println!("✘ 不对。你说的是 {parsed}，正确答案是 {expected}。");
                }
            }
            DisplayIntent::Retry { message } => {
                println!("… {message}");
            }
            DisplayIntent::Aborted => {
                println!("练习已中止，进度已保存。");
            }
            DisplayIntent::Summary(s) => {
                println!();
                println!("=== 练习总结 ===");
                println!("题数：{}，有首答：{}", s.total_questions, s.questions_with_first_answer);
                println!(
                    "首答正确：{}（{:.0}%）",
                    s.first_attempt_correct,
                    s.first_answer_rate().unwrap_or(0.0) * 100.0
                );
                println!(
                    "订正成功：{} 次；未识别：{} 次；歧义重说：{} 次",
                    s.corrections, s.unparseable_attempts, s.ambiguous_attempts
                );
                println!();
                println!("逐题：");
                for q in &s.per_question {
                    let first = match q.first_valid_correct {
                        Some(true) => "√",
                        Some(false) => "×",
                        None => "—",
                    };
                    println!(
                        "  第{}题 {}×{}={}  首答:{}  订正:{} 次",
                        q.seq, q.a, q.b, q.answer, first, q.correction_successes
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::question::QuestionConfig;
    use crate::rng::Rng;
    use crate::session::Session;

    #[test]
    fn all_intents_render_without_panic() {
        let mut screen = TextScreen;
        let intents = [
            DisplayIntent::Welcome {
                count: 10,
                difficulty: "basic",
            },
            DisplayIntent::Question {
                seq: 1,
                total: 10,
                a: 23,
                b: 14,
            },
            DisplayIntent::Judged {
                correct: false,
                parsed: 99,
                expected: 322,
            },
            DisplayIntent::Retry {
                message: "没有听到有效数字，请重说".into(),
            },
            DisplayIntent::Aborted,
        ];
        for i in &intents {
            screen.show(i);
        }
        let s = Session::new(QuestionConfig::default(), &mut Rng::new(1)).unwrap();
        screen.show(&DisplayIntent::Summary(s.summary()));
    }
}
