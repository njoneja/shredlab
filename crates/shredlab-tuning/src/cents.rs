// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Cents arithmetic and the weighted circular mean over a 100-cent period.

use std::f64::consts::TAU;

/// Cents of `hz` relative to A440 (not folded).
pub fn cents_from_a440(hz: f64) -> f64 {
    1200.0 * (hz / 440.0).log2()
}

/// Folds cents into −50..+50.
pub fn fold(cents: f64) -> f64 {
    cents - 100.0 * (cents / 100.0).round()
}

/// Weighted sum of unit vectors at angle `2π·cents/100`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Circular {
    pub x: f64,
    pub y: f64,
    pub weight: f64,
    pub count: u32,
}

impl Circular {
    pub fn add(&mut self, cents: f64, weight: f64) {
        let theta = TAU * cents / 100.0;
        self.x += weight * theta.cos();
        self.y += weight * theta.sin();
        self.weight += weight;
        self.count += 1;
    }

    /// Adds another sum scaled by `scale` (stem weights).
    pub fn merge(&mut self, other: &Circular, scale: f64) {
        self.x += scale * other.x;
        self.y += scale * other.y;
        self.weight += scale * other.weight;
        self.count += other.count;
    }

    /// Mean offset in −50..+50 cents.
    pub fn mean_cents(&self) -> f64 {
        self.y.atan2(self.x) * 100.0 / TAU
    }

    /// Resultant length, 0..1 (0 when empty).
    pub fn resultant(&self) -> f64 {
        if self.weight > 0.0 {
            (self.x.hypot(self.y) / self.weight).min(1.0)
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_wraps_to_half_semitone() {
        assert!((fold(130.0) - 30.0).abs() < 1e-9);
        assert!((fold(-170.0) - 30.0).abs() < 1e-9);
        assert!((fold(49.0) - 49.0).abs() < 1e-9);
    }

    #[test]
    fn circular_mean_handles_the_wrap() {
        let mut c = Circular::default();
        c.add(48.0, 1.0);
        c.add(-48.0, 1.0);
        assert!(fold(c.mean_cents() - 50.0).abs() < 1e-6);
        assert!(c.resultant() > 0.99);
    }
}
