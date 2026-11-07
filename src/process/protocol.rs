//! Messages between the compositor and the forker.
//!
//! One message is one seqpacket. Descriptors travel beside the bytes, through
//! `SCM_RIGHTS`, and are not part of this encoding. A message is at most
//! [`MAX_MESSAGE`] bytes and carries at most [`super::MAX_ATTACHED_FDS`]
//! descriptors.
//!
//! Integers are little-endian and fixed width. Variant order is the wire
//! discriminant, so variants and fields stay in this order.

use std::io::{Error, ErrorKind};

use {
    bincode::Options,
    serde::{de::DeserializeOwned, Serialize},
};

use super::ChildExit;

/// Largest encoded message. The compositor's receive buffer is this size.
pub(crate) const MAX_MESSAGE: usize = 32 * 1024;

/// Compositor to forker.
#[derive(Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
pub(crate) enum ToForker {
    /// Set or remove one variable in the forker's own environment.
    ///
    /// `None` removes `key`. Later spawns inherit the result.
    SetEnv { key: Vec<u8>, value: Option<Vec<u8>> },
    /// Spawn `program`. `args` does not include `argv[0]`.
    ///
    /// `slots[i]` is the child descriptor number of the i-th descriptor
    /// attached to this packet. The two lists have the same length, at most
    /// [`super::MAX_ATTACHED_FDS`]. An env value of `None` removes that variable for
    /// this child only.
    Spawn {
        token: u64,
        program: Vec<u8>,
        args: Vec<Vec<u8>>,
        env: Vec<(Vec<u8>, Option<Vec<u8>>)>,
        slots: Vec<i32>,
    },
}

/// Forker to compositor.
#[derive(Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
pub(crate) enum FromForker {
    /// A line from the forker. `level` is [`log::Level`] as `u8` (`Error` is 1).
    Log { level: u8, message: String },
    /// `program` exec'd. The packet carries the child's pidfd.
    Spawned { token: u64, pid: i32 },
    /// The child never exec'd.
    SpawnFailed { token: u64, error: String },
    /// A spawned child stopped running.
    Exited { token: u64, exit: ChildExit },
}

pub(crate) fn encode(message: &impl Serialize) -> std::io::Result<Vec<u8>> {
    wire().serialize(message).map_err(invalid)
}

pub(crate) fn decode<T: DeserializeOwned>(bytes: &[u8]) -> std::io::Result<T> {
    if bytes.len() > MAX_MESSAGE {
        return Err(Error::new(ErrorKind::InvalidData, "message exceeds 32 KiB"));
    }
    wire().deserialize(bytes).map_err(invalid)
}

fn wire() -> impl bincode::Options {
    bincode::options()
        .with_little_endian()
        .with_fixint_encoding()
        .with_limit(MAX_MESSAGE as u64)
        .reject_trailing_bytes()
}

fn invalid(error: bincode::Error) -> Error {
    Error::new(ErrorKind::InvalidData, error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T>(message: &T)
    where
        T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let bytes = encode(message).unwrap();
        assert!(bytes.len() <= MAX_MESSAGE);
        assert_eq!(decode::<T>(&bytes).unwrap(), *message);
    }

    #[test]
    fn every_request_round_trips() {
        round_trip(&ToForker::SetEnv { key: b"DISPLAY".to_vec(), value: None });
        round_trip(&ToForker::SetEnv { key: b"FOO".to_vec(), value: Some(b"bar".to_vec()) });
        round_trip(&ToForker::Spawn {
            token: 7,
            program: b"sh".to_vec(),
            args: vec![b"-lc".to_vec(), b"echo hi".to_vec()],
            env: vec![(b"FOO".to_vec(), Some(b"bar".to_vec())), (b"DISPLAY".to_vec(), None)],
            slots: vec![5, 6],
        });
    }

    #[test]
    fn every_reply_round_trips() {
        round_trip(&FromForker::Log { level: log::Level::Info as u8, message: "started".into() });
        round_trip(&FromForker::Spawned { token: 7, pid: 42 });
        round_trip(&FromForker::SpawnFailed {
            token: 7,
            error: "No such file or directory".into(),
        });
        round_trip(&FromForker::Exited { token: 7, exit: ChildExit::Code(3) });
        round_trip(&FromForker::Exited { token: 8, exit: ChildExit::Signal(9) });
    }

    #[test]
    fn a_message_can_fill_the_buffer_but_not_exceed_it() {
        let empty = encode(&ToForker::SetEnv { key: Vec::new(), value: Some(Vec::new()) }).unwrap();
        let fit = MAX_MESSAGE - empty.len();
        let full = ToForker::SetEnv { key: Vec::new(), value: Some(vec![0; fit]) };
        let bytes = encode(&full).unwrap();
        assert_eq!(bytes.len(), MAX_MESSAGE);
        assert_eq!(decode::<ToForker>(&bytes).unwrap(), full);

        let over = ToForker::SetEnv { key: Vec::new(), value: Some(vec![0; fit + 1]) };
        let error = encode(&over).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn garbage_bytes_are_rejected() {
        assert_eq!(decode::<ToForker>(&[]).unwrap_err().kind(), ErrorKind::InvalidData);
        assert_eq!(decode::<FromForker>(&[0xff; 8]).unwrap_err().kind(), ErrorKind::InvalidData);

        let bytes = encode(&ToForker::SetEnv { key: b"A".to_vec(), value: None }).unwrap();
        assert!(decode::<ToForker>(&bytes[..bytes.len() - 1]).is_err());
        let mut extra = bytes.clone();
        extra.push(0);
        assert!(decode::<ToForker>(&extra).is_err());
        assert!(decode::<ToForker>(&vec![0; MAX_MESSAGE + 1]).is_err());
    }
}
