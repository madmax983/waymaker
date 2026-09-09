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

    /// Bytes after a complete value are a refusal, not a value.
    ///
    /// `postcard::from_bytes` reads a *prefix* and never asks what follows, so this is the
    /// half of the contract that a codec does not give for free. It is the case a firmware
    /// meets after narrowing a result type: the old record still starts with a legal value.
    #[test]
    fn trailing_bytes_are_refused_rather_than_ignored() {
        let refused = FromPostcard::<(u8, u16)>::decode(&[7, 0x2a, 0xff, 0xff]);

        assert_eq!(
            refused.err(),
            Some(::postcard::Error::DeserializeBadEncoding)
        );
    }

    /// A shorter type does not quietly read a longer record.
    #[test]
    fn a_narrowed_type_does_not_read_an_old_record() {
        let recorded = [7_u8, 0x2a];

        assert!(FromPostcard::<(u8, u16)>::decode(&recorded).is_ok());
        assert!(FromPostcard::<u8>::decode(&recorded).is_err());
    }

    /// A decoded value owns what it kept.
    ///
    /// `bytes` is the caller's buffer and the next boundary overwrites it. That a value
    /// *cannot* borrow it is the `compile_fail` doctest on `FromPostcard`, because
    /// `Decode::decode` returns a `Self` that no elided lifetime can reach. What this adds
    /// is the other half: the value survives the buffer it was read from.
    #[test]
    fn a_decoded_value_outlives_the_buffer_it_was_read_from() {
        let mut buffer = [7_u8, 0x2a];
        let coded = FromPostcard::<(u8, u16)>::decode(&buffer).expect("two postcard varints");
        buffer.fill(0);

        assert_eq!(coded.into_inner(), (7, 42));
    }

    /// A workflow's own type, derived through the re-exported serde.
    ///
    /// The derive comes from `decode::serde`, not from a `serde` dependency of this test.
    /// That is what the re-export is for, and it works in every configuration that has the
    /// module because the `serde` feature turns `serde/derive` on.
    #[derive(waymaker_embassy::decode::serde::Deserialize, Clone, PartialEq, Eq, Debug)]
    #[serde(crate = "waymaker_embassy::decode::serde")]
    struct Report {
        first: u8,
        second: u16,
    }

    /// A derived workflow type decodes, and the wrapper carries its `Clone`.
    ///
    /// `Report` is not `Copy`, so the clone is a real one. The wrapper carries no `Debug`:
    /// that would forward to `T`'s and pull `core::fmt` into a firmware that only wanted to
    /// decode, so `into_inner` is what a caller prints.
    #[test]
    fn a_derived_type_decodes_and_the_wrapper_clones_it() {
        let coded = FromPostcard::<Report>::decode(&[7, 0x2a]).expect("two postcard varints");
        let kept = coded.clone().into_inner();

        assert_eq!(kept, coded.into_inner());
        assert_eq!(
            kept,
            Report {
                first: 7,
                second: 42
            }
        );
    }

    /// The wrapper is `Copy` when its value is.
    #[test]
    fn the_wrapper_copies_when_the_value_does() {
        let coded = FromPostcard::<(u8, u16)>::decode(&[7, 0x2a]).expect("two postcard varints");
        let copied = coded;

        assert_eq!(coded.into_inner(), (7, 42));
        assert_eq!(copied.into_inner(), (7, 42));
    }
}
