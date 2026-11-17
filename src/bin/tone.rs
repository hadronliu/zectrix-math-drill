//! 调试工具：按指定音量播放 440Hz 提示音（默认 100）。
//! 用法: tone <IP> [音量0-100]

use std::time::Duration;
use zectrix_sdk::{AudioClip, Device, OpenOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let ip = args.get(1).cloned().ok_or("用法: tone <IP> [音量]")?;
    let volume: u8 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(100);
    let device = Device::open(OpenOptions {
        target: Some(ip),
        client_name: Some("math-drill tone".into()),
    })
    .await?;
    // 0.6 秒 880Hz 短音，前后各 50ms 淡入淡出防爆音
    let rate = 16000usize;
    let n = rate * 6 / 10;
    let fade = rate / 20;
    let samples: Vec<i16> = (0..n)
        .map(|i| {
            let t = i as f32;
            let env = (i.min(n - i)).min(fade) as f32 / fade as f32;
            ((t * 880.0 * std::f32::consts::TAU / rate as f32).sin() * 12000.0 * env) as i16
        })
        .collect();
    let end = device.play(&AudioClip { samples }, volume).await?;
    println!("播放结果：{end:?}（音量 {volume}）");
    tokio::time::sleep(Duration::from_millis(100)).await;
    device.close().await?;
    Ok(())
}
