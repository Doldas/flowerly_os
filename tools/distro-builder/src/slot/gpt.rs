//! Minimal, read-only GPT reader used to bind the disk manifest to the real
//! partition table before any slot is written (ADR-0019). Slots are resolved
//! by verified partition identity, never by a caller-supplied offset.

use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;

const SECTOR: u64 = 512;
const MAX_ENTRIES: u64 = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GptPartition {
    pub name: String,
    pub partuuid: String,
    pub offset: u64,
    pub size: u64,
}

/// Positioned reads; implemented for files and, in tests, byte slices.
pub trait ReadAt {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()>;
}

impl ReadAt for File {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        FileExt::read_exact_at(self, buf, offset)
    }
}

#[cfg(test)]
impl ReadAt for [u8] {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        let start = offset as usize;
        let end = start + buf.len();
        match self.get(start..end) {
            Some(src) => {
                buf.copy_from_slice(src);
                Ok(())
            }
            None => Err(io::ErrorKind::UnexpectedEof.into()),
        }
    }
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn le32(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(buf[at..at + 4].try_into().unwrap())
}

fn le64(buf: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(buf[at..at + 8].try_into().unwrap())
}

/// GPT stores the first three GUID fields little-endian.
pub fn guid_text(raw: &[u8]) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        u32::from_le_bytes(raw[0..4].try_into().unwrap()),
        u16::from_le_bytes(raw[4..6].try_into().unwrap()),
        u16::from_le_bytes(raw[6..8].try_into().unwrap()),
        raw[8],
        raw[9],
        raw[10],
        raw[11],
        raw[12],
        raw[13],
        raw[14],
        raw[15]
    )
}

/// Read and checksum-verify the primary GPT of a disk image.
pub fn read<F: ReadAt + ?Sized>(file: &F, disk_size: u64) -> Result<Vec<GptPartition>> {
    let mut header = [0u8; 512];
    file.read_exact_at(&mut header, SECTOR)
        .context("reading GPT header")?;
    if &header[0..8] != b"EFI PART" || le32(&header, 8) != 0x0001_0000 {
        bail!("not a GPT disk");
    }
    let header_size = le32(&header, 12) as usize;
    if !(92..=512).contains(&header_size) {
        bail!("invalid GPT header size");
    }
    let stored = le32(&header, 16);
    let mut zeroed = header[..header_size].to_vec();
    zeroed[16..20].fill(0);
    if crc32(&zeroed) != stored {
        bail!("GPT header checksum mismatch");
    }
    let entries_lba = le64(&header, 72);
    let count = u64::from(le32(&header, 80));
    let entry_size = u64::from(le32(&header, 84));
    if entry_size < 128 || entry_size % 8 != 0 || count == 0 || count > MAX_ENTRIES {
        bail!("invalid GPT entry array geometry");
    }
    let array_len = count * entry_size;
    if entries_lba < 2 || entries_lba * SECTOR + array_len > disk_size {
        bail!("GPT entry array outside the disk");
    }
    let mut array = vec![0u8; array_len as usize];
    file.read_exact_at(&mut array, entries_lba * SECTOR)
        .context("reading GPT entries")?;
    if crc32(&array) != le32(&header, 88) {
        bail!("GPT entry array checksum mismatch");
    }
    let mut parts = Vec::new();
    for raw in array.chunks_exact(entry_size as usize) {
        if raw[0..16].iter().all(|b| *b == 0) {
            continue;
        }
        let first = le64(raw, 32);
        let last = le64(raw, 40);
        if last < first {
            bail!("GPT partition ends before it starts");
        }
        let units: Vec<u16> = raw[56..128]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .take_while(|u| *u != 0)
            .collect();
        let end = (last + 1) * SECTOR;
        if end > disk_size {
            bail!("GPT partition outside the disk");
        }
        parts.push(GptPartition {
            name: String::from_utf16(&units).context("GPT partition name")?,
            partuuid: guid_text(&raw[16..32]),
            offset: first * SECTOR,
            size: end - first * SECTOR,
        });
    }
    Ok(parts)
}

#[cfg(test)]
pub fn build_for_test(parts: &[(&str, &str, u64, u64)], disk_size: u64) -> Vec<u8> {
    // (name, partuuid, first_lba, sector_count) -> bytes of LBA0..LBA33.
    let mut image = vec![0u8; 34 * 512];
    let mut array = vec![0u8; 128 * 128];
    for (i, (name, uuid, first, sectors)) in parts.iter().enumerate() {
        let raw = &mut array[i * 128..(i + 1) * 128];
        raw[0] = 1; // non-zero type GUID
        let fields: Vec<&str> = uuid.split('-').collect();
        raw[16..20].copy_from_slice(&u32::from_str_radix(fields[0], 16).unwrap().to_le_bytes());
        raw[20..22].copy_from_slice(&u16::from_str_radix(fields[1], 16).unwrap().to_le_bytes());
        raw[22..24].copy_from_slice(&u16::from_str_radix(fields[2], 16).unwrap().to_le_bytes());
        let tail = format!("{}{}", fields[3], fields[4]);
        for j in 0..8 {
            raw[24 + j] = u8::from_str_radix(&tail[j * 2..j * 2 + 2], 16).unwrap();
        }
        raw[32..40].copy_from_slice(&first.to_le_bytes());
        raw[40..48].copy_from_slice(&(first + sectors - 1).to_le_bytes());
        for (j, unit) in name.encode_utf16().enumerate() {
            raw[56 + j * 2..58 + j * 2].copy_from_slice(&unit.to_le_bytes());
        }
    }
    let mut header = [0u8; 92];
    header[0..8].copy_from_slice(b"EFI PART");
    header[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    header[12..16].copy_from_slice(&92u32.to_le_bytes());
    header[24..32].copy_from_slice(&1u64.to_le_bytes());
    header[72..80].copy_from_slice(&2u64.to_le_bytes());
    header[80..84].copy_from_slice(&128u32.to_le_bytes());
    header[84..88].copy_from_slice(&128u32.to_le_bytes());
    header[88..92].copy_from_slice(&crc32(&array).to_le_bytes());
    let header_crc = crc32(&header);
    header[16..20].copy_from_slice(&header_crc.to_le_bytes());
    image[512..512 + 92].copy_from_slice(&header);
    image[1024..1024 + array.len()].copy_from_slice(&array);
    let _ = disk_size;
    image
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn guid_text_uses_gpt_mixed_endianness() {
        let raw = [
            0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e,
            0xc9, 0x3b,
        ];
        assert_eq!(guid_text(&raw), "c12a7328-f81f-11d2-ba4b-00a0c93ec93b");
    }

    #[test]
    fn round_trips_and_rejects_corruption() {
        let uuid = "11111111-2222-3333-4444-555555555555";
        let mut image = build_for_test(&[("root-a", uuid, 2048, 4096)], 8 << 20);
        image.resize(8 << 20, 0);
        let parts = read(image.as_slice(), image.len() as u64).unwrap();
        assert_eq!(
            parts,
            vec![GptPartition {
                name: "root-a".into(),
                partuuid: uuid.into(),
                offset: 2048 * 512,
                size: 4096 * 512,
            }]
        );
        let mut broken = image.clone();
        broken[1024 + 32] ^= 1; // entry array
        assert!(read(broken.as_slice(), broken.len() as u64).is_err());
        let mut broken = image.clone();
        broken[512 + 40] ^= 1; // header field
        assert!(read(broken.as_slice(), broken.len() as u64).is_err());
        assert!(read(image.as_slice(), 4096).is_err(), "table beyond disk");
    }
}
