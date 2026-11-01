//! Reading the list of files in a zip archive from its central directory,
//! for the save-as dialog's Preview. Only the end of the archive is read:
//! the end-of-central-directory record (and its ZIP64 form), then the
//! central directory itself, through a function that fetches byte ranges.

use serde::{Deserialize, Serialize};

/// A file inside the archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZipEntry {
    pub name: String,
    /// Its size once unpacked.
    pub size: u64,
}

/// The longest a zip comment can make the end record's search.
const TAIL: u64 = 65_557;
/// A central directory larger than this is not read for a preview.
const MAX_DIRECTORY: u64 = 16 * 1024 * 1024;
/// Most entries listed.
pub const MAX_ENTRIES: usize = 5000;

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// The files in a zip archive of `total` bytes. `read(start, end)` fetches
/// the inclusive byte range. The message says why when it cannot be read.
pub fn list(total: u64, mut read: impl FnMut(u64, u64) -> Result<Vec<u8>, String>) -> Result<Vec<ZipEntry>, String> {
    let not_zip = || "This is not a zip archive, or its end could not be read.".to_string();
    if total < 22 {
        return Err(not_zip());
    }
    let tail_start = total.saturating_sub(TAIL);
    let tail = read(tail_start, total - 1)?;
    // The end record is the last "PK\5\6" with room for its 22 bytes.
    let eocd = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&i| tail[i..].starts_with(b"PK\x05\x06"))
        .ok_or_else(not_zip)?;
    let mut count = u16_at(&tail, eocd + 10).ok_or_else(not_zip)? as u64;
    let mut size = u32_at(&tail, eocd + 12).ok_or_else(not_zip)? as u64;
    let mut offset = u32_at(&tail, eocd + 16).ok_or_else(not_zip)? as u64;
    // ZIP64: a locator just before the end record points at a larger one.
    if (count == 0xffff || size == 0xffff_ffff || offset == 0xffff_ffff) && eocd >= 20 {
        let loc = eocd - 20;
        if tail[loc..].starts_with(b"PK\x06\x07") {
            let rec = u64_at(&tail, loc + 8).ok_or_else(not_zip)?;
            let record = read(rec, rec + 55)?;
            if !record.starts_with(b"PK\x06\x06") {
                return Err(not_zip());
            }
            count = u64_at(&record, 32).ok_or_else(not_zip)?;
            size = u64_at(&record, 40).ok_or_else(not_zip)?;
            offset = u64_at(&record, 48).ok_or_else(not_zip)?;
        }
    }
    if size > MAX_DIRECTORY {
        return Err("This archive lists too many files to preview.".into());
    }
    if offset.checked_add(size).is_none_or(|end| end > total) {
        return Err(not_zip());
    }
    if size == 0 {
        return Ok(Vec::new());
    }
    // The directory is often inside the tail already read.
    // A server may send less of the tail than asked; then the directory is
    // read on its own.
    let in_tail = (offset >= tail_start)
        .then(|| (offset - tail_start) as usize)
        .and_then(|from| tail.get(from..from + size as usize));
    let dir = if let Some(dir) = in_tail {
        dir.to_vec()
    } else {
        read(offset, offset + size - 1)?
    };
    parse_directory(&dir, count)
}

