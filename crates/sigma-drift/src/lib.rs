//! SigmaDrift motor-synergy trajectory generator.
//!
//! Faithful Rust port of `SigmaDrift/motor_synergy.h`: Fitts MT, sigma-lognormal
//! primary + corrections, curvature profile, OU Euler–Maruyama, tremor, SDN,
//! gamma-sampled dt, plus a WindMouse comparison implementation.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use rand_distr::{Distribution, Gamma, Normal};

/// One sample along a generated cursor path. `t` is milliseconds from start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrajectoryPoint {
    pub x: f64,
    pub y: f64,
    pub t: f64,
}

/// Tunable parameters for [`generate`]. Defaults match C++ `motor_synergy::config`.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub fitts_a: f64,
    pub fitts_b: f64,
    pub target_width: f64,

    pub undershoot_min: f64,
    pub undershoot_max: f64,
    pub peak_time_ratio: f64,
    pub primary_sigma_min: f64,
    pub primary_sigma_max: f64,

    pub overshoot_prob: f64,
    pub overshoot_min: f64,
    pub overshoot_max: f64,
    pub correction_sigma_min: f64,
    pub correction_sigma_max: f64,
    pub second_correction_prob: f64,

    pub curvature_scale: f64,

    pub ou_theta: f64,
    pub ou_sigma: f64,

    pub tremor_freq_min: f64,
    pub tremor_freq_max: f64,
    pub tremor_amp_min: f64,
    pub tremor_amp_max: f64,

    pub sdn_k: f64,

    pub sample_dt_mean: f64,
    pub gamma_shape: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            fitts_a: 50.0,
            fitts_b: 150.0,
            target_width: 20.0,

            undershoot_min: 0.92,
            undershoot_max: 0.97,
            peak_time_ratio: 0.35,
            primary_sigma_min: 0.18,
            primary_sigma_max: 0.28,

            overshoot_prob: 0.15,
            overshoot_min: 1.02,
            overshoot_max: 1.08,
            correction_sigma_min: 0.12,
            correction_sigma_max: 0.20,
            second_correction_prob: 0.25,

            curvature_scale: 0.025,

            ou_theta: 3.5,
            ou_sigma: 1.2,

            tremor_freq_min: 8.0,
            tremor_freq_max: 12.0,
            tremor_amp_min: 0.15,
            tremor_amp_max: 0.55,

            sdn_k: 0.04,

            sample_dt_mean: 7.8,
            gamma_shape: 3.5,
        }
    }
}

/// Aggregate path statistics (same fields as C++ `motor_synergy::metrics`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Metrics {
    pub movement_time: f64,
    pub path_length: f64,
    pub straight_distance: f64,
    pub path_efficiency: f64,
    pub peak_speed: f64,
    pub time_to_peak: f64,
    pub num_submovements: i32,
    pub endpoint_error: f64,
    pub fitts_predicted_mt: f64,
}

mod detail {
    /// Abramowitz & Stegun 7.1.26 approximation (max abs error ~1.5e-7).
    fn erf(x: f64) -> f64 {
        let sign = if x < 0.0 { -1.0 } else { 1.0 };
        let x = x.abs();
        let t = 1.0 / (1.0 + 0.3275911 * x);
        let y = 1.0
            - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t
                + 0.254829592)
                * t
                * (-x * x).exp();
        sign * y
    }

    pub fn normal_cdf(x: f64) -> f64 {
        0.5 * (1.0 + erf(x / std::f64::consts::SQRT_2))
    }

    pub fn lognormal_cdf(t: f64, t0: f64, mu: f64, sigma: f64) -> f64 {
        if t <= t0 {
            return 0.0;
        }
        normal_cdf(((t - t0).ln() - mu) / sigma)
    }

    pub fn lognormal_pdf(t: f64, t0: f64, mu: f64, sigma: f64) -> f64 {
        if t <= t0 {
            return 0.0;
        }
        let dt = t - t0;
        let z = (dt.ln() - mu) / sigma;
        1.0 / (sigma * (2.0 * std::f64::consts::PI).sqrt() * dt) * (-0.5 * z * z).exp()
    }

    /// `s^2*(1-s)^3` normalized to peak = 1.0 at `s = 0.4`.
    pub fn curvature_profile(s: f64) -> f64 {
        if s <= 0.0 || s >= 1.0 {
            return 0.0;
        }
        let v = s * s * (1.0 - s) * (1.0 - s) * (1.0 - s);
        const NORM: f64 = 0.4 * 0.4 * 0.6 * 0.6 * 0.6;
        v / NORM
    }

    /// Vertical movements produce more curvature due to wrist/forearm geometry.
    pub fn direction_factor(angle: f64) -> f64 {
        let sa = angle.sin().abs();
        let ca = angle.cos().abs();
        0.5 + 0.8 * sa - 0.15 * ca
    }
}

