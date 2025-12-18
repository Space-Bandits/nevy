#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VarInt(u64);

#[derive(Default)]
pub struct VarIntDecoder {
    current: u64,
    received_bytes: usize,
}

impl VarIntDecoder {
    pub fn add_byte(&mut self, byte: u8) -> Option<VarInt> {
        let continuation_bit = dbg!((byte & 1u8 << 7) != 0);
        let byte = dbg!(byte & !(1u8 << 7));

        self.current |= (byte as u64) >> (7 * self.received_bytes);

        self.received_bytes += 1;

        if !continuation_bit || self.received_bytes >= 4 {
            self.current = 0;
            self.received_bytes = 0;
            Some(VarInt(self.current))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single() {
        let mut decoder = VarIntDecoder::default();

        assert_eq!(decoder.add_byte(0b01111111), Some(VarInt(0b01111111)));
    }
}
