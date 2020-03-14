use rocksdb::{ColumnFamilyDescriptor, DBRecoveryMode, IteratorMode, Options, DB};
use std::{collections::HashMap, convert::TryInto, io::Cursor, mem, path::Path, sync::Arc};

use crate::{
    account::{Account, AccountId},
    asset::Asset,
    serializer::*,
    tx::{TxId, TxVariant, TxVariantV0},
};

const CF_BLOCK_BYTE_POS: &str = "block_byte_pos";
const CF_ACCOUNT: &str = "account";
const CF_TX_EXPIRY: &str = "tx_expiry";

const KEY_NET_OWNER: &[u8] = b"network_owner";
const KEY_CHAIN_HEIGHT: &[u8] = b"chain_height";
const KEY_TOKEN_SUPPLY: &[u8] = b"token_supply";
const KEY_INDEX_STATUS: &[u8] = b"index_status";

const TX_EXPIRY_ADJUSTMENT: u64 = 30;

#[derive(Debug)]
pub struct Indexer {
    db: DB,
}

impl Indexer {
    pub fn new(path: &Path) -> Indexer {
        let mut db_opts = Options::default();
        db_opts.create_missing_column_families(true);
        db_opts.create_if_missing(true);
        db_opts.set_wal_recovery_mode(DBRecoveryMode::AbsoluteConsistency);

        let col_families = vec![
            ColumnFamilyDescriptor::new(CF_BLOCK_BYTE_POS, Options::default()),
            ColumnFamilyDescriptor::new(CF_ACCOUNT, Options::default()),
            ColumnFamilyDescriptor::new(CF_TX_EXPIRY, Options::default()),
        ];
        let db = DB::open_cf_descriptors(&db_opts, path, col_families).unwrap();
        Indexer { db }
    }

    pub fn index_status(&self) -> IndexStatus {
        let buf_status = self.db.get_pinned(KEY_INDEX_STATUS).unwrap();
        match buf_status {
            Some(buf_status) => match buf_status[0] {
                0 => IndexStatus::None,
                1 => IndexStatus::Partial,
                2 => IndexStatus::Complete,
                _ => panic!("unhandled index status: {:?}", &buf_status[..]),
            },
            None => IndexStatus::None,
        }
    }

    pub fn set_index_status(&self, status: IndexStatus) {
        let buf = match status {
            IndexStatus::None => vec![0],
            IndexStatus::Partial => vec![1],
            IndexStatus::Complete => vec![2],
        };
        self.db.put(KEY_INDEX_STATUS, buf).unwrap();
    }

    pub fn get_block_byte_pos(&self, height: u64) -> Option<u64> {
        let cf = self.db.cf_handle(CF_BLOCK_BYTE_POS).unwrap();
        let buf = self.db.get_pinned_cf(cf, height.to_be_bytes()).unwrap()?;

        Some(u64::from_be_bytes(buf.as_ref().try_into().unwrap()))
    }

    pub fn get_chain_height(&self) -> u64 {
        match self.db.get_pinned(KEY_CHAIN_HEIGHT).unwrap() {
            Some(buf) => u64::from_be_bytes(buf.as_ref().try_into().unwrap()),
            None => 0,
        }
    }

    pub fn get_owner(&self) -> Option<TxVariant> {
        let tx_buf = self.db.get_pinned(KEY_NET_OWNER).unwrap()?;
        let cur = &mut Cursor::<&[u8]>::new(&tx_buf);
        let tx = TxVariant::deserialize(cur).expect("Failed to deserialize owner tx");
        match tx {
            TxVariant::V0(ref var) => match var {
                TxVariantV0::OwnerTx(_) => Some(tx),
                _ => panic!("expected owner transaction"),
            },
        }
    }

    pub fn get_account(&self, id: AccountId) -> Option<Account> {
        let cf = self.db.cf_handle(CF_ACCOUNT).unwrap();
        let buf = self.db.get_pinned_cf(cf, id.to_be_bytes()).unwrap()?;
        let cur = &mut Cursor::<&[u8]>::new(&buf);
        let account = Account::deserialize(cur).expect("failed to deserialize indexed account");
        Some(account)
    }

    pub fn get_token_supply(&self) -> Asset {
        let supply_buf = self.db.get_pinned(KEY_TOKEN_SUPPLY).unwrap();
        match supply_buf {
            Some(supply_buf) => {
                let cur = &mut Cursor::<&[u8]>::new(&supply_buf);
                cur.take_asset().unwrap()
            }
            None => Asset::default(),
        }
    }
}

