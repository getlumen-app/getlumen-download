#![allow(dead_code)]

use std::collections::BTreeMap;

const FRAME_MAGIC: &[u8; 4] = b"LWMP";
const FRAME_VERSION: u8 = 1;
const FRAME_HEADER_LEN: usize = 4 + 1 + 1 + 1 + 1 + 8 + 4 + 8 + 8 + 4;
const TRANSPORT_RECORD_LEN_BYTES: usize = 4;

pub const FRAME_FLAG_FIN: u8 = 0x01;
pub const FRAME_FLAG_RST: u8 = 0x02;
pub const DEFAULT_MAX_PAYLOAD_BYTES: usize = 12 * 1024;
pub const MAX_TRANSPORT_RECORD_BYTES: usize = FRAME_HEADER_LEN + DEFAULT_MAX_PAYLOAD_BYTES;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultipathFrame {
    pub session_id: u64,
    pub stream_id: u32,
    pub seq: u64,
    pub offset: u64,
    pub path_id: u8,
    pub flags: u8,
    pub payload: Vec<u8>,
}

impl MultipathFrame {
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        validate_frame(self)?;
        let mut out = Vec::with_capacity(FRAME_HEADER_LEN + self.payload.len());
        out.extend_from_slice(FRAME_MAGIC);
        out.push(FRAME_VERSION);
        out.push(self.flags);
        out.push(self.path_id);
        out.push(0);
        out.extend_from_slice(&self.session_id.to_be_bytes());
        out.extend_from_slice(&self.stream_id.to_be_bytes());
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.extend_from_slice(&self.offset.to_be_bytes());
        out.extend_from_slice(&(self.payload.len() as u32).to_be_bytes());
        out.extend_from_slice(&self.payload);
        Ok(out)
    }

    pub fn decode(input: &[u8]) -> Result<Self, String> {
        if input.len() < FRAME_HEADER_LEN {
            return Err("multipath frame is shorter than header".to_string());
        }
        if &input[0..4] != FRAME_MAGIC {
            return Err("multipath frame magic mismatch".to_string());
        }
        if input[4] != FRAME_VERSION {
            return Err(format!("unsupported multipath frame version {}", input[4]));
        }
        let flags = input[5];
        if flags & !(FRAME_FLAG_FIN | FRAME_FLAG_RST) != 0 {
            return Err(format!("unsupported multipath frame flags 0x{:02x}", flags));
        }
        let path_id = input[6];
        let session_id = read_u64(input, 8)?;
        let stream_id = read_u32(input, 16)?;
        let seq = read_u64(input, 20)?;
        let offset = read_u64(input, 28)?;
        let payload_len = read_u32(input, 36)? as usize;
        let expected_len = FRAME_HEADER_LEN + payload_len;
        if input.len() != expected_len {
            return Err(format!(
                "multipath frame length mismatch: expected {}, got {}",
                expected_len,
                input.len()
            ));
        }
        if payload_len > DEFAULT_MAX_PAYLOAD_BYTES {
            return Err(format!(
                "multipath frame payload too large: {} > {}",
                payload_len, DEFAULT_MAX_PAYLOAD_BYTES
            ));
        }
        Ok(Self {
            session_id,
            stream_id,
            seq,
            offset,
            path_id,
            flags,
            payload: input[FRAME_HEADER_LEN..].to_vec(),
        })
    }
}

fn validate_frame(frame: &MultipathFrame) -> Result<(), String> {
    if frame.payload.len() > DEFAULT_MAX_PAYLOAD_BYTES {
        return Err(format!(
            "multipath frame payload too large: {} > {}",
            frame.payload.len(),
            DEFAULT_MAX_PAYLOAD_BYTES
        ));
    }
    if frame.flags & !(FRAME_FLAG_FIN | FRAME_FLAG_RST) != 0 {
        return Err(format!(
            "unsupported multipath frame flags 0x{:02x}",
            frame.flags
        ));
    }
    Ok(())
}

fn read_u32(input: &[u8], offset: usize) -> Result<u32, String> {
    let bytes: [u8; 4] = input
        .get(offset..offset + 4)
        .ok_or_else(|| "multipath frame missing u32 field".to_string())?
        .try_into()
        .map_err(|_| "multipath frame invalid u32 field".to_string())?;
    Ok(u32::from_be_bytes(bytes))
}

fn read_u64(input: &[u8], offset: usize) -> Result<u64, String> {
    let bytes: [u8; 8] = input
        .get(offset..offset + 8)
        .ok_or_else(|| "multipath frame missing u64 field".to_string())?
        .try_into()
        .map_err(|_| "multipath frame invalid u64 field".to_string())?;
    Ok(u64::from_be_bytes(bytes))
}

#[derive(Debug)]
pub struct ReorderBuffer {
    next_offset: u64,
    max_buffered_bytes: usize,
    buffered_bytes: usize,
    chunks: BTreeMap<u64, Vec<u8>>,
}

impl ReorderBuffer {
    pub fn new(max_buffered_bytes: usize) -> Self {
        Self {
            next_offset: 0,
            max_buffered_bytes,
            buffered_bytes: 0,
            chunks: BTreeMap::new(),
        }
    }

