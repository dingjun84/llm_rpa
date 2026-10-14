//! Cubic Bezier path + ±127 relative chunking (no OS deps).

use crate::protocol::{AXIS_MAX, AXIS_MIN};

/// One absolute screen sample along a planned path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathPoint {
    pub x: i32,
    pub y: i32,
}

/// Split a relative delta into HID-legal chunks of ±127.
pub fn chunk_relative(mut dx: i32, mut dy: i32) -> Vec<(i8, i8)> {
    let mut out = Vec::new();
    while dx != 0 || dy != 0 {
        let sx = dx.clamp(AXIS_MIN, AXIS_MAX);
        let sy = dy.clamp(AXIS_MIN, AXIS_MAX);
        out.push((sx as i8, sy as i8));
        dx -= sx;
        dy -= sy;
    }
    out
}

/// Ease in-out (smoothstep) for t in [0,1].
pub fn ease_in_out(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Cubic Bezier: B(t) = (1-t)³P0 + 3(1-t)²t P1 + 3(1-t)t² P2 + t³P3
pub fn cubic_bezier(p0: (f64, f64), p1: (f64, f64), p2: (f64, f64), p3: (f64, f64), t: f64) -> (f64, f64) {
    let u = 1.0 - t;
    let uu = u * u;
    let uuu = uu * u;
    let tt = t * t;
    let ttt = tt * t;
    let x = uuu * p0.0 + 3.0 * uu * t * p1.0 + 3.0 * u * tt * p2.0 + ttt * p3.0;
    let y = uuu * p0.1 + 3.0 * uu * t * p1.1 + 3.0 * u * tt * p2.1 + ttt * p3.1;
    (x, y)
}

/// Build control points with a mild lateral offset (human-like arc).
///
/// `jitter01` in [0,1) picks offset sign/magnitude deterministically when seeded.
pub fn control_points(
    start: (i32, i32),
    end: (i32, i32),
    jitter01: f64,
) -> ((f64, f64), (f64, f64)) {
    let (x0, y0) = (start.0 as f64, start.1 as f64);
    let (x3, y3) = (end.0 as f64, end.1 as f64);
    let dx = x3 - x0;
    let dy = y3 - y0;
    let dist = (dx * dx + dy * dy).sqrt().max(1.0);
    // Perpendicular unit vector.
    let px = -dy / dist;
    let py = dx / dist;
    let amp = (dist * 0.18).clamp(8.0, 80.0);
    let j = (jitter01 * 2.0 - 1.0).clamp(-1.0, 1.0);
    let o1 = amp * (0.55 + 0.35 * j);
    let o2 = amp * (-0.45 + 0.25 * j);
    let p1 = (x0 + dx * 0.30 + px * o1, y0 + dy * 0.30 + py * o1);
    let p2 = (x0 + dx * 0.70 + px * o2, y0 + dy * 0.70 + py * o2);
    (p1, p2)
}

/// Number of path samples from distance (pixels).
pub fn sample_count(distance_px: f64) -> usize {
    let n = 20.0 + distance_px / 18.0;
    n.round().clamp(20.0, 60.0) as usize
}

/// Plan absolute screen points from start→end along a cubic Bezier with ease-in-out.
pub fn plan_path(start: (i32, i32), end: (i32, i32), jitter01: f64) -> Vec<PathPoint> {
    let dist = {
        let dx = (end.0 - start.0) as f64;
        let dy = (end.1 - start.1) as f64;
        (dx * dx + dy * dy).sqrt()
    };
    if dist < 1.0 {
        return vec![PathPoint { x: end.0, y: end.1 }];
    }
    let (p1, p2) = control_points(start, end, jitter01);
    let p0 = (start.0 as f64, start.1 as f64);
    let p3 = (end.0 as f64, end.1 as f64);
    let n = sample_count(dist);
    let mut pts = Vec::with_capacity(n);
    for i in 1..=n {
        let t = ease_in_out(i as f64 / n as f64);
        let (x, y) = cubic_bezier(p0, p1, p2, p3, t);
        pts.push(PathPoint {
            x: x.round() as i32,
            y: y.round() as i32,
        });
    }
    // Ensure last sample is exact end.
    if let Some(last) = pts.last_mut() {
        *last = PathPoint { x: end.0, y: end.1 };
    }
    pts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_splits_large_delta() {
        let c = chunk_relative(300, -250);
        assert!(c.len() >= 3);
        let sx: i32 = c.iter().map(|(x, _)| *x as i32).sum();
        let sy: i32 = c.iter().map(|(_, y)| *y as i32).sum();
        assert_eq!((sx, sy), (300, -250));
        for (x, y) in c {
            assert!((-127..=127).contains(&(x as i32)));
            assert!((-127..=127).contains(&(y as i32)));
        }
    }

    #[test]
    fn plan_ends_at_target() {
        let pts = plan_path((10, 10), (400, 250), 0.3);
        assert!(pts.len() >= 20);
        let last = pts.last().unwrap();
        assert_eq!((last.x, last.y), (400, 250));
    }

    #[test]
    fn notches_wheel_dir() {
        assert_eq!(crate::protocol::notches_to_wheel_z(3), -3);
        assert_eq!(crate::protocol::notches_to_wheel_z(-2), 2);
        assert_eq!(crate::protocol::notches_to_wheel_z(200), -127);
    }
}
