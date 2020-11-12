pub mod storage;

pub use storage::*;

use crate::net::Serializable;
use bytes::{BufMut, Bytes, BytesMut};
use godcoin::serializer::BufRead;
use std::{
    io::{self, Cursor, Read},
    mem,
};

#[derive(Debug)]
pub struct Log<S: Storage> {
    unstable_ents: Vec<Entry>,
    storage: S,
    last_term: u64,
}

impl<S: Storage> Log<S> {
    pub fn new(storage: S) -> Self {
        Self {
            unstable_ents: Vec::with_capacity(32),
            storage,
            last_term: 0,
        }
    }

    pub fn last_term(&self) -> u64 {
        self.last_term
    }

    pub fn last_index(&self) -> u64 {
        self.unstable_ents
            .last()
            .map_or_else(|| self.stable_index(), |e| e.index)
    }

    #[inline]
    pub fn stable_index(&self) -> u64 {
        self.storage.stable_index()
    }

    pub fn stabilize_to(&mut self, index: u64) {
        if index <= self.stable_index() {
            return;
        }
        let entries = if let Some(pos) = self.find_index_pos(index) {
            self.unstable_ents.drain(..=pos).collect()
        } else if let Some(last_ent) = self.unstable_ents.last() {
            // Index will never be equal or lower than our last unstable entry, this assertion will
            // fail if there is a bug finding an index that *should* exist in the unstable log
            assert!(index > last_ent.index);
            let mut unstable_ents = Vec::with_capacity(self.unstable_ents.capacity());
            mem::swap(&mut self.unstable_ents, &mut unstable_ents);
            unstable_ents
        } else {
            vec![]
        };
        self.storage.commit_stable_entries(entries);
    }

    /// `entries` are assumed to be in order from lowest index to highest index.
    pub fn try_commit(&mut self, entries: Vec<Entry>) -> Result<(), CommitErr> {
        if let Some(e) = entries.first() {
            let stable_index = self.stable_index();
            if e.index <= stable_index {
                return Err(CommitErr::CannotRevertStableIndex);
            } else if let Some(our_ent) = self.unstable_ents.last() {
                // Check to prevent gaps in the log
                if e.index > our_ent.index + 1 {
                    return Err(CommitErr::IndexTooHigh);
                }
            } else if stable_index + 1 != e.index {
                // Unstable entries has no entries in it, so we check to make sure the next index
                // that comes next is after the stable index.
                return Err(CommitErr::IndexTooHigh);
            }
        } else {
            // Entries vec is empty
            return Ok(());
        }

        if let Some(index) = self.find_conflict(&entries) {
            self.unstable_ents.truncate(index);
        }

        // Unwrap here will never panic as we bail earlier if the entries vec is empty.
        self.last_term = entries.last().unwrap().term;
        self.unstable_ents.extend(entries);

        Ok(())
    }

    pub fn contains_entry(&self, term: u64, index: u64) -> bool {
        if index <= self.stable_index() {
            return true;
        }
        for e in &self.unstable_ents {
            if e.index == index {
                return e.term == term;
            }
        }
        false
    }

    pub fn is_up_to_date(&self, last_index: u64, last_term: u64) -> bool {
        let term = self.last_term();
        last_term > term || (last_term == term && last_index >= self.last_index())
    }

    pub fn get_entry_by_index(&self, index: u64) -> Option<Entry> {
        if index > self.stable_index() {
            self.unstable_ents
                .iter()
                .find(|e| e.index == index)
                .cloned()
        } else {
            self.storage
                .retrieve_stable_entry(index)
                .map(|bytes| Entry {
                    term: self.last_term,
                    index,
                    data: bytes,
                })
        }
    }

    fn find_index_pos(&self, index: u64) -> Option<usize> {
        for (pos, e) in self.unstable_ents.iter().enumerate() {
            if e.index == index {
                return Some(pos);
            }
        }
        None
    }

