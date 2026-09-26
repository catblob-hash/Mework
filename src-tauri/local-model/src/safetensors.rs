//! Read-only access to a `.safetensors` file, memory-mapped.
//!
//! The format is an 8-byte little-endian header length, a JSON header naming
//! each tensor's dtype, shape and byte range, then the raw data. Mapping the
//! file keeps the weights as clean, file-backed pages: converters read them
//! once, and the system can drop them again without writing anything back.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

use memmap2::Mmap;
use serde::Deserialize;

/// Headers larger than this are not weights files.
const MAX_HEADER_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dtype {
    F32,
    F16,
    BF16,
}

impl Dtype {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "F32" => Some(Self::F32),
            "F16" => Some(Self::F16),
            "BF16" => Some(Self::BF16),
            _ => None,
        }
    }

    pub fn size(self) -> usize {
        match self {
            Self::F32 => 4,
            Self::F16 | Self::BF16 => 2,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TensorInfo {
    pub dtype: Dtype,
    pub shape: Vec<usize>,
    start: usize,
    end: usize,
}

impl TensorInfo {
    pub fn element_count(&self) -> usize {
        self.shape.iter().product()
    }
}

pub struct SafeTensors {
    map: Mmap,
    data_start: usize,
    tensors: BTreeMap<String, TensorInfo>,
}

#[derive(Deserialize)]
struct RawEntry {
    dtype: String,
    shape: Vec<usize>,
    data_offsets: [usize; 2],
}

impl SafeTensors {
    pub fn open(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|error| format!("无法打开权重文件 {}: {error}", path.display()))?;
        // SAFETY: the file is opened read-only and the model directory belongs to
        // the app; a concurrent writer would be a bug elsewhere, not a data race
        // this reader could observe as undefined behaviour beyond wrong values.
        let map = unsafe { Mmap::map(&file) }.map_err(|error| format!("无法映射权重文件: {error}"))?;
        Self::from_map(map)
    }

    fn from_map(map: Mmap) -> Result<Self, String> {
        if map.len() < 8 {
            return Err("权重文件过短".into());
        }
        let header_len = u64::from_le_bytes(map[..8].try_into().expect("8 bytes"));
        if header_len > MAX_HEADER_BYTES || 8 + header_len as usize > map.len() {
            return Err("权重文件头损坏".into());
        }
        let data_start = 8 + header_len as usize;
        let header: BTreeMap<String, serde_json::Value> =
            serde_json::from_slice(&map[8..data_start]).map_err(|error| format!("权重文件头无法解析: {error}"))?;
        let data_len = map.len() - data_start;
        let mut tensors = BTreeMap::new();
        for (name, value) in header {
            if name == "__metadata__" {
                continue;
            }
            let raw: RawEntry =
                serde_json::from_value(value).map_err(|error| format!("张量 {name} 的描述无法解析: {error}"))?;
            let Some(dtype) = Dtype::parse(&raw.dtype) else {
                return Err(format!("张量 {name} 的类型 {} 不受支持", raw.dtype));
            };
            let [start, end] = raw.data_offsets;
            let expected = raw.shape.iter().product::<usize>() * dtype.size();
            if start > end || end > data_len || end - start != expected {
                return Err(format!("张量 {name} 的数据范围与形状不符"));
            }
            tensors.insert(name, TensorInfo { dtype, shape: raw.shape, start, end });
        }
        Ok(Self { map, data_start, tensors })
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tensors.keys().map(String::as_str)
    }

    pub fn info(&self, name: &str) -> Option<&TensorInfo> {
        self.tensors.get(name)
    }

    pub fn tensor(&self, name: &str) -> Result<Tensor<'_>, String> {
        let info = self.tensors.get(name).ok_or_else(|| format!("权重里缺少张量 {name}"))?;
        Ok(Tensor {
            dtype: info.dtype,
            shape: &info.shape,
            bytes: &self.map[self.data_start + info.start..self.data_start + info.end],
        })
    }
}

#[derive(Clone, Copy)]
pub struct Tensor<'a> {
    pub dtype: Dtype,
    pub shape: &'a [usize],
    pub bytes: &'a [u8],
}

impl Tensor<'_> {
    pub fn len(&self) -> usize {
        self.bytes.len() / self.dtype.size()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn get_f32(&self, index: usize) -> f32 {
        let size = self.dtype.size();
        let raw = &self.bytes[index * size..index * size + size];
        match self.dtype {
            Dtype::F32 => f32::from_le_bytes(raw.try_into().expect("4 bytes")),
            Dtype::F16 => f16_to_f32(u16::from_le_bytes(raw.try_into().expect("2 bytes"))),
            Dtype::BF16 => bf16_to_f32(u16::from_le_bytes(raw.try_into().expect("2 bytes"))),
        }
    }

    pub fn to_f32(&self) -> Vec<f32> {
        (0..self.len()).map(|index| self.get_f32(index)).collect()
    }
}