fn make_rng(seed: Option<u64>) -> StdRng {
    match seed {
        Some(s) if s != 0 => StdRng::seed_from_u64(s),
        _ => StdRng::from_entropy(),
    }
}

fn uniform(rng: &mut StdRng, lo: f64, hi: f64) -> f64 {
    rng.gen_range(lo..hi)
}

fn normal(rng: &mut StdRng, mean: f64, std_dev: f64) -> f64 {
    let dist = Normal::new(mean, std_dev).expect("valid normal params");
    dist.sample(rng)
}

fn gamma_sample(rng: &mut StdRng, shape: f64, scale: f64) -> f64 {
    let dist = Gamma::new(shape, scale).expect("valid gamma params");
    dist.sample(rng)
}

struct Correction {
    d: f64,
    t0: f64,
    mu: f64,
    sigma: f64,
    dir_x: f64,
    dir_y: f64,
}

/// Generate a sigma-lognormal motor-synergy trajectory from `(x0,y0)` to `(x1,y1)`.
///
/// If `seed` is `None` or `Some(0)`, OS entropy is used (same idea as
/// C++ `seed ? seed : std::random_device{}()`). A non-zero seed seeds `StdRng`.
pub fn generate(
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    cfg: &Config,
    seed: Option<u64>,
) -> Vec<TrajectoryPoint> {
    let mut rng = make_rng(seed);

    let dx = x1 - x0;
    let dy = y1 - y0;
    let distance = dx.hypot(dy);
    let direction = dy.atan2(dx);

    if distance < 1.0 {
        return vec![
            TrajectoryPoint {
                x: x0,
                y: y0,
                t: 0.0,
            },
            TrajectoryPoint {
                x: x1,
                y: y1,
                t: 50.0,
            },
        ];
    }

    let tx = dx / distance;
    let ty = dy / distance;
    let nx = -ty;
    let ny = tx;

    let id = (distance / cfg.target_width + 1.0).log2();
    let mut mt = (cfg.fitts_a + cfg.fitts_b * id) * (normal(&mut rng, 0.0, 0.08)).exp();
    mt = mt.max(80.0);

    let overshoot = uniform(&mut rng, 0.0, 1.0) < cfg.overshoot_prob;
    let reach = if overshoot {
        uniform(&mut rng, cfg.overshoot_min, cfg.overshoot_max)
    } else {
        uniform(&mut rng, cfg.undershoot_min, cfg.undershoot_max)
    };

    let primary_d = distance * reach;
    let primary_sigma = uniform(&mut rng, cfg.primary_sigma_min, cfg.primary_sigma_max);

    // mu from mode = exp(mu - sigma^2) so peak velocity lands at peak_t
    let peak_t = mt * uniform(
        &mut rng,
        cfg.peak_time_ratio - 0.03,
        cfg.peak_time_ratio + 0.03,
    );
    let primary_mu = peak_t.ln() + primary_sigma * primary_sigma;

    let mut corrections: Vec<Correction> = Vec::new();
    let remaining = distance - primary_d;
    if remaining.abs() > 0.5 {
        let dir = if remaining > 0.0 { 1.0 } else { -1.0 };
        let c_d = remaining.abs() * uniform(&mut rng, 0.88, 1.02);
        let c_s = uniform(
            &mut rng,
            cfg.correction_sigma_min,
            cfg.correction_sigma_max,
        );
        let c_peak = mt * uniform(&mut rng, 0.12, 0.18);
        corrections.push(Correction {
            d: c_d,
            t0: mt * uniform(&mut rng, 0.55, 0.68),
            mu: c_peak.ln() + c_s * c_s,
            sigma: c_s,
            dir_x: tx * dir,
            dir_y: ty * dir,
        });

        let left = remaining - c_d * dir;
        if left.abs() > 0.3 && uniform(&mut rng, 0.0, 1.0) < cfg.second_correction_prob {
            let d2 = if left > 0.0 { 1.0 } else { -1.0 };
            let c_d2 = left.abs() * uniform(&mut rng, 0.85, 1.05);
            let c_s2 = uniform(&mut rng, 0.10, 0.16);
            let c_p2 = mt * uniform(&mut rng, 0.08, 0.12);
            corrections.push(Correction {
                d: c_d2,
                t0: mt * uniform(&mut rng, 0.78, 0.88),
                mu: c_p2.ln() + c_s2 * c_s2,
                sigma: c_s2,
                dir_x: tx * d2,
                dir_y: ty * d2,
            });
        }
    }

    let curv_amp =
        distance * cfg.curvature_scale * detail::direction_factor(direction) * normal(&mut rng, 0.0, 1.0);

    let tremor_freq = uniform(&mut rng, cfg.tremor_freq_min, cfg.tremor_freq_max);
    let tremor_amp = uniform(&mut rng, cfg.tremor_amp_min, cfg.tremor_amp_max);
    let tph_x = uniform(&mut rng, 0.0, 2.0 * std::f64::consts::PI);
    let tph_y = uniform(&mut rng, 0.0, 2.0 * std::f64::consts::PI);
    let mut ou_x = 0.0;
    let mut ou_y = 0.0;

    let total_t = mt * 1.15;
    let g_scale = cfg.sample_dt_mean / cfg.gamma_shape;

    let mut times = vec![0.0];
    let mut t = 0.0;
    while t < total_t {
        let dt = gamma_sample(&mut rng, cfg.gamma_shape, g_scale).clamp(2.0, 25.0);
        t += dt;
        if t <= total_t + 15.0 {
            times.push(t);
        }
    }

    let mut result = Vec::with_capacity(times.len());
    for i in 0..times.len() {
        let t = times[i];
        let dt_ms = if i > 0 {
            t - times[i - 1]
        } else {
            cfg.sample_dt_mean
        };
        let dt_s = dt_ms / 1000.0;

        let s = detail::lognormal_cdf(t, 0.0, primary_mu, primary_sigma);

        let mut bx = x0 + tx * primary_d * s;
        let mut by = y0 + ty * primary_d * s;

        let curv = detail::curvature_profile(s);
        bx += nx * curv_amp * curv;
        by += ny * curv_amp * curv;

        for c in &corrections {
            let cs = detail::lognormal_cdf(t, c.t0, c.mu, c.sigma);
            bx += c.dir_x * c.d * cs;
            by += c.dir_y * c.d * cs;
        }

        let mut speed = primary_d * detail::lognormal_pdf(t, 0.0, primary_mu, primary_sigma);
        for c in &corrections {
            speed += c.d * detail::lognormal_pdf(t, c.t0, c.mu, c.sigma);
        }

        // Euler–Maruyama step for OU process (dt_s in seconds)
        ou_x += -cfg.ou_theta * ou_x * dt_s
            + cfg.ou_sigma * dt_s.sqrt() * normal(&mut rng, 0.0, 1.0);
        ou_y += -cfg.ou_theta * ou_y * dt_s
            + cfg.ou_sigma * dt_s.sqrt() * normal(&mut rng, 0.0, 1.0);

        // Tremor gain drops with speed (proprioceptive suppression)
        let t_s = t / 1000.0;
        let trem_mod = 1.0 / (1.0 + speed * 0.3);
        let tr_x = tremor_amp
            * trem_mod
            * (2.0 * std::f64::consts::PI * tremor_freq * t_s + tph_x).sin();
        let tr_y = tremor_amp
            * trem_mod
            * (2.0 * std::f64::consts::PI * tremor_freq * t_s + tph_y).sin();

        // Noise magnitude proportional to motor command (Harris–Wolpert SDN)
        let sdn_x = cfg.sdn_k * speed * normal(&mut rng, 0.0, 1.0);
        let sdn_y = cfg.sdn_k * speed * normal(&mut rng, 0.0, 1.0);

        result.push(TrajectoryPoint {
            x: bx + ou_x + tr_x + sdn_x,
            y: by + ou_y + tr_y + sdn_y,
            t,
        });
    }

    result
}

