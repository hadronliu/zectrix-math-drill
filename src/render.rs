//! 400×300 卡片渲染（开发方案 §9.6 显示适配层的设备实现）。
//!
//! 为墨水屏优化的两个关键决策：
//! 1. **基线对齐**：同一行内所有字形共享基线（此前按包围盒顶对齐，中英混排上下错位）；
//! 2. **二值化输出（无抗锯齿）**：覆盖率达到阈值即落墨，画布只剩 0/255 两色。
//!    设备端 Floyd–Steinberg 抖色对二值图不产生噪斑，笔画边缘干净（16 灰实测）。
//!
//! 中文用系统 Arial Unicode.ttf；fontdue 栅格化，image 编码灰度 PNG。
//! SDK 侧只做色板量化，本层不做抖色（§4 API 约定）。

use fontdue::{Font, FontSettings};
use image::GrayImage;
use std::io::Cursor;
use std::path::Path;

/// 落墨阈值：覆盖率 ≥ 该值视为黑。取偏低值让笔画在墨水屏上略粗、更清晰。
const INK_THRESHOLD: u8 = 64;

const CANDIDATE_FONTS: [&str; 3] = [
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/System/Library/Fonts/Supplemental/Songti.ttc",
    "/Library/Fonts/Arial Unicode.ttf",
];

/// 解析字体路径：优先环境变量 ZECTRIX_FONT（自定义/跨平台部署），再退回候选列表。
fn resolve_font() -> Option<String> {
    if let Ok(p) = std::env::var("ZECTRIX_FONT") {
        if !p.trim().is_empty() && Path::new(&p).exists() {
            return Some(p);
        }
    }
    CANDIDATE_FONTS.iter().find(|p| Path::new(p).exists()).map(|p| p.to_string())
}

pub struct Renderer {
    font: Font,
}

/// 画布：灰度 255=白。
pub struct Canvas {
    pub w: u32,
    pub h: u32,
    px: Vec<u8>,
}

impl Canvas {
    pub fn new(w: u32, h: u32) -> Self {
        Canvas {
            w,
            h,
            px: vec![255u8; (w * h) as usize],
        }
    }

    /// 画一条水平细线（分隔用）。
    pub fn hline(&mut self, y: i32, x0: i32, x1: i32) {
        let (a, b) = (x0.min(x1).max(0), x1.max(x0).min(self.w as i32 - 1));
        if y < 0 || y >= self.h as i32 {
            return;
        }
        for x in a..=b {
            self.px[(y as u32 * self.w + x as u32) as usize] = 0;
        }
    }

    /// 以 **基线** 为基准画一行文本（`y` 为基线 y），返回文本宽度。
    pub fn text(&mut self, r: &Renderer, x: i32, baseline_y: i32, size: f32, s: &str) -> i32 {
        let mut cx = x;
        for ch in s.chars() {
            let (m, cov) = r.font.rasterize(ch, size);
            // fontdue 的 ymin/ymax 相对基线（向上为正）；位图第 0 行对应 bbox 顶 = 基线 - ymax
            let top = baseline_y - m.ymin - m.height as i32;
            for py in 0..m.height {
                for pxx in 0..m.width {
                    let a = cov[py * m.width + pxx];
                    if a < INK_THRESHOLD {
                        continue;
                    }
                    let dx = cx + m.xmin + pxx as i32;
                    let dy = top + py as i32;
                    if dx < 0 || dy < 0 || dx >= self.w as i32 || dy >= self.h as i32 {
                        continue;
                    }
                    self.px[(dy as u32 * self.w + dx as u32) as usize] = 0;
                }
            }
            cx += m.advance_width as i32;
        }
        cx - x
    }

    /// 基线居中排版：`baseline_y` 为该行基线，返回建议的下一行基线。
    pub fn text_centered(&mut self, r: &Renderer, baseline_y: i32, size: f32, s: &str) -> i32 {
        let w = r.measure(size, s);
        self.text(r, ((self.w as i32 - w) / 2).max(2), baseline_y, size, s);
        baseline_y + (size as i32 * 3 / 2)
    }

    /// 居中 + 超宽自动缩小（保留 16px 边距）。
    pub fn text_fit_centered(&mut self, r: &Renderer, baseline_y: i32, mut size: f32, s: &str) -> i32 {
        let max_w = self.w as i32 - 16;
        while size > 16.0 && r.measure(size, s) > max_w {
            size -= 2.0;
        }
        self.text_centered(r, baseline_y, size, s)
    }

    pub fn encode_png(&self) -> Result<Vec<u8>, String> {
        let img = GrayImage::from_raw(self.w, self.h, self.px.clone())
            .ok_or("画布尺寸不匹配")?;
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png)
            .map_err(|e| format!("PNG 编码失败：{e}"))?;
        Ok(out.into_inner())
    }

    /// 调试用：落盘查看。
    pub fn save(&self, path: &Path) -> Result<(), String> {
        std::fs::write(path, self.encode_png()?).map_err(|e| e.to_string())
    }
}

