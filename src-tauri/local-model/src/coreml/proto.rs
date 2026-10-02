//! Just enough protobuf encoding to write a Core ML model specification.
//!
//! Only the writer side is needed and the message set is small (Model.proto,
//! MIL.proto, FeatureTypes.proto from coremltools), so the few messages are
//! built by hand instead of pulling in a protobuf code generator.

#[derive(Default, Clone)]
pub struct Msg(Vec<u8>);

const VARINT: u32 = 0;
const LEN: u32 = 2;

impl Msg {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    fn key(&mut self, field: u32, wire: u32) {
        self.raw_varint(((field as u64) << 3) | wire as u64);
    }

    fn raw_varint(&mut self, mut value: u64) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                self.0.push(byte);
                return;
            }
            self.0.push(byte | 0x80);
        }
    }

    pub fn uint(&mut self, field: u32, value: u64) -> &mut Self {
        self.key(field, VARINT);
        self.raw_varint(value);
        self
    }

    /// int32/int64 and enums: negative values are sign-extended to ten bytes.
    pub fn int(&mut self, field: u32, value: i64) -> &mut Self {
        self.uint(field, value as u64)
    }

    pub fn boolean(&mut self, field: u32, value: bool) -> &mut Self {
        self.uint(field, value as u64)
    }

    pub fn bytes(&mut self, field: u32, value: &[u8]) -> &mut Self {
        self.key(field, LEN);
        self.raw_varint(value.len() as u64);
        self.0.extend_from_slice(value);
        self
    }

    pub fn string(&mut self, field: u32, value: &str) -> &mut Self {
        self.bytes(field, value.as_bytes())
    }

    pub fn message(&mut self, field: u32, value: &Msg) -> &mut Self {
        self.bytes(field, &value.0)
    }

    pub fn packed_i32(&mut self, field: u32, values: &[i32]) -> &mut Self {
        let mut body = Msg::new();
        for value in values {
            body.raw_varint(*value as i64 as u64);
        }
        self.bytes(field, &body.0)
    }

    pub fn packed_i64(&mut self, field: u32, values: &[i64]) -> &mut Self {
        let mut body = Msg::new();
        for value in values {
            body.raw_varint(*value as u64);
        }
        self.bytes(field, &body.0)
    }

    pub fn packed_bool(&mut self, field: u32, values: &[bool]) -> &mut Self {
        let body: Vec<u8> = values.iter().map(|value| *value as u8).collect();
        self.bytes(field, &body)
    }

    /// One entry of a `map<string, V>` field.
    pub fn map_entry(&mut self, field: u32, key: &str, value: &Msg) -> &mut Self {
        let mut entry = Msg::new();
        entry.string(1, key).message(2, value);
        self.message(field, &entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_like_protoc() {
        let mut msg = Msg::new();
        msg.uint(1, 150).string(2, "testing").int(3, -1).packed_i32(4, &[3, 270]);
        assert_eq!(
            msg.into_bytes(),
            [
                0x08, 0x96, 0x01, // field 1 = 150
                0x12, 0x07, b't', b'e', b's', b't', b'i', b'n', b'g', // field 2
                0x18, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01, // field 3 = -1
                0x22, 0x03, 0x03, 0x8e, 0x02, // packed [3, 270]
            ]
        );
    }

    #[test]
    fn nests_messages_and_maps() {
        let mut inner = Msg::new();
        inner.uint(1, 1);
        let mut outer = Msg::new();
        outer.map_entry(2, "k", &inner);
        assert_eq!(outer.into_bytes(), [0x12, 0x07, 0x0a, 0x01, b'k', 0x12, 0x02, 0x08, 0x01]);
    }
}
