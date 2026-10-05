//! 一批多图 OCR：有限并发调用 [`LocalOcr`]，结果按输入下标汇总。
//!
//! 真实模式的 `HttpOcr` 是一次 HTTP 一张图；列表多框 / 气泡多框若串行会把
//! 延迟线性放大。这里用 `std::thread::scope` + 工作队列，把飞行中的识别数
//! 卡在 [`MAX_OCR_CONCURRENCY`]。
//!
//! 单张图请继续走 [`LocalOcr::recognize`] / `recognize_with_raw`——不必绕这里。
//!
//! ## 线程安全
//!
//! [`LocalOcr`] 已约束 `Send + Sync`；`HttpOcr`（每次独立 `ureq::post`）与
//! `ExternalOcr`（每次新起子进程）都可以并发。脚本化替身若按**调用次序**
//! 吐结果，并发下次序不保证——那是替身契约，不是本批处理的保证。

use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use crate::ports::{AutomationError, LocalOcr, Screenshot, TextBox};

/// 同一时刻最多允许多少路 OCR 在飞。
pub const MAX_OCR_CONCURRENCY: usize = 10;

/// 对多张图并发 `recognize_with_raw`，返回与 `images` **等长、同序**的结果。
///
/// - 空输入 → 空 Vec
/// - 仅 1 张 → 当前线程直接调，不启线程
/// - 多张 → 最多 [`MAX_OCR_CONCURRENCY`] 个 worker 抢活
pub fn recognize_many_with_raw(
    ocr: &dyn LocalOcr,
    images: &[Screenshot],
) -> Vec<Result<(Vec<TextBox>, String), AutomationError>> {
    let n = images.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![ocr.recognize_with_raw(&images[0])];
    }

    let (job_tx, job_rx) = mpsc::channel::<usize>();
    for i in 0..n {
        job_tx.send(i).expect("ocr batch job channel open");
    }
    drop(job_tx);

    let job_rx = Arc::new(Mutex::new(job_rx));
    let slots: Vec<Option<Result<(Vec<TextBox>, String), AutomationError>>> =
        (0..n).map(|_| None).collect();
    // 用裸指针式索引写入：scope 内共享一块 Vec，靠 Mutex 护。
    let slots = Arc::new(Mutex::new(slots));

    let workers = n.min(MAX_OCR_CONCURRENCY);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let job_rx = Arc::clone(&job_rx);
            let slots = Arc::clone(&slots);
            scope.spawn(move || loop {
                let idx = {
                    let rx = job_rx.lock().unwrap_or_else(|e| e.into_inner());
                    match rx.recv() {
                        Ok(i) => i,
                        Err(_) => break,
                    }
                };
                let outcome = ocr.recognize_with_raw(&images[idx]);
                let mut guard = slots.lock().unwrap_or_else(|e| e.into_inner());
                guard[idx] = Some(outcome);
            });
        }
    });

    let slots = Arc::into_inner(slots)
        .expect("ocr batch: all workers joined")
        .into_inner()
        .unwrap_or_else(|e| e.into_inner());
    slots
        .into_iter()
        .map(|slot| slot.expect("ocr batch: every index filled"))
        .collect()
}

/// 与 [`recognize_many_with_raw`] 相同，但每个下标在可重试错误上按 `attempts` /
/// `backoff` 重试（语义对齐编排器的 `with_retry`：只重试 Platform / Timeout）。
pub fn recognize_many_with_raw_retrying(
    ocr: &dyn LocalOcr,
    images: &[Screenshot],
    attempts: u32,
    backoff: Duration,
) -> Vec<Result<(Vec<TextBox>, String), AutomationError>> {
    let attempts = attempts.max(1);
    let n = images.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![recognize_one_with_retry(ocr, &images[0], attempts, backoff)];
    }

    let (job_tx, job_rx) = mpsc::channel::<usize>();
    for i in 0..n {
        job_tx.send(i).expect("ocr batch job channel open");
    }
    drop(job_tx);

    let job_rx = Arc::new(Mutex::new(job_rx));
    let slots: Vec<Option<Result<(Vec<TextBox>, String), AutomationError>>> =
        (0..n).map(|_| None).collect();
    let slots = Arc::new(Mutex::new(slots));

    let workers = n.min(MAX_OCR_CONCURRENCY);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let job_rx = Arc::clone(&job_rx);
            let slots = Arc::clone(&slots);
            scope.spawn(move || loop {
                let idx = {
                    let rx = job_rx.lock().unwrap_or_else(|e| e.into_inner());
                    match rx.recv() {
                        Ok(i) => i,
                        Err(_) => break,
                    }
                };
                let outcome = recognize_one_with_retry(ocr, &images[idx], attempts, backoff);
                let mut guard = slots.lock().unwrap_or_else(|e| e.into_inner());
                guard[idx] = Some(outcome);
            });
        }
    });

    let slots = Arc::into_inner(slots)
        .expect("ocr batch: all workers joined")
        .into_inner()
        .unwrap_or_else(|e| e.into_inner());
    slots
        .into_iter()
        .map(|slot| slot.expect("ocr batch: every index filled"))
        .collect()
}

