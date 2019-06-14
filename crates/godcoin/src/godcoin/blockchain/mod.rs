use parking_lot::Mutex;
use std::{path::*, sync::Arc};

pub mod block;
pub mod index;
pub mod store;
pub mod verify;

pub use self::{
    block::*,
    index::{Indexer, WriteBatch},
    store::BlockStore,
    verify::TxErr,
};

use crate::{asset::Asset, constants::*, crypto::*, script::*, tx::*};

#[derive(Clone, Debug, PartialEq)]
pub struct Properties {
    pub height: u64,
    pub owner: Box<OwnerTx>,
    pub token_supply: Asset,
    pub network_fee: Asset,
}

pub struct Blockchain {
    indexer: Arc<Indexer>,
    store: Mutex<BlockStore>,
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

    pub fn indexer(&self) -> Arc<Indexer> {
        Arc::clone(&self.indexer)
    }

    pub fn get_properties(&self) -> Properties {
        Properties {
            height: self.get_chain_height(),
            owner: Box::new(self.get_owner()),
            token_supply: self.indexer.get_token_supply(),
            network_fee: self
                .get_network_fee()
                .expect("unexpected error retrieving network fee"),
        }
    }

    #[inline]
    pub fn get_owner(&self) -> OwnerTx {
        self.indexer
            .get_owner()
            .expect("Failed to retrieve owner from index")
    }

    #[inline]
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

    pub fn get_total_fee(&self, hash: &ScriptHash, additional_txs: &[TxVariant]) -> Option<Asset> {
        let net_fee = self.get_network_fee()?;
        let addr_fee = self.get_address_fee(hash, additional_txs)?;
        addr_fee.add(net_fee)
    }

    pub fn get_address_fee(
        &self,
        hash: &ScriptHash,
        additional_txs: &[TxVariant],
    ) -> Option<Asset> {
        let mut tx_count = 1;
        let mut delta = 0;

        macro_rules! handle_tx_match {
            ($tx:expr) => {
                let has_match = match $tx {
                    TxVariant::OwnerTx(_) => false,
                    TxVariant::MintTx(_) => false,
                    TxVariant::RewardTx(_) => false,
                    TxVariant::TransferTx(tx) => &tx.from == hash,
                };
                if has_match {
                    tx_count += 1;
                    delta = 0;
                }
            };
        }

        for tx in additional_txs {
            handle_tx_match!(tx);
        }

        for i in (0..=self.get_chain_height()).rev() {
            delta += 1;
            let block = self.get_block(i).unwrap();
            for tx in &block.transactions {
                handle_tx_match!(tx);
            }
            if delta == FEE_RESET_WINDOW {
                break;
            }
        }

        GRAEL_FEE_MIN.mul(GRAEL_FEE_MULT.pow(tx_count as u16)?)
    }

    pub fn get_network_fee(&self) -> Option<Asset> {
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

        GRAEL_FEE_MIN.mul(GRAEL_FEE_NET_MULT.pow(tx_count as u16)?)
    }

    pub fn get_balance(&self, hash: &ScriptHash) -> Asset {
        self.indexer.get_balance(hash).unwrap_or_default()
    }

    pub fn get_balance_with_txs(&self, hash: &ScriptHash, txs: &[TxVariant]) -> Option<Asset> {
        let mut bal = self.indexer.get_balance(hash).unwrap_or_default();
        for tx in txs {
            match tx {
                TxVariant::OwnerTx(_) => {}
                TxVariant::MintTx(tx) => {
                    if &tx.to == hash {
                        bal = bal.add(tx.amount)?;
                    }
                }
                TxVariant::RewardTx(tx) => {
                    if &tx.to == hash {
                        bal = bal.add(tx.rewards)?;
                    }
                }
                TxVariant::TransferTx(tx) => {
                    if &tx.from == hash {
                        bal = bal.sub(tx.fee)?;
                        bal = bal.sub(tx.amount)?;
                    } else if &tx.to == hash {
                        bal = bal.add(tx.amount)?;
                    }
                }
            }
        }

        Some(bal)
    }

