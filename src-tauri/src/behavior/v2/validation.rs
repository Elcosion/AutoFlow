use crate::AppError;

pub const V2_MAX_COORDINATE_ABS: i32 = 1_000_000;
pub const V2_MAX_SESSION_EVENTS: usize = 250_000;
pub const V2_MAX_GENERATED_POINTS: usize = 128;
pub const V2_MAX_GENERATED_DELAY_MS: u64 = 5_000;

pub fn validate_coordinate(x: i32, y: i32) -> Result<(), AppError> {
    if i64::from(x).abs() > i64::from(V2_MAX_COORDINATE_ABS)
        || i64::from(y).abs() > i64::from(V2_MAX_COORDINATE_ABS)
    {
        return Err(AppError::invalid(
            "behavior_v2_coordinate_invalid",
            "坐标超出安全范围",
        ));
    }
    Ok(())
}

pub fn validate_finite(value: f32, field: &str) -> Result<(), AppError> {
    if !value.is_finite() {
        return Err(AppError::invalid(
            "behavior_v2_non_finite",
            format!("{field} 必须是有限数值"),
        ));
    }
    Ok(())
}

pub fn clamp_coordinate(value: f32) -> i32 {
    value.round().clamp(
        -(V2_MAX_COORDINATE_ABS as f32),
        V2_MAX_COORDINATE_ABS as f32,
    ) as i32
}
