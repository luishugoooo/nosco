use std::io::{self, Write};

use xxhash_rust::xxh3::xxh3_64;

use crate::cache::{Reader, U32s, invalid, write_array};

/// A UTF-8 byte pool with consecutive, stable string IDs.
pub(crate) struct StringPool {
    bytes: String,
    offsets: U32s,
}

impl Default for StringPool {
    fn default() -> Self {
        Self {
            bytes: String::new(),
            offsets: U32s::zeros(1),
        }
    }
}

impl StringPool {
    pub(crate) fn len(&self) -> usize {
        self.offsets.len() - 1
    }

    pub(crate) fn get(&self, id: u32) -> &str {
        let id = id as usize;
        &self.bytes[self.offsets.get(id) as usize..self.offsets.get(id + 1) as usize]
    }

    pub(crate) fn push(&mut self, word: &str) -> u32 {
        let id = self.len() as u32;
        self.bytes.push_str(word);
        self.offsets
            .push(u32::try_from(self.bytes.len()).expect("lexicon string pool exceeds 4 GiB"));
        id
    }

    pub(crate) fn shrink_to_fit(&mut self) {
        self.bytes.shrink_to_fit();
        self.offsets.shrink_to_fit();
    }

    pub(crate) fn read(reader: &mut Reader) -> io::Result<Self> {
        let bytes = reader.string()?;
        let offsets = U32s::read(reader)?;
        let mut iter = offsets.iter();
        if iter.next() != Some(0) {
            return Err(invalid("invalid initial pool offset"));
        }
        let mut previous = 0;
        for offset in iter {
            let offset = offset as usize;
            if offset < previous || !bytes.is_char_boundary(offset) {
                return Err(invalid("invalid string pool offset"));
            }
            previous = offset;
        }
        if previous != bytes.len() {
            return Err(invalid("invalid final pool offset"));
        }
        Ok(Self { bytes, offsets })
    }

    pub(crate) fn write(&self, writer: &mut impl Write) -> io::Result<()> {
        write_array(writer, self.bytes.len(), self.bytes.as_bytes())?;
        self.offsets.write(writer)
    }
}

/// An open-addressed XXH3 table of string IDs. Zero denotes a vacant slot.
#[derive(Default)]
pub(crate) struct WordSet {
    pool: StringPool,
    slots: U32s,
}

impl WordSet {
    pub(crate) fn len(&self) -> usize {
        self.pool.len()
    }

    fn probe(&self, word: &str, hash: u64) -> (usize, Option<u32>) {
        let mask = self.slots.len() - 1;
        let mut slot = hash as usize & mask;
        loop {
            let entry = self.slots.get(slot);
            if entry == 0 {
                return (slot, None);
            }
            if self.pool.get(entry - 1) == word {
                return (slot, Some(entry - 1));
            }
            slot = (slot + 1) & mask;
        }
    }

    pub(crate) fn find(&self, word: &str) -> Option<u32> {
        if self.slots.len() == 0 {
            return None;
        }
        self.probe(word, xxh3_64(word.as_bytes())).1
    }

    pub(crate) fn contains(&self, word: &str) -> bool {
        self.find(word).is_some()
    }

    pub(crate) fn get(&self, id: u32) -> &str {
        self.pool.get(id)
    }

    pub(crate) fn intern(&mut self, word: &str) -> u32 {
        let hash = xxh3_64(word.as_bytes());
        if self.slots.len() != 0 {
            let (slot, id) = self.probe(word, hash);
            if let Some(id) = id {
                return id;
            }
            if self.len() < self.slots.len() * 4 / 5 {
                return self.insert(word, slot);
            }
        }

        self.slots = U32s::zeros((self.slots.len() * 2).max(4));
        for id in 0..self.len() as u32 {
            let old_word = self.pool.get(id);
            let (slot, _) = self.probe(old_word, xxh3_64(old_word.as_bytes()));
            self.slots.set(slot, id + 1);
        }
        let (slot, _) = self.probe(word, hash);
        self.insert(word, slot)
    }

    fn insert(&mut self, word: &str, slot: usize) -> u32 {
        let id = self.pool.push(word);
        self.slots.set(slot, id + 1);
        id
    }

    pub(crate) fn shrink_to_fit(&mut self) {
        self.pool.shrink_to_fit();
    }

    pub(crate) fn read(reader: &mut Reader) -> io::Result<Self> {
        let pool = StringPool::read(reader)?;
        let slots = U32s::read(reader)?;
        let mut occupied = 0;
        for entry in slots.iter() {
            if entry as usize > pool.len() {
                return Err(invalid("word table references an unknown string"));
            }
            occupied += usize::from(entry != 0);
        }
        if occupied != pool.len()
            || (slots.len() != 0 && (!slots.len().is_power_of_two() || occupied == slots.len()))
        {
            return Err(invalid("invalid word table size"));
        }
        Ok(Self { pool, slots })
    }

    pub(crate) fn write(&self, writer: &mut impl Write) -> io::Result<()> {
        self.pool.write(writer)?;
        self.slots.write(writer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deduplicates_and_preserves_ids_after_growth() {
        let mut set = WordSet::default();
        assert!(!set.contains("café"));
        assert_eq!(set.intern("café"), 0);
        assert_eq!(set.intern(""), 1);
        for i in 0..1000 {
            assert_eq!(set.intern(&i.to_string()), i + 2);
        }
        assert_eq!(set.intern("café"), 0);
        assert_eq!(set.get(0), "café");
        assert!(set.contains(""));
        assert!(set.contains("456"));
        assert!(!set.contains("CAFÉ"));
        assert!(!set.contains("missing"));
        assert_eq!(set.len(), 1002);
    }
}
