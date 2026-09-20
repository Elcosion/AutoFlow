//! Bounded documents for configuration/recording RPC. Large sessions use
//! 64KiB binary chunks, never an unbounded single pipe frame.
use crate::runtime_protocol::{read_frame, write_frame, ProtocolError};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::time::{Duration, Instant};

pub const MAX_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;
const CHUNK_BYTES: usize = 64 * 1024;
#[derive(Serialize, Deserialize)]
struct DocumentHeader {
    bytes: usize,
    chunks: usize,
}

struct LimitedBuffer {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}
impl Write for LimitedBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(std::io::Error::other("document limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The pipe owner may block, but the authority waiting for it never joins an
/// unresponsive writer. Only one reply can be outstanding; failure is terminal.
pub struct DocumentWriter<T> {
    queue: SyncSender<T>,
    written: Receiver<Result<(), ProtocolError>>,
    failed: bool,
}
impl<T: Serialize + Send + 'static> DocumentWriter<T> {
    pub fn spawn(mut writer: impl Write + Send + 'static) -> std::io::Result<Self> {
        let (queue, incoming) = sync_channel::<T>(1);
        let (finished, written) = sync_channel(1);
        std::thread::Builder::new()
            .name("autoflow-service-reply-writer".into())
            .spawn(move || {
                while let Ok(value) = incoming.recv() {
                    let result = write_document(&mut writer, &value);
                    let failed = result.is_err();
                    if finished.send(result).is_err() || failed {
                        break;
                    }
                }
            })?;
        Ok(Self {
            queue,
            written,
            failed: false,
        })
    }
    pub fn send(
        &mut self,
        value: T,
        cancelled: &AtomicBool,
        timeout: Duration,
    ) -> Result<(), ProtocolError> {
        if self.failed || cancelled.load(Ordering::Acquire) {
            return Err(ProtocolError::Io);
        }
        if self.queue.try_send(value).is_err() {
            self.failed = true;
            return Err(ProtocolError::Io);
        }
        let deadline = Instant::now() + timeout.min(Duration::from_millis(500));
        let result = loop {
            if cancelled.load(Ordering::Acquire) {
                break Err(ProtocolError::Io);
            }
            match self.written.recv_timeout(Duration::from_millis(2)) {
                Ok(result) => break result,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    break Err(ProtocolError::Io)
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
                Err(_) => break Err(ProtocolError::Io),
            }
        };
        self.failed = result.is_err();
        result
    }
}

pub fn write_document<T: Serialize>(
    writer: &mut impl Write,
    value: &T,
) -> Result<(), ProtocolError> {
    let mut buffer = LimitedBuffer {
        bytes: Vec::new(),
        limit: MAX_DOCUMENT_BYTES,
        exceeded: false,
    };
    if serde_json::to_writer(&mut buffer, value).is_err() {
        return Err(if buffer.exceeded {
            ProtocolError::FrameTooLarge
        } else {
            ProtocolError::InvalidJson
        });
    }
    let bytes = buffer.bytes;
    if bytes.is_empty() || bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(ProtocolError::FrameTooLarge);
    }
    write_frame(
        writer,
        &DocumentHeader {
            bytes: bytes.len(),
            chunks: bytes.len().div_ceil(CHUNK_BYTES),
        },
    )?;
    for chunk in bytes.chunks(CHUNK_BYTES) {
        writer
            .write_all(&(chunk.len() as u32).to_le_bytes())
            .map_err(|_| ProtocolError::Io)?;
        writer.write_all(chunk).map_err(|_| ProtocolError::Io)?;
    }
    writer.flush().map_err(|_| ProtocolError::Io)
}

pub fn read_document<T: for<'de> Deserialize<'de>>(
    reader: &mut impl Read,
) -> Result<T, ProtocolError> {
    let header: DocumentHeader = read_frame(reader)?;
    if header.bytes == 0
        || header.bytes > MAX_DOCUMENT_BYTES
        || header.chunks != header.bytes.div_ceil(CHUNK_BYTES)
    {
        return Err(ProtocolError::FrameTooLarge);
    }
    let mut bytes = Vec::with_capacity(header.bytes);
    for _ in 0..header.chunks {
        let mut length = [0; 4];
        reader
            .read_exact(&mut length)
            .map_err(|_| ProtocolError::Io)?;
        let length = u32::from_le_bytes(length) as usize;
        let expected = (header.bytes - bytes.len()).min(CHUNK_BYTES);
        if length != expected {
            return Err(ProtocolError::FrameTooLarge);
        }
        let start = bytes.len();
        bytes.resize(start + length, 0);
        reader
            .read_exact(&mut bytes[start..])
            .map_err(|_| ProtocolError::Io)?;
    }
    serde_json::from_slice(&bytes).map_err(|_| ProtocolError::InvalidJson)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn serialization_limit_stops_before_accepting_oversized_bytes() {
        let mut buffer = LimitedBuffer {
            bytes: Vec::new(),
            limit: 4,
            exceeded: false,
        };
        assert!(serde_json::to_writer(&mut buffer, &"long value").is_err());
        assert!(buffer.exceeded);
        assert!(buffer.bytes.len() <= 4);
    }
    #[test]
    fn blocked_reply_times_out_and_cannot_be_reused() {
        struct Blocked(Receiver<()>);
        impl Write for Blocked {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                let _ = self.0.recv();
                Err(std::io::Error::other("fixture disconnected"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let (release, blocked) = sync_channel(1);
        let mut writer = DocumentWriter::spawn(Blocked(blocked)).expect("writer");
        let cancelled = AtomicBool::new(false);
        assert!(writer
            .send(1u32, &cancelled, Duration::from_millis(10))
            .is_err());
        assert!(writer
            .send(2, &cancelled, Duration::from_millis(10))
            .is_err());
        drop(writer);
        release
            .send(())
            .expect("release fixture, no leaked blocked thread");
    }
    #[test]
    fn multi_chunk_round_trip_and_oversized_document_is_rejected_before_body() {
        let value = "x".repeat(2 * CHUNK_BYTES + 1);
        let mut encoded = Vec::new();
        write_document(&mut encoded, &value).expect("encode");
        assert_eq!(
            read_document::<String>(&mut encoded.as_slice()).expect("decode"),
            value
        );
        let mut bad = Vec::new();
        write_frame(
            &mut bad,
            &DocumentHeader {
                bytes: MAX_DOCUMENT_BYTES + 1,
                chunks: 1,
            },
        )
        .expect("header");
        assert_eq!(
            read_document::<String>(&mut bad.as_slice()),
            Err(ProtocolError::FrameTooLarge)
        );
    }
    #[test]
    fn invalid_chunk_size_is_rejected_without_reading_chunk() {
        let mut bad = Vec::new();
        write_frame(
            &mut bad,
            &DocumentHeader {
                bytes: 1,
                chunks: 1,
            },
        )
        .expect("header");
        bad.extend_from_slice(&2u32.to_le_bytes());
        assert_eq!(
            read_document::<String>(&mut bad.as_slice()),
            Err(ProtocolError::FrameTooLarge)
        );
    }
}
