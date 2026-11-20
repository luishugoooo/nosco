//! The v1 cache stores length-prefixed arrays in little-endian order.

use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::ops::Range;
use std::path::Path;

pub(crate) fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(crate) struct Reader {
    file: BufReader<File>,
    remaining: u64,
}

impl Reader {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let remaining = file.metadata()?.len();
        Ok(Self {
            file: BufReader::new(file),
            remaining,
        })
    }

    pub(crate) fn read(&mut self, bytes: &mut [u8]) -> io::Result<()> {
        self.remaining = self
            .remaining
            .checked_sub(bytes.len() as u64)
            .ok_or_else(|| invalid("truncated cache"))?;
        self.file.read_exact(bytes)
    }

    fn bytes(&mut self, width: usize) -> io::Result<Vec<u8>> {
        let mut count = [0; 4];
        self.read(&mut count)?;
        let len = (u32::from_le_bytes(count) as usize)
            .checked_mul(width)
            .ok_or_else(|| invalid("array exceeds address space"))?;
        self.remaining = self
            .remaining
            .checked_sub(len as u64)
            .ok_or_else(|| invalid("array exceeds cache length"))?;
        let mut bytes = Vec::with_capacity(len);
        self.file
            .by_ref()
            .take(len as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() != len {
            return Err(invalid("truncated cache"));
        }
        Ok(bytes)
    }

    pub(crate) fn string(&mut self) -> io::Result<String> {
        String::from_utf8(self.bytes(1)?).map_err(|_| invalid("invalid UTF-8 pool"))
    }

    pub(crate) fn finish(self) -> io::Result<()> {
        if self.remaining != 0 {
            return Err(invalid("trailing cache data"));
        }
        Ok(())
    }
}

pub(crate) fn write_array(writer: &mut impl Write, len: usize, bytes: &[u8]) -> io::Result<()> {
    let len = u32::try_from(len).map_err(|_| invalid("array exceeds cache format limits"))?;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(bytes)
}

/// Packed integers can be read and written in bulk on any architecture.
#[derive(Default)]
pub(crate) struct Packed<const N: usize>(Vec<u8>);

pub(crate) type U32s = Packed<4>;

impl<const N: usize> Packed<N> {
    pub(crate) fn zeros(len: usize) -> Self {
        Self(vec![0; N * len])
    }

    pub(crate) fn len(&self) -> usize {
        self.0.len() / N
    }

    pub(crate) fn get(&self, index: usize) -> u32 {
        Self::decode(&self.0.as_chunks::<N>().0[index])
    }

    pub(crate) fn set(&mut self, index: usize, value: u32) {
        self.0.as_chunks_mut::<N>().0[index].copy_from_slice(&value.to_le_bytes()[..N]);
    }

    pub(crate) fn push(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_le_bytes()[..N]);
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.0.as_chunks::<N>().0.iter().map(Self::decode)
    }

    pub(crate) fn is_strictly_sorted(&self, range: Range<usize>) -> bool {
        self.0.as_chunks::<N>().0[range]
            .iter()
            .map(Self::decode)
            .is_sorted_by(|previous, next| previous < next)
    }

    fn decode(bytes: &[u8; N]) -> u32 {
        let mut full = [0; 4];
        full[..N].copy_from_slice(bytes);
        u32::from_le_bytes(full)
    }

    pub(crate) fn shrink_to_fit(&mut self) {
        self.0.shrink_to_fit();
    }

    pub(crate) fn read(reader: &mut Reader) -> io::Result<Self> {
        Ok(Self(reader.bytes(N)?))
    }

    pub(crate) fn write(&self, writer: &mut impl Write) -> io::Result<()> {
        write_array(writer, self.len(), &self.0)
    }
}
