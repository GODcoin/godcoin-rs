use log::info;
use parking_lot::Mutex;
use std::path::*;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub mod block;
pub mod index;
pub mod store;

pub use self::block::*;
pub use self::index::Indexer;
pub use self::store::BlockStore;

use crate::asset::{self, Asset, Balance};
use crate::crypto::*;
use crate::script::*;
use crate::tx::*;

pub struct Blockchain {
    indexer: Arc<Indexer>,
    store: Mutex<BlockStore>,
}

#[derive(Clone, Debug)]
pub struct Properties {
    pub height: u64,
    pub token_supply: Balance,
    pub network_fee: Balance,
}

impl Blockchain {
    ///
    /// Creates a new `Blockchain` with an associated indexer and backing
    /// storage is automatically created based on the given `path`.
    ///
    pub fn new(path: &Path) -> Blockchain {
        let indexer = Arc::new(Indexer::new(&Path::join(path, "index")));
        let store = BlockStore::new(&Path::join(path, "blklog"), Arc::clone(&indexer));
        Blockchain {
            indexer,
            store: Mutex::new(store),
        }
    }

    pub fn get_properties(&self) -> Properties {
        Properties {
            height: self.get_chain_height(),
            token_supply: self.indexer.get_token_supply(),
            network_fee: self
                .get_network_fee()
                .expect("unexpected error retrieving network fee"),
        }
    }

    #[inline(always)]
    pub fn get_owner(&self) -> OwnerTx {
        self.indexer.get_owner().expect("Failed to retrieve owner from index")
    }

    #[inline(always)]
    pub fn get_chain_height(&self) -> u64 {
        self.indexer.get_chain_height()
    }

    pub fn get_chain_head(&self) -> Arc<SignedBlock> {
        let store = self.store.lock();
        let height = store.get_chain_height();
        store.get(height).expect("Failed to get blockchain head")
    }

    pub fn get_block(&self, height: u64) -> Option<Arc<SignedBlock>> {
        let store = self.store.lock();
        store.get(height)
    }

    pub fn get_total_fee(&self, hash: &ScriptHash) -> Option<Balance> {
        let net_fee = self.get_network_fee()?;
        let addr_fee = self.get_address_fee(hash)?;
        Some(Balance {
            gold: net_fee.gold.add(&addr_fee.gold)?,
            silver: net_fee.silver.add(&addr_fee.silver)?,
        })
    }

    pub fn get_address_fee(&self, hash: &ScriptHash) -> Option<Balance> {
        use crate::constants::*;

        let mut tx_count = 1;
        let head = self.get_chain_height();
        for (delta, i) in (0..=head).rev().enumerate() {
            let block = self.get_block(i).unwrap();
            for tx in &block.transactions {
                let has_match = match tx {
                    TxVariant::OwnerTx(_) => false,
                    TxVariant::RewardTx(_) => false,
                    TxVariant::TransferTx(tx) => &tx.from == hash,
                };
                if has_match {
                    tx_count += 1;
                }
            }
            if delta + 1 == FEE_RESET_WINDOW {
                break;
            }
        }

        let prec = asset::MAX_PRECISION;
        let gold = GOLD_FEE_MIN.mul(&GOLD_FEE_MULT.pow(tx_count as u16, prec)?, prec)?;
        let silver = SILVER_FEE_MIN.mul(&SILVER_FEE_MULT.pow(tx_count as u16, prec)?, prec)?;
        Some(Balance { gold, silver })
    }

    pub fn get_network_fee(&self) -> Option<Balance> {
        // The network fee adjusts every 5 blocks so that users have a bigger time
        // frame to confirm the fee they want to spend without suddenly changing.
        use crate::constants::*;
        let max_height = self.get_chain_height();
        let max_height = max_height - (max_height % 5);
        let min_height = if max_height > NETWORK_FEE_AVG_WINDOW {
            max_height - NETWORK_FEE_AVG_WINDOW
        } else {
            0
        };

        let mut tx_count: u64 = 1;
        for i in min_height..=max_height {
            tx_count += self.get_block(i).unwrap().transactions.len() as u64;
        }
        tx_count /= NETWORK_FEE_AVG_WINDOW;
        if tx_count > u64::from(u16::max_value()) {
            return None;
        }

        let prec = asset::MAX_PRECISION;
        let gold = GOLD_FEE_MIN.mul(&GOLD_FEE_NET_MULT.pow(tx_count as u16, prec)?, prec)?;
        let silver = SILVER_FEE_MIN.mul(&SILVER_FEE_NET_MULT.pow(tx_count as u16, prec)?, prec)?;

        Some(Balance { gold, silver })
    }

    pub fn get_balance(&self, hash: &ScriptHash) -> Balance {
        self.indexer.get_balance(hash).unwrap_or_default()
    }

    pub fn get_balance_with_txs(&self, hash: &ScriptHash, txs: &[TxVariant]) -> Option<Balance> {
        let mut bal = self.indexer.get_balance(hash).unwrap_or_default();
        for tx in txs {
            match tx {
                TxVariant::OwnerTx(_) => {}
                TxVariant::RewardTx(tx) => {
                    if &tx.to == hash {
                        for reward in &tx.rewards {
                            bal.add(&reward)?;
                        }
                    }
                }
                TxVariant::TransferTx(tx) => {
                    if &tx.from == hash {
                        bal.sub(&tx.fee)?;
                        bal.sub(&tx.amount)?;
                    } else if &tx.to == hash {
                        bal.add(&tx.amount)?;
                    }
                }
            }
        }

        Some(bal)
    }

    pub fn insert_block(&self, block: SignedBlock) -> Result<(), String> {
        self.verify_block(&block, &self.get_chain_head())?;
        for tx in &block.transactions {
            self.index_tx(tx);
        }
        self.store.lock().insert(block);

        Ok(())
    }

