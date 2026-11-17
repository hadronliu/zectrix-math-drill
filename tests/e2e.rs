//! 端到端集成测试（对照开发方案 §13.1 功能验收）。
//!
//! 通过库级 API 模拟完整练习：出题 → 作答（中文/数字/静音/歧义/迟到/重复）
//! → 判题 → 总结 → 持久化 → 恢复。

use math_drill::app::DrillApp;
use math_drill::question::{Difficulty, QuestionConfig};
use math_drill::rng::Rng;
use math_drill::session::SubmitResult;
use math_drill::store::Store;

/// §13.1：能完成一整组练习并保存结果；能区分答错、未识别和系统异常；
/// 不出现跨题提交和重复计分。
#[test]
fn full_session_with_mixed_inputs() {
    let store = Store::open_in_memory().unwrap();
    let mut app = DrillApp::new(
        QuestionConfig {
            count: 10,
            difficulty: Difficulty::Basic,
            swap_dedup: true,
        },
        &mut Rng::new(2026),
        &store,
    )
    .unwrap();

    let mut judged = 0;
    while let Some(seq) = app.session.current_seq() {
        app.session.mark_presented();
        let q = app.session.current_question().unwrap().clone();
        match seq {
            // 第 1 题：中文数位读法，答对
            1 => {
                let text = format!("{}", q.answer);
                assert!(matches!(
                    app.submit(seq, "a1", &text).unwrap(),
                    SubmitResult::Judged { correct: true, .. }
                ));
                judged += 1;
            }
            // 第 2 题：先静音，再歧义重说，最后答错
            2 => {
                assert!(matches!(
                    app.submit(seq, "a2", "").unwrap(),
                    SubmitResult::RetryNeeded { .. }
                ));
                assert!(matches!(
                    app.submit(seq, "a3", "那个……三百二").unwrap(),
                    SubmitResult::RetryNeeded { .. }
                ));
                assert!(matches!(
                    app.submit(seq, "a4", &format!("{}", q.answer as i64 - 1)).unwrap(),
                    SubmitResult::Judged { correct: false, .. }
                ));
                judged += 1;
            }
            // 第 3 题：答对后重复提交同一尝试 → 拒绝；迟到跨题 → 拒绝
            3 => {
                let text = q.answer.to_string();
                assert!(matches!(
                    app.submit(seq, "a5", &text).unwrap(),
                    SubmitResult::Judged { correct: true, .. }
                ));
                judged += 1;
                assert_eq!(
                    app.submit(seq, "a5", &text).unwrap(),
                    SubmitResult::DuplicateAttempt
                );
                assert!(matches!(
                    app.submit(seq + 1, "a6", "1"),
                    Ok(SubmitResult::StaleQuestion { .. })
                        | Err(_)
                ));
                // 注：seq+1 越界时由库层返回 Stale 或错误均可
            }
            // 其余题目：全部答对
            _ => {
                let text = q.answer.to_string();
                assert!(matches!(
                    app.submit(seq, &format!("ok{seq}"), &text).unwrap(),
                    SubmitResult::Judged { correct: true, .. }
                ));
                judged += 1;
            }
        }
        app.advance().unwrap();
    }

    assert!(app.session.is_finished());
    app.finish().unwrap();

    let sum = app.session.summary();
    assert_eq!(sum.total_questions, 10);
    // 首答统计：第 1 题对；第 2 题未识别/歧义后首答错；第 3 题对；4~10 对
    assert_eq!(sum.questions_with_first_answer, 10);
    assert_eq!(sum.first_attempt_correct, 9);
    assert_eq!(sum.first_answer_rate(), Some(0.9));
    assert_eq!(sum.unparseable_attempts, 1);
    assert_eq!(sum.ambiguous_attempts, 1);
    assert_eq!(judged, 10);

    // SQL 聚合与内存互验（§13.1：判题与统计一致）
    let st = store.session_stats(app.session_id()).unwrap();
    assert_eq!(st.first_valid_answers, 10);
    assert_eq!(st.first_valid_correct, 9);
    assert_eq!(st.unparseable, 1);
    assert_eq!(st.ambiguous, 1);

    // 题目均满足配置范围且组内无重复（含交换律）
    let mut keys = std::collections::HashSet::new();
    for q in app.session.questions() {
        assert!((10..=99).contains(&q.a) && (10..=99).contains(&q.b));
        assert_eq!(q.answer, q.a * q.b);
        assert!(keys.insert((q.a.min(q.b), q.a.max(q.b))));
    }
}