pub struct WriteBatch {
    indexer: Arc<Indexer>,
    block_byte_pos: HashMap<u64, u64>,
    chain_height: Option<u64>,
    owner: Option<TxVariant>,
    accounts: HashMap<AccountId, Account>,
    token_supply: Option<Asset>,
}

impl WriteBatch {
    pub fn new(indexer: Arc<Indexer>) -> Self {
        WriteBatch {
            indexer,
            block_byte_pos: HashMap::with_capacity(1),
            chain_height: None,
            owner: None,
            accounts: HashMap::with_capacity(64),
            token_supply: None,
        }
    }

    pub fn commit(self) {
        let mut batch = rocksdb::WriteBatch::default();

        {
            let cf = self.indexer.db.cf_handle(CF_BLOCK_BYTE_POS).unwrap();
            for (height, pos) in self.block_byte_pos {
                let height = height.to_be_bytes();
                let pos = pos.to_be_bytes();
                batch.put_cf(cf, &height, &pos).unwrap();
            }
        }

        if let Some(height) = self.chain_height {
            batch.put(KEY_CHAIN_HEIGHT, height.to_be_bytes()).unwrap();
        }

        if let Some(owner) = self.owner {
            let val = {
                let mut buf = Vec::with_capacity(4096);
                owner.serialize(&mut buf);
                buf
            };
            batch.put(KEY_NET_OWNER, &val).unwrap();
        }

        if let Some(token_supply) = self.token_supply {
            let val = {
                let mut buf = Vec::with_capacity(mem::size_of::<Asset>());
                buf.push_asset(token_supply);
                buf
            };
            batch.put(KEY_TOKEN_SUPPLY, &val).unwrap();
        }

        {
            let cf = self.indexer.db.cf_handle(CF_ACCOUNT).unwrap();
            let mut buf = Vec::with_capacity(mem::size_of::<Account>());
            for (id, account) in self.accounts {
                account.serialize(&mut buf);
                batch.put_cf(cf, id.to_be_bytes(), &buf).unwrap();
                buf.clear();
            }
        }

        self.indexer.db.write(batch).unwrap();
    }

    pub fn set_block_byte_pos(&mut self, height: u64, pos: u64) {
        self.block_byte_pos.insert(height, pos);
    }

    pub fn set_chain_height(&mut self, height: u64) {
        self.chain_height = Some(height);
    }

    pub fn set_owner(&mut self, owner: TxVariant) {
        match owner {
            TxVariant::V0(ref tx) => match tx {
                TxVariantV0::OwnerTx(_) => {}
                _ => panic!(
                    "expected owner tx for set_owner operation, got: {:?}",
                    owner
                ),
            },
        }
        self.owner = Some(owner);
    }

    #[inline]
    pub fn get_owner(&self) -> Option<&TxVariant> {
        self.owner.as_ref()
    }

    pub fn add_token_supply(&mut self, amount: Asset) {
        match self.token_supply.as_mut() {
            Some(token_supply) => {
                *token_supply = token_supply.checked_add(amount).unwrap();
            }
            None => {
                let amt = self.indexer.get_token_supply().checked_add(amount).unwrap();
                self.token_supply = Some(amt);
            }
        }
    }

    pub fn add_bal(&mut self, id: AccountId, amount: Asset) {
        match self.accounts.get_mut(&id) {
            Some(account) => {
                account.balance = account.balance.checked_add(amount).unwrap();
            }
            None => {
                let mut account = self.indexer.get_account(id).unwrap();
                account.balance = account.balance.checked_add(amount).unwrap();
                self.accounts.insert(id, account);
            }
        }
    }

    pub fn sub_bal(&mut self, id: AccountId, amount: Asset) {
        match self.accounts.get_mut(&id) {
            Some(account) => {
                account.balance = account.balance.checked_sub(amount).unwrap();
            }
            None => {
                let mut account = self.indexer.get_account(id).unwrap();
                account.balance = account.balance.checked_sub(amount).unwrap();
                self.accounts.insert(id, account);
            }
        }
    }

    #[inline]
    pub fn insert_or_update_account(&mut self, account: Account) {
        self.accounts.insert(account.id, account);
    }
}

