use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static RANDOM_SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct SeededRng {
    state: u64,
}

impl SeededRng {
    pub fn from_seed(seed: u64) -> Self {
        let mixed = seed ^ 0x9E37_79B9_7F4A_7C15;
        Self {
            state: if mixed == 0 { 1 } else { mixed },
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut value = self.state;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.state = value;
        value
    }

    pub fn next_unit(&mut self) -> f32 {
        (self.next_u64() as f64 / u64::MAX as f64) as f32
    }
}

pub fn random_seed() -> u64 {
    let time_seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let counter = RANDOM_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    mix_seed(time_seed, &[counter as i64])
}

pub fn mix_seed(seed: u64, values: &[i64]) -> u64 {
    values.iter().fold(seed, |state, value| {
        state.rotate_left(11).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ (*value as u64).wrapping_mul(0x1000_0001)
    })
}