/// §11.1 / §10.1：中途恢复；恢复后重复尝试标识仍拒收，首答口径不重置。
#[test]
fn interrupted_session_resume_keeps_scoring_invariants() {
    let store = Store::open_in_memory().unwrap();
    let sid;
    {
        let mut app = DrillApp::new(
            QuestionConfig {
                count: 5,
                difficulty: Difficulty::Entry,
                swap_dedup: true,
            },
            &mut Rng::new(7),
            &store,
        )
        .unwrap();
        sid = app.session_id().to_string();
        for _ in 0..2 {
            let seq = app.session.current_seq().unwrap();
            app.session.mark_presented();
            let q = app.session.current_question().unwrap().clone();
            app.submit(seq, &format!("p{seq}"), &q.answer.to_string())
                .unwrap();
            app.advance().unwrap();
        }
        // 第 3 题答错后「断电」
        let seq = app.session.current_seq().unwrap();
        app.session.mark_presented();
        let q = app.session.current_question().unwrap().clone();
        app.submit(seq, "p3-wrong", &(q.answer as i64 + 3).to_string())
            .unwrap();
        // 不 advance、不 finish
    }

    let mut app = DrillApp::resume(&store, &sid).unwrap();
    assert_eq!(app.session.current_seq(), Some(3));

    // 重复旧尝试标识 → 拒收（不重复计分）
    assert_eq!(
        app.submit(3, "p3-wrong", "0").unwrap(),
        SubmitResult::DuplicateAttempt
    );
    // 恢复后首答已记录：这题再答对属于订正
    let q = app.session.current_question().unwrap().clone();
    assert!(matches!(
        app.submit(3, "p3-fix", &q.answer.to_string()).unwrap(),
        SubmitResult::Judged {
            correct: true,
            first_valid: false,
            ..
        }
    ));
    app.advance().unwrap();
    while let Some(seq) = app.session.current_seq() {
        app.session.mark_presented();
        let q = app.session.current_question().unwrap().clone();
        app.submit(seq, &format!("z{seq}"), &q.answer.to_string())
            .unwrap();
        app.advance().unwrap();
    }
    app.finish().unwrap();

    let st = store.session_stats(&sid).unwrap();
    assert_eq!(st.first_valid_answers, 5);
    assert_eq!(st.first_valid_correct, 4); // 第 3 题首答错误
    assert_eq!(st.correction_successes, 1);
}

/// 越界数字判错而非识别失败（§8.2）：说「九十九」应得到判题结果。
#[test]
fn out_of_range_spoken_number_judged_wrong() {
    let store = Store::open_in_memory().unwrap();
    let mut app = DrillApp::new(
        QuestionConfig {
            count: 1,
            ..Default::default()
        },
        &mut Rng::new(14),
        &store,
    )
    .unwrap();
    let seq = app.session.current_seq().unwrap();
    app.session.mark_presented();
    let expected = app.session.current_question().unwrap().answer as i64;
    let r = app.submit(seq, "o1", "九十九").unwrap();
    match r {
        SubmitResult::Judged {
            parsed,
            correct,
            expected: exp,
            ..
        } => {
            assert_eq!(parsed, 99);
            assert_eq!(exp, expected);
            assert_eq!(correct, 99 == expected);
        }
        other => panic!("越界数字应判题而非重说：{other:?}"),
    }
    app.advance().unwrap();
    let sum = app.session.summary();
    assert_eq!(sum.unparseable_attempts, 0);
    assert_eq!(sum.questions_with_first_answer, 1);
}

#[test]
fn aborted_session_can_resume_and_completed_cursor_is_safe() {
    let store = Store::open_in_memory().unwrap();
    let mut app = DrillApp::new(QuestionConfig { count: 1, ..Default::default() }, &mut Rng::new(1), &store).unwrap();
    let id = app.session_id().to_owned();
    app.abort().unwrap();
    let mut app = DrillApp::resume(&store, &id).unwrap();
    let answer = app.session.current_question().unwrap().answer.to_string();
    app.submit(1, "answer", &answer).unwrap();
    assert!(!app.advance().unwrap());
    let mut resumed = DrillApp::resume(&store, &id).unwrap();
    assert!(resumed.session.is_finished());
    assert_eq!(resumed.submit(1, "late", "0").unwrap(), SubmitResult::SessionFinished);
}
