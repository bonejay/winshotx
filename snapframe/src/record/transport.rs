//! Timestamped, video-only MPEG-TS bridge into FFmpeg's live muxer. Elementary
//! H.264/HEVC bytes alone do not carry DTS/PTS; trying to infer them can stall
//! live audio interleaving. Each packet carries the 90 kHz PTS and DTS NVENC's
//! B-frame reordering needs.
//!
//! MPEG-TS timestamps are unsigned, but the first B-frame packets decode before
//! frame 0 is shown (negative DTS). Every timestamp is therefore offset by one
//! second; FFmpeg subtracts each input's start time, so frame 0 still lands at
//! 0 and audio stays aligned.
use super::nvenc::Packet;

pub struct Transport {
    video: u8,
    pat: u8,
    pmt: u8,
    hevc: bool,
    fps: u32,
    sent: u64,
}

/// 33-bit PES timestamp field with the given 4-bit prefix.
fn stamp(prefix: u8, t: u64) -> [u8; 5] {
    [
        prefix << 4 | (((t >> 30) & 7) as u8) << 1 | 1,
        (t >> 22) as u8,
        (((t >> 15) & 0x7f) as u8) << 1 | 1,
        (t >> 7) as u8,
        ((t & 0x7f) as u8) << 1 | 1,
    ]
}

impl Transport {
    pub fn new(hevc: bool, fps: u32) -> Self {
        Self {
            video: 0,
            pat: 0,
            pmt: 0,
            hevc,
            fps,
            sent: 0,
        }
    }

    /// 90 kHz time of a frame number, offset by one second (see the module comment).
    fn clock(&self, frame: i64) -> u64 {
        let fps = i64::from(self.fps);
        ((frame + fps) * 90_000 / fps) as u64
    }

    pub fn packet(&mut self, packet: &Packet) -> Vec<u8> {
        let annex_b = &packet.data;
        let mut bytes = Vec::with_capacity(annex_b.len() * 2 + 1024);
        // Program tables about once a second, so the stream can be parsed from anywhere.
        let tables = self.sent % u64::from(self.fps) == 0;
        self.sent += 1;
        if tables {
            let mut pat = vec![0x00, 0xb0, 0x0d, 0, 1, 0xc1, 0, 0, 0, 1, 0xf0, 0];
            let mut pmt = vec![
                0x02,
                0xb0,
                0x12,
                0,
                1,
                0xc1,
                0,
                0,
                0xe1,
                0,
                0xf0,
                0,
                if self.hevc { 0x24 } else { 0x1b },
                0xe1,
                0,
                0xf0,
                0,
            ];
            for (pid, table, continuity) in [
                (0, &mut pat, &mut self.pat),
                (0x1000, &mut pmt, &mut self.pmt),
            ] {
                table.extend_from_slice(&crc(table).to_be_bytes());
                let mut section = vec![0]; // pointer field
                section.extend_from_slice(table);
                packets(&mut bytes, pid, continuity, &section, None, false);
            }
        }
        let pts = self.clock(packet.pts as i64);
        let dts = self.clock(packet.dts);
        let mut pes = vec![0, 0, 1, 0xe0, 0, 0, 0x80, 0xc0, 10];
        pes.extend_from_slice(&stamp(3, pts));
        pes.extend_from_slice(&stamp(1, dts));
        // Access-unit delimiters make the bridge independently parseable.
        if self.hevc {
            pes.extend_from_slice(&[0, 0, 0, 1, 0x46, 1, 0x50]);
        } else {
            pes.extend_from_slice(&[0, 0, 0, 1, 9, 0xf0]);
        }
        pes.extend_from_slice(annex_b);
        // The program clock must not run ahead of decoding.
        packets(
            &mut bytes,
            0x100,
            &mut self.video,
            &pes,
            Some(dts),
            packet.keyframe,
        );
        bytes
    }
}

