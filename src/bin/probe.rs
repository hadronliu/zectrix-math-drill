//! 设备探针（调试工具，非业务代码）：
//! 打印设备身份 → 常驻连接推卡并计时 → 保持连接观察画面是否持续显示。
//!
//! 用法: probe <IP> <png路径> [保持秒数，默认60]

use std::time::{Duration, Instant};
use zectrix_sdk::{Device, ImageFormat, OpenOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        return Err("用法: probe <IP> <png路径> [保持秒数]".into());
    }
    let ip = args[1].clone();
    let png = std::fs::read(&args[2])?;
    let hold = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(60u64);

    let t0 = Instant::now();
    let device = Device::open(OpenOptions {
        target: Some(ip),
        client_name: Some("math-drill probe".into()),
    })
    .await?;
    println!("[probe] 连接+PALETTE 耗时: {:.2?}", t0.elapsed());
    println!("[probe] 设备身份: {:#?}", device.summary);

    let t1 = Instant::now();
    device.show_with_format(&png, ImageFormat::Gray16).await?;
    println!("[probe] 显示完成耗时（常驻连接）: {:.2?}", t1.elapsed());

    println!("[probe] 连接保持 {hold}s，请观察设备屏幕是否显示卡片…");
    for left in (1..=hold).rev().step_by(10) {
        tokio::time::sleep(Duration::from_secs(10)).await;
        println!("[probe] 剩余 {left}s");
    }
    device.close().await?;
    println!("[probe] 已断开。若断开后画面变化，请记录当时屏幕内容。");
    Ok(())
}
