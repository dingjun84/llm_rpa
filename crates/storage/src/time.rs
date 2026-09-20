//! `SystemTime` 与数据库整型时间戳之间的换算。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn to_unix_ms(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(delta) => delta.as_millis() as i64,
        // 早于纪元的时间按 0 处理，避免出现负数时间戳。
        Err(_) => 0,
    }
}

pub fn from_unix_ms(ms: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms.max(0) as u64)
}

pub fn now_unix_ms() -> i64 {
    to_unix_ms(SystemTime::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_timestamp() {
        let time = UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
        assert_eq!(to_unix_ms(from_unix_ms(to_unix_ms(time))), to_unix_ms(time));
    }

    #[test]
    fn clamps_negative_timestamps() {
        assert_eq!(from_unix_ms(-5), UNIX_EPOCH);
    }
}
