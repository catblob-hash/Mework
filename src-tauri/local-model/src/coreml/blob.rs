//! The weight file of an ML Program (`weights/weight.bin`, MILBlob storage
//! format v2): a 64-byte header, then per blob a 64-byte metadata record and
//! the data, each 64-byte aligned. Program constants refer to a blob by the
//! offset of its metadata record.
//!
//! Offsets are planned first, while the graph is built, and the data is then
//! streamed from the checkpoint in one pass, so the 1.5 GB of fp16 weights
//! never sit in memory.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::safetensors::{f32_to_f16, SafeTensors};

const ALIGN: u64 = 64;
const SENTINEL: u32 = 0xDEAD_BEEF;
const FLOAT16: u32 = 1;
pub const CANCELLED: &str = "模型构建已取消";

pub enum BlobSource {
    /// Rows `rows` of a checkpoint tensor (all of it when `None`), times `scale`.
    Checkpoint { tensor: String, rows: Option<(usize, usize)>, scale: f32 },
    Owned(Vec<u16>),
}

struct Entry {
    metadata: u64,
    data: u64,
    elements: usize,
    source: BlobSource,
}

pub struct BlobPlan {
    entries: Vec<Entry>,
    by_key: HashMap<String, u64>,
    end: u64,
}

fn align(offset: u64) -> u64 {
    offset.div_ceil(ALIGN) * ALIGN
}

impl Default for BlobPlan {
    fn default() -> Self {
        Self { entries: Vec::new(), by_key: HashMap::new(), end: ALIGN }
    }
}

impl BlobPlan {
    /// Reserves a blob of `elements` fp16 values and returns its metadata
    /// offset. The same `key` always gets the same blob, which is how the
    /// functions of one model share weights.
    pub fn add(&mut self, key: &str, elements: usize, source: BlobSource) -> u64 {
        if let Some(offset) = self.by_key.get(key) {
            return *offset;
        }
        let metadata = align(self.end);
        let data = align(metadata + ALIGN);
        self.end = data + elements as u64 * 2;
        self.entries.push(Entry { metadata, data, elements, source });
        self.by_key.insert(key.to_string(), metadata);
        metadata
    }

    pub fn total_bytes(&self) -> u64 {
        self.end
    }