    pub fn insert_block(&self, block: SignedBlock) -> Result<(), verify::BlockErr> {
        static CONFIG: verify::Config = verify::Config { skip_reward: true };
        self.verify_block(&block, &self.get_chain_head(), CONFIG)?;
        let mut batch = WriteBatch::new(Arc::clone(&self.indexer));
        for tx in &block.transactions {
            Self::index_tx(&mut batch, tx);
        }
        self.store.lock().insert(&mut batch, block);
        batch.commit();

        Ok(())
    }

    fn verify_block(
        &self,
        block: &SignedBlock,
        prev_block: &SignedBlock,
        config: verify::Config,
    ) -> Result<(), verify::BlockErr> {
        if prev_block.height + 1 != block.height {
            return Err(verify::BlockErr::InvalidBlockHeight);
        } else if !block.verify_tx_merkle_root() {
            return Err(verify::BlockErr::InvalidMerkleRoot);
        } else if !block.verify_previous_hash(prev_block) {
            return Err(verify::BlockErr::InvalidPrevHash);
        }

        let owner = self.get_owner();
        if !block.sig_pair.verify(block.calc_hash().as_ref()) {
            return Err(verify::BlockErr::InvalidHash);
        } else if block.sig_pair.pub_key != owner.minter {
            return Err(verify::BlockErr::InvalidSignature);
        }

        let len = block.transactions.len();
        for i in 0..len {
            let tx = &block.transactions[i];
            let txs = &block.transactions[0..i];
            if let Err(e) = self.verify_tx(&TxPrecompData::from_tx(tx), txs, config) {
                return Err(verify::BlockErr::Tx(e));
            }
        }

        Ok(())
    }

    pub fn verify_tx(
        &self,
        data: &TxPrecompData,
        additional_txs: &[TxVariant],
        config: verify::Config,
    ) -> Result<(), TxErr> {
        macro_rules! check_zero_fee {
            ($asset:expr) => {
                if $asset.amount != 0 {
                    return Err(TxErr::InvalidFeeAmount);
                }
            };
        }

        macro_rules! check_suf_bal {
            ($asset:expr) => {
                if $asset.amount < 0 {
                    return Err(TxErr::InsufficientBalance);
                }
            };
        }

        let tx = data.tx();

        if tx.signature_pairs.len() > MAX_TX_SIGNATURES {
            return Err(TxErr::TooManySignatures);
        } else if let Some(script) = tx.script() {
            if script.len() > MAX_SCRIPT_BYTE_SIZE {
                return Err(TxErr::TxTooLarge);
            }
        }

        match tx {
            TxVariant::OwnerTx(new_owner) => {
                check_zero_fee!(tx.fee);

                let owner = self.get_owner();
                if owner.wallet != (&new_owner.script).into() {
                    return Err(TxErr::ScriptHashMismatch);
                }

                let success = ScriptEngine::new(data, &new_owner.script)
                    .eval()
                    .map_err(TxErr::ScriptEval)?;
                if !success {
                    return Err(TxErr::ScriptRetFalse);
                }
            }
            TxVariant::MintTx(mint_tx) => {
                check_zero_fee!(tx.fee);

                let owner = self.get_owner();
                if owner.wallet != (&mint_tx.script).into() {
                    return Err(TxErr::ScriptHashMismatch);
                }
                let success = ScriptEngine::new(data, &mint_tx.script)
                    .eval()
                    .map_err(TxErr::ScriptEval)?;
                if !success {
                    return Err(TxErr::ScriptRetFalse);
                }

                // Sanity check to ensure too many new coins can't be minted
                self.get_balance_with_txs(&mint_tx.to, additional_txs)
                    .ok_or(TxErr::Arithmetic)?
                    .add(mint_tx.amount)
                    .ok_or(TxErr::Arithmetic)?;

                self.indexer
                    .get_token_supply()
                    .add(mint_tx.amount)
                    .ok_or(TxErr::Arithmetic)?;
            }
            TxVariant::RewardTx(tx) => {
                if !config.skip_reward {
                    return Err(TxErr::TxProhibited);
                }
                // Reward transactions are internally generated, thus should panic on failure
                if tx.fee.amount != 0 {
                    panic!("reward tx must have no fee");
                } else if !tx.signature_pairs.is_empty() {
                    panic!("reward tx must not be signed");
                } else if tx.timestamp != 0 {
                    panic!("reward tx must have a timestamp of 0");
                }
            }
            TxVariant::TransferTx(transfer) => {
                if transfer.memo.len() > MAX_MEMO_BYTE_SIZE {
                    return Err(TxErr::TxTooLarge);
                }
                let total_fee = self
                    .get_total_fee(&transfer.from, additional_txs)
                    .ok_or(TxErr::Arithmetic)?;
                if tx.fee < total_fee {
                    return Err(TxErr::InvalidFeeAmount);
                } else if transfer.from != (&transfer.script).into() {
                    return Err(TxErr::ScriptHashMismatch);
                }

                let success = ScriptEngine::new(data, &transfer.script)
                    .eval()
                    .map_err(TxErr::ScriptEval)?;
                if !success {
                    return Err(TxErr::ScriptRetFalse);
                }

                let bal = self
                    .get_balance_with_txs(&transfer.from, additional_txs)
                    .ok_or(TxErr::Arithmetic)?
                    .sub(transfer.fee)
                    .ok_or(TxErr::Arithmetic)?
                    .sub(transfer.amount)
                    .ok_or(TxErr::Arithmetic)?;
                check_suf_bal!(bal);
            }
        }
        Ok(())
    }