pub fn bf16_to_f32(bits: u16) -> f32 {
    f32::from_bits((bits as u32) << 16)
}

pub fn f16_to_f32(bits: u16) -> f32 {
    let sign = ((bits >> 15) as u32) << 31;
    let exponent = ((bits >> 10) & 0x1f) as u32;
    let mantissa = (bits & 0x3ff) as u32;
    let value = match exponent {
        0 if mantissa == 0 => sign,
        0 => {
            // Subnormal: renormalize into an f32 exponent.
            let mut exponent = 127 - 15 + 1;
            let mut mantissa = mantissa;
            while mantissa & 0x400 == 0 {
                mantissa <<= 1;
                exponent -= 1;
            }
            sign | ((exponent as u32) << 23) | ((mantissa & 0x3ff) << 13)
        }
        0x1f => sign | 0x7f80_0000 | (mantissa << 13),
        _ => sign | ((exponent + 127 - 15) << 23) | (mantissa << 13),
    };
    f32::from_bits(value)
}

/// Round-to-nearest-even, saturating to infinity like IEEE conversion does.
pub fn f32_to_f16(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32;
    let mantissa = bits & 0x7f_ffff;
    if exponent == 0xff {
        let nan = if mantissa != 0 { 0x200 | (mantissa >> 13) as u16 } else { 0 };
        return sign | 0x7c00 | nan;
    }
    let half_exponent = exponent - 127 + 15;
    if half_exponent >= 0x1f {
        return sign | 0x7c00;
    }
    if half_exponent <= 0 {
        if half_exponent < -10 {
            return sign;
        }
        let mantissa = mantissa | 0x80_0000;
        let shift = (14 - half_exponent) as u32;
        let half = mantissa >> shift;
        let remainder = mantissa & ((1 << shift) - 1);
        let halfway = 1 << (shift - 1);
        let rounded = if remainder > halfway || (remainder == halfway && half & 1 == 1) { half + 1 } else { half };
        return sign | rounded as u16;
    }
    let half = ((half_exponent as u32) << 10) | (mantissa >> 13);
    let remainder = mantissa & 0x1fff;
    let rounded = if remainder > 0x1000 || (remainder == 0x1000 && half & 1 == 1) { half + 1 } else { half };
    // A carry out of the mantissa correctly bumps the exponent, up to infinity.
    sign | rounded as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn f16_round_trips_representable_values() {
        for value in [0.0f32, -0.0, 1.0, -2.5, 65504.0, 6.1035156e-5, 5.9604645e-8, 0.33325195] {
            assert_eq!(f16_to_f32(f32_to_f16(value)), value, "{value}");
        }
        assert_eq!(f32_to_f16(1e6), 0x7c00);
        assert_eq!(f32_to_f16(-1e6), 0xfc00);
        assert!(f16_to_f32(f32_to_f16(f32::NAN)).is_nan());
        // Ties go to even: 1 + 2^-11 lies between 1 and the next f16.
        assert_eq!(f32_to_f16(1.0 + 1.0 / 2048.0), 0x3c00);
        assert_eq!(f32_to_f16(1.0 + 3.0 / 2048.0), 0x3c02);
    }

    #[test]
    fn f16_matches_exhaustive_decode_then_encode() {
        for bits in 0u16..=0xffff {
            let value = f16_to_f32(bits);
            if value.is_nan() {
                continue;
            }
            assert_eq!(f32_to_f16(value), bits, "{bits:#06x}");
        }
    }

    #[test]
    fn reads_tensors_and_rejects_bad_ranges() {
        let header = r#"{"a":{"dtype":"BF16","shape":[2],"data_offsets":[0,4]},"__metadata__":{"format":"pt"}}"#;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&(header.len() as u64).to_le_bytes()).unwrap();
        file.write_all(header.as_bytes()).unwrap();
        file.write_all(&[0x80, 0x3f, 0x00, 0xc0]).unwrap();
        let tensors = SafeTensors::open(file.path()).unwrap();
        let a = tensors.tensor("a").unwrap();
        assert_eq!(a.to_f32(), vec![1.0, -2.0]);
        assert!(tensors.tensor("missing").is_err());

        let bad = r#"{"a":{"dtype":"BF16","shape":[3],"data_offsets":[0,4]}}"#;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&(bad.len() as u64).to_le_bytes()).unwrap();
        file.write_all(bad.as_bytes()).unwrap();
        file.write_all(&[0; 4]).unwrap();
        assert!(SafeTensors::open(file.path()).is_err());
    }
}
