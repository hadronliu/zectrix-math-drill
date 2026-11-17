//! 中文数字答案解析（开发方案 §8.3/§8.4）。
//!
//! 输入为 ASR 识别文本，输出三类结果：
//! - [`ParsedAnswer::Number`]：明确整数，进入判题；
//! - [`ParsedAnswer::Ambiguous`]：存在修正、省略、多个候选或混合格式，
//!   按方案 §7.3 要求重说/确认，**不静默选择**；
//! - [`ParsedAnswer::NotANumber`]：无有效数字，按方案 §11.1 不判错，提示重说。
//!
//! 防偏向原则（§8.4）：本模块**不接触标准答案**，接口中不存在答案参数；
//! 解析结果与任何题目无关，可独立测试。

/// 歧义原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmbiguityReason {
    /// 含修正语句，如「三百二十……不对，三百二十二」。
    Correction,
    /// 省略位值表达，如「三百二」「一千二」「一万二」。
    OmittedUnit,
    /// 分隔出多个数字候选。
    MultipleNumbers,
    /// 阿拉伯数字与中文数位混排，如「3百2」。
    MixedFormat,
}

impl AmbiguityReason {
    pub fn message(self) -> &'static str {
        match self {
            AmbiguityReason::Correction => "听到自我修正，请重说一遍",
            AmbiguityReason::OmittedUnit => "听到省略读法（如“三百二”），请说完整，如“三百二十”",
            AmbiguityReason::MultipleNumbers => "听到多个数字，请只说一个答案",
            AmbiguityReason::MixedFormat => "数字格式混乱，请重说",
        }
    }
}

/// 解析结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParsedAnswer {
    /// 明确整数（即使超出本题答案范围也照常返回，由判题判错，见 §8.2）。
    Number(i64),
    Ambiguous(AmbiguityReason),
    /// 无有效数字：不判错，提示重说。
    NotANumber,
}

/// 数字用汉字（含「两」）。
const CN_DIGITS: [char; 9] = ['一', '二', '三', '四', '五', '六', '七', '八', '九'];
/// 位值单位。
const CN_UNITS: [char; 4] = ['十', '百', '千', '万'];

/// 填充/口头语，长词在前以便优先剔除。
const FILLERS: [&str; 23] = [
    "我的答案是", "答案是", "答案", "等于", "就是", "我说", "我猜", "我答", "那个", "呃", "嗯",
    "啊", "呀", "哦", "噢", "喔", "吧", "呢", "了", "是", "的", "得", "那么",
];
/// 修正标记，剔除填充词前先在原文检测。
const CORRECTION_MARKERS: [&str; 3] = ["不对", "错了", "说错"];
/// 候选分隔符。
const SEPARATORS: [char; 8] = [' ', '，', ',', '。', '、', '…', '—', '~'];

fn digit_value(c: char) -> Option<u32> {
    match c {
        '0'..='9' => Some(c as u32 - '0' as u32),
        '零' => Some(0),
        _ => CN_DIGITS.iter().position(|&d| d == c).map(|i| i as u32 + 1),
    }
}