/// The entries of a central directory.
fn parse_directory(dir: &[u8], count: u64) -> Result<Vec<ZipEntry>, String> {
    let bad = || "The archive's list of files is damaged.".to_string();
    let mut entries = Vec::new();
    let mut at = 0usize;
    for _ in 0..count.min(MAX_ENTRIES as u64) {
        if !dir.get(at..).is_some_and(|d| d.starts_with(b"PK\x01\x02")) {
            return Err(bad());
        }
        let flags = u16_at(dir, at + 8).ok_or_else(bad)?;
        let mut size = u32_at(dir, at + 24).ok_or_else(bad)? as u64;
        let name_len = u16_at(dir, at + 28).ok_or_else(bad)? as usize;
        let extra_len = u16_at(dir, at + 30).ok_or_else(bad)? as usize;
        let comment_len = u16_at(dir, at + 32).ok_or_else(bad)? as usize;
        let name_start = at + 46;
        let name = dir.get(name_start..name_start + name_len).ok_or_else(bad)?;
        // Bit 11: the name is UTF-8; otherwise it is CP437, which matches
        // UTF-8 for plain ASCII names.
        let name = if flags & 0x0800 != 0 {
            String::from_utf8_lossy(name).into_owned()
        } else {
            name.iter().map(|&b| if b < 0x80 { b as char } else { '?' }).collect()
        };
        let extra = dir.get(name_start + name_len..name_start + name_len + extra_len).ok_or_else(bad)?;
        if size == 0xffff_ffff {
            // The ZIP64 extra field holds the real size first.
            let mut e = 0;
            while e + 4 <= extra.len() {
                let id = u16_at(extra, e).ok_or_else(bad)?;
                let len = u16_at(extra, e + 2).ok_or_else(bad)? as usize;
                if id == 0x0001 {
                    size = u64_at(extra, e + 4).ok_or_else(bad)?;
                    break;
                }
                e += 4 + len;
            }
        }
        entries.push(ZipEntry { name, size });
        at = name_start + name_len + extra_len + comment_len;
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A central directory entry for `name` of `size` bytes.
    fn entry(name: &str, size: u32, utf8: bool, zip64: Option<u64>) -> Vec<u8> {
        let mut e = b"PK\x01\x02".to_vec();
        e.extend([20, 0, 20, 0]);
        e.extend(if utf8 { 0x0800u16 } else { 0 }.to_le_bytes());
        // Method, time, date and CRC.
        e.extend([0u8; 10]);
        e.extend(0u32.to_le_bytes()); // compressed size
        e.extend(if zip64.is_some() { 0xffff_ffff } else { size }.to_le_bytes());
        e.extend((name.len() as u16).to_le_bytes());
        let extra: Vec<u8> = match zip64 {
            Some(real) => [1u16.to_le_bytes().to_vec(), 8u16.to_le_bytes().to_vec(), real.to_le_bytes().to_vec()].concat(),
            None => Vec::new(),
        };
        e.extend((extra.len() as u16).to_le_bytes());
        // Comment length, disk, attributes and the local header's offset.
        e.extend([0u8; 14]);
        e.extend(name.as_bytes());
        e.extend(extra);
        e
    }

    /// An archive: `pad` bytes of file data, then the directory and the end
    /// record.
    fn archive(pad: usize, entries: &[Vec<u8>]) -> Vec<u8> {
        let mut z = vec![0u8; pad];
        let offset = z.len() as u32;
        let dir: Vec<u8> = entries.concat();
        z.extend(&dir);
        z.extend(b"PK\x05\x06");
        z.extend([0u8; 4]);
        z.extend((entries.len() as u16).to_le_bytes());
        z.extend((entries.len() as u16).to_le_bytes());
        z.extend((dir.len() as u32).to_le_bytes());
        z.extend(offset.to_le_bytes());
        z.extend(3u16.to_le_bytes());
        z.extend(b"hi!");
        z
    }

    fn reader(z: &[u8]) -> impl FnMut(u64, u64) -> Result<Vec<u8>, String> + '_ {
        |a, b| Ok(z[a as usize..=b as usize].to_vec())
    }

    #[test]
    fn lists_names_and_sizes() {
        let z = archive(1000, &[entry("readme.txt", 120, false, None), entry("Fotos/été.jpg", 5000, true, None)]);
        let list = list(z.len() as u64, reader(&z)).unwrap();
        assert_eq!(
            list,
            [
                ZipEntry { name: "readme.txt".into(), size: 120 },
                ZipEntry { name: "Fotos/été.jpg".into(), size: 5000 }
            ]
        );
    }

    #[test]
    fn reads_a_directory_before_the_tail_and_zip64_sizes() {
        let big = 6u64 << 30;
        let z = archive(100_000, &[entry("disk.img", 0, false, Some(big))]);
        let mut reads = Vec::new();
        let total = z.len() as u64;
        let got = list(total, |a, b| {
            reads.push((a, b));
            Ok(z[a as usize..=b as usize].to_vec())
        })
        .unwrap();
        assert_eq!(got, [ZipEntry { name: "disk.img".into(), size: big }]);
        assert_eq!(reads.len(), 1, "the directory was inside the tail");
    }

    #[test]
    fn other_files_fail_gracefully() {
        let not = vec![7u8; 5000];
        assert!(list(not.len() as u64, reader(&not)).is_err());
        assert!(list(10, |_, _| Ok(Vec::new())).is_err());
        let failing = list(100_000, |_, _| Err("the server would not send a range".to_string()));
        assert_eq!(failing.unwrap_err(), "the server would not send a range");
    }
}
