//! 会话与状态管理、判题与训练记录（开发方案 §9.2/§9.5/§10.2）。
//!
//! 关键不变式：
//! - 每次作答绑定题目序号与尝试标识（§9.2）；
//! - 上一题延迟返回的结果提交到非当前题 → 拒收（StaleQuestion）；
//! - 同一尝试标识重复提交 → 拒收（DuplicateAttempt），判题计分至多一次；
//! - 无效/歧义输入不计入算术错误，首答有效数字决定首答正确率（§10.2）。

use crate::parser::{self, AmbiguityReason, ParsedAnswer};
use crate::question::{self, Question, QuestionConfig};
use crate::rng::Rng;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// 重说原因（不判错）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryReason {
    NotANumber,
    Ambiguous(AmbiguityReason),
}

impl RetryReason {
    pub fn message(self) -> String {
        match self {
            RetryReason::NotANumber => "没有听到有效数字，请重说".into(),
            RetryReason::Ambiguous(r) => r.message().into(),
        }
    }
}

/// 尝试记录类别（存储用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptOutcome {
    Correct,
    Wrong,
    Unparseable,
    Ambiguous(AmbiguityReason),
}

impl AttemptOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            AttemptOutcome::Correct => "correct",
            AttemptOutcome::Wrong => "wrong",
            AttemptOutcome::Unparseable => "unparseable",
            AttemptOutcome::Ambiguous(AmbiguityReason::Correction) => "ambiguous_correction",
            AttemptOutcome::Ambiguous(AmbiguityReason::OmittedUnit) => "ambiguous_omitted",
            AttemptOutcome::Ambiguous(AmbiguityReason::MultipleNumbers) => "ambiguous_multiple",
            AttemptOutcome::Ambiguous(AmbiguityReason::MixedFormat) => "ambiguous_mixed",
        }
    }
}

/// 单次作答尝试（§10.1）。
#[derive(Debug, Clone)]
pub struct AttemptRecord {
    pub attempt_id: String,
    pub question_seq: usize,
    pub asr_text: String,
    /// 解析出的整数；无效/歧义为 None。
    pub parsed: Option<i64>,
    /// 判题结果；None = 未判题。
    pub correct: Option<bool>,
    pub outcome: AttemptOutcome,
    /// 是否为该题首次有效数字回答（决定首答正确率）。
    pub first_valid: bool,
    /// 题面呈现到提交的耗时（估算值，§10.2：不计入 ASR 耗时）。
    pub elapsed_ms: Option<u64>,
    pub created_at: u64,
}

/// 提交结果。
#[derive(Debug, Clone, PartialEq)]
pub enum SubmitResult {
    /// 已判题。
    Judged {
        correct: bool,
        parsed: i64,
        expected: i64,
        /// 是否首答（首次有效数字回答）。
        first_valid: bool,
    },
    /// 需要重说，不计错。
    RetryNeeded { reason: RetryReason },
    /// 提交到非当前题（迟到结果），已丢弃。
    StaleQuestion { current_seq: usize },
    /// 尝试标识重复，已丢弃（不重复计分）。
    DuplicateAttempt,
    SessionFinished,
}

/// 单题统计。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionStat {
    pub seq: usize,
    pub a: u32,
    pub b: u32,
    pub answer: u32,
    /// 首答是否正确；该题从未给出有效数字时为 None。
    pub first_valid_correct: Option<bool>,
    /// 首答之后（订正）答对的次数。
    pub correction_successes: usize,
    pub attempt_count: usize,
}

/// 练习总结（§6.1/§10.2）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub total_questions: usize,
    /// 给出过首次有效数字回答的题数。
    pub questions_with_first_answer: usize,
    pub first_attempt_correct: usize,
    /// 首答错误后订正成功的总次数（不覆盖首答错误）。
    pub corrections: usize,
    pub unparseable_attempts: usize,
    pub ambiguous_attempts: usize,
    pub per_question: Vec<QuestionStat>,
}