    fn find_conflict(&self, entries: &[Entry]) -> Option<usize> {
        for (index, self_e) in self.unstable_ents.iter().enumerate() {
            for other_e in entries {
                if self_e.index == other_e.index {
                    return Some(index);
                }
            }
        }
        None
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CommitErr {
    CannotRevertStableIndex,
    /// This represents an attempted commit does not have the next required index
    IndexTooHigh,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub index: u64,
    pub term: u64,
    pub data: Bytes,
}

impl Serializable<Entry> for Entry {
    fn serialize(&self, dst: &mut BytesMut) {
        dst.put_u64(self.index);
        dst.put_u64(self.term);
        dst.put_u64(self.data.len() as u64);
        dst.extend(&self.data);
    }

    fn byte_size(&self) -> usize {
        24 + self.data.len()
    }

    fn deserialize(src: &mut Cursor<&[u8]>) -> io::Result<Entry> {
        let index = src.take_u64()?;
        let term = src.take_u64()?;
        let data_len = src.take_u64()?;
        let mut data = vec![0u8; data_len as usize];
        src.read_exact(&mut data)?;

        Ok(Self {
            index,
            term,
            data: data.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_during_empty_log_with_stable_index() {
        let stable_index = 100;
        let mut log = init_log(stable_index);

        let entries = gen_ents(stable_index + 1, 25);
        assert_eq!(log.try_commit(entries), Ok(()));
        assert_eq!(log.unstable_ents.len(), 25);
    }

    #[test]
    fn commit_new_log() {
        let mut log = init_log(0);

        let entries = gen_ents(1, 25);
        assert_eq!(log.try_commit(entries), Ok(()));
        assert_eq!(log.unstable_ents.len(), 25);
        assert_eq!(log.stable_index(), 0);
    }

    #[test]
    fn commit_multiple_times() {
        let mut log = init_log(0);
        for i in 1..=10 {
            let entries = gen_ents(i, 1);
            assert_eq!(log.try_commit(entries), Ok(()));
            assert_eq!(log.unstable_ents.len() as u64, i);
            assert_eq!(log.stable_index(), 0);
        }

        for i in 1..=10 {
            assert_eq!(log.unstable_ents[i - 1].index, i as u64);
        }
    }

    #[test]
    fn commit_err_with_gap_in_log() {
        let mut log = init_log(0);

        {
            let entries = gen_ents(2, 1);
            assert_eq!(log.try_commit(entries), Err(CommitErr::IndexTooHigh));
            assert_eq!(log.unstable_ents.len(), 0);
            assert_eq!(log.stable_index(), 0);
        }
        {
            let entries = gen_ents(1, 10);
            assert_eq!(log.try_commit(entries), Ok(()));

            let entries = gen_ents(12, 10);
            assert_eq!(log.try_commit(entries), Err(CommitErr::IndexTooHigh));

            assert_eq!(log.unstable_ents.len(), 10);
            assert_eq!(log.stable_index(), 0);
        }
    }

    #[test]
    fn cannot_revert_stable_index() {
        fn run_test(start_index: u64) {
            let mut log = init_log(start_index);

            let entries = gen_ents(start_index, 10);
            assert_eq!(
                log.try_commit(entries),
                Err(CommitErr::CannotRevertStableIndex)
            );
            assert_eq!(log.unstable_ents.len(), 0);
            assert_eq!(log.stable_index(), start_index);
        };

        run_test(0);
        run_test(100);
    }

    #[test]
    fn commit_with_conflicts_same_term() {
        let mut log = init_log(0);

        {
            let mut entries = gen_ents(1, 25);
            replace_entry_data(&mut entries, Bytes::from(vec![1]));
            assert_eq!(log.try_commit(entries), Ok(()));
            assert_eq!(log.unstable_ents.len(), 25);
        }
        {
            let mut entries = gen_ents(20, 25);
            replace_entry_data(&mut entries, Bytes::from(vec![2]));
            assert_eq!(log.try_commit(entries), Ok(()));
            // Entries [20-25] gets replaced (6 total), 19 additional entries are appended to the
            // log.
            assert_eq!(log.unstable_ents.len(), 44);
        }

        for i in 0..19 {
            assert_eq!(log.unstable_ents[i].data[0], 1);
        }
        for i in 20..44 {
            assert_eq!(log.unstable_ents[i].data[0], 2);
        }
    }

    #[test]
    fn commit_with_conflicts_diff_term() {
        let mut log = init_log(0);

        {
            let mut entries = gen_ents(1, 25);
            replace_entry_data(&mut entries, Bytes::from(vec![1]));
            assert_eq!(log.try_commit(entries), Ok(()));
            assert_eq!(log.unstable_ents.len(), 25);
        }
        {
            let mut entries = gen_ents_term(20, 2, 25);
            replace_entry_data(&mut entries, Bytes::from(vec![2]));
            assert_eq!(log.try_commit(entries), Ok(()));
            // Entries [20-25] gets replaced (6 total), 19 additional entries are appended to the
            // log.
            assert_eq!(log.unstable_ents.len(), 44);
        }

        for i in 0..19 {
            assert_eq!(log.unstable_ents[i].data[0], 1);
        }
        for i in 20..44 {
            assert_eq!(log.unstable_ents[i].data[0], 2);
        }
    }

    #[test]
    fn can_find_conflict() {
        let mut log = init_log(0);

        let entries = gen_ents(1, 25);
        assert_eq!(log.try_commit(entries), Ok(()));

        let entries = gen_ents(20, 30);
        let idx = log
            .find_conflict(&entries)
            .expect("Failed to find conflict");
        assert_eq!(log.unstable_ents[idx].index, 20);

        let entries = gen_ents(26, 5);
        assert_eq!(log.find_conflict(&entries), None);
    }

    #[test]
    fn can_find_entry() {
        let mut log = init_log(0);

        let entries = gen_ents(1, 25);
        assert_eq!(log.try_commit(entries), Ok(()));

        // Starting entry that every log starts with
        assert!(log.contains_entry(0, 0));

        for i in 1..=25 {
            assert!(log.contains_entry(0, i));
            assert!(!log.contains_entry(1, i));
        }
    }

    #[test]
    fn can_find_entry_pos() {
        let mut log = init_log(0);

        let entries = gen_ents(1, 25);
        assert_eq!(log.try_commit(entries), Ok(()));

        assert_eq!(log.find_index_pos(0), None);
        assert_eq!(log.find_index_pos(26), None);
        for i in 0..25 {
            assert_eq!(log.find_index_pos(i + 1), Some(i as usize));
        }
    }

    #[test]
    fn stabilize_to_empty_unstable_entries() {
        let mut log = init_log(10);
        log.stabilize_to(11);
        assert_eq!(log.stable_index(), 10);
    }

    #[test]
    fn stabilize_to_already_stable_index() {
        let mut log = init_log(10);

        log.stabilize_to(9);
        assert_eq!(log.stable_index(), 10);

        log.stabilize_to(10);
        assert_eq!(log.stable_index(), 10);
    }

    #[test]
    fn stabilize_to_under_last_unstable_index() {
        let mut log = init_log(0);
        let entries = gen_ents(1, 25);
        assert_eq!(log.try_commit(entries), Ok(()));

        log.stabilize_to(20);
        for (expected_index, (_, entry)) in (1..=20).zip(log.storage.stable_entries()) {
            assert_eq!(expected_index, entry.index);
        }

        assert_eq!(log.stable_index(), 20);
        assert_eq!(log.unstable_ents.len(), 5);

        for (expected_index, entry) in (21..=25).zip(&log.unstable_ents) {
            assert_eq!(expected_index, entry.index);
        }
    }

    #[test]
    fn stabilize_to_last_unstable_index() {
        let mut log = init_log(0);
        let entries = gen_ents(1, 25);
        assert_eq!(log.try_commit(entries), Ok(()));

        log.stabilize_to(25);
        for (expected_index, (_, entry)) in (1..=25).zip(log.storage.stable_entries()) {
            assert_eq!(expected_index, entry.index);
        }

        assert_eq!(log.stable_index(), 25);
        assert_eq!(log.unstable_ents.len(), 0);
    }

    #[test]
    fn stabilize_to_above_last_unstable_index() {
        let mut log = init_log(0);
        let entries = gen_ents(1, 25);
        assert_eq!(log.try_commit(entries), Ok(()));

        log.stabilize_to(30);
        for (expected_index, (_, entry)) in (1..=25).zip(log.storage.stable_entries()) {
            assert_eq!(expected_index, entry.index);
        }

        assert_eq!(log.stable_index(), 25);
        assert_eq!(log.unstable_ents.len(), 0);
    }

    #[test]
    fn log_up_to_date_checks() {
        let mut log = init_log(0);
        let entries = gen_ents_term(1, 5, 25);
        assert_eq!(log.try_commit(entries), Ok(()));

        assert!(log.is_up_to_date(log.last_index(), log.last_term()));
        assert!(log.is_up_to_date(20, 6));
        assert!(log.is_up_to_date(25, 5));
        assert!(log.is_up_to_date(26, 5));

        assert!(!log.is_up_to_date(24, 5));
        assert!(!log.is_up_to_date(25, 4));
        assert!(!log.is_up_to_date(26, 4));
    }

    fn gen_ents(start_index: u64, len: usize) -> Vec<Entry> {
        gen_ents_term(start_index, 0, len)
    }

    fn gen_ents_term(start_index: u64, term: u64, len: usize) -> Vec<Entry> {
        let mut entries = Vec::with_capacity(len);
        for i in 0..len {
            entries.push(Entry {
                index: start_index + i as u64,
                term,
                data: Bytes::new(),
            });
        }
        entries
    }

    fn replace_entry_data(entries: &mut [Entry], data: Bytes) {
        for e in entries {
            e.data = data.clone();
        }
    }

    fn init_log(stable_index: u64) -> Log<MemStorage> {
        let mut storage = MemStorage::default();
        storage.commit_stable_entries(gen_ents(1, stable_index as usize));
        Log::new(storage)
    }
}