    pub fn push(&mut self, frame: MultipathFrame) -> Result<Vec<u8>, String> {
        if frame.offset < self.next_offset {
            return Ok(Vec::new());
        }
        if self.chunks.contains_key(&frame.offset) {
            return Ok(Vec::new());
        }
        let next_buffered = self.buffered_bytes + frame.payload.len();
        if next_buffered > self.max_buffered_bytes {
            return Err(format!(
                "multipath reorder buffer overflow: {} > {}",
                next_buffered, self.max_buffered_bytes
            ));
        }
        self.buffered_bytes = next_buffered;
        self.chunks.insert(frame.offset, frame.payload);

        let mut ready = Vec::new();
        while let Some(payload) = self.chunks.remove(&self.next_offset) {
            self.buffered_bytes -= payload.len();
            self.next_offset += payload.len() as u64;
            ready.extend_from_slice(&payload);
        }
        Ok(ready)
    }

    pub fn next_offset(&self) -> u64 {
        self.next_offset
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReassemblerOutput {
    pub bytes: Vec<u8>,
    pub complete: bool,
}

#[derive(Debug)]
pub struct StreamReassembler {
    session_id: Option<u64>,
    stream_id: Option<u32>,
    final_offset: Option<u64>,
    reorder: ReorderBuffer,
}

impl StreamReassembler {
    pub fn new(max_buffered_bytes: usize) -> Self {
        Self {
            session_id: None,
            stream_id: None,
            final_offset: None,
            reorder: ReorderBuffer::new(max_buffered_bytes),
        }
    }

    pub fn push(&mut self, frame: MultipathFrame) -> Result<ReassemblerOutput, String> {
        self.bind_or_validate_stream(&frame)?;
        if frame.flags & FRAME_FLAG_RST != 0 {
            return Err("multipath stream reset".to_string());
        }
        if frame.flags & FRAME_FLAG_FIN != 0 {
            let end = frame.offset + frame.payload.len() as u64;
            if let Some(existing) = self.final_offset {
                if existing != end {
                    return Err("conflicting multipath FIN offset".to_string());
                }
            }
            self.final_offset = Some(end);
        }

        let bytes = self.reorder.push(frame)?;
        Ok(ReassemblerOutput {
            bytes,
            complete: self.is_complete(),
        })
    }

    pub fn is_complete(&self) -> bool {
        self.final_offset
            .map(|end| self.reorder.next_offset() == end)
            .unwrap_or(false)
    }

    fn bind_or_validate_stream(&mut self, frame: &MultipathFrame) -> Result<(), String> {
        match (self.session_id, self.stream_id) {
            (Some(session_id), Some(stream_id))
                if session_id == frame.session_id && stream_id == frame.stream_id =>
            {
                Ok(())
            }
            (Some(_), Some(_)) => Err("multipath frame belongs to a different stream".to_string()),
            _ => {
                self.session_id = Some(frame.session_id);
                self.stream_id = Some(frame.stream_id);
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathScore {
    pub path_id: u8,
    pub rtt_ms: u32,
    pub inflight_bytes: usize,
    pub healthy: bool,
}

#[derive(Debug)]
pub struct PathScheduler {
    paths: Vec<PathScore>,
}

impl PathScheduler {
    pub fn new(paths: Vec<PathScore>) -> Result<Self, String> {
        if paths.is_empty() {
            return Err("multipath scheduler requires at least one path".to_string());
        }
        Ok(Self { paths })
    }

    pub fn choose_path(&self) -> Option<u8> {
        self.paths
            .iter()
            .filter(|path| path.healthy)
            .min_by_key(|path| (path.inflight_bytes, path.rtt_ms, path.path_id))
            .map(|path| path.path_id)
    }

    pub fn mark_sent(&mut self, path_id: u8, bytes: usize) {
        if let Some(path) = self.paths.iter_mut().find(|path| path.path_id == path_id) {
            path.inflight_bytes = path.inflight_bytes.saturating_add(bytes);
        }
    }

    pub fn mark_acked(&mut self, path_id: u8, bytes: usize, rtt_ms: u32) {
        if let Some(path) = self.paths.iter_mut().find(|path| path.path_id == path_id) {
            path.inflight_bytes = path.inflight_bytes.saturating_sub(bytes);
            path.rtt_ms = ewma_rtt(path.rtt_ms, rtt_ms);
            path.healthy = true;
        }
    }

    pub fn mark_failed(&mut self, path_id: u8) {
        if let Some(path) = self.paths.iter_mut().find(|path| path.path_id == path_id) {
            path.healthy = false;
        }
    }
}

fn ewma_rtt(previous_ms: u32, latest_ms: u32) -> u32 {
    ((previous_ms as u64 * 7 + latest_ms as u64) / 8) as u32
}

#[derive(Debug)]
pub struct StreamSplitter {
    session_id: u64,
    stream_id: u32,
    chunk_size: usize,
    next_seq: u64,
    next_offset: u64,
    finished: bool,
}

impl StreamSplitter {
    pub fn new(session_id: u64, stream_id: u32, chunk_size: usize) -> Result<Self, String> {
        if chunk_size == 0 || chunk_size > DEFAULT_MAX_PAYLOAD_BYTES {
            return Err(format!(
                "invalid multipath chunk size: {} (max {})",
                chunk_size, DEFAULT_MAX_PAYLOAD_BYTES
            ));
        }
        Ok(Self {
            session_id,
            stream_id,
            chunk_size,
            next_seq: 0,
            next_offset: 0,
            finished: false,
        })
    }

    pub fn split(
        &mut self,
        bytes: &[u8],
        finish: bool,
        scheduler: &mut PathScheduler,
    ) -> Result<Vec<MultipathFrame>, String> {
        if self.finished {
            return Err("multipath stream splitter already finished".to_string());
        }

        let mut frames = Vec::new();
        if bytes.is_empty() && finish {
            frames.push(self.next_frame(Vec::new(), FRAME_FLAG_FIN, scheduler)?);
            self.finished = true;
            return Ok(frames);
        }

        let chunk_count = bytes.chunks(self.chunk_size).count();
        for (index, chunk) in bytes.chunks(self.chunk_size).enumerate() {
            let flags = if finish && index + 1 == chunk_count {
                FRAME_FLAG_FIN
            } else {
                0
            };
            frames.push(self.next_frame(chunk.to_vec(), flags, scheduler)?);
        }
        if finish {
            self.finished = true;
        }
        Ok(frames)
    }

    fn next_frame(
        &mut self,
        payload: Vec<u8>,
        flags: u8,
        scheduler: &mut PathScheduler,
    ) -> Result<MultipathFrame, String> {
        let path_id = scheduler
            .choose_path()
            .ok_or_else(|| "no healthy multipath paths available".to_string())?;
        let frame = MultipathFrame {
            session_id: self.session_id,
            stream_id: self.stream_id,
            seq: self.next_seq,
            offset: self.next_offset,
            path_id,
            flags,
            payload,
        };
        self.next_seq += 1;
        self.next_offset += frame.payload.len() as u64;
        scheduler.mark_sent(path_id, frame.payload.len());
        Ok(frame)
    }
}

pub fn split_stream_bytes(
    session_id: u64,
    stream_id: u32,
    bytes: &[u8],
    chunk_size: usize,
    scheduler: &mut PathScheduler,
) -> Result<Vec<MultipathFrame>, String> {
    StreamSplitter::new(session_id, stream_id, chunk_size)?.split(bytes, true, scheduler)
}

pub fn reassemble_stream_bytes(
    frames: impl IntoIterator<Item = MultipathFrame>,
    max_buffered_bytes: usize,
) -> Result<Vec<u8>, String> {
    let mut reassembler = StreamReassembler::new(max_buffered_bytes);
    let mut out = Vec::new();
    for frame in frames {
        out.extend_from_slice(&reassembler.push(frame)?.bytes);
    }
    Ok(out)
}

pub fn encode_transport_record(frame: &MultipathFrame) -> Result<Vec<u8>, String> {
    let encoded = frame.encode()?;
    if encoded.len() > MAX_TRANSPORT_RECORD_BYTES {
        return Err(format!(
            "multipath transport record too large: {} > {}",
            encoded.len(),
            MAX_TRANSPORT_RECORD_BYTES
        ));
    }
    let mut out = Vec::with_capacity(TRANSPORT_RECORD_LEN_BYTES + encoded.len());
    out.extend_from_slice(&(encoded.len() as u32).to_be_bytes());
    out.extend_from_slice(&encoded);
    Ok(out)
}

#[derive(Debug, Default)]
pub struct TransportRecordDecoder {
    buffer: Vec<u8>,
}

impl TransportRecordDecoder {
    pub fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) -> Result<Vec<MultipathFrame>, String> {
        self.buffer.extend_from_slice(bytes);
        let mut frames = Vec::new();
        loop {
            if self.buffer.len() < TRANSPORT_RECORD_LEN_BYTES {
                break;
            }
            let len = u32::from_be_bytes(
                self.buffer[0..TRANSPORT_RECORD_LEN_BYTES]
                    .try_into()
                    .map_err(|_| "invalid multipath transport length prefix".to_string())?,
            ) as usize;
            if len > MAX_TRANSPORT_RECORD_BYTES {
                return Err(format!(
                    "multipath transport record too large: {} > {}",
                    len, MAX_TRANSPORT_RECORD_BYTES
                ));
            }
            let record_end = TRANSPORT_RECORD_LEN_BYTES + len;
            if self.buffer.len() < record_end {
                break;
            }
            let frame =
                MultipathFrame::decode(&self.buffer[TRANSPORT_RECORD_LEN_BYTES..record_end])?;
            self.buffer.drain(0..record_end);
            frames.push(frame);
        }
        Ok(frames)
    }

    pub fn buffered_bytes(&self) -> usize {
        self.buffer.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::runtime::Builder;
    use tokio::sync::mpsc;
    use tokio::time::{sleep, Duration};

    fn frame(offset: u64, payload: &[u8]) -> MultipathFrame {
        MultipathFrame {
            session_id: 42,
            stream_id: 7,
            seq: offset,
            offset,
            path_id: 1,
            flags: 0,
            payload: payload.to_vec(),
        }
    }

    #[test]
    fn frame_codec_round_trips_and_preserves_order_fields() {
        let original = MultipathFrame {
            session_id: 123,
            stream_id: 9,
            seq: 55,
            offset: 4096,
            path_id: 2,
            flags: FRAME_FLAG_FIN,
            payload: b"hello".to_vec(),
        };

        let encoded = original.encode().unwrap();
        let decoded = MultipathFrame::decode(&encoded).unwrap();

        assert_eq!(decoded, original);
    }

    #[test]
    fn frame_decode_rejects_bad_magic_version_flags_and_length() {
        let encoded = frame(0, b"ok").encode().unwrap();

        let mut bad_magic = encoded.clone();
        bad_magic[0] = b'X';
        assert!(MultipathFrame::decode(&bad_magic)
            .unwrap_err()
            .contains("magic"));

        let mut bad_version = encoded.clone();
        bad_version[4] = 99;
        assert!(MultipathFrame::decode(&bad_version)
            .unwrap_err()
            .contains("version"));

        let mut bad_flags = encoded.clone();
        bad_flags[5] = 0x80;
        assert!(MultipathFrame::decode(&bad_flags)
            .unwrap_err()
            .contains("flags"));

        assert!(MultipathFrame::decode(&encoded[..encoded.len() - 1])
            .unwrap_err()
            .contains("length mismatch"));
    }

    #[test]
    fn reorder_buffer_waits_for_missing_prefix_then_flushes_contiguous_bytes() {
        let mut reorder = ReorderBuffer::new(64);

        assert_eq!(reorder.push(frame(5, b"world")).unwrap(), b"");
        assert_eq!(reorder.push(frame(0, b"hello")).unwrap(), b"helloworld");
    }

    #[test]
    fn reorder_buffer_ignores_duplicates_and_old_frames() {
        let mut reorder = ReorderBuffer::new(64);

        assert_eq!(reorder.push(frame(0, b"abc")).unwrap(), b"abc");
        assert_eq!(reorder.push(frame(0, b"abc")).unwrap(), b"");
        assert_eq!(reorder.push(frame(3, b"def")).unwrap(), b"def");
    }

    #[test]
    fn reorder_buffer_rejects_unbounded_out_of_order_growth() {
        let mut reorder = ReorderBuffer::new(4);

        assert!(reorder
            .push(frame(10, b"12345"))
            .unwrap_err()
            .contains("overflow"));
    }

    #[test]
    fn scheduler_prefers_healthy_low_inflight_path_then_updates_on_ack_and_fail() {
        let mut scheduler = PathScheduler::new(vec![
            PathScore {
                path_id: 1,
                rtt_ms: 40,
                inflight_bytes: 1200,
                healthy: true,
            },
            PathScore {
                path_id: 2,
                rtt_ms: 80,
                inflight_bytes: 0,
                healthy: true,
            },
        ])
        .unwrap();

        assert_eq!(scheduler.choose_path(), Some(2));
        scheduler.mark_sent(2, 2048);
        assert_eq!(scheduler.choose_path(), Some(1));
        scheduler.mark_failed(1);
        assert_eq!(scheduler.choose_path(), Some(2));
        scheduler.mark_acked(2, 2048, 56);
        assert_eq!(scheduler.choose_path(), Some(2));
    }

    #[test]
    fn splitter_spreads_one_stream_across_paths_and_reassembler_restores_order() {
        let mut scheduler = PathScheduler::new(vec![
            PathScore {
                path_id: 1,
                rtt_ms: 40,
                inflight_bytes: 0,
                healthy: true,
            },
            PathScore {
                path_id: 2,
                rtt_ms: 40,
                inflight_bytes: 0,
                healthy: true,
            },
        ])
        .unwrap();
        let payload = b"abcdefghijklmnopqrstuvwxyz";

        let frames = split_stream_bytes(77, 3, payload, 5, &mut scheduler).unwrap();

        assert!(frames.iter().all(|frame| frame.payload.len() <= 5));
        assert!(frames.iter().any(|frame| frame.path_id == 1));
        assert!(frames.iter().any(|frame| frame.path_id == 2));
        assert_eq!(frames.last().unwrap().flags, FRAME_FLAG_FIN);

        let mut out_of_order = frames.clone();
        out_of_order.swap(0, 1);
        let reassembled = reassemble_stream_bytes(out_of_order, 128).unwrap();
        assert_eq!(reassembled, payload);
    }

    #[test]
    fn stream_reassembler_reports_completion_only_after_fin_offset_is_flushed() {
        let mut reassembler = StreamReassembler::new(128);
        let fin = MultipathFrame {
            flags: FRAME_FLAG_FIN,
            ..frame(5, b"world")
        };

        let out = reassembler.push(fin).unwrap();
        assert_eq!(out.bytes, b"");
        assert!(!out.complete);

        let out = reassembler.push(frame(0, b"hello")).unwrap();
        assert_eq!(out.bytes, b"helloworld");
        assert!(out.complete);
        assert!(reassembler.is_complete());
    }

    #[test]
    fn stream_reassembler_rejects_cross_stream_frames_and_conflicting_fin() {
        let mut reassembler = StreamReassembler::new(128);
        reassembler.push(frame(0, b"hello")).unwrap();

        let other_stream = MultipathFrame {
            stream_id: 8,
            offset: 5,
            seq: 1,
            payload: b"world".to_vec(),
            ..frame(5, b"world")
        };
        assert!(reassembler
            .push(other_stream)
            .unwrap_err()
            .contains("different stream"));

        let mut reassembler = StreamReassembler::new(128);
        reassembler
            .push(MultipathFrame {
                flags: FRAME_FLAG_FIN,
                ..frame(0, b"abc")
            })
            .unwrap();
        assert!(reassembler
            .push(MultipathFrame {
                flags: FRAME_FLAG_FIN,
                offset: 0,
                payload: b"abcd".to_vec(),
                ..frame(0, b"abcd")
            })
            .unwrap_err()
            .contains("conflicting"));
    }

    #[test]
    fn stream_reassembler_treats_rst_as_terminal_error() {
        let mut reassembler = StreamReassembler::new(128);
        assert!(reassembler
            .push(MultipathFrame {
                flags: FRAME_FLAG_RST,
                ..frame(0, b"")
            })
            .unwrap_err()
            .contains("reset"));
    }

    #[test]
    fn splitter_rejects_missing_paths_and_invalid_chunk_sizes() {
        assert!(PathScheduler::new(Vec::new()).is_err());

        let mut scheduler = PathScheduler::new(vec![PathScore {
            path_id: 1,
            rtt_ms: 40,
            inflight_bytes: 0,
            healthy: false,
        }])
        .unwrap();
        assert!(split_stream_bytes(1, 1, b"abc", 3, &mut scheduler)
            .unwrap_err()
            .contains("no healthy"));

        let mut scheduler = PathScheduler::new(vec![PathScore {
            path_id: 1,
            rtt_ms: 40,
            inflight_bytes: 0,
            healthy: true,
        }])
        .unwrap();
        assert!(split_stream_bytes(1, 1, b"abc", 0, &mut scheduler)
            .unwrap_err()
            .contains("invalid"));
    }

    #[test]
    fn stream_splitter_preserves_offsets_across_incremental_writes() {
        let mut scheduler = PathScheduler::new(vec![PathScore {
            path_id: 1,
            rtt_ms: 40,
            inflight_bytes: 0,
            healthy: true,
        }])
        .unwrap();
        let mut splitter = StreamSplitter::new(1, 1, 4).unwrap();

        let first = splitter.split(b"hello", false, &mut scheduler).unwrap();
        let second = splitter.split(b"world", true, &mut scheduler).unwrap();

        let offsets: Vec<u64> = first
            .iter()
            .chain(second.iter())
            .map(|frame| frame.offset)
            .collect();
        assert_eq!(offsets, vec![0, 4, 5, 9]);
        assert_eq!(second.last().unwrap().flags, FRAME_FLAG_FIN);
        assert_eq!(
            reassemble_stream_bytes(first.into_iter().chain(second), 128).unwrap(),
            b"helloworld"
        );
    }

    #[test]
    fn transport_record_decoder_reassembles_fragmented_byte_records() {
        let first = frame(0, b"hello");
        let second = MultipathFrame {
            flags: FRAME_FLAG_FIN,
            ..frame(5, b"world")
        };
        let mut bytes = encode_transport_record(&first).unwrap();
        bytes.extend_from_slice(&encode_transport_record(&second).unwrap());

        let split_at = 7;
        let mut decoder = TransportRecordDecoder::new();
        assert_eq!(decoder.push_bytes(&bytes[..split_at]).unwrap(), Vec::new());
        assert!(decoder.buffered_bytes() > 0);

        let frames = decoder.push_bytes(&bytes[split_at..]).unwrap();
        assert_eq!(frames, vec![first, second]);
        assert_eq!(decoder.buffered_bytes(), 0);
    }

    #[test]
    fn transport_record_decoder_rejects_oversized_record_prefix() {
        let mut decoder = TransportRecordDecoder::new();
        let oversized = ((MAX_TRANSPORT_RECORD_BYTES + 1) as u32).to_be_bytes();

        assert!(decoder
            .push_bytes(&oversized)
            .unwrap_err()
            .contains("too large"));
    }

    #[test]
    fn loopback_harness_survives_out_of_order_delivery_and_midstream_path_failure() {
        let mut scheduler = PathScheduler::new(vec![
            PathScore {
                path_id: 1,
                rtt_ms: 20,
                inflight_bytes: 0,
                healthy: true,
            },
            PathScore {
                path_id: 2,
                rtt_ms: 40,
                inflight_bytes: 0,
                healthy: true,
            },
            PathScore {
                path_id: 3,
                rtt_ms: 80,
                inflight_bytes: 0,
                healthy: true,
            },
        ])
        .unwrap();
        let mut splitter = StreamSplitter::new(55, 4, 6).unwrap();

        let mut frames = splitter
            .split(b"abcdefghijklmnopqrstuvwxyz", false, &mut scheduler)
            .unwrap();
        scheduler.mark_failed(1);
        let post_failure = splitter.split(b"0123456789", true, &mut scheduler).unwrap();
        assert!(post_failure.iter().all(|frame| frame.path_id != 1));
        frames.extend(post_failure);

        let delivered = deliver_by_simulated_latency(frames, &[(1, 90), (2, 20), (3, 50)]);
        let mut reassembler = StreamReassembler::new(256);
        let mut out = Vec::new();
        let mut complete = false;
        for frame in delivered {
            let chunk = reassembler.push(frame).unwrap();
            out.extend_from_slice(&chunk.bytes);
            complete = chunk.complete;
        }

        assert!(complete);
        assert_eq!(out, b"abcdefghijklmnopqrstuvwxyz0123456789");
    }

    #[test]
    fn bounded_async_loopback_harness_applies_backpressure_and_reassembles_stream() {
        Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let mut scheduler = PathScheduler::new(vec![
                    PathScore {
                        path_id: 1,
                        rtt_ms: 20,
                        inflight_bytes: 0,
                        healthy: true,
                    },
                    PathScore {
                        path_id: 2,
                        rtt_ms: 40,
                        inflight_bytes: 0,
                        healthy: true,
                    },
                    PathScore {
                        path_id: 3,
                        rtt_ms: 80,
                        inflight_bytes: 0,
                        healthy: true,
                    },
                ])
                .unwrap();
                let mut splitter = StreamSplitter::new(99, 2, 4).unwrap();
                let mut frames = splitter
                    .split(b"the quick brown fox jumps over ", false, &mut scheduler)
                    .unwrap();
                scheduler.mark_failed(1);
                let after_failure = splitter
                    .split(b"the lazy dog", true, &mut scheduler)
                    .unwrap();
                assert!(after_failure.iter().all(|frame| frame.path_id != 1));
                frames.extend(after_failure);

                let (tx, mut rx) = mpsc::channel::<MultipathFrame>(1);
                tx.try_send(frame(0, b"held")).unwrap();
                assert!(tx.try_send(frame(4, b"backpressure")).is_err());
                drop(tx);
                assert!(rx.recv().await.is_some());

                let delivered = run_bounded_async_loopback(frames, &[(1, 90), (2, 10), (3, 40)], 1)
                    .await
                    .unwrap();
                let mut reassembler = StreamReassembler::new(512);
                let mut out = Vec::new();
                let mut complete = false;
                for frame in delivered {
                    let chunk = reassembler.push(frame).unwrap();
                    out.extend_from_slice(&chunk.bytes);
                    complete = chunk.complete;
                }

                assert!(complete);
                assert_eq!(out, b"the quick brown fox jumps over the lazy dog");
            });
    }

    #[test]
    fn local_tcp_harness_moves_stream_through_splitter_paths_and_aggregator() {
        Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let echoed = run_local_tcp_harness(
                    b"GET /through-multipath HTTP/1.1\r\nhost: local\r\n\r\n".to_vec(),
                    &[(1, 30), (2, 5), (3, 15)],
                )
                .await
                .unwrap();

                assert_eq!(
                    echoed,
                    b"GET /through-multipath HTTP/1.1\r\nhost: local\r\n\r\n"
                );
            });
    }

    #[test]
    fn bidirectional_tcp_harness_moves_request_and_response_through_multipath() {
        Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let response = run_bidirectional_tcp_harness(
                    b"POST /bidirectional HTTP/1.1\r\nhost: local\r\n\r\npayload".to_vec(),
                    &[(1, 25), (2, 5), (3, 15)],
                    &[(1, 10), (2, 30), (3, 20)],
                )
                .await
                .unwrap();

                assert_eq!(
                    response,
                    b"POST /bidirectional HTTP/1.1\r\nhost: local\r\n\r\npayload"
                );
            });
    }