impl Renderer {
    pub fn new() -> Result<Self, String> {
        let Some(path) = resolve_font() else {
            return Err(format!(
                "未找到含中文的 TTF 字体；可设置 ZECTRIX_FONT 指向 .ttf 文件，或安装：{CANDIDATE_FONTS:?}"
            ));
        };
        let bytes = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
        let font = Font::from_bytes(bytes, FontSettings::default())
            .map_err(|e| format!("{path}: 字体解析失败 {e:?}（.ttc 字体集合不受支持，请改用单字体 .ttf）"))?;
        Ok(Renderer { font })
    }

    /// 测量一行文本宽度（不做断行）。
    pub fn measure(&self, size: f32, s: &str) -> i32 {
        s.chars()
            .map(|ch| self.font.metrics(ch, size).advance_width as i32)
            .sum()
    }

    /// 题目卡：小标题 / 大题面 / 底部提示。
    pub fn question_card(&self, seq: usize, total: usize, a: u32, b: u32) -> Canvas {
        let mut c = Canvas::new(400, 300);
        c.text_centered(self, 40, 26.0, &format!("第 {seq}/{total} 题"));
        c.hline(56, 24, 376);
        c.text_fit_centered(self, 178, 76.0, &format!("{a} × {b} = ?"));
        c.text_centered(self, 274, 22.0, "长按说话，松开提交");
        c
    }

    /// 判题结果卡：正确 / 错误两档版式。
    pub fn result_card(&self, correct: bool, parsed: i64, expected: i64) -> Canvas {
        let mut c = Canvas::new(400, 300);
        if correct {
            c.text_fit_centered(self, 172, 84.0, &format!("对！{parsed}"));
        } else {
            c.text_fit_centered(self, 118, 68.0, &format!("错，你说了 {parsed}"));
            c.text_fit_centered(self, 196, 40.0, &format!("正确答案 {expected}"));
        }
        c.text_centered(self, 274, 22.0, "短按确认，下一题");
        c
    }

    /// 重说提示卡（未识别/歧义/识别失败）。
    pub fn retry_card(&self, message: &str) -> Canvas {
        let mut c = Canvas::new(400, 300);
        c.text_fit_centered(self, 160, 34.0, message);
        c.text_centered(self, 274, 22.0, "长按说话，松开提交");
        c
    }

    /// 练习总结卡。
    pub fn summary_card(
        &self,
        first_correct: usize,
        answered: usize,
        corrections: usize,
        unparseable: usize,
    ) -> Canvas {
        let mut c = Canvas::new(400, 300);
        c.text_centered(self, 62, 40.0, "练习总结");
        c.hline(82, 24, 376);
        c.text_centered(self, 146, 32.0, &format!("首答正确 {first_correct}/{answered}"));
        c.text_centered(
            self,
            198,
            26.0,
            &format!("订正 {corrections} 次 · 未识别 {unparseable} 次"),
        );
        c.text_centered(self, 274, 22.0, "辛苦了！");
        c
    }

    /// 中止卡。
    pub fn aborted_card(&self) -> Canvas {
        let mut c = Canvas::new(400, 300);
        c.text_centered(self, 152, 46.0, "练习已中止");
        c.text_centered(self, 216, 26.0, "进度已保存");
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_all_cards_bilevel() {
        let r = match Renderer::new() {
            Ok(r) => r,
            // 无中文字体的环境（如部分 Linux CI）：跳过而非失败
            Err(e) => {
                eprintln!("跳过渲染测试：{e}");
                return;
            }
        };
        let dir = std::env::temp_dir().join("math-drill-render");
        std::fs::create_dir_all(&dir).unwrap();
        let cards = vec![
            ("question.png", r.question_card(3, 10, 23, 14)),
            ("result_ok.png", r.result_card(true, 322, 322)),
            ("result_bad.png", r.result_card(false, 99, 322)),
            ("retry.png", r.retry_card("听到省略读法，请说完整")),
            ("summary.png", r.summary_card(8, 10, 2, 1)),
            ("aborted.png", r.aborted_card()),
        ];
        for (name, card) in &cards {
            card.save(&dir.join(name)).unwrap();
            // 二值化保证：画布只能有 0/255 两种像素（过设备抖色器不生噪斑）
            assert!(
                card.px.iter().all(|&p| p == 0 || p == 255),
                "{name} 存在中间灰度"
            );
            // 尺寸头校验
            let data = std::fs::read(dir.join(name)).unwrap();
            assert_eq!(&data[12..16], b"IHDR");
            let w = (data[16] as u32) << 24 | (data[17] as u32) << 16 | (data[18] as u32) << 8 | data[19] as u32;
            let h = (data[20] as u32) << 24 | (data[21] as u32) << 16 | (data[22] as u32) << 8 | data[23] as u32;
            assert_eq!((w, h), (400, 300), "{name}");
        }
    }

    #[test]
    fn long_expression_auto_shrinks() {
        let Ok(r) = Renderer::new() else {
            eprintln!("跳过渲染测试：无可用字体");
            return;
        };
        // 97×97 是最宽题面，必须能放下
        let c = r.question_card(10, 10, 97, 97);
        let png = c.encode_png().unwrap();
        assert!(png.len() > 100);
    }
}
