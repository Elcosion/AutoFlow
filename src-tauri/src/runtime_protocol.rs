//! Versioned, length-delimited executor protocol. No transport trusts run IDs
//! alone: the receiver also binds every action to its private session secret.
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_PLAN_POINTS: usize = 4096;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunIdentity {
    pub session: String,
    pub run: u64,
    pub generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanPoint {
    pub x: i32,
    pub y: i32,
    pub delay_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutorAction {
    Move { x: i32, y: i32 },
    Key { virtual_key: u32, down: bool },
    Button { button: u8, down: bool },
    Scroll { x: i32, y: i32 },
    Text { text: String },
    PointerPlan { points: Vec<PlanPoint> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionEnvelope {
    pub version: u32,
    pub secret: String,
    pub identity: RunIdentity,
    pub sequence: u64,
    pub action: ExecutorAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolError {
    Io,
    FrameTooLarge,
    InvalidJson,
    Version,
    Unauthorized,
    StaleRun,
    Sequence,
    InvalidAction,
    Revoked,
}

pub fn read_frame<T: for<'de> Deserialize<'de>>(
    reader: &mut impl Read,
) -> Result<T, ProtocolError> {
    let mut header = [0; 4];
    reader
        .read_exact(&mut header)
        .map_err(|_| ProtocolError::Io)?;
    let length = u32::from_le_bytes(header) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge);
    }
    let mut bytes = vec![0; length];
    reader
        .read_exact(&mut bytes)
        .map_err(|_| ProtocolError::Io)?;
    serde_json::from_slice(&bytes).map_err(|_| ProtocolError::InvalidJson)
}

pub fn write_frame<T: Serialize>(writer: &mut impl Write, value: &T) -> Result<(), ProtocolError> {
    let bytes = serde_json::to_vec(value).map_err(|_| ProtocolError::InvalidJson)?;
    if bytes.is_empty() || bytes.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge);
    }
    writer
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .map_err(|_| ProtocolError::Io)?;
    writer.write_all(&bytes).map_err(|_| ProtocolError::Io)?;
    writer.flush().map_err(|_| ProtocolError::Io)
}

/// One receiver per run, owned exclusively by the safety broker. Invalid
/// messages never advance the sequence and cannot grant/recreate a permit.
pub struct ActionReceiver {
    identity: RunIdentity,
    secret: String,
    next_sequence: u64,
    revoked: bool,
}

impl ActionReceiver {
    pub fn new(identity: RunIdentity, secret: String) -> Result<Self, ProtocolError> {
        if identity.session.is_empty()
            || identity.session.len() > 128
            || identity.run == 0
            || secret.len() != 64
        {
            return Err(ProtocolError::Unauthorized);
        }
        Ok(Self {
            identity,
            secret,
            next_sequence: 1,
            revoked: false,
        })
    }

    pub fn revoke(&mut self) {
        self.revoked = true;
    }

    pub fn accept(&mut self, envelope: ActionEnvelope) -> Result<ExecutorAction, ProtocolError> {
        if self.revoked {
            return Err(ProtocolError::Revoked);
        }
        if envelope.version != PROTOCOL_VERSION {
            return Err(ProtocolError::Version);
        }
        let mut difference = self.secret.len() ^ envelope.secret.len();
        for (expected, received) in self.secret.bytes().zip(envelope.secret.bytes()) {
            difference |= usize::from(expected ^ received);
        }
        if difference != 0 {
            return Err(ProtocolError::Unauthorized);
        }
        if envelope.identity != self.identity {
            return Err(ProtocolError::StaleRun);
        }
        if envelope.sequence != self.next_sequence || self.next_sequence == u64::MAX {
            return Err(ProtocolError::Sequence);
        }
        validate_action(&envelope.action)?;
        self.next_sequence += 1;
        Ok(envelope.action)
    }
}

fn valid_position(x: i32, y: i32) -> bool {
    x.unsigned_abs() <= 100_000 && y.unsigned_abs() <= 100_000
}

fn validate_action(action: &ExecutorAction) -> Result<(), ProtocolError> {
    let valid = match action {
        ExecutorAction::Move { x, y } => valid_position(*x, *y),
        ExecutorAction::Key { virtual_key, .. } => (1..=254).contains(virtual_key),
        ExecutorAction::Button { button, .. } => (1..=5).contains(button),
        ExecutorAction::Scroll { x, y } => x.unsigned_abs() <= 12000 && y.unsigned_abs() <= 12000,
        ExecutorAction::Text { text } => text.len() <= 16_384 && !text.contains('\0'),
        ExecutorAction::PointerPlan { points } => {
            !points.is_empty()
                && points.len() <= MAX_PLAN_POINTS
                && points
                    .iter()
                    .all(|p| valid_position(p.x, p.y) && p.delay_ms <= 10_000)
                && points
                    .iter()
                    .try_fold(0u64, |sum, p| sum.checked_add(p.delay_ms))
                    .is_some_and(|sum| sum <= 60_000)
        }
    };
    if valid {
        Ok(())
    } else {
        Err(ProtocolError::InvalidAction)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn envelope() -> ActionEnvelope {
        ActionEnvelope {
            version: PROTOCOL_VERSION,
            secret: "a".repeat(64),
            identity: RunIdentity {
                session: "test-session".into(),
                run: 1,
                generation: 0,
            },
            sequence: 1,
            action: ExecutorAction::Move { x: -100, y: 50 },
        }
    }
    fn receiver() -> ActionReceiver {
        ActionReceiver::new(envelope().identity, envelope().secret).expect("receiver")
    }
    #[test]
    fn frames_round_trip_and_reject_oversize_before_reading_body() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &envelope()).expect("write");
        let decoded: ActionEnvelope = read_frame(&mut bytes.as_slice()).expect("read");
        assert_eq!(decoded, envelope());
        let oversized = (MAX_FRAME_BYTES as u32 + 1).to_le_bytes();
        assert_eq!(
            read_frame::<ActionEnvelope>(&mut oversized.as_slice()),
            Err(ProtocolError::FrameTooLarge)
        );
    }
    #[test]
    fn stale_duplicate_and_unauthenticated_actions_do_not_advance_receiver() {
        let mut receiver = receiver();
        let mut bad = envelope();
        bad.secret = "b".repeat(64);
        assert_eq!(receiver.accept(bad), Err(ProtocolError::Unauthorized));
        let mut stale = envelope();
        stale.identity.generation = 1;
        assert_eq!(receiver.accept(stale), Err(ProtocolError::StaleRun));
        assert!(receiver.accept(envelope()).is_ok());
        assert_eq!(receiver.accept(envelope()), Err(ProtocolError::Sequence));
        receiver.revoke();
        let mut late = envelope();
        late.sequence = 2;
        assert_eq!(receiver.accept(late), Err(ProtocolError::Revoked));
    }
    #[test]
    fn invalid_or_unbounded_plans_cannot_enter_broker() {
        let mut receiver = receiver();
        let mut bad = envelope();
        bad.action = ExecutorAction::PointerPlan {
            points: vec![PlanPoint {
                x: i32::MIN,
                y: 0,
                delay_ms: 0,
            }],
        };
        assert_eq!(receiver.accept(bad), Err(ProtocolError::InvalidAction));
        let mut oversized = envelope();
        oversized.action = ExecutorAction::PointerPlan {
            points: vec![
                PlanPoint {
                    x: 0,
                    y: 0,
                    delay_ms: 0
                };
                MAX_PLAN_POINTS + 1
            ],
        };
        assert_eq!(
            receiver.accept(oversized),
            Err(ProtocolError::InvalidAction)
        );
        assert!(receiver.accept(envelope()).is_ok());
    }
}