    #[test]
    fn framed_transport_boundary_moves_fragmented_frames_between_client_and_aggregator_tasks() {
        Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let payload = b"framed transport boundary payload".to_vec();
                let decoded = route_bytes_through_framed_task_boundary(
                    777,
                    11,
                    payload.clone(),
                    &[(1, 25), (2, 5), (3, 15)],
                )
                .await
                .unwrap();

                assert_eq!(decoded, payload);
            });
    }

    async fn route_bytes_through_framed_task_boundary(
        session_id: u64,
        stream_id: u32,
        bytes: Vec<u8>,
        latencies: &[(u8, u64)],
    ) -> Result<Vec<u8>, String> {
        let mut scheduler = PathScheduler::new(vec![
            PathScore {
                path_id: 1,
                rtt_ms: 25,
                inflight_bytes: 0,
                healthy: true,
            },
            PathScore {
                path_id: 2,
                rtt_ms: 5,
                inflight_bytes: 0,
                healthy: true,
            },
            PathScore {
                path_id: 3,
                rtt_ms: 15,
                inflight_bytes: 0,
                healthy: true,
            },
        ])?;
        let mut splitter = StreamSplitter::new(session_id, stream_id, 7)?;
        let frames = splitter.split(&bytes, true, &mut scheduler)?;
        let delivered = run_bounded_async_loopback(frames, latencies, 1).await?;

        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(2);
        let sender = tokio::spawn(async move {
            for frame in delivered {
                let encoded = encode_transport_record(&frame)?;
                let split_at = encoded.len().min(5);
                tx.send(encoded[..split_at].to_vec())
                    .await
                    .map_err(|_| "framed transport receiver closed".to_string())?;
                tx.send(encoded[split_at..].to_vec())
                    .await
                    .map_err(|_| "framed transport receiver closed".to_string())?;
            }
            Ok::<(), String>(())
        });

        let mut decoder = TransportRecordDecoder::new();
        let mut reassembler = StreamReassembler::new(2048);
        let mut out = Vec::new();
        while let Some(chunk) = rx.recv().await {
            for frame in decoder.push_bytes(&chunk)? {
                out.extend_from_slice(&reassembler.push(frame)?.bytes);
            }
            if reassembler.is_complete() {
                break;
            }
        }
        sender
            .await
            .map_err(|e| format!("framed sender join failed: {}", e))??;
        if !reassembler.is_complete() {
            return Err("framed transport stream did not complete".to_string());
        }
        Ok(out)
    }

    async fn run_bidirectional_tcp_harness(
        request: Vec<u8>,
        forward_latencies: &[(u8, u64)],
        reverse_latencies: &[(u8, u64)],
    ) -> Result<Vec<u8>, String> {
        let echo = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| format!("bind echo: {}", e))?;
        let echo_port = echo.local_addr().map_err(|e| e.to_string())?.port();
        let echo_task = tokio::spawn(async move {
            let (mut stream, _) = echo.accept().await.map_err(|e| e.to_string())?;
            let mut buf = Vec::new();
            stream
                .read_to_end(&mut buf)
                .await
                .map_err(|e| e.to_string())?;
            stream.write_all(&buf).await.map_err(|e| e.to_string())?;
            Ok::<(), String>(())
        });

        let local = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| format!("bind local bidirectional harness: {}", e))?;
        let local_port = local.local_addr().map_err(|e| e.to_string())?.port();
        let forward_latencies = forward_latencies.to_vec();
        let reverse_latencies = reverse_latencies.to_vec();
        let server_task = tokio::spawn(async move {
            let (mut client, _) = local.accept().await.map_err(|e| e.to_string())?;
            let mut inbound = Vec::new();
            client
                .read_to_end(&mut inbound)
                .await
                .map_err(|e| e.to_string())?;

            let upstream_payload =
                route_bytes_through_multipath(9001, 1, inbound, &forward_latencies).await?;

            let mut upstream = TcpStream::connect(("127.0.0.1", echo_port))
                .await
                .map_err(|e| format!("connect echo: {}", e))?;
            upstream
                .write_all(&upstream_payload)
                .await
                .map_err(|e| e.to_string())?;
            upstream
                .shutdown()
                .await
                .map_err(|e| format!("shutdown upstream write: {}", e))?;
            let mut upstream_response = Vec::new();
            upstream
                .read_to_end(&mut upstream_response)
                .await
                .map_err(|e| e.to_string())?;

            let client_payload =
                route_bytes_through_multipath(9001, 2, upstream_response, &reverse_latencies)
                    .await?;
            client
                .write_all(&client_payload)
                .await
                .map_err(|e| e.to_string())?;
            Ok::<(), String>(())
        });

        let mut client = TcpStream::connect(("127.0.0.1", local_port))
            .await
            .map_err(|e| format!("connect local bidirectional harness: {}", e))?;
        client
            .write_all(&request)
            .await
            .map_err(|e| e.to_string())?;
        client
            .shutdown()
            .await
            .map_err(|e| format!("shutdown client write: {}", e))?;
        let mut response = Vec::new();
        client
            .read_to_end(&mut response)
            .await
            .map_err(|e| e.to_string())?;

        server_task
            .await
            .map_err(|e| format!("bidirectional server join failed: {}", e))??;
        echo_task
            .await
            .map_err(|e| format!("echo join failed: {}", e))??;
        Ok(response)
    }

    async fn route_bytes_through_multipath(
        session_id: u64,
        stream_id: u32,
        bytes: Vec<u8>,
        latencies: &[(u8, u64)],
    ) -> Result<Vec<u8>, String> {
        let mut scheduler = PathScheduler::new(vec![
            PathScore {
                path_id: 1,
                rtt_ms: 30,
                inflight_bytes: 0,
                healthy: true,
            },
            PathScore {
                path_id: 2,
                rtt_ms: 5,
                inflight_bytes: 0,
                healthy: true,
            },
            PathScore {
                path_id: 3,
                rtt_ms: 15,
                inflight_bytes: 0,
                healthy: true,
            },
        ])?;
        let mut splitter = StreamSplitter::new(session_id, stream_id, 8)?;
        let frames = splitter.split(&bytes, true, &mut scheduler)?;
        let delivered = run_bounded_async_loopback(frames, latencies, 1).await?;
        let mut reassembler = StreamReassembler::new(2048);
        let mut out = Vec::new();
        for frame in delivered {
            out.extend_from_slice(&reassembler.push(frame)?.bytes);
        }
        if !reassembler.is_complete() {
            return Err("multipath stream did not complete".to_string());
        }
        Ok(out)
    }

    async fn run_local_tcp_harness(
        request: Vec<u8>,
        latencies: &[(u8, u64)],
    ) -> Result<Vec<u8>, String> {
        let echo = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| format!("bind echo: {}", e))?;
        let echo_port = echo.local_addr().map_err(|e| e.to_string())?.port();
        let echo_task = tokio::spawn(async move {
            let (mut stream, _) = echo.accept().await.map_err(|e| e.to_string())?;
            let mut buf = Vec::new();
            stream
                .read_to_end(&mut buf)
                .await
                .map_err(|e| e.to_string())?;
            stream.write_all(&buf).await.map_err(|e| e.to_string())?;
            Ok::<(), String>(())
        });

        let local = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| format!("bind local harness: {}", e))?;
        let local_port = local.local_addr().map_err(|e| e.to_string())?.port();
        let latencies = latencies.to_vec();
        let server_task = tokio::spawn(async move {
            let (mut client, _) = local.accept().await.map_err(|e| e.to_string())?;
            let mut inbound = Vec::new();
            client
                .read_to_end(&mut inbound)
                .await
                .map_err(|e| e.to_string())?;

            let mut scheduler = PathScheduler::new(vec![
                PathScore {
                    path_id: 1,
                    rtt_ms: 30,
                    inflight_bytes: 0,
                    healthy: true,
                },
                PathScore {
                    path_id: 2,
                    rtt_ms: 5,
                    inflight_bytes: 0,
                    healthy: true,
                },
                PathScore {
                    path_id: 3,
                    rtt_ms: 15,
                    inflight_bytes: 0,
                    healthy: true,
                },
            ])?;
            let mut splitter = StreamSplitter::new(1234, 1, 8)?;
            let frames = splitter.split(&inbound, true, &mut scheduler)?;
            let delivered = run_bounded_async_loopback(frames, &latencies, 1).await?;
            let mut reassembler = StreamReassembler::new(1024);
            let mut upstream_payload = Vec::new();
            for frame in delivered {
                upstream_payload.extend_from_slice(&reassembler.push(frame)?.bytes);
            }
            if !reassembler.is_complete() {
                return Err("local TCP harness stream did not complete".to_string());
            }

            let mut upstream = TcpStream::connect(("127.0.0.1", echo_port))
                .await
                .map_err(|e| format!("connect echo: {}", e))?;
            upstream
                .write_all(&upstream_payload)
                .await
                .map_err(|e| e.to_string())?;
            upstream
                .shutdown()
                .await
                .map_err(|e| format!("shutdown upstream write: {}", e))?;
            let mut echoed = Vec::new();
            upstream
                .read_to_end(&mut echoed)
                .await
                .map_err(|e| e.to_string())?;
            client.write_all(&echoed).await.map_err(|e| e.to_string())?;
            Ok::<(), String>(())
        });

        let mut client = TcpStream::connect(("127.0.0.1", local_port))
            .await
            .map_err(|e| format!("connect local harness: {}", e))?;
        client
            .write_all(&request)
            .await
            .map_err(|e| e.to_string())?;
        client
            .shutdown()
            .await
            .map_err(|e| format!("shutdown client write: {}", e))?;
        let mut echoed = Vec::new();
        client
            .read_to_end(&mut echoed)
            .await
            .map_err(|e| e.to_string())?;

        server_task
            .await
            .map_err(|e| format!("server join failed: {}", e))??;
        echo_task
            .await
            .map_err(|e| format!("echo join failed: {}", e))??;
        Ok(echoed)
    }

    async fn run_bounded_async_loopback(
        frames: Vec<MultipathFrame>,
        latencies: &[(u8, u64)],
        channel_capacity: usize,
    ) -> Result<Vec<MultipathFrame>, String> {
        let (aggregator_tx, mut aggregator_rx) = mpsc::channel::<MultipathFrame>(frames.len());
        let mut path_senders = Vec::new();
        let mut handles = Vec::new();

        for (path_id, latency_ms) in latencies.iter().copied() {
            let (path_tx, mut path_rx) = mpsc::channel::<MultipathFrame>(channel_capacity);
            let aggregator_tx = aggregator_tx.clone();
            let handle = tokio::spawn(async move {
                while let Some(frame) = path_rx.recv().await {
                    sleep(Duration::from_millis(latency_ms)).await;
                    aggregator_tx
                        .send(frame)
                        .await
                        .map_err(|_| "aggregator channel closed".to_string())?;
                }
                Ok::<(), String>(())
            });
            path_senders.push((path_id, path_tx));
            handles.push(handle);
        }
        drop(aggregator_tx);

        let expected = frames.len();
        for frame in frames {
            let Some((_, tx)) = path_senders
                .iter()
                .find(|(path_id, _)| *path_id == frame.path_id)
            else {
                return Err(format!("missing async path {}", frame.path_id));
            };
            tx.send(frame)
                .await
                .map_err(|_| "path channel closed".to_string())?;
        }
        drop(path_senders);

        let mut delivered = Vec::new();
        while let Some(frame) = aggregator_rx.recv().await {
            delivered.push(frame);
            if delivered.len() == expected {
                break;
            }
        }
        for handle in handles {
            handle
                .await
                .map_err(|e| format!("path worker join failed: {}", e))??;
        }
        Ok(delivered)
    }

    fn deliver_by_simulated_latency(
        mut frames: Vec<MultipathFrame>,
        latencies: &[(u8, u64)],
    ) -> Vec<MultipathFrame> {
        frames.sort_by_key(|frame| {
            let latency = latencies
                .iter()
                .find(|(path_id, _)| *path_id == frame.path_id)
                .map(|(_, latency)| *latency)
                .unwrap_or(0);
            (latency, frame.seq)
        });
        frames
    }
}