    pub fn write(
        &self,
        path: &Path,
        checkpoint: &SafeTensors,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<(), String> {
        let file = File::create(path).map_err(|error| format!("无法创建权重文件: {error}"))?;
        let mut out = BufWriter::with_capacity(1 << 20, file);
        let mut position = 0u64;
        let io = |error: std::io::Error| format!("写入权重文件失败: {error}");

        let mut header = [0u8; 64];
        header[0..4].copy_from_slice(&(self.entries.len() as u32).to_le_bytes());
        header[4..8].copy_from_slice(&2u32.to_le_bytes());
        out.write_all(&header).map_err(io)?;
        position += 64;

        let mut buffer = Vec::with_capacity(1 << 20);
        for entry in &self.entries {
            if cancel.load(Ordering::Acquire) {
                return Err(CANCELLED.into());
            }
            pad(&mut out, &mut position, entry.metadata).map_err(io)?;
            let mut metadata = [0u8; 64];
            metadata[0..4].copy_from_slice(&SENTINEL.to_le_bytes());
            metadata[4..8].copy_from_slice(&FLOAT16.to_le_bytes());
            metadata[8..16].copy_from_slice(&(entry.elements as u64 * 2).to_le_bytes());
            metadata[16..24].copy_from_slice(&entry.data.to_le_bytes());
            out.write_all(&metadata).map_err(io)?;
            position += 64;
            pad(&mut out, &mut position, entry.data).map_err(io)?;

            match &entry.source {
                BlobSource::Owned(values) => {
                    assert_eq!(values.len(), entry.elements);
                    buffer.clear();
                    buffer.extend(values.iter().flat_map(|v| v.to_le_bytes()));
                    out.write_all(&buffer).map_err(io)?;
                }
                BlobSource::Checkpoint { tensor, rows, scale } => {
                    let view = checkpoint.tensor(tensor)?;
                    let row_len = if view.shape.len() >= 2 { view.len() / view.shape[0] } else { view.len() };
                    let (start, end) = match rows {
                        Some((a, b)) => (a * row_len, b * row_len),
                        None => (0, view.len()),
                    };
                    assert_eq!(end - start, entry.elements, "blob size for {tensor}");
                    const CHUNK: usize = 1 << 19;
                    let mut index = start;
                    while index < end {
                        let stop = (index + CHUNK).min(end);
                        buffer.clear();
                        for i in index..stop {
                            buffer.extend_from_slice(&f32_to_f16(view.get_f32(i) * scale).to_le_bytes());
                        }
                        out.write_all(&buffer).map_err(io)?;
                        index = stop;
                        if cancel.load(Ordering::Acquire) {
                            return Err(CANCELLED.into());
                        }
                    }
                }
            }
            position += entry.elements as u64 * 2;
            progress(position, self.end);
        }
        out.flush().map_err(io)?;
        out.into_inner().map_err(|error| io(error.into_error()))?.sync_all().map_err(io)?;
        Ok(())
    }
}

/// Reads the fp16 blob whose metadata record is at `metadata` in a weight
/// file: its data offset and size in bytes, checked against the file.
pub fn read_record(file: &[u8], metadata: u64) -> Result<(u64, u64), String> {
    let at = usize::try_from(metadata).map_err(|_| "权重记录偏移越界".to_string())?;
    let record = file.get(at..at + 64).ok_or("权重记录偏移越界")?;
    let word = |range: std::ops::Range<usize>| u64::from_le_bytes(record[range].try_into().expect("8 bytes"));
    let sentinel = u32::from_le_bytes(record[0..4].try_into().expect("4 bytes"));
    let dtype = u32::from_le_bytes(record[4..8].try_into().expect("4 bytes"));
    let (size, data) = (word(8..16), word(16..24));
    if sentinel != SENTINEL || dtype != FLOAT16 {
        return Err(format!("偏移 {metadata} 处不是 fp16 权重记录"));
    }
    if data.checked_add(size).is_none_or(|end| end > file.len() as u64) {
        return Err(format!("偏移 {metadata} 处的权重超出文件"));
    }
    Ok((data, size))
}

fn pad(out: &mut impl Write, position: &mut u64, target: u64) -> std::io::Result<()> {
    debug_assert!(target >= *position);
    let zeros = [0u8; 64];
    while *position < target {
        let n = ((target - *position) as usize).min(zeros.len());
        out.write_all(&zeros[..n])?;
        *position += n as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_aligned_offsets_and_dedups() {
        let mut plan = BlobPlan::default();
        let a = plan.add("a", 3, BlobSource::Owned(vec![1, 2, 3]));
        let b = plan.add("b", 40, BlobSource::Owned(vec![0; 40]));
        assert_eq!(a, 64);
        assert_eq!(b, 192); // a's data at 128..134, next metadata aligned up
        assert_eq!(plan.add("a", 3, BlobSource::Owned(vec![9, 9, 9])), a);
    }

    #[test]
    fn writes_the_milblob_layout() {
        let mut plan = BlobPlan::default();
        plan.add("a", 2, BlobSource::Owned(vec![0x3c00, 0xc000]));
        let dir = tempfile::tempdir().unwrap();
        // An empty checkpoint: owned blobs never touch it.
        let ckpt_path = dir.path().join("empty.safetensors");
        let header = b"{}";
        let mut raw = (header.len() as u64).to_le_bytes().to_vec();
        raw.extend_from_slice(header);
        std::fs::write(&ckpt_path, raw).unwrap();
        let ckpt = SafeTensors::open(&ckpt_path).unwrap();
        let out = dir.path().join("weight.bin");
        plan.write(&out, &ckpt, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
        let bytes = std::fs::read(out).unwrap();
        assert_eq!(&bytes[0..8], &[1, 0, 0, 0, 2, 0, 0, 0]);
        assert_eq!(&bytes[64..68], &SENTINEL.to_le_bytes());
        assert_eq!(u64::from_le_bytes(bytes[72..80].try_into().unwrap()), 4);
        assert_eq!(u64::from_le_bytes(bytes[80..88].try_into().unwrap()), 128);
        assert_eq!(&bytes[128..132], &[0x00, 0x3c, 0x00, 0xc0]);
        assert_eq!(read_record(&bytes, 64), Ok((128, 4)));
        assert!(read_record(&bytes, 0).is_err(), "the file header is not a record");
        assert!(read_record(&bytes, 128).is_err());
    }
}
