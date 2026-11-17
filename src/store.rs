//! SQLite 持久化（开发方案 §10）。
//!
//! 数据模型：练习会话 / 题目记录 / 答题尝试。
//! 统计口径与 [`crate::session`] 内存汇总一致，测试中互相校验。

use crate::parser::AmbiguityReason;
use crate::question::{Difficulty, Question};
use crate::session::{AttemptOutcome, AttemptRecord};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

/// 会话状态。
pub const STATUS_ACTIVE: &str = "active";
pub const STATUS_FINISHED: &str = "finished";
pub const STATUS_ABORTED: &str = "aborted";

/// 会话元数据（§10.1）。
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub id: String,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub difficulty: String,
    pub question_count: usize,
    pub swap_dedup: bool,
    pub current_seq: usize,
    pub status: String,
}

/// 加载出的完整会话数据。
pub type LoadedSession = (SessionRow, Vec<Question>, Vec<AttemptRecord>);

/// 以 SQL 聚合的会话统计（用于与内存汇总互验，§13.1）。
#[derive(Debug, Clone, PartialEq)]
pub struct SqlStats {
    pub first_valid_answers: i64,
    pub first_valid_correct: i64,
    pub correction_successes: i64,
    pub unparseable: i64,
    pub ambiguous: i64,
}

/// SQLite 存储。
pub struct Store {
    conn: Connection,
}

fn outcome_to_str(o: AttemptOutcome) -> &'static str {
    o.as_str()
}

fn str_to_outcome(s: &str) -> Option<AttemptOutcome> {
    Some(match s {
        "correct" => AttemptOutcome::Correct,
        "wrong" => AttemptOutcome::Wrong,
        "unparseable" => AttemptOutcome::Unparseable,
        "ambiguous_correction" => AttemptOutcome::Ambiguous(AmbiguityReason::Correction),
        "ambiguous_omitted" => AttemptOutcome::Ambiguous(AmbiguityReason::OmittedUnit),
        "ambiguous_multiple" => AttemptOutcome::Ambiguous(AmbiguityReason::MultipleNumbers),
        "ambiguous_mixed" => AttemptOutcome::Ambiguous(AmbiguityReason::MixedFormat),
        _ => return None,
    })
}

