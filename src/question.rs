//! 题目引擎（开发方案 §9.1）。
//!
//! 生成两个因数均为 10~99 的乘法题，支持难度分级、同组内去重，
//! 并可选将交换因数视为同题（23×14 与 14×23 不重复出现）。

use crate::rng::Rng;

/// 难度分级（§9.1）。错题重练属阶段五，暂不在此层。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Difficulty {
    /// 入门：两因数均在 10~30。
    Entry,
    /// 基础：两因数均在 10~99（默认）。
    Basic,
    /// 进阶：两因数均在 25~99，结果更大。
    Advanced,
    /// 平方特训：两位数平方 n×n（n∈10~99）。
    Square,
}

impl Difficulty {
    pub fn as_str(self) -> &'static str {
        match self {
            Difficulty::Entry => "entry",
            Difficulty::Basic => "basic",
            Difficulty::Advanced => "advanced",
            Difficulty::Square => "square",
        }
    }

    pub fn parse(s: &str) -> Option<Difficulty> {
        match s {
            "entry" | "入门" => Some(Difficulty::Entry),
            "basic" | "基础" => Some(Difficulty::Basic),
            "advanced" | "进阶" => Some(Difficulty::Advanced),
            "square" | "平方" => Some(Difficulty::Square),
            _ => None,
        }
    }

    fn factor_bounds(self) -> (u32, u32) {
        match self {
            Difficulty::Entry => (10, 30),
            Difficulty::Basic => (10, 99),
            Difficulty::Advanced => (25, 99),
            Difficulty::Square => (10, 99), // 平方特训：两因数相同
        }
    }

    /// 该难度下可用的规范题目总数。
    pub fn pool_size(self, swap_dedup: bool) -> usize {
        let (lo, hi) = self.factor_bounds();
        let n = (hi - lo + 1) as usize;
        match self {
            // 平方：a==b，共 90 道，与交换去重无关
            Difficulty::Square => n,
            _ => {
                if swap_dedup {
                    n * (n + 1) / 2
                } else {
                    n * n
                }
            }
        }
    }
}

/// 单道题目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    /// 组内顺序，从 1 开始。
    pub seq: usize,
    pub a: u32,
    pub b: u32,
    /// 标准答案 a*b，判题只做确定性整数比较（§9.5）。
    pub answer: u32,
}

/// 题目配置。`count` 默认 10（§6.1），可配置。
#[derive(Debug, Clone)]
pub struct QuestionConfig {
    pub count: usize,
    pub difficulty: Difficulty,
    /// true 时 (a,b) 与 (b,a) 视为同题，不出现在同一组。
    pub swap_dedup: bool,
}

impl Default for QuestionConfig {
    fn default() -> Self {
        QuestionConfig {
            count: 10,
            difficulty: Difficulty::Basic,
            swap_dedup: true,
        }
    }
}

impl QuestionConfig {
    /// 校验配置可行性：题量不得超过题目池。
    pub fn validate(&self) -> Result<(), String> {
        if self.count == 0 {
            return Err("题量 count 至少为 1".into());
        }
        let pool = self.difficulty.pool_size(self.swap_dedup);
        if self.count > pool {
            return Err(format!(
                "题量 {} 超过难度 {:?} 的题目池上限 {}",
                self.count, self.difficulty, pool
            ));
        }
        Ok(())
    }
}

/// 生成一组互不重复的题目（含交换律去重时按规范键 (min,max) 去重）。
pub fn generate(config: &QuestionConfig, rng: &mut Rng) -> Result<Vec<Question>, String> {
    config.validate()?;
    let (lo, hi) = config.difficulty.factor_bounds();
    let mut seen = std::collections::HashSet::new();
    let mut questions = Vec::with_capacity(config.count);

    while questions.len() < config.count {
        // 平方特训：两因数相同（n×n）；其余难度两因数独立抽取。
        let (a, b) = if config.difficulty == Difficulty::Square {
            let a = rng.range_u32(lo, hi);
            (a, a)
        } else {
            (rng.range_u32(lo, hi), rng.range_u32(lo, hi))
        };
        let key = if config.swap_dedup {
            (a.min(b), a.max(b))
        } else {
            (a, b)
        };
        if !seen.insert(key) {
            continue;
        }
        let seq = questions.len() + 1;
        questions.push(Question {
            seq,
            a,
            b,
            answer: a * b,
        });
    }
    Ok(questions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gen(count: usize, d: Difficulty, swap: bool, seed: u64) -> Vec<Question> {
        let cfg = QuestionConfig {
            count,
            difficulty: d,
            swap_dedup: swap,
        };
        generate(&cfg, &mut Rng::new(seed)).unwrap()
    }

    #[test]
    fn factors_within_difficulty_bounds() {
        for (d, lo, hi) in [
            (Difficulty::Entry, 10, 30),
            (Difficulty::Basic, 10, 99),
            (Difficulty::Advanced, 25, 99),
            (Difficulty::Square, 10, 99),
        ] {
            for q in gen(50, d, true, 1) {
                assert!((lo..=hi).contains(&q.a) && (lo..=hi).contains(&q.b), "{:?}", q);
                assert_eq!(q.answer, q.a * q.b);
                assert!(q.seq >= 1);
            }
        }
    }

    #[test]
    fn no_duplicates_with_swap_dedup() {
        let qs = gen(100, Difficulty::Basic, true, 3);
        let mut keys = std::collections::HashSet::new();
        for q in &qs {
            assert!(keys.insert((q.a.min(q.b), q.a.max(q.b))), "重复：{:?}", q);
        }
        assert_eq!(qs.len(), 100);
        assert!(qs.iter().all(|q| (100..=9801).contains(&q.answer)));
    }

    #[test]
    fn swap_allowed_when_dedup_disabled() {
        // 关闭交换去重后，交换对是合法输出（允许存在，仅验证不报错且长度正确）
        let qs = gen(200, Difficulty::Basic, false, 5);
        assert_eq!(qs.len(), 200);
    }

    #[test]
    fn count_exceeding_pool_is_error() {
        let cfg = QuestionConfig {
            count: Difficulty::Entry.pool_size(true) + 1,
            difficulty: Difficulty::Entry,
            swap_dedup: true,
        };
        assert!(generate(&cfg, &mut Rng::new(1)).is_err());
        assert!(QuestionConfig::default().validate().is_ok());
    }

    #[test]
    fn square_mode_generates_equal_factors() {
        let qs = gen(50, Difficulty::Square, true, 9);
        assert_eq!(qs.len(), 50);
        let mut seen = std::collections::HashSet::new();
        for q in &qs {
            assert_eq!(q.a, q.b, "平方特训必须两因数相同：{:?}", q);
            assert!((10..=99).contains(&q.a));
            assert_eq!(q.answer, q.a * q.a);
            assert!(seen.insert(q.a), "同一底数重复：{:?}", q);
        }
        assert_eq!(Difficulty::Square.pool_size(true), 90);
        let too_many = QuestionConfig {
            count: 91,
            difficulty: Difficulty::Square,
            swap_dedup: true,
        };
        assert!(generate(&too_many, &mut Rng::new(1)).is_err());
        assert_eq!(Difficulty::parse("平方"), Some(Difficulty::Square));
    }

    #[test]
    fn deterministic_with_same_seed() {
        let x = gen(10, Difficulty::Basic, true, 99);
        let y = gen(10, Difficulty::Basic, true, 99);
        assert_eq!(x, y);
    }
}