/// Compute path metrics against a known target (same as C++ `compute_metrics`).
pub fn compute_metrics(
    path: &[TrajectoryPoint],
    target_x: f64,
    target_y: f64,
    target_width: f64,
    straight_dist: f64,
) -> Metrics {
    let mut m = Metrics::default();
    if path.len() < 2 {
        return m;
    }

    m.movement_time = path.last().unwrap().t - path.first().unwrap().t;
    m.straight_distance = straight_dist;

    let mut max_speed = 0.0;
    m.path_length = 0.0;
    let mut speeds = vec![0.0; path.len()];

    for i in 1..path.len() {
        let dx = path[i].x - path[i - 1].x;
        let dy = path[i].y - path[i - 1].y;
        let dt = path[i].t - path[i - 1].t;
        let seg = dx.hypot(dy);
        m.path_length += seg;
        let spd = if dt > 0.0 { seg / dt } else { 0.0 };
        speeds[i] = spd;
        if spd > max_speed {
            max_speed = spd;
            m.time_to_peak = path[i].t;
        }
    }

    m.peak_speed = max_speed;
    m.path_efficiency = if m.path_length > 0.0 {
        m.straight_distance / m.path_length
    } else {
        1.0
    };

    // Peaks above 15% of max in the speed signal → sub-movement count
    let threshold = max_speed * 0.15;
    let mut peaks = 0;
    let n = speeds.len();
    if n >= 4 {
        for i in 2..(n - 1) {
            if speeds[i] > threshold && speeds[i] > speeds[i - 1] && speeds[i] > speeds[i + 1] {
                peaks += 1;
            }
        }
    }
    m.num_submovements = peaks.max(1);

    let last = path.last().unwrap();
    m.endpoint_error = (last.x - target_x).hypot(last.y - target_y);

    let id = (straight_dist / target_width + 1.0).log2();
    m.fitts_predicted_mt = 50.0 + 150.0 * id;

    m
}

