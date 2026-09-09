//! The optional codec helpers, over borrowed bytes.
//!
//! Design document §02 decision 4: a codec is a convenience, never a wire-format
//! requirement. These tests are about what the helpers do with bytes a journal already
//! holds. That the bytes are durable is `waymaker-flash`'s, and that replay hands back the
//! same ones is `waymaker-core`'s.
//!
//! The file is behind the `serde` feature, which the `postcard` feature enables. A default
//! build compiles none of it, which is the point.

#![cfg(feature = "serde")]

use waymaker_embassy::Decode;
use waymaker_embassy::decode::{Coded, Format};

/// A format that reads one byte, so the bridge can be tested without a codec.
struct OneByte;

/// The bytes are not one byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NotOneByte;

impl Format for OneByte {
    type Error = NotOneByte;

    fn read<T: waymaker_embassy::decode::DeserializeOwned>(bytes: &[u8]) -> Result<T, NotOneByte> {
        use waymaker_embassy::decode::serde::de::IntoDeserializer as _;

        let [byte] = bytes else {
            return Err(NotOneByte);
        };
        T::deserialize(byte.into_deserializer())
            .map_err(|_ignored: waymaker_embassy::decode::serde::de::value::Error| NotOneByte)
    }
}

/// The bridge names a format the caller chose, and hands back the value it read.
#[test]
fn a_format_the_caller_chose_reads_the_value() {
    let coded = Coded::<OneByte, u8>::decode(&[7]).expect("one byte is a u8");

    assert_eq!(coded.into_inner(), 7);
}

/// A decode failure is the format's own error, not a second vocabulary.
///
/// The workflow sees why its own codec refused. A wrapper error would tell it only that
/// something refused, and the bytes are already committed, so the same call fails the same
/// way on every boot.
#[test]
fn the_error_is_the_formats_own() {
    let refused = Coded::<OneByte, u8>::decode(&[1, 2]);

    assert_eq!(refused.err(), Some(NotOneByte));
}

/// The answer a workflow does not read accepts any bytes.
#[test]
fn the_unread_answer_accepts_anything() {
    assert!(<()>::decode(&[9, 9, 9]).is_ok());
}

#[cfg(feature = "postcard")]
mod postcard_format {
    use waymaker_embassy::Decode;
    use waymaker_embassy::decode::FromPostcard;

    /// Postcard reads a value out of the bytes an activity recorded.
    ///
    /// A tuple rather than a derived struct: the helper is what is under test, and a
    /// `#[derive(Deserialize)]` here would put `serde_derive` — a proc macro — into a
    /// firmware crate's test build for nothing.
    #[test]
    fn postcard_reads_a_recorded_answer() {
        let coded = FromPostcard::<(u8, u16)>::decode(&[7, 0x2a]).expect("two postcard varints");

        assert_eq!(coded.into_inner(), (7, 42));
    }

    /// Bytes postcard cannot read are its own error.
    #[test]
    fn truncated_bytes_are_a_postcard_error() {
        let refused = FromPostcard::<(u8, u16)>::decode(&[7]);

        assert_eq!(
            refused.err(),
            Some(::postcard::Error::DeserializeUnexpectedEnd)
        );
    }

    /// A decoded value owns what it kept.
    ///
    /// `bytes` is the caller's buffer and the next boundary overwrites it, so a value that
    /// borrowed it would be a dangling read one `.await` later. The bound is
    /// `DeserializeOwned`, and this is what says so: the buffer is overwritten and the
    /// value is unchanged.
    #[test]
    fn a_decoded_value_does_not_borrow_the_buffer() {
        let mut buffer = [7_u8, 0x2a];
        let coded = FromPostcard::<(u8, u16)>::decode(&buffer).expect("two postcard varints");
        buffer.fill(0);

        assert_eq!(coded.into_inner(), (7, 42));
    }
}
