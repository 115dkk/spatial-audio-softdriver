//! Bounded seekable storage used by the main-thread VST 3 IBStream adapter.
use std::io::{self, Read, Seek, SeekFrom, Write};
const LIMIT: u64 = 64 * 1024 * 1024 - 8;
#[derive(Debug, Default)]
pub struct StateBuffer(std::io::Cursor<Vec<u8>>);
impl StateBuffer {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(std::io::Cursor::new(bytes))
    }
    pub fn snapshot(&self) -> Vec<u8> {
        self.0.get_ref().clone()
    }
    pub fn position(&self) -> u64 {
        self.0.position()
    }
}
impl Read for StateBuffer {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.0.read(bytes)
    }
}
impl Write for StateBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.position().saturating_add(bytes.len() as u64) > LIMIT {
            return Err(io::Error::other("state buffer limit"));
        }
        self.0.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Seek for StateBuffer {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let position = match from {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::Current(n) => self.position().checked_add_signed(n),
            SeekFrom::End(n) => (self.0.get_ref().len() as u64).checked_add_signed(n),
        }
        .filter(|n| *n <= LIMIT)
        .ok_or_else(|| io::Error::other("invalid state seek"))?;
        self.0.set_position(position);
        Ok(position)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stream_buffer_read_write_seek_bounds() {
        let mut stream = StateBuffer::new(vec![1, 2]);
        let mut bytes = [0; 4];
        assert_eq!(stream.read(&mut bytes).unwrap(), 2);
        assert_eq!(stream.read(&mut bytes).unwrap(), 0);
        stream.seek(SeekFrom::End(2)).unwrap();
        stream.write_all(&[3]).unwrap();
        assert_eq!(stream.snapshot(), [1, 2, 0, 0, 3]);
        assert_eq!(stream.seek(SeekFrom::Current(-1)).unwrap(), 4);
        assert!(stream.seek(SeekFrom::Current(-5)).is_err());
        assert!(stream.seek(SeekFrom::Start(LIMIT + 1)).is_err());
        stream.seek(SeekFrom::Start(LIMIT)).unwrap();
        assert!(stream.write(&[1]).is_err());
    }
}
