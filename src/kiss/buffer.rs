// Copyright (C) 2026 Jared Crapo
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

use crate::error::Error;
use crate::kiss::frame::FEND;

pub struct KissReassemblyBuffer {
    buf: Vec<u8>,
    max_size: usize,
    in_frame: bool,
}

impl KissReassemblyBuffer {
    pub fn new(max_size: usize) -> Self {
        Self {
            buf: Vec::new(),
            max_size,
            in_frame: false,
        }
    }

    pub fn push(&mut self, data: &[u8]) -> Result<(), Error> {
        if self.buf.len() + data.len() > self.max_size {
            self.buf.clear();
            self.in_frame = false;
            return Err(Error::KissFrame("reassembly buffer overflow".into()));
        }
        self.buf.extend_from_slice(data);
        Ok(())
    }

    /// Extract all complete KISS frames from the buffer.
    /// Returns the raw bytes between FEND delimiters (not including the FENDs).
    /// Leaves any trailing partial frame in the buffer.
    pub fn drain_frames(&mut self) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        let mut frame_start: Option<usize> = if self.in_frame { Some(0) } else { None };
        let mut last_consumed = 0;

        for i in 0..self.buf.len() {
            if self.buf[i] == FEND {
                if let Some(s) = frame_start {
                    if i > s {
                        frames.push(self.buf[s..i].to_vec());
                    }
                }
                frame_start = Some(i + 1);
                last_consumed = i + 1;
            }
        }

        // Keep un-consumed bytes (partial frame after the last FEND)
        if last_consumed > 0 {
            self.buf.drain(..last_consumed);
        }

        // If we found at least one FEND, we're now inside a frame
        // (waiting for the closing FEND). If we never found a FEND
        // and weren't in a frame before, we still aren't.
        if last_consumed > 0 || frame_start.is_some() {
            self.in_frame = true;
        }

        frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_complete_frame() {
        let mut buf = KissReassemblyBuffer::new(4096);
        buf.push(&[FEND, 0x00, 0x01, 0x02, FEND]).unwrap();
        let frames = buf.drain_frames();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], vec![0x00, 0x01, 0x02]);
    }

    #[test]
    fn frame_split_across_pushes() {
        let mut buf = KissReassemblyBuffer::new(4096);
        buf.push(&[FEND, 0x00, 0x01]).unwrap();
        assert!(buf.drain_frames().is_empty());
        buf.push(&[0x02, 0x03, FEND]).unwrap();
        let frames = buf.drain_frames();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], vec![0x00, 0x01, 0x02, 0x03]);
    }

    #[test]
    fn multiple_frames_in_one_push() {
        let mut buf = KissReassemblyBuffer::new(4096);
        buf.push(&[FEND, 0x00, 0xAA, FEND, FEND, 0x00, 0xBB, FEND])
            .unwrap();
        let frames = buf.drain_frames();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0], vec![0x00, 0xAA]);
        assert_eq!(frames[1], vec![0x00, 0xBB]);
    }

    #[test]
    fn consecutive_fends_ignored() {
        let mut buf = KissReassemblyBuffer::new(4096);
        buf.push(&[FEND, FEND, FEND, 0x00, 0xAA, FEND]).unwrap();
        let frames = buf.drain_frames();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], vec![0x00, 0xAA]);
    }

    #[test]
    fn garbage_before_first_fend() {
        let mut buf = KissReassemblyBuffer::new(4096);
        buf.push(&[0xFF, 0xFE, FEND, 0x00, 0xAA, FEND]).unwrap();
        let frames = buf.drain_frames();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], vec![0x00, 0xAA]);
    }

    #[test]
    fn partial_frame_retained() {
        let mut buf = KissReassemblyBuffer::new(4096);
        buf.push(&[FEND, 0x00, 0xAA, FEND, FEND, 0x00, 0xBB]).unwrap();
        let frames = buf.drain_frames();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], vec![0x00, 0xAA]);

        // Complete the partial frame
        buf.push(&[0xCC, FEND]).unwrap();
        let frames = buf.drain_frames();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], vec![0x00, 0xBB, 0xCC]);
    }

    #[test]
    fn empty_push() {
        let mut buf = KissReassemblyBuffer::new(4096);
        buf.push(&[]).unwrap();
        assert!(buf.drain_frames().is_empty());
    }

    #[test]
    fn overflow_clears_buffer() {
        let mut buf = KissReassemblyBuffer::new(10);
        buf.push(&[FEND, 0x00, 0x01, 0x02, 0x03]).unwrap();
        let err = buf.push(&[0x04, 0x05, 0x06, 0x07, 0x08, 0x09]);
        assert!(err.is_err());
        // Buffer was cleared, so new data should work
        buf.push(&[FEND, 0x00, 0xAA, FEND]).unwrap();
        let frames = buf.drain_frames();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], vec![0x00, 0xAA]);
    }

    #[test]
    fn no_fend_means_no_frames() {
        let mut buf = KissReassemblyBuffer::new(4096);
        buf.push(&[0x00, 0x01, 0x02]).unwrap();
        assert!(buf.drain_frames().is_empty());
    }

    #[test]
    fn frame_with_escaped_fend_in_data() {
        let mut buf = KissReassemblyBuffer::new(4096);
        buf.push(&[FEND, 0x00, 0xDB, 0xDC, FEND]).unwrap();
        let frames = buf.drain_frames();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], vec![0x00, 0xDB, 0xDC]);
    }

    #[test]
    fn multiple_partial_pushes() {
        let mut buf = KissReassemblyBuffer::new(4096);
        buf.push(&[FEND]).unwrap();
        assert!(buf.drain_frames().is_empty());
        buf.push(&[0x00]).unwrap();
        assert!(buf.drain_frames().is_empty());
        buf.push(&[0xAA, 0xBB]).unwrap();
        assert!(buf.drain_frames().is_empty());
        buf.push(&[FEND]).unwrap();
        let frames = buf.drain_frames();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], vec![0x00, 0xAA, 0xBB]);
    }
}
