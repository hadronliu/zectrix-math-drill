//! 生成 README 预览图：docs/previews/*.png（400×300 实际推送卡片）。
//! 无设备依赖；需要系统 CJK 字体（见 render.rs）。

use math_drill::render::Renderer;
use std::path::Path;

fn main() {
    let r = match Renderer::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("previews：{e}（设置 ZECTRIX_FONT 指向含中文的 TTF 后重试）");
            std::process::exit(1);
        }
    };
    let dir = Path::new("docs/previews");
    std::fs::create_dir_all(dir).expect("创建目录");

    let cards: Vec<(&str, math_drill::render::Canvas)> = vec![
        ("question.png", r.question_card(3, 10, 23, 14)),
        ("square.png", r.question_card(1, 10, 41, 41)),
        ("result_ok.png", r.result_card(true, 3136, 3136)),
        ("result_bad.png", r.result_card(false, 1746, 1764)),
        ("retry.png", r.retry_card("听到省略读法，请说完整")),
        ("summary.png", r.summary_card(9, 10, 0, 1)),
    ];
    for (name, card) in cards {
        let path = dir.join(name);
        card.save(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        println!("生成 {}", path.display());
    }
}
