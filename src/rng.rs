//! 可注入种子的极简随机数（SplitMix64），避免引入 rand 依赖，便于测试复现。

/// SplitMix64 伪随机数生成器。
pub struct Rng(u64);

impl Rng {
    /// 以任意种子构造；内部保证状态非零。
    pub fn new(seed: u64) -> Self {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    /// 以当前系统时间（纳秒）构造，用于实际运行。
    pub fn from_time() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x1234_5678_9ABC_DEF0);
        Rng::new(nanos)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// 闭区间 [lo, hi] 均匀整数。lo > hi 时 panic（调用方保证）。
    pub fn range_u32(&mut self, lo: u32, hi: u32) -> u32 {
        assert!(lo <= hi, "range_u32: lo > hi");
        let span = (hi - lo) as u64 + 1;
        lo + (self.next_u64() % span) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_same_seed() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn range_in_bounds() {
        let mut r = Rng::new(7);
        for _ in 0..1000 {
            let v = r.range_u32(10, 99);
            assert!((10..=99).contains(&v));
        }
    }
}