/// Split a section or PES into 188-byte TS packets. The first carries the PCR and, for a
/// keyframe, the random-access indicator.
fn packets(
    out: &mut Vec<u8>,
    pid: u16,
    continuity: &mut u8,
    mut payload: &[u8],
    pcr: Option<u64>,
    random_access: bool,
) {
    let mut first = true;
    while !payload.is_empty() {
        let clock = first.then_some(pcr).flatten();
        let take = payload.len().min(if clock.is_some() { 176 } else { 184 });
        let adaptation = take < 184;
        let mut ts = [0xff; 188];
        ts[..4].copy_from_slice(&[
            0x47,
            (u8::from(first) << 6) | ((pid >> 8) as u8 & 0x1f),
            pid as u8,
            (if adaptation { 0x30 } else { 0x10 }) | (*continuity & 15),
        ]);
        *continuity = continuity.wrapping_add(1) & 15;
        if adaptation {
            let len = 183 - take;
            ts[4] = len as u8;
            if len > 0 {
                ts[5] = if clock.is_some() {
                    0x10 | if random_access { 0x40 } else { 0 }
                } else {
                    0
                };
            }
            if let Some(base) = clock {
                ts[6..12].copy_from_slice(&[
                    (base >> 25) as u8,
                    (base >> 17) as u8,
                    (base >> 9) as u8,
                    (base >> 1) as u8,
                    ((base & 1) as u8) << 7 | 0x7e,
                    0,
                ]);
            }
        }
        ts[188 - take..].copy_from_slice(&payload[..take]);
        out.extend_from_slice(&ts);
        payload = &payload[take..];
        first = false;
    }
}
fn crc(bytes: &[u8]) -> u32 {
    let mut value = 0xffff_ffffu32;
    for byte in bytes {
        value ^= u32::from(*byte) << 24;
        for _ in 0..8 {
            value = if value & 0x8000_0000 != 0 {
                value << 1 ^ 0x04c1_1db7
            } else {
                value << 1
            };
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    fn packet(pts: u64, dts: i64) -> Packet {
        Packet {
            data: vec![0x55; 500],
            pts,
            dts,
            keyframe: pts == 0,
        }
    }

    fn read(field: &[u8]) -> u64 {
        (u64::from(field[0] >> 1 & 7) << 30)
            | (u64::from(field[1]) << 22)
            | (u64::from(field[2] >> 1) << 15)
            | (u64::from(field[3]) << 7)
            | u64::from(field[4] >> 1)
    }

    #[test]
    fn tables_have_valid_crc_and_pes_packets_carry_pts_and_dts() {
        let mut bridge = Transport::new(false, 60);
        // First packet of a B-frame stream: shown at frame 0, decoded two frames earlier.
        let result = bridge.packet(&packet(0, -2));
        assert_eq!(result.len() % 188, 0);
        for ts in result.chunks_exact(188) {
            assert_eq!(ts[0], 0x47);
        }
        for ts in result.chunks_exact(188).take(2) {
            let start = 5 + usize::from(ts[4]);
            let section = &ts[start + 1..];
            let size = (usize::from(section[1] & 15) << 8) + usize::from(section[2]) + 3;
            assert_eq!(crc(&section[..size]), 0);
        }
        let ts = result.chunks_exact(188).nth(2).unwrap();
        let pes = &ts[5 + usize::from(ts[4])..];
        assert_eq!(pes[7], 0xc0, "PTS and DTS present");
        assert_eq!(pes[9] >> 4, 3);
        assert_eq!(pes[14] >> 4, 1);
        assert_eq!(read(&pes[9..14]), 90_000, "one second offset");
        assert_eq!(
            read(&pes[14..19]),
            90_000 - 3_000,
            "two frames earlier at 60 fps"
        );
        // Later packets: no tables, timestamps still offset.
        let next = bridge.packet(&packet(3, -1));
        let pes = &next[5 + usize::from(next[4])..];
        assert_eq!(read(&pes[9..14]), 90_000 + 4_500);
        assert_eq!(read(&pes[14..19]), 90_000 - 1_500);
    }
}
