//! 命令行入口。用法见 `lib.rs` 顶部的文档。
//!
//! 参数解析**手写**：只有几个参数，为一个"读目录、打几行字"的小工具引
//! `clap` 不划算（`CONVENTIONS.md` §6：新依赖先问标准库）。

use std::path::PathBuf;
use std::process::ExitCode;

use replay::Engine;

const USAGE: &str = "\
用法：replay <任务目录> [--min-confidence 0.70] [--ocr <引擎路径|auto|none>] [--upscale 2]

  任务目录            data/tasks/<任务ID>/（里面要有 events.jsonl）
  --min-confidence    用这个阈值重跑所有判据；不写就用盘上各条自己记的值
  --ocr               真 OCR 模式：把 raw/ 下那张**未标注**的输入图喂回引擎再跑一遍，
                      由此说出卡在 OCR 层还是判据层。默认 auto（找 target/debug/macosocr
                      等位置）；也可以直接给引擎路径。--ocr none 关掉。
  --upscale           传给引擎的放大倍数（1~8）；不写就用引擎自己的默认值

例：
  cargo run -p replay -- data/tasks/task-a859ae74
  cargo run -p replay -- data/tasks/task-a859ae74 --min-confidence 0.70
  cargo run -p replay -- data/tasks/task-a859ae74 --upscale 3
";

/// `--upscale` 的合法范围。与 `tools/macosocr` 的 `MAX_UPSCALE` 一致：
/// 越界的值引擎会直接拒掉，在这里先拦一道，省得写成"读数变成了 0 块"。
const UPSCALE_RANGE: std::ops::RangeInclusive<f32> = 1.0..=8.0;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<String, String> {
    let mut dir: Option<PathBuf> = None;
    let mut min_confidence: Option<f32> = None;
    let mut ocr_arg: Option<String> = None;
    let mut upscale: Option<f32> = None;
    let mut index = 0;

    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "-h" | "--help" => return Ok(USAGE.to_string()),
            "--min-confidence" => {
                let raw = value(args, index, "--min-confidence")?;
                min_confidence = Some(
                    raw.parse::<f32>()
                        .map_err(|err| format!("--min-confidence 的值读不出来：{err}"))?,
                );
                index += 1;
            }
            "--ocr" => {
                ocr_arg = Some(value(args, index, "--ocr")?.to_string());
                index += 1;
            }
            "--upscale" => {
                let raw = value(args, index, "--upscale")?;
                let parsed = raw
                    .parse::<f32>()
                    .map_err(|err| format!("--upscale 的值读不出来：{err}"))?;
                if !UPSCALE_RANGE.contains(&parsed) {
                    return Err(format!(
                        "--upscale 只认 {}~{} 之间的数（引擎自己也只认这个范围），给的是 {parsed}",
                        UPSCALE_RANGE.start(),
                        UPSCALE_RANGE.end()
                    ));
                }
                upscale = Some(parsed);
                index += 1;
            }
            other if other.starts_with('-') && other != "-" => {
                return Err(format!("认不出的参数：{other}\n\n{USAGE}"));
            }
            other => {
                if dir.is_some() {
                    return Err(format!("只认一个任务目录，多出来的是：{other}\n\n{USAGE}"));
                }
                dir = Some(PathBuf::from(other));
            }
        }
        index += 1;
    }

    let dir = dir.ok_or_else(|| USAGE.to_string())?;
    let ocr = engine(ocr_arg.as_deref(), upscale)?;
    let replay = replay::replay_dir_with_ocr(&dir, min_confidence, ocr)?;
    Ok(replay::render(&dir, &replay, min_confidence))
}

/// 取值型参数的下一项。缺了就说清是哪个参数缺值。
fn value<'a>(args: &'a [String], index: usize, flag: &str) -> Result<&'a str, String> {
    args.get(index + 1)
        .map(String::as_str)
        .ok_or_else(|| format!("{flag} 后面要跟一个值"))
}

/// 这次用哪个引擎。
///
/// `auto`（默认）找不到引擎时**只提醒、不报错**：判据重跑本身仍然有用，
/// 只是"卡在哪一层"只能到判据层。提醒走 stderr，报告本身照常打出来。
fn engine(ocr_arg: Option<&str>, upscale: Option<f32>) -> Result<Option<Engine>, String> {
    match ocr_arg {
        // 明确了要关掉。
        Some("none") => Ok(None),
        // 给了路径就照用：路径不存在时起进程会失败，报告里会如实写"没跑成"，
        // 比在参数解析阶段拦下来更贴近"哪一步、哪张图出问题"。
        Some(path) if path != "auto" => Ok(Some(Engine::at(path).with_upscale(upscale))),
        // `auto` 与不写是同一件事：找本机已经编好的那个引擎。
        None | Some("auto") => match Engine::discover() {
            Ok(engine) => Ok(Some(engine.with_upscale(upscale))),
            Err(err) => {
                eprintln!("replay：{err}；这次只重跑判据（卡在哪一层只能到判据层）");
                Ok(None)
            }
        },
        Some(other) => Err(format!("--ocr 认不出：{other}\n\n{USAGE}")),
    }
}