pub struct TxManager {
    indexer: Arc<Indexer>,
}

impl TxManager {
    pub fn new(indexer: Arc<Indexer>) -> Self {
        Self { indexer }
    }

    pub fn has(&self, id: &TxId) -> bool {
        let db = &self.indexer.db;
        let cf = db.cf_handle(CF_TX_EXPIRY).unwrap();
        self.indexer.db.get_cf(cf, id).unwrap().is_some()
    }

    pub fn insert(&self, id: &TxId, expiry: u64) {
        let db = &self.indexer.db;
        let cf = db.cf_handle(CF_TX_EXPIRY).unwrap();
        db.put_cf(cf, id, expiry.to_be_bytes()).unwrap();
    }

    pub fn purge_expired(&self) {
        let db = &self.indexer.db;
        let cf = db.cf_handle(CF_TX_EXPIRY).unwrap();
        // Pretend to be slightly in the past in case system time adjusts in the future.
        let current_time = crate::get_epoch_time() - TX_EXPIRY_ADJUSTMENT;

        let mut batch = rocksdb::WriteBatch::default();
        for (key, value) in db.iterator_cf(cf, IteratorMode::Start).unwrap() {
            let expiry = u64::from_be_bytes(value.as_ref().try_into().unwrap());
            if expiry < current_time {
                batch.delete_cf(cf, key).unwrap();
            }
        }
        db.write(batch).unwrap();
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum IndexStatus {
    None,
    Partial,
    Complete,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::Digest;
    use sodiumoxide::randombytes;
    use std::{env, fs, panic};

    #[test]
    fn get_block_pos() {
        run_test(|indexer| {
            let mut batch = WriteBatch::new(Arc::clone(&indexer));
            batch.set_block_byte_pos(1, 327);
            batch.commit();
            assert!(indexer.get_block_byte_pos(0).is_none());
            assert_eq!(indexer.get_block_byte_pos(1).unwrap(), 327);
        });
    }

    #[test]
    fn get_chain_height() {
        run_test(|indexer| {
            assert_eq!(indexer.get_chain_height(), 0);
            let mut batch = WriteBatch::new(Arc::clone(&indexer));
            batch.set_chain_height(42);
            batch.commit();
            assert_eq!(indexer.get_chain_height(), 42);
        });
    }

    #[test]
    fn tx_manager() {
        run_test(|indexer| {
            let id = TxId::from_digest(Digest::from_slice(&[0u8; 32]).unwrap());
            let expiry = crate::get_epoch_time();
            let manager = TxManager::new(Arc::clone(&indexer));
            assert!(!manager.has(&id));
            manager.insert(&id, expiry);
            assert!(manager.has(&id));

            let cf = indexer.db.cf_handle(CF_TX_EXPIRY).unwrap();
            indexer.db.delete_cf(cf, &id).unwrap();
            assert!(!manager.has(&id));

            manager.insert(&id, expiry - TX_EXPIRY_ADJUSTMENT + 1);
            manager.purge_expired();
            // The transaction has expired, but we give additional time before purging it.
            assert!(manager.has(&id));

            let cf = indexer.db.cf_handle(CF_TX_EXPIRY).unwrap();
            indexer.db.delete_cf(cf, &id).unwrap();
            assert!(!manager.has(&id));
            manager.insert(&id, expiry - TX_EXPIRY_ADJUSTMENT - 1);
            assert!(manager.has(&id));
            manager.purge_expired();
            // Test that the expiry is completely over
            assert!(!manager.has(&id));
        });
    }

    fn run_test<F>(func: F)
    where
        F: FnOnce(Arc<Indexer>) -> () + panic::UnwindSafe,
    {
        let mut tmp_dir = env::temp_dir();
        {
            let mut s = String::from("godcoin_test_");
            let mut num: [u8; 8] = [0; 8];
            randombytes::randombytes_into(&mut num);
            s.push_str(&format!("{}", u64::from_be_bytes(num)));
            tmp_dir.push(s);
        }
        fs::create_dir(&tmp_dir).expect(&format!("Could not create temp dir {:?}", &tmp_dir));

        let result = panic::catch_unwind(|| {
            let indexer = Indexer::new(&tmp_dir);
            func(Arc::new(indexer));
        });

        fs::remove_dir_all(&tmp_dir).expect("Failed to rm dir");
        assert!(result.is_ok());
    }
}
