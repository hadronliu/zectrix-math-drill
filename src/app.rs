//! 应用编排层：组合 [`Session`] 与 [`Store`]，写通持久化。
//!
//! 逻辑层保持纯内存；本层负责：
//! - 创建会话时落库（会话头 + 题目）；
//! - 每次判题/重说后落库尝试记录；
//! - 推进游标、结束/中止会话；
//! - 服务重启后从库恢复未完成会话（§11.1）。

use crate::question::QuestionConfig;
use crate::rng::Rng;
use crate::session::{AttemptRecord, Session, SubmitResult};
use crate::store::{Store, SessionRow, STATUS_ABORTED, STATUS_ACTIVE, STATUS_FINISHED};

pub struct DrillApp<'a> {
    pub session: Session,
    store: &'a Store,
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl<'a> DrillApp<'a> {
    /// 新建练习并持久化。
    pub fn new(config: QuestionConfig, rng: &mut Rng, store: &'a Store) -> Result<Self, String> {
        let session = Session::new(config, rng)?;
        let meta = SessionRow {
            id: session.id.clone(),
            started_at: now_unix_secs(),
            ended_at: None,
            difficulty: session.config.difficulty.as_str().into(),
            question_count: session.total_questions(),
            swap_dedup: session.config.swap_dedup,
            current_seq: 1,
            status: STATUS_ACTIVE.into(),
        };
        store
            .create_session(&meta, session.questions())
            .map_err(|e| format!("写库失败：{e}"))?;
        Ok(DrillApp { session, store })
    }

    /// 从库恢复未完成会话；仅接受 active 状态。
    pub fn resume(store: &'a Store, session_id: &str) -> Result<Self, String> {
        let (meta, questions, attempts) = store
            .load_session(session_id)
            .map_err(|e| format!("读库失败：{e}"))?
            .ok_or_else(|| format!("会话 {session_id} 不存在"))?;
        if meta.status != STATUS_ACTIVE && meta.status != STATUS_ABORTED {
            return Err(format!("会话 {session_id} 状态为 {}，不能恢复", meta.status));
        }
        let session = Session::from_parts(
            meta.id.clone(),
            QuestionConfig {
                count: meta.question_count,
                difficulty: crate::store::parse_difficulty(&meta.difficulty)
                    .ok_or_else(|| format!("未知难度 {}", meta.difficulty))?,
                swap_dedup: meta.swap_dedup,
            },
            questions,
            meta.current_seq.saturating_sub(1),
            attempts,
            false,
        );
        Ok(DrillApp { session, store })
    }

    /// 提交作答；产生记录时写库。
    pub fn submit(
        &mut self,
        question_seq: usize,
        attempt_id: &str,
        asr_text: &str,
    ) -> Result<SubmitResult, String> {
        let mut candidate = self.session.clone();
        let result = candidate.submit(question_seq, attempt_id, asr_text);
        match &result {
            SubmitResult::Judged { .. } | SubmitResult::RetryNeeded { .. } => {
                let record: &AttemptRecord = candidate
                    .attempts()
                    .last()
                    .ok_or("内部错误：缺少尝试记录")?;
                self.store
                    .insert_attempt(&self.session.id, record)
                    .map_err(|e| format!("写库失败：{e}"))?;
            }
            _ => {}
        }
        self.session = candidate;
        Ok(result)
    }

    /// 进入下一题并更新库内游标。
    pub fn advance(&mut self) -> Result<bool, String> {
        let mut candidate = self.session.clone();
        let has_next = candidate.advance();
        self.store
            .update_progress(&self.session.id, candidate.progress_cursor())
            .map_err(|e| format!("写库失败：{e}"))?;
        self.session = candidate;
        Ok(has_next)
    }

    /// 正常结束练习。
    pub fn finish(&mut self) -> Result<(), String> {
        self.store
            .close_session(&self.session.id, STATUS_FINISHED, now_unix_secs())
            .map_err(|e| format!("写库失败：{e}"))
    }

    /// 中止练习（保留已完成部分，§11.1）。
    pub fn abort(&mut self) -> Result<(), String> {
        self.store
            .close_session(&self.session.id, STATUS_ABORTED, now_unix_secs())
            .map_err(|e| format!("写库失败：{e}"))
    }

    /// 最近一次会话的会话 ID（提示给用户，便于 --resume）。
    pub fn session_id(&self) -> &str {
        &self.session.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::question::Difficulty;
    use crate::session::RetryReason;

    #[test]
    fn full_session_persisted_and_recoverable() {
        let store = Store::open_in_memory().unwrap();
        let mut app = DrillApp::new(
            QuestionConfig {
                count: 3,
                difficulty: Difficulty::Entry,
                swap_dedup: true,
            },
            &mut Rng::new(2),
            &store,
        )
        .unwrap();
        let sid = app.session_id().to_string();

        // 第 1 题答错 → 第 2 题重说一次 → 答对 → 第 3 题答对
        app.session.mark_presented();
        let q1 = app.session.current_question().unwrap().clone();
        assert!(matches!(
            app.submit(1, "e1", &(q1.answer as i64 + 5).to_string()).unwrap(),
            SubmitResult::Judged { correct: false, .. }
        ));
        app.advance().unwrap();

        app.session.mark_presented();
        assert!(matches!(
            app.submit(2, "e2", "").unwrap(),
            SubmitResult::RetryNeeded {
                reason: RetryReason::NotANumber
            }
        ));
        let q2 = app.session.current_question().unwrap().clone();
        assert!(matches!(
            app.submit(2, "e3", &q2.answer.to_string()).unwrap(),
            SubmitResult::Judged {
                correct: true,
                first_valid: true,
                ..
            }
        ));
        app.advance().unwrap();

        app.session.mark_presented();
        let q3 = app.session.current_question().unwrap().clone();
        assert!(matches!(
            app.submit(3, "e4", &q3.answer.to_string()).unwrap(),
            SubmitResult::Judged { correct: true, .. }
        ));
        app.advance().unwrap();
        app.finish().unwrap();

        // 从库读取，统计与内存一致
        let st = store.session_stats(&sid).unwrap();
        assert_eq!(st.first_valid_answers, 3);
        assert_eq!(st.first_valid_correct, 2);
        assert_eq!(st.unparseable, 1);
        let mem = app.session.summary();
        assert_eq!(st.first_valid_correct as usize, mem.first_attempt_correct);
    }

    #[test]
    fn resume_active_session_and_finish() {
        let store = Store::open_in_memory().unwrap();
        let sid;
        {
            let mut app = DrillApp::new(
                QuestionConfig {
                    count: 2,
                    ..Default::default()
                },
                &mut Rng::new(4),
                &store,
            )
            .unwrap();
            sid = app.session_id().to_string();
            app.session.mark_presented();
            let q = app.session.current_question().unwrap().clone();
            app.submit(1, "r-a", &q.answer.to_string()).unwrap();
            app.advance().unwrap();
            // 未结束，模拟服务重启
        }
        {
            let mut app = DrillApp::resume(&store, &sid).unwrap();
            assert_eq!(app.session.current_seq(), Some(2));
            app.session.mark_presented();
            let q = app.session.current_question().unwrap().clone();
            assert!(matches!(
                app.submit(2, "r-b", &q.answer.to_string()).unwrap(),
                SubmitResult::Judged {
                    correct: true,
                    first_valid: true,
                    ..
                }
            ));
            app.advance().unwrap();
            app.finish().unwrap();
            let (_, _, attempts) = store.load_session(&sid).unwrap().unwrap();
            assert_eq!(attempts.len(), 2);
        }
        // 已结束的会话不可恢复
        assert!(DrillApp::resume(&store, &sid).is_err());
    }
}