fn recognize_one_with_retry(
    ocr: &dyn LocalOcr,
    image: &Screenshot,
    attempts: u32,
    backoff: Duration,
) -> Result<(Vec<TextBox>, String), AutomationError> {
    let mut last_err: Option<AutomationError> = None;
    for attempt in 1..=attempts {
        match ocr.recognize_with_raw(image) {
            Ok(v) => return Ok(v),
            Err(err) => {
                let retryable = err.is_retryable() && attempt < attempts;
                last_err = Some(err);
                if !retryable {
                    break;
                }
                if !backoff.is_zero() {
                    std::thread::sleep(backoff);
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| {
        AutomationError::Platform("OCR 失败且没有返回错误信息".into())
    }))
}

/// 只要文字框、不要引擎原文时用这个。
pub fn recognize_many(
    ocr: &dyn LocalOcr,
    images: &[Screenshot],
) -> Vec<Result<Vec<TextBox>, AutomationError>> {
    recognize_many_with_raw(ocr, images)
        .into_iter()
        .map(|r| r.map(|(boxes, _)| boxes))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, SystemTime};

    struct SlowOcr {
        inflight: AtomicUsize,
        peak: AtomicUsize,
    }

    impl LocalOcr for SlowOcr {
        fn recognize(&self, image: &Screenshot) -> Result<Vec<TextBox>, AutomationError> {
            Ok(self.recognize_with_raw(image)?.0)
        }

        fn recognize_with_raw(
            &self,
            image: &Screenshot,
        ) -> Result<(Vec<TextBox>, String), AutomationError> {
            let now = self.inflight.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(30));
            self.inflight.fetch_sub(1, Ordering::SeqCst);
            Ok((
                vec![TextBox {
                    text: format!("{}x{}", image.width, image.height),
                    bounds: crate::ports::Rect {
                        x: 0,
                        y: 0,
                        width: image.width as i32,
                        height: image.height as i32,
                    },
                    confidence: 1.0,
                }],
                format!("raw-{}", image.fingerprint),
            ))
        }
    }

    fn shot(fp: &str, w: u32) -> Screenshot {
        Screenshot {
            pixels: vec![0; (w * w * 4) as usize],
            width: w,
            height: w,
            captured_at: SystemTime::UNIX_EPOCH,
            fingerprint: fp.into(),
        }
    }

    #[test]
    fn empty_and_single_skip_the_pool() {
        let ocr = SlowOcr {
            inflight: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        };
        assert!(recognize_many_with_raw(&ocr, &[]).is_empty());
        let one = recognize_many_with_raw(&ocr, &[shot("a", 1)]);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].as_ref().unwrap().1, "raw-a");
    }

    #[test]
    fn results_keep_input_order_and_cap_inflight() {
        let ocr = SlowOcr {
            inflight: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        };
        // 15 张 > 上限 10，峰值不得越过 10；结果按指纹序。
        let images: Vec<_> = (0..15).map(|i| shot(&format!("f{i}"), (i + 1) as u32)).collect();
        let out = recognize_many_with_raw(&ocr, &images);
        assert_eq!(out.len(), 15);
        for (i, item) in out.iter().enumerate() {
            let (boxes, raw) = item.as_ref().unwrap();
            assert_eq!(raw, &format!("raw-f{i}"));
            assert_eq!(boxes[0].text, format!("{}x{}", i + 1, i + 1));
        }
        let peak = ocr.peak.load(Ordering::SeqCst);
        assert!(
            peak <= MAX_OCR_CONCURRENCY,
            "peak concurrency {peak} exceeded {}",
            MAX_OCR_CONCURRENCY
        );
        assert!(peak >= 2, "expected some parallelism, peak={peak}");
    }
}
