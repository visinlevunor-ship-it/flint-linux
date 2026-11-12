//! The math functions Flint's programs use (awk), exported for C callers.

use crate::math;

#[no_mangle]
pub extern "C" fn sin(x: f64) -> f64 {
    math::sin(x)
}
#[no_mangle]
pub extern "C" fn cos(x: f64) -> f64 {
    math::cos(x)
}
#[no_mangle]
pub extern "C" fn exp(x: f64) -> f64 {
    math::exp(x)
}
#[no_mangle]
pub extern "C" fn log(x: f64) -> f64 {
    math::log(x)
}
#[no_mangle]
pub extern "C" fn pow(x: f64, y: f64) -> f64 {
    math::pow(x, y)
}
#[no_mangle]
pub extern "C" fn atan2(y: f64, x: f64) -> f64 {
    math::atan2(y, x)
}
#[no_mangle]
pub extern "C" fn atan(x: f64) -> f64 {
    math::atan(x)
}