impl Store {
    /// 打开（或创建）数据库文件；父目录需已存在。
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Self::init(conn)
    }

    /// 内存库（测试用）。
    pub fn open_in_memory() -> Result<Self, rusqlite::Error> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self, rusqlite::Error> {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS sessions (
                id             TEXT PRIMARY KEY,
                started_at     INTEGER NOT NULL,
                ended_at       INTEGER,
                difficulty     TEXT NOT NULL,
                question_count INTEGER NOT NULL,
                swap_dedup     INTEGER NOT NULL,
                current_seq    INTEGER NOT NULL DEFAULT 1,
                status         TEXT NOT NULL DEFAULT 'active'
            );
            CREATE TABLE IF NOT EXISTS questions (
                session_id TEXT NOT NULL REFERENCES sessions(id),
                seq        INTEGER NOT NULL,
                a          INTEGER NOT NULL,
                b          INTEGER NOT NULL,
                answer     INTEGER NOT NULL,
                PRIMARY KEY (session_id, seq)
            );
            CREATE TABLE IF NOT EXISTS attempts (
                id           TEXT PRIMARY KEY,
                session_id   TEXT NOT NULL,
                question_seq INTEGER NOT NULL,
                asr_text     TEXT NOT NULL,
                parsed_int   INTEGER,
                correct      INTEGER,
                outcome      TEXT NOT NULL,
                first_valid  INTEGER NOT NULL DEFAULT 0,
                elapsed_ms   INTEGER,
                created_at   INTEGER NOT NULL,
                FOREIGN KEY (session_id, question_seq)
                    REFERENCES questions(session_id, seq)
            );
            CREATE INDEX IF NOT EXISTS idx_attempts_session
                ON attempts(session_id, question_seq);
            "#,
        )?;
        Ok(Store { conn })
    }

    /// 写入会话头与全部题目（会话创建时一次性完成）。
    pub fn create_session(
        &self,
        meta: &SessionRow,
        questions: &[Question],
    ) -> Result<(), rusqlite::Error> {
        let tx = self.conn.unchecked_transaction()?;
        self.conn.execute(
            "INSERT INTO sessions (id, started_at, ended_at, difficulty, question_count, swap_dedup, current_seq, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                meta.id,
                meta.started_at as i64,
                meta.ended_at.map(|t| t as i64),
                meta.difficulty,
                meta.question_count as i64,
                meta.swap_dedup as i64,
                meta.current_seq as i64,
                meta.status,
            ],
        )?;
        for q in questions {
            self.insert_question(&meta.id, q)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn insert_question(&self, session_id: &str, q: &Question) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "INSERT INTO questions (session_id, seq, a, b, answer) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session_id, q.seq as i64, q.a, q.b, q.answer],
        )?;
        Ok(())
    }

    /// 幂等写入尝试记录（重复 attempt_id 忽略，配合会话层去重）。
    pub fn insert_attempt(
        &self,
        session_id: &str,
        a: &AttemptRecord,
    ) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "INSERT OR IGNORE INTO attempts
             (id, session_id, question_seq, asr_text, parsed_int, correct, outcome, first_valid, elapsed_ms, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                a.attempt_id,
                session_id,
                a.question_seq as i64,
                a.asr_text,
                a.parsed,
                a.correct.map(|c| c as i64),
                outcome_to_str(a.outcome),
                a.first_valid as i64,
                a.elapsed_ms.map(|t| t as i64),
                a.created_at as i64,
            ],
        )?;
        Ok(())
    }

    /// 推进当前题游标。
    pub fn update_progress(&self, session_id: &str, current_seq: usize) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "UPDATE sessions SET current_seq = ?2 WHERE id = ?1",
            params![session_id, current_seq as i64],
        )?;
        Ok(())
    }

    /// 结束会话（finished / aborted）。
    pub fn close_session(
        &self,
        session_id: &str,
        status: &str,
        ended_at: u64,
    ) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "UPDATE sessions SET status = ?2, ended_at = ?3 WHERE id = ?1",
            params![session_id, status, ended_at as i64],
        )?;
        Ok(())
    }

    /// 加载会话（恢复未完成练习，§11.1）。
    pub fn load_session(&self, session_id: &str) -> Result<Option<LoadedSession>, rusqlite::Error> {
        let meta = self.conn
            .query_row(
                "SELECT id, started_at, ended_at, difficulty, question_count, swap_dedup, current_seq, status
                 FROM sessions WHERE id = ?1",
                params![session_id],
                |row| {
                    Ok(SessionRow {
                        id: row.get(0)?,
                        started_at: row.get::<_, i64>(1)? as u64,
                        ended_at: row.get::<_, Option<i64>>(2)?.map(|t| t as u64),
                        difficulty: row.get(3)?,
                        question_count: row.get::<_, i64>(4)? as usize,
                        swap_dedup: row.get::<_, i64>(5)? != 0,
                        current_seq: row.get::<_, i64>(6)? as usize,
                        status: row.get(7)?,
                    })
                },
            )
            .optional()?;
        let Some(meta) = meta else {
            return Ok(None);
        };

        let mut stmt = self.conn.prepare(
            "SELECT seq, a, b, answer FROM questions WHERE session_id = ?1 ORDER BY seq",
        )?;
        let questions = stmt
            .query_map(params![session_id], |row| {
                Ok(Question {
                    seq: row.get::<_, i64>(0)? as usize,
                    a: row.get(1)?,
                    b: row.get(2)?,
                    answer: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut stmt = self.conn.prepare(
            "SELECT id, question_seq, asr_text, parsed_int, correct, outcome, first_valid, elapsed_ms, created_at
             FROM attempts WHERE session_id = ?1 ORDER BY created_at, rowid",
        )?;
        let attempts = stmt
            .query_map(params![session_id], |row| {
                let outcome_s: String = row.get(5)?;
                Ok(AttemptRecord {
                    attempt_id: row.get(0)?,
                    question_seq: row.get::<_, i64>(1)? as usize,
                    asr_text: row.get(2)?,
                    parsed: row.get(3)?,
                    correct: row.get::<_, Option<i64>>(4)?.map(|c| c != 0),
                    outcome: str_to_outcome(&outcome_s)
                        .unwrap_or(AttemptOutcome::Unparseable),
                    first_valid: row.get::<_, i64>(6)? != 0,
                    elapsed_ms: row.get::<_, Option<i64>>(7)?.map(|t| t as u64),
                    created_at: row.get::<_, i64>(8)? as u64,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Some((meta, questions, attempts)))
    }

    /// 以 SQL 聚合的会话统计（用于与内存汇总互验，§13.1）。
    pub fn session_stats(&self, session_id: &str) -> Result<SqlStats, rusqlite::Error> {
        self.conn.query_row(
            "SELECT
               COALESCE(SUM(first_valid), 0),
               COALESCE(SUM(CASE WHEN first_valid = 1 AND correct = 1 THEN 1 ELSE 0 END), 0),
               COALESCE(SUM(CASE WHEN first_valid = 0 AND correct = 1 THEN 1 ELSE 0 END), 0),
               COALESCE(SUM(CASE WHEN outcome = 'unparseable' THEN 1 ELSE 0 END), 0),
               COALESCE(SUM(CASE WHEN outcome LIKE 'ambiguous%' THEN 1 ELSE 0 END), 0)
             FROM attempts WHERE session_id = ?1",
            params![session_id],
            |row| {
                Ok(SqlStats {
                    first_valid_answers: row.get(0)?,
                    first_valid_correct: row.get(1)?,
                    correction_successes: row.get(2)?,
                    unparseable: row.get(3)?,
                    ambiguous: row.get(4)?,
                })
            },
        )
    }
}

/// 由难度字符串重建难度枚举（恢复会话用）。
pub fn parse_difficulty(s: &str) -> Option<Difficulty> {
    Difficulty::parse(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::question::QuestionConfig;
    use crate::rng::Rng;
    use crate::session::Session;

    #[test]
    fn failed_creation_rolls_back_header_and_questions() {
        let (session, store) = seed_session();
        let mut meta = store.load_session(&session.id).unwrap().unwrap().0;
        meta.id = "rollback".into();
        let q = session.questions()[0].clone();
        assert!(store.create_session(&meta, &[q.clone(), q]).is_err());
        assert!(store.load_session("rollback").unwrap().is_none());
    }

    fn seed_session() -> (Session, Store) {
        let store = Store::open_in_memory().unwrap();
        let s = Session::new(
            QuestionConfig {
                count: 3,
                ..Default::default()
            },
            &mut Rng::new(11),
        )
        .unwrap();
        let meta = SessionRow {
            id: s.id.clone(),
            started_at: 1_700_000_000,
            ended_at: None,
            difficulty: s.config.difficulty.as_str().into(),
            question_count: s.total_questions(),
            swap_dedup: s.config.swap_dedup,
            current_seq: 1,
            status: STATUS_ACTIVE.into(),
        };
        store.create_session(&meta, s.questions()).unwrap();
        (s, store)
    }

    fn run_full(s: &mut Session, store: &Store, deltas: &[i64]) {
        let mut i = 0;
        while let Some(seq) = s.current_seq() {
            s.mark_presented();
            let q = s.current_question().unwrap().clone();
            let attempt_id = format!("t{}", i);
            let r = s.submit(seq, &attempt_id, &(q.answer as i64 + deltas[i]).to_string());
            store.insert_attempt(&s.id, s.attempts().last().unwrap()).unwrap();
            assert!(matches!(r, crate::session::SubmitResult::Judged { .. }));
            if s.advance() {
                store.update_progress(&s.id, s.progress_cursor()).unwrap();
            }
            i += 1;
        }
    }

    #[test]
    fn roundtrip_and_sql_stats_match_memory() {
        let (mut s, store) = seed_session();
        run_full(&mut s, &store, &[0, 1, -1]); // 对、错、错
        store.close_session(&s.id, STATUS_FINISHED, 1_700_000_100).unwrap();

        let (meta, questions, attempts) = store.load_session(&s.id).unwrap().unwrap();
        assert_eq!(meta.status, STATUS_FINISHED);
        assert_eq!(questions.len(), 3);
        assert_eq!(attempts.len(), 3);
        assert_eq!(questions, s.questions());

        let st = store.session_stats(&s.id).unwrap();
        let mem = s.summary();
        assert_eq!(st.first_valid_answers as usize, mem.questions_with_first_answer);
        assert_eq!(st.first_valid_correct as usize, mem.first_attempt_correct);
        assert_eq!(st.correction_successes as usize, mem.corrections);
        assert_eq!(st.unparseable as usize, mem.unparseable_attempts);
        assert_eq!(st.ambiguous as usize, mem.ambiguous_attempts);
    }

    #[test]
    fn resume_session_from_db() {
        let (mut s, store) = seed_session();
        // 只答第 1 题
        s.mark_presented();
        let q = s.current_question().unwrap().clone();
        s.submit(1, "half", &(q.answer as i64).to_string());
        store.insert_attempt(&s.id, s.attempts().last().unwrap()).unwrap();
        s.advance();
        store.update_progress(&s.id, s.progress_cursor()).unwrap();

        let (meta, questions, attempts) = store.load_session(&s.id).unwrap().unwrap();
        let mut resumed = Session::from_parts(
            meta.id.clone(),
            QuestionConfig {
                count: meta.question_count,
                difficulty: parse_difficulty(&meta.difficulty).unwrap(),
                swap_dedup: meta.swap_dedup,
            },
            questions,
            meta.current_seq - 1,
            attempts,
            meta.status == STATUS_FINISHED,
        );
        assert_eq!(resumed.current_seq(), Some(2));
        assert!(!resumed.is_finished());
        // 首答状态已恢复：第 2 题新答案是首答
        let q2 = resumed.current_question().unwrap().clone();
        assert!(matches!(
            resumed.submit(2, "after-resume", &q2.answer.to_string()),
            crate::session::SubmitResult::Judged {
                correct: true,
                first_valid: true,
                ..
            }
        ));
    }

    #[test]
    fn duplicate_attempt_insert_ignored() {
        let (mut s, store) = seed_session();
        s.mark_presented();
        let q = s.current_question().unwrap().clone();
        s.submit(1, "once", &q.answer.to_string());
        let rec = s.attempts().last().unwrap().clone();
        store.insert_attempt(&s.id, &rec).unwrap();
        store.insert_attempt(&s.id, &rec).unwrap(); // 幂等
        let (_, _, attempts) = store.load_session(&s.id).unwrap().unwrap();
        assert_eq!(attempts.len(), 1);
    }
}
