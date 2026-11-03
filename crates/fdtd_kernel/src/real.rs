//! Field precision. Single precision is the normal choice, twice the speed and half the memory;
//! the engine is generic so a long ring-down can be checked against double precision.

use std::ops::{Add, AddAssign, Mul, Sub};

pub trait Real: Copy + Send + Sync + Default + PartialOrd + Add<Output = Self> + Sub<Output = Self> + Mul<Output = Self> + AddAssign + 'static {
    fn of(v: f64) -> Self;
    fn f(self) -> f64;
}

impl Real for f32 {
    #[inline(always)]
    fn of(v: f64) -> Self {
        v as f32
    }
    #[inline(always)]
    fn f(self) -> f64 {
        self as f64
    }
}

impl Real for f64 {
    #[inline(always)]
    fn of(v: f64) -> Self {
        v
    }
    #[inline(always)]
    fn f(self) -> f64 {
        self
    }
}