/// WindMouse comparison implementation (same defaults as C++ `windmouse::generate`).
pub mod windmouse {
    use super::{make_rng, uniform, TrajectoryPoint};

    /// Generate a WindMouse-style trajectory using C++ defaults
    /// (`gravity=9`, `wind=3`, `max_step=15`, `target_area=8`).
    pub fn generate(
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
        seed: Option<u64>,
    ) -> Vec<TrajectoryPoint> {
        generate_with(x0, y0, x1, y1, 9.0, 3.0, 15.0, 8.0, seed)
    }

    /// WindMouse with explicit gravity / wind / step / target-area parameters.
    pub fn generate_with(
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
        gravity: f64,
        wind_str: f64,
        max_step: f64,
        target_area: f64,
        seed: Option<u64>,
    ) -> Vec<TrajectoryPoint> {
        let mut rng = make_rng(seed);

        let mut result = Vec::new();
        let mut xs = x0;
        let mut ys = y0;
        let mut vx = 0.0;
        let mut vy = 0.0;
        let mut wx = 0.0;
        let mut wy = 0.0;
        let mut t = 0.0;
        let mut step = max_step;

        result.push(TrajectoryPoint {
            x: xs,
            y: ys,
            t: 0.0,
        });

        for _iter in 0..5000 {
            let dist = (x1 - xs).hypot(y1 - ys);
            if dist < 1.0 {
                break;
            }

            let w = wind_str.min(dist);
            if dist >= target_area {
                wx = wx / 3.0_f64.sqrt() + uniform(&mut rng, -w, w) / 5.0_f64.sqrt();
                wy = wy / 3.0_f64.sqrt() + uniform(&mut rng, -w, w) / 5.0_f64.sqrt();
            } else {
                wx /= 3.0_f64.sqrt();
                wy /= 3.0_f64.sqrt();
                if step < 3.0 {
                    step = uniform(&mut rng, 3.0, 6.0);
                } else {
                    step /= 5.0_f64.sqrt();
                }
            }

            vx += wx + gravity * (x1 - xs) / dist;
            vy += wy + gravity * (y1 - ys) / dist;
            let vmag = vx.hypot(vy);
            if vmag > step {
                let r = step / 2.0 + uniform(&mut rng, 0.0, step / 2.0);
                vx = vx / vmag * r;
                vy = vy / vmag * r;
            }

            xs += vx;
            ys += vy;
            t += uniform(&mut rng, 5.0, 15.0);
            result.push(TrajectoryPoint { x: xs, y: ys, t });
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_path_length_gt_2() {
        let cfg = Config::default();
        let path = generate(100.0, 100.0, 500.0, 400.0, &cfg, Some(42));
        assert!(path.len() > 2, "path len = {}", path.len());
        assert!(path[0].t == 0.0);
        assert!(path.last().unwrap().t > path[0].t);
    }

    #[test]
    fn metrics_are_finite() {
        let cfg = Config::default();
        let x0 = 50.0;
        let y0 = 50.0;
        let x1 = 400.0;
        let y1 = 300.0;
        let path = generate(x0, y0, x1, y1, &cfg, Some(7));
        let dist = (x1 - x0).hypot(y1 - y0);
        let m = compute_metrics(&path, x1, y1, cfg.target_width, dist);
        assert!(m.movement_time.is_finite());
        assert!(m.path_length.is_finite() && m.path_length > 0.0);
        assert!(m.straight_distance.is_finite());
        assert!(m.path_efficiency.is_finite());
        assert!(m.peak_speed.is_finite());
        assert!(m.time_to_peak.is_finite());
        assert!(m.num_submovements >= 1);
        assert!(m.endpoint_error.is_finite());
        assert!(m.fitts_predicted_mt.is_finite());
    }

    #[test]
    fn windmouse_terminates() {
        let path = windmouse::generate(10.0, 10.0, 200.0, 150.0, Some(99));
        assert!(path.len() > 1);
        assert!(path.len() <= 5001);
        let last = path.last().unwrap();
        let dist = (200.0 - last.x).hypot(150.0 - last.y);
        assert!(
            dist < 50.0 || path.len() == 5001,
            "should approach target or hit iter cap; dist={dist}, len={}",
            path.len()
        );
    }

    #[test]
    fn short_distance_returns_two_points() {
        let cfg = Config::default();
        let path = generate(0.0, 0.0, 0.5, 0.0, &cfg, Some(1));
        assert_eq!(path.len(), 2);
        assert_eq!(path[1].t, 50.0);
    }

    #[test]
    fn seed_zero_and_none_do_not_panic() {
        let cfg = Config::default();
        let _ = generate(0.0, 0.0, 100.0, 100.0, &cfg, None);
        let _ = generate(0.0, 0.0, 100.0, 100.0, &cfg, Some(0));
    }
}