impl SessionSummary {
    /// 首答正确率：首答正确 / 有首答的题数（分母不含未作答题，§10.2）。
    pub fn first_answer_rate(&self) -> Option<f64> {
        if self.questions_with_first_answer == 0 {
            None
        } else {
            Some(
                self.first_attempt_correct as f64 / self.questions_with_first_answer as f64,
            )
        }
    }
}

/// 一次练习会话（纯内存逻辑；持久化由 `app::DrillApp` 写通到 [`crate::store`]）。
#[derive(Clone)]
pub struct Session {
    pub id: String,
    pub config: QuestionConfig,
    questions: Vec<Question>,
    current_index: usize,
    attempts: Vec<AttemptRecord>,
    seen_attempt_ids: HashSet<String>,
    /// 题号 → 首次有效数字回答是否正确。
    first_valid_by_q: HashMap<usize, bool>,
    shown_at: Option<Instant>,
    finished: bool,
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Session {
    /// 新建会话并生成题目。
    pub fn new(config: QuestionConfig, rng: &mut Rng) -> Result<Self, String> {
        let questions = question::generate(&config, rng)?;
        Ok(Session {
            id: uuid::Uuid::new_v4().to_string(),
            config,
            questions,
            current_index: 0,
            attempts: Vec::new(),
            seen_attempt_ids: HashSet::new(),
            first_valid_by_q: HashMap::new(),
            shown_at: None,
            finished: false,
        })
    }

    /// 从既有数据恢复（服务重启场景，§11.1）；持久化层调用。
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        id: String,
        config: QuestionConfig,
        questions: Vec<Question>,
        current_index: usize,
        attempts: Vec<AttemptRecord>,
        finished: bool,
    ) -> Self {
        let mut seen = HashSet::new();
        let mut first_valid_by_q = HashMap::new();
        for a in &attempts {
            seen.insert(a.attempt_id.clone());
            if a.first_valid {
                first_valid_by_q.insert(a.question_seq, a.correct == Some(true));
            }
        }
        let question_count = questions.len();
        Session {
            id,
            config,
            questions,
            current_index: current_index.min(question_count),
            attempts,
            seen_attempt_ids: seen,
            first_valid_by_q,
            shown_at: None,
            finished: finished || current_index >= question_count,
        }
    }

    pub fn current_seq(&self) -> Option<usize> {
        if self.finished {
            None
        } else {
            Some(self.current_index + 1)
        }
    }

    pub fn current_question(&self) -> Option<&Question> {
        self.questions.get(self.current_index)
    }