    fn verify_block(&self, block: &SignedBlock, prev_block: &SignedBlock) -> Result<(), String> {
        if prev_block.height + 1 != block.height {
            return Err("invalid block height".to_owned());
        } else if !block.verify_tx_merkle_root() {
            return Err("invalid merkle root".to_owned());
        } else if !block.verify_previous_hash(prev_block) {
            return Err("invalid previous hash".to_owned());
        }

        let owner = self.get_owner();
        if !block.sig_pair.verify(block.calc_hash().as_ref()) {
            return Err("invalid block signature".to_owned());
        } else if block.sig_pair.pub_key != owner.minter {
            return Err("invalid owner signature".to_owned());
        }

        let len = block.transactions.len();
        for i in 0..len {
            let tx = &block.transactions[i];
            let txs = &block.transactions[0..i];
            if let Err(s) = self.verify_tx(tx, txs) {
                return Err(format!("tx verification failed: {}", s));
            }
        }

        Ok(())
    }

    pub fn verify_tx(&self, tx: &TxVariant, additional_txs: &[TxVariant]) -> Result<(), String> {
        macro_rules! check_amt {
            ($asset:expr, $name:expr) => {
                if $asset.amount < 0 {
                    return Err(format!("{} must be greater than 0", $name));
                }
            };
        }

        macro_rules! check_suf_bal {
            ($asset:expr) => {
                if $asset.amount < 0 {
                    return Err("insufficient balance".to_owned());
                }
            };
        }

        check_amt!(tx.fee, "fee");
        match tx {
            TxVariant::OwnerTx(tx) => {
                let pairs = &tx.signature_pairs;
                if pairs.len() != 2 {
                    return Err("not enough signatures to change ownership".to_owned());
                }
                let owner = self.get_owner();
                if !(pairs[0].pub_key == owner.minter && pairs[1].pub_key == tx.wallet) {
                    return Err("signatures don't match previous ownership".to_owned())
                }
                let mut buf = Vec::with_capacity(4096);
                tx.encode(&mut buf);
                for sig_pair in &tx.signature_pairs {
                    if !sig_pair.verify(&buf) {
                        return Err("signature validation failed".to_owned())
                    }
                }
            }
            TxVariant::RewardTx(tx) => {
                if !tx.signature_pairs.is_empty() {
                    return Err("reward transaction must not be signed".to_owned());
                }
            }
            TxVariant::TransferTx(transfer) => {
                if transfer.fee.symbol != transfer.amount.symbol {
                    return Err("symbol mismatch between fee and amount".to_owned());
                } else if transfer.from != ScriptHash::from(&transfer.script) {
                    return Err("from and script hash mismatch".to_owned());
                }

                let success = ScriptEngine::checked_new(tx, &transfer.script)
                    .ok_or_else(|| "failed to initialize script engine")?
                    .eval()
                    .map_err(|e| format!("{}: {:?}", e.pos, e.err))?;
                if !success {
                    return Err("script returned false".to_owned());
                }

                let mut bal = self
                    .get_balance_with_txs(&transfer.from, additional_txs)
                    .ok_or_else(|| "failed to get balance")?;
                bal.sub(&transfer.fee)
                    .ok_or("failed to subtract fee")?
                    .sub(&transfer.amount)
                    .ok_or("failed to subtract amount")?;
                check_suf_bal!(&bal.gold);
                check_suf_bal!(&bal.silver);
            }
        }
        Ok(())
    }

    fn index_tx(&self, tx: &TxVariant) {
        match tx {
            TxVariant::OwnerTx(tx) => {
                self.indexer.set_owner(tx);
            }
            TxVariant::RewardTx(tx) => {
                let mut bal = self.get_balance(&tx.to);
                let mut supply = self.indexer.get_token_supply();
                for r in &tx.rewards {
                    bal.add(r).unwrap();
                    supply.add(r).unwrap();
                }
                self.indexer.set_balance(&tx.to, &bal);
                self.indexer.set_token_supply(&supply);
            }
            TxVariant::TransferTx(tx) => {
                let mut from_bal = self.get_balance(&tx.from);
                let mut to_bal = self.get_balance(&tx.to);

                from_bal.sub(&tx.fee).unwrap().sub(&tx.amount).unwrap();
                to_bal.add(&tx.amount).unwrap();

                self.indexer.set_balance(&tx.from, &from_bal);
                self.indexer.set_balance(&tx.to, &to_bal);
            }
        }
    }

    pub fn create_genesis_block(&self, minter_key: &KeyPair) {
        use sodiumoxide::crypto::hash::sha256::Digest;

        info!("=> Generating new block chain");
        let wallet_key = KeyPair::gen_keypair();
        info!("=> Wallet private key: {}", wallet_key.1.to_wif());
        info!("=> Wallet public key: {}", wallet_key.0.to_wif());

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as u32;

        let owner_tx = OwnerTx {
            base: Tx {
                tx_type: TxType::OWNER,
                fee: Asset::from_str("0 GOLD").unwrap(),
                timestamp,
                signature_pairs: Vec::new(),
            },
            minter: minter_key.0.clone(),
            wallet: wallet_key.0.clone().into(),
        };

        let block = (Block {
            height: 0,
            previous_hash: Digest::from_slice(&[0u8; 32]).unwrap(),
            tx_merkle_root: Digest::from_slice(&[0u8; 32]).unwrap(),
            timestamp: timestamp as u32,
            transactions: vec![TxVariant::OwnerTx(owner_tx.clone())],
        })
        .sign(&minter_key);

        self.store.lock().insert_genesis(block);
        self.indexer.set_owner(&owner_tx);
    }
}