    fn index_tx(batch: &mut WriteBatch, tx: &TxVariant) {
        match tx {
            TxVariant::OwnerTx(tx) => {
                batch.set_owner(tx.clone());
            }
            TxVariant::MintTx(tx) => {
                batch.add_token_supply(tx.amount);
                batch.add_bal(&tx.to, tx.amount);
            }
            TxVariant::RewardTx(tx) => {
                batch.add_bal(&tx.to, tx.rewards);
            }
            TxVariant::TransferTx(tx) => {
                batch.sub_bal(&tx.from, tx.fee.add(tx.amount).unwrap());
                batch.add_bal(&tx.to, tx.amount);
            }
        }
    }

    pub fn create_genesis_block(&self, minter_key: KeyPair) -> GenesisBlockInfo {
        let info = GenesisBlockInfo::new(minter_key);
        let timestamp = crate::util::get_epoch_ms();

        let owner_tx = OwnerTx {
            base: Tx {
                tx_type: TxType::OWNER,
                fee: Asset::default(),
                timestamp,
                signature_pairs: Vec::new(),
            },
            minter: info.minter_key.0.clone(),
            wallet: (&info.script).into(),
            script: Builder::new().push(OpFrame::False).build(),
        };

        let block = (Block {
            height: 0,
            previous_hash: Digest::from_slice(&[0u8; 32]).unwrap(),
            tx_merkle_root: Digest::from_slice(&[0u8; 32]).unwrap(),
            timestamp,
            transactions: vec![TxVariant::OwnerTx(owner_tx.clone())],
        })
        .sign(&info.minter_key);

        let mut batch = WriteBatch::new(Arc::clone(&self.indexer));
        self.store.lock().insert_genesis(&mut batch, block);
        batch.set_owner(owner_tx);
        batch.commit();

        info
    }
}

pub struct GenesisBlockInfo {
    pub minter_key: KeyPair,
    pub wallet_keys: [KeyPair; 4],
    pub script: Script,
}

impl GenesisBlockInfo {
    pub fn new(minter_key: KeyPair) -> Self {
        let wallet_keys = [
            KeyPair::gen(),
            KeyPair::gen(),
            KeyPair::gen(),
            KeyPair::gen(),
        ];

        let script = Builder::new()
            .push(OpFrame::PubKey(wallet_keys[0].0.clone()))
            .push(OpFrame::PubKey(wallet_keys[1].0.clone()))
            .push(OpFrame::PubKey(wallet_keys[2].0.clone()))
            .push(OpFrame::PubKey(wallet_keys[3].0.clone()))
            .push(OpFrame::OpCheckMultiSig(2, 4))
            .build();

        Self {
            minter_key,
            wallet_keys,
            script,
        }
    }
}