    pub fn total_questions(&self) -> usize {
        self.questions.len()
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    pub fn attempts(&self) -> &[AttemptRecord] {
        &self.attempts
    }

    pub fn questions(&self) -> &[Question] {
        &self.questions
    }

    /// 当前题在存储中的游标（1 起始；结束时为题量+1）。
    pub fn progress_cursor(&self) -> usize {
        (self.current_index + 1).min(self.questions.len() + 1)
    }

    /// 记录题面呈现时刻（计时为估算，§10.2）。
    pub fn mark_presented(&mut self) {
        self.shown_at = Some(Instant::now());
    }

    /// 提交一次作答。`question_seq` 为本次录音绑定的题目序号（1 起始）。
    pub fn submit(&mut self, question_seq: usize, attempt_id: &str, asr_text: &str) -> SubmitResult {
        if self.finished {
            return SubmitResult::SessionFinished;
        }
        // 幂等：重复尝试标识不重复判题、不重复计分（§9.2）。
        if self.seen_attempt_ids.contains(attempt_id) {
            return SubmitResult::DuplicateAttempt;
        }
        let current_seq = self.current_index + 1;
        if question_seq != current_seq {
            return SubmitResult::StaleQuestion { current_seq };
        }

        let created_at = now_unix_secs();
        let elapsed_ms = self.shown_at.map(|t| t.elapsed().as_millis() as u64);
        let expected = self.questions[self.current_index].answer;

        let record = match parser::parse_answer(asr_text) {
            ParsedAnswer::Number(v) => {
                let correct = v == expected as i64;
                let first_valid = !self.first_valid_by_q.contains_key(&current_seq);
                if first_valid {
                    self.first_valid_by_q.insert(current_seq, correct);
                }
                AttemptRecord {
                    attempt_id: attempt_id.to_string(),
                    question_seq: current_seq,
                    asr_text: asr_text.to_string(),
                    parsed: Some(v),
                    correct: Some(correct),
                    outcome: if correct {
                        AttemptOutcome::Correct
                    } else {
                        AttemptOutcome::Wrong
                    },
                    first_valid,
                    elapsed_ms,
                    created_at,
                }
            }
            ParsedAnswer::NotANumber => AttemptRecord {
                attempt_id: attempt_id.to_string(),
                question_seq: current_seq,
                asr_text: asr_text.to_string(),
                parsed: None,
                correct: None,
                outcome: AttemptOutcome::Unparseable,
                first_valid: false,
                elapsed_ms,
                created_at,
            },
            ParsedAnswer::Ambiguous(r) => AttemptRecord {
                attempt_id: attempt_id.to_string(),
                question_seq: current_seq,
                asr_text: asr_text.to_string(),
                parsed: None,
                correct: None,
                outcome: AttemptOutcome::Ambiguous(r),
                first_valid: false,
                elapsed_ms,
                created_at,
            },
        };

        self.seen_attempt_ids.insert(attempt_id.to_string());
        let result = match record.outcome {
            AttemptOutcome::Correct | AttemptOutcome::Wrong => SubmitResult::Judged {
                correct: record.correct == Some(true),
                parsed: record.parsed.unwrap_or(0),
                expected: expected as i64,
                first_valid: record.first_valid,
            },
            AttemptOutcome::Unparseable => SubmitResult::RetryNeeded {
                reason: RetryReason::NotANumber,
            },
            AttemptOutcome::Ambiguous(r) => SubmitResult::RetryNeeded {
                reason: RetryReason::Ambiguous(r),
            },
        };
        self.attempts.push(record);
        result
    }

    /// 进入下一题；返回是否仍有当前题。最后一题判题后调用将结束会话。
    pub fn advance(&mut self) -> bool {
        if self.finished {
            return false;
        }
        if self.current_index + 1 < self.questions.len() {
            self.current_index += 1;
            true
        } else {
            self.current_index = self.questions.len();
            self.finished = true;
            false
        }
    }

    /// 练习总结。
    pub fn summary(&self) -> SessionSummary {
        let mut per_question = Vec::with_capacity(self.questions.len());
        let mut questions_with_first_answer = 0;
        let mut first_attempt_correct = 0;
        let mut corrections = 0;
        let mut unparseable = 0;
        let mut ambiguous = 0;

        for q in &self.questions {
            let mut stat = QuestionStat {
                seq: q.seq,
                a: q.a,
                b: q.b,
                answer: q.answer,
                first_valid_correct: None,
                correction_successes: 0,
                attempt_count: 0,
            };
            for a in self.attempts.iter().filter(|a| a.question_seq == q.seq) {
                stat.attempt_count += 1;
                match a.outcome {
                    AttemptOutcome::Unparseable => unparseable += 1,
                    AttemptOutcome::Ambiguous(_) => ambiguous += 1,
                    AttemptOutcome::Correct | AttemptOutcome::Wrong => {
                        if a.first_valid {
                            stat.first_valid_correct = Some(a.correct == Some(true));
                            questions_with_first_answer += 1;
                            if a.correct == Some(true) {
                                first_attempt_correct += 1;
                            }
                        } else if a.correct == Some(true) {
                            stat.correction_successes += 1;
                            corrections += 1;
                        }
                    }
                }
            }
            per_question.push(stat);
        }

        SessionSummary {
            total_questions: self.questions.len(),
            questions_with_first_answer,
            first_attempt_correct,
            corrections,
            unparseable_attempts: unparseable,
            ambiguous_attempts: ambiguous,
            per_question,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::question::Difficulty;

    fn session(count: usize) -> Session {
        Session::new(
            QuestionConfig {
                count,
                difficulty: Difficulty::Basic,
                swap_dedup: true,
            },
            &mut Rng::new(1),
        )
        .unwrap()
    }

    fn answer_text(q: &Question, delta: i64) -> String {
        (q.answer as i64 + delta).to_string()
    }

    #[test]
    fn happy_path_full_session() {
        let mut s = session(3);
        let mut firsts = Vec::new();
        while let Some(q_seq) = s.current_seq() {
            s.mark_presented();
            let expected = s.current_question().unwrap().answer;
            let r = s.submit(q_seq, &format!("a{}", q_seq), &expected.to_string());
            match r {
                SubmitResult::Judged {
                    correct, first_valid, ..
                } => {
                    assert!(correct && first_valid);
                    firsts.push(q_seq);
                }
                other => panic!("意外结果 {other:?}"),
            }
            s.advance();
        }
        assert_eq!(firsts, vec![1, 2, 3]);
        assert!(s.is_finished());
        let sum = s.summary();
        assert_eq!(sum.first_attempt_correct, 3);
        assert_eq!(sum.questions_with_first_answer, 3);
        assert_eq!(sum.first_answer_rate(), Some(1.0));
    }

    #[test]
    fn wrong_answer_recorded_and_summary_counts() {
        let mut s = session(1);
        let seq = s.current_seq().unwrap();
        s.mark_presented();
        let q = s.current_question().unwrap().clone();
        let r = s.submit(seq, "x1", &answer_text(&q, 1));
        assert!(matches!(r, SubmitResult::Judged { correct: false, .. }));
        s.advance();
        let sum = s.summary();
        assert_eq!(sum.first_attempt_correct, 0);
        assert_eq!(sum.first_answer_rate(), Some(0.0));
        assert_eq!(sum.per_question[0].first_valid_correct, Some(false));
    }

    #[test]
    fn unparseable_not_counted_as_error_retry_keeps_question() {
        let mut s = session(1);
        let seq = s.current_seq().unwrap();
        s.mark_presented();
        assert!(matches!(
            s.submit(seq, "u1", "嗯嗯"),
            SubmitResult::RetryNeeded { .. }
        ));
        // 静音与歧义同样重说且不计错
        assert!(matches!(
            s.submit(seq, "u2", "三百二"),
            SubmitResult::RetryNeeded { .. }
        ));
        // 重说后答对：首答正确
        let q = s.current_question().unwrap().clone();
        assert!(matches!(
            s.submit(seq, "u3", &q.answer.to_string()),
            SubmitResult::Judged {
                correct: true,
                first_valid: true,
                ..
            }
        ));
        let sum = s.summary();
        assert_eq!(sum.first_attempt_correct, 1);
        assert_eq!(sum.unparseable_attempts, 1);
        assert_eq!(sum.ambiguous_attempts, 1);
    }

    #[test]
    fn correction_counts_but_first_answer_stays_wrong() {
        let mut s = session(1);
        let seq = s.current_seq().unwrap();
        s.mark_presented();
        let q = s.current_question().unwrap().clone();
        s.submit(seq, "c1", &answer_text(&q, -1)); // 首答错误
        s.submit(seq, "c2", &q.answer.to_string()); // 订正成功
        let sum = s.summary();
        assert_eq!(sum.first_attempt_correct, 0);
        assert_eq!(sum.corrections, 1);
        assert_eq!(sum.per_question[0].first_valid_correct, Some(false));
        assert_eq!(sum.per_question[0].correction_successes, 1);
    }

    #[test]
    fn stale_late_result_rejected() {
        let mut s = session(2);
        let seq1 = s.current_seq().unwrap();
        s.mark_presented();
        let q1 = s.current_question().unwrap().clone();
        s.submit(seq1, "s1", &q1.answer.to_string());
        s.advance();
        // 迟到结果：拿着第 1 题的答案提交到当前第 2 题
        assert_eq!(
            s.submit(1, "s1-late", &q1.answer.to_string()),
            SubmitResult::StaleQuestion { current_seq: 2 }
        );
        // 拿着第 2 题序号提交第 1 题文本同样拒绝
        assert!(matches!(
            s.submit(2, "s1-late2", "随便"),
            SubmitResult::RetryNeeded { .. } // 文本绑定了正确题号，正常进入解析
        ));
    }

    #[test]
    fn duplicate_attempt_scored_once() {
        let mut s = session(1);
        let seq = s.current_seq().unwrap();
        s.mark_presented();
        let q = s.current_question().unwrap().clone();
        assert!(matches!(
            s.submit(seq, "dup", &q.answer.to_string()),
            SubmitResult::Judged { correct: true, .. }
        ));
        assert_eq!(
            s.submit(seq, "dup", &q.answer.to_string()),
            SubmitResult::DuplicateAttempt
        );
        let sum = s.summary();
        assert_eq!(sum.per_question[0].attempt_count, 1);
        assert_eq!(sum.first_attempt_correct, 1);
    }

    #[test]
    fn submit_after_finish_rejected() {
        let mut s = session(1);
        let seq = s.current_seq().unwrap();
        s.mark_presented();
        let q = s.current_question().unwrap().clone();
        s.submit(seq, "f1", &q.answer.to_string());
        s.advance();
        assert_eq!(
            s.submit(1, "f2", &q.answer.to_string()),
            SubmitResult::SessionFinished
        );
        assert!(!s.advance());
        assert!(s.is_finished());
    }

    #[test]
    fn out_of_range_answer_is_wrong_not_invalid() {
        // §8.2：题 23×14，用户说「九十九」→ 记录 99 并判错，不当识别失败
        let mut s = session(1);
        let seq = s.current_seq().unwrap();
        s.mark_presented();
        let r = s.submit(seq, "o1", "九十九");
        match r {
            SubmitResult::Judged {
                correct, parsed, ..
            } => {
                assert!(!correct);
                assert_eq!(parsed, 99);
            }
            other => panic!("意外结果 {other:?}"),
        }
        let sum = s.summary();
        assert_eq!(sum.per_question[0].first_valid_correct, Some(false));
        assert_eq!(sum.unparseable_attempts, 0);
    }

    #[test]
    fn resume_rebuilds_state() {
        let mut s = session(2);
        let seq = s.current_seq().unwrap();
        s.mark_presented();
        let q1 = s.current_question().unwrap().clone();
        s.submit(seq, "r1", &q1.answer.to_string());
        s.advance();
        let cursor = s.progress_cursor();

        let attempts: Vec<AttemptRecord> = s.attempts().to_vec();
        let questions = s.questions().to_vec();
        let mut r = Session::from_parts(
            s.id.clone(),
            s.config.clone(),
            questions,
            cursor - 1,
            attempts,
            false,
        );
        assert_eq!(r.current_seq(), Some(2));
        // 恢复后重复尝试标识仍被识别
        assert_eq!(r.submit(2, "r1", "1"), SubmitResult::DuplicateAttempt);
        // 首答状态保留：第 2 题首答正确
        let q2 = r.current_question().unwrap().clone();
        assert!(matches!(
            r.submit(2, "r2", &q2.answer.to_string()),
            SubmitResult::Judged {
                correct: true,
                first_valid: true,
                ..
            }
        ));
        assert_eq!(r.summary().first_attempt_correct, 2);
    }
}