/// 解析 ASR 文本为答案。见模块文档。
pub fn parse_answer(text: &str) -> ParsedAnswer {
    // 预处理：全角数字转半角、修剪空白、「两」归一为「二」、「幺」归一为「一」。
    // 「幺」：电信/军事读法的 1，ASR 常直接转写为汉字「幺」（真机实测）。
    let cleaned: String = text
        .chars()
        .map(|c| match c {
            '０'..='９' => (c as u32 - '０' as u32 + '0' as u32) as u8 as char,
            '两' => '二',
            '幺' => '一',
            _ => c,
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return ParsedAnswer::NotANumber;
    }

    // 剔除填充词。
    let mut body = trimmed.to_string();
    for f in FILLERS {
        if body.contains(f) {
            body = body.replace(f, "");
        }
    }

    let has_number_chars = body
        .chars()
        .any(|c| digit_value(c).is_some() || CN_UNITS.contains(&c));

    // 修正语句：有数字内容即视为歧义（不猜测修正后是哪个数，§7.3）。
    if CORRECTION_MARKERS.iter().any(|m| trimmed.contains(m)) {
        return if has_number_chars {
            ParsedAnswer::Ambiguous(AmbiguityReason::Correction)
        } else {
            ParsedAnswer::NotANumber
        };
    }

    // 按分隔符切段；多段含数字 → 多候选歧义。
    let numeric_segments: Vec<&str> = body
        .split(|c: char| SEPARATORS.contains(&c))
        .filter(|s| {
            let s = s.trim();
            !s.is_empty() && s.chars().any(|c| digit_value(c).is_some() || CN_UNITS.contains(&c))
        })
        .map(|s| s.trim())
        .collect();
    match numeric_segments.len() {
        0 => return ParsedAnswer::NotANumber,
        2.. => return ParsedAnswer::Ambiguous(AmbiguityReason::MultipleNumbers),
        _ => {}
    }
    let seg = numeric_segments[0];

    let has_ascii_digit = seg.chars().any(|c| c.is_ascii_digit());
    let has_cn_unit = seg.chars().any(|c| CN_UNITS.contains(&c));
    let has_cn_digit = seg.chars().any(|c| CN_DIGITS.contains(&c) || c == '零');
    if has_ascii_digit && (has_cn_unit || has_cn_digit) {
        return ParsedAnswer::Ambiguous(AmbiguityReason::MixedFormat);
    }

    // 纯汉字路径：逐字必须是数字或单位，否则按无有效数字处理。
    // （否则垃圾字符会混入位值/逐位解析，可能静默产出错误数字，违反 §8.4。）
    if !has_ascii_digit
        && !seg
            .chars()
            .all(|c| digit_value(c).is_some() || CN_UNITS.contains(&c))
    {
        return ParsedAnswer::NotANumber;
    }

    let parsed = if has_ascii_digit {
        parse_ascii_digits(seg)
    } else if !has_cn_unit {
        parse_digitwise(seg)
    } else {
        parse_place_value(seg)
    };

    // 统一量级护栏：口算语境外的超长数字视为不可靠输入。
    match parsed {
        ParsedAnswer::Number(v) if !(0..=99_999_999).contains(&v) => ParsedAnswer::NotANumber,
        other => other,
    }
}

/// 纯阿拉伯数字段。
fn parse_ascii_digits(seg: &str) -> ParsedAnswer {
    if !seg.chars().all(|c| c.is_ascii_digit()) {
        // 夹杂其他字符（如“322分”）：无法可靠提取，不猜。
        return ParsedAnswer::NotANumber;
    }
    if seg.len() > 9 {
        return ParsedAnswer::NotANumber;
    }
    match seg.parse::<i64>() {
        Ok(v) => ParsedAnswer::Number(v),
        Err(_) => ParsedAnswer::NotANumber,
    }
}

/// 逐位读法：「三二二」→ 322、「二零四零」→ 2040。
fn parse_digitwise(seg: &str) -> ParsedAnswer {
    let chars: Vec<char> = seg.chars().collect();
    if chars.len() > 8 {
        return ParsedAnswer::NotANumber;
    }
    let mut value: i64 = 0;
    for &c in &chars {
        let Some(d) = digit_value(c) else {
            // 调用方已做全字符校验；此处兜底防御，绝不 panic（ASR 文本不可信）。
            return ParsedAnswer::NotANumber;
        };
        value = value * 10 + d as i64;
    }
    ParsedAnswer::Number(value)
}

/// 位值读法：「三百二十二」「两千零四十」，带严格语法校验。
/// 末位数字直接紧跟 百/千/万（中间无零、无十）→ 省略读法歧义（§8.3）。
fn parse_place_value(seg: &str) -> ParsedAnswer {
    let chars: Vec<char> = seg.chars().collect();

    let mut total: i64 = 0; // 已结算的万以上部分
    let mut current: i64 = 0; // 当前万段累计
    let mut pending: Option<i64> = None; // 待挂到单位的数字
    let mut prev_rank: u8 = 0; // 当前万段内已用单位的最高位级：十=1 百=2 千=3
    let mut wan_seen = false;

    for &c in &chars {
        if let Some(d) = digit_value(c) {
            match pending {
                // 「零X」占位继续；非零数字连排为非法语法（交给逐位或判重说）。
                Some(0) => pending = Some(d as i64), // 「零X」占位
                Some(_) => return ParsedAnswer::NotANumber,
                None => pending = Some(d as i64),
            }
        } else if c == '万' {
            if wan_seen {
                return ParsedAnswer::NotANumber; // 万至多一次
            }
            let d = match pending.take() {
                Some(d) if d > 0 => d,
                _ => return ParsedAnswer::NotANumber, // 「万」前缺有效数字
            };
            current = (current + d) * 10_000;
            total += current;
            current = 0;
            prev_rank = 0;
            wan_seen = true;
        } else {
            // 此时剩余单位字符只会是 十/百/千（万已在前一分支处理）。
            let (rank, base): (u8, i64) = match c {
                '十' => (1, 10),
                '百' => (2, 100),
                _ => (3, 1000), // '千'
            };
            let d = match pending.take() {
                Some(d) => d,
                // 「十二」的「十」可省略前导一；「百」「千」开头非法。
                None if c == '十' && current == 0 && total == 0 => 1,
                None => return ParsedAnswer::NotANumber,
            };
            if d == 0 {
                return ParsedAnswer::NotANumber; // 「零十」非法
            }
            // 单位必须逐级下降（千→百→十）；首个单位不受限。
            if prev_rank != 0 && rank >= prev_rank {
                return ParsedAnswer::NotANumber; // 「二十百」「十百」等乱序
            }
            current += d * base;
            prev_rank = rank;
        }
    }

    match pending {
        None => ParsedAnswer::Number(total + current), // 「三百」「三千」「二万三千」
        Some(0) => ParsedAnswer::NotANumber, // 「三百零」不完整
        Some(d) => {
            // 省略读法检测：末位数字直接跟在 百/千/万 后 → 歧义；
            // 「三百二十二」末位前是「十」→ 合法；「一千零一」末位前是「零」→ 合法。
            let before_last = chars[chars.len() - 2];
            match before_last {
                '百' | '千' | '万' => ParsedAnswer::Ambiguous(AmbiguityReason::OmittedUnit),
                _ => ParsedAnswer::Number(total + current + d),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(text: &str) -> i64 {
        match parse_answer(text) {
            ParsedAnswer::Number(v) => v,
            other => panic!("{text:?} 应解析为整数，实际 {other:?}"),
        }
    }

    fn amb(text: &str, reason: AmbiguityReason) {
        assert_eq!(
            parse_answer(text),
            ParsedAnswer::Ambiguous(reason),
            "{text:?}"
        );
    }

    fn nan(text: &str) {
        assert_eq!(parse_answer(text), ParsedAnswer::NotANumber, "{text:?}");
    }

    #[test]
    fn place_value_readings() {
        assert_eq!(n("三百二十二"), 322);
        assert_eq!(n("九千八百零一"), 9801);
        assert_eq!(n("两千零四十"), 2040);
        assert_eq!(n("二千零四"), 2004);
        assert_eq!(n("一百"), 100);
        assert_eq!(n("十五"), 15);
        assert_eq!(n("十"), 10);
        assert_eq!(n("三千"), 3000);
        assert_eq!(n("一万"), 10000); // 超出答案范围仍解析（§8.2）
        assert_eq!(n("二万三千"), 23000);
        assert_eq!(n("二百二十二"), 222);
        assert_eq!(n("两百二十二"), 222); // 「两」归一
        assert_eq!(n("九十九"), 99);
        assert_eq!(n("零"), 0);
    }

    #[test]
    fn digitwise_readings() {
        assert_eq!(n("三二二"), 322);
        assert_eq!(n("二零四零"), 2040);
        assert_eq!(n("三二零"), 320);
        assert_eq!(n("九八零一"), 9801);
        assert_eq!(n("五"), 5);
        assert_eq!(n("一零零"), 100);
    }

    #[test]
    fn arabic_digits_and_prefixes() {
        assert_eq!(n("322"), 322);
        assert_eq!(n("答案是三百二十二"), 322);
        assert_eq!(n("答案是322"), 322);
        assert_eq!(n("就是九十九"), 99);
        assert_eq!(n("我说是二百零五"), 205);
        assert_eq!(n("等于三百"), 300);
        assert_eq!(n("０４５"), 45); // 全角数字
    }

    #[test]
    fn with_zero_placeholder() {
        assert_eq!(n("一千零一"), 1001); // 不得误判为省略读法
        assert_eq!(n("三百零二"), 302);
        assert_eq!(n("二万零一"), 20001);
    }

    #[test]
    fn ambiguous_omitted_units() {
        amb("三百二", AmbiguityReason::OmittedUnit);
        amb("一千二", AmbiguityReason::OmittedUnit);
        amb("一万二", AmbiguityReason::OmittedUnit);
        amb("二百五", AmbiguityReason::OmittedUnit);
        amb("三千五", AmbiguityReason::OmittedUnit);
    }

    #[test]
    fn ambiguous_correction_and_multiple() {
        // 方案 §7.3 示例：不静默选择修正后的数字
        amb("三百二十不对三百二十二", AmbiguityReason::Correction);
        amb("三百二十……不对，三百二十二", AmbiguityReason::Correction);
        amb("错了，是四百", AmbiguityReason::Correction);
        amb("322 333", AmbiguityReason::MultipleNumbers);
        amb("三百二十二，三百二十三", AmbiguityReason::MultipleNumbers);
    }

    #[test]
    fn ambiguous_mixed_format() {
        amb("3百2", AmbiguityReason::MixedFormat);
        amb("3百二十二", AmbiguityReason::MixedFormat);
        amb("3二2", AmbiguityReason::MixedFormat);
    }

    #[test]
    fn not_a_number_cases() {
        nan(""); // 静音/空输入
        nan("   ");
        nan("苹果");
        nan("不知道"); // 不判错（§11.1）
        nan("嗯嗯");
        nan("不对"); // 修正词但无数字
        nan("三百零"); // 不完整语法
        nan("零十"); // 非法语序
        nan("二十百"); // 单位乱序
        nan("十百"); // 单位乱序
        nan("二三十"); // 范围表达，非单一整数
        nan("三五百"); // 乱序
        nan("三百二十二分"); // 夹杂其他字，不猜
        nan("1234567890123"); // 超长
        nan("一万万"); // 万重复
        // 实机 ASR 真实输出的语气词组合（曾触发 panic 的回归用例）
        nan("嗯，六吗？");
        nan("六吗");
        nan("九吗");
        nan("三吗四"); // 垃圾字符曾可能被当「千」静默产出 3004
        nan("十二吗");
        nan("两千吗零五");
    }

    /// parse_answer 必须是全函数：任何输入都不 panic（ASR 文本不可信）。
    #[test]
    fn never_panics_on_arbitrary_input() {
        let alphabet: Vec<char> = "一二三四五六七八九十百千万零两0123456789吗呢诶？，。！啊x×= ?322"
            .chars()
            .collect();
        let mut rng = crate::rng::Rng::new(2026);
        for _ in 0..3000 {
            let len = (rng.next_u64() % 10) + 1;
            let s: String = (0..len)
                .map(|_| alphabet[(rng.next_u64() as usize) % alphabet.len()])
                .collect();
            let _ = parse_answer(&s); // 只要不 panic 就行
        }
    }

    #[test]
    fn filler_removal_does_not_corrupt_numbers() {
        assert_eq!(n("嗯 三百二十二"), 322);
        assert_eq!(n("那个……三百二十二吧"), 322);
        assert_eq!(n("三百二十二了"), 322);
        assert_eq!(n("答案 322"), 322);
    }

    #[test]
    fn telephony_yao_is_one() {
        // 真机 ASR 实测：口语「幺」（1）被转写为汉字「幺」（平方特训 41²/56² 场景）
        assert_eq!(n("三幺三六"), 3136);
        assert_eq!(n("一六八幺"), 1681);
        assert_eq!(n("幺零二"), 102);
        assert_eq!(n("两幺五"), 215);
        // 「幺」不出现在其他数字词中，归一无副作用
        assert_eq!(n("幺"), 1);
    }

    #[test]
    fn out_of_range_still_a_number() {
        // §8.2：说出的有效整数即使超出本题结果范围也正常判错，不当识别失败
        assert_eq!(n("九十九"), 99);
        assert_eq!(n("99999"), 99999);
        assert_eq!(n("九万九千九百九十九"), 99999);
    }
}
