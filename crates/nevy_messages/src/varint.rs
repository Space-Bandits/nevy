#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VarInt(u64);

#[derive(Default)]
pub struct VarIntDecoder {
    current: u64,
    received_bytes: usize,
}

impl VarInt {
    pub fn from_u64(value: u64) -> Option<Self> {
        if value <= u64::MAX >> 7 {
            Some(VarInt(value))
        } else {
            None
        }
    }

    pub fn as_u64(self) -> u64 {
        self.0
    }

    pub fn encode(&self, buffer: &mut Vec<u8>) {
        let &VarInt(mut value) = self;
        let mut bytes_used = 0;

        loop {
            let limit_reached = bytes_used >= 7;

            let mask = if limit_reached {
                0b1111_1111
            } else {
                0b0111_1111
            };
            let byte = value as u8 & mask;

            value = value >> 7;
            let continuation_bit = value != 0;

            buffer.push(byte | ((continuation_bit as u8) << 7));

            bytes_used += 1;
            if continuation_bit {
                continue;
            }

            return;
        }
    }
}

impl VarIntDecoder {
    pub fn push_byte(&mut self, mut byte: u8) -> Option<VarInt> {
        let limit_reached = self.received_bytes >= 7;
        let continuation_bit = (byte & 1u8 << 7) != 0;

        if !limit_reached {
            byte = byte & !(1u8 << 7);
        }

        self.current |= (byte as u64) << (7 * self.received_bytes);

        self.received_bytes += 1;

        if !continuation_bit || limit_reached {
            let out = VarInt(self.current);
            self.current = 0;
            self.received_bytes = 0;
            Some(out)
        } else {
            None
        }
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) -> (Option<VarInt>, usize) {
        let mut taken = 0;

        for &byte in bytes.iter() {
            taken += 1;

            let Some(out) = self.push_byte(byte) else {
                continue;
            };

            return (Some(out), taken);
        }

        (None, taken)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_byte() {
        let mut decoder = VarIntDecoder::default();

        assert_eq!(decoder.push_byte(0b01111111), Some(VarInt(0b1111111)));
    }

    #[test]
    fn two_byte() {
        let mut decoder = VarIntDecoder::default();

        assert_eq!(decoder.push_byte(0b11111111), None);
        assert_eq!(
            decoder.push_byte(0b01111111),
            Some(VarInt(0b1111111_1111111))
        );
    }

    #[test]
    fn eight_byte() {
        let mut decoder = VarIntDecoder::default();

        assert_eq!(decoder.push_byte(0b11111111), None);
        assert_eq!(decoder.push_byte(0b11111111), None);
        assert_eq!(decoder.push_byte(0b11111111), None);
        assert_eq!(decoder.push_byte(0b11111111), None);
        assert_eq!(decoder.push_byte(0b11111111), None);
        assert_eq!(decoder.push_byte(0b11111111), None);
        assert_eq!(decoder.push_byte(0b11111111), None);
        assert_eq!(
            decoder.push_byte(0b11111111),
            Some(VarInt(
                0b1_1111111_1111111_1111111_1111111_1111111_1111111_1111111_1111111
            ))
        );
    }

    #[test]
    fn overflow() {
        let mut decoder = VarIntDecoder::default();

        assert_eq!(
            decoder.push_bytes(&[
                0b1111_1111,
                0b1111_1111,
                0b1111_1111,
                0b1111_1111,
                0b1111_1111,
                0b1111_1111,
                0b1111_1111,
                0b1111_1111,
                0b1111_1111
            ]),
            (
                Some(VarInt(
                    0b1_1111111_1111111_1111111_1111111_1111111_1111111_1111111_1111111
                )),
                8
            )
        );
    }

    #[test]
    fn creation_guard() {
        assert!(
            VarInt::from_u64(0b1_1111111_1111111_1111111_1111111_1111111_1111111_1111111_1111111)
                .is_some()
        );
        assert!(
            VarInt::from_u64(0b10_0000000_0000000_0000000_0000000_0000000_0000000_0000000_0000000)
                .is_none()
        );
    }

    #[test]
    fn encode() {
        let number =
            VarInt::from_u64(0b1_1111110_1111101_1111011_1110111_1101111_1011111_0111111_1111111)
                .unwrap();

        let mut buffer = Vec::<u8>::new();
        number.encode(&mut buffer);

        let mut decoder = VarIntDecoder::default();
        assert_eq!(decoder.push_bytes(&buffer).0, Some(number));
    }
}
