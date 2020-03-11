use std::{
    borrow::Cow,
    io::Cursor,
    ops::{Deref, DerefMut},
};

use crate::{
    account::{Account, AccountId},
    asset::Asset,
    constants::CHAIN_ID,
    crypto::{Digest, DoubleSha256, KeyPair, PublicKey, ScriptHash, SigPair},
    script::Script,
    serializer::*,
};

#[macro_use]
mod util;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TxType {
    Owner = 0x00,
    Mint = 0x01,
    CreateAccount = 0x02,
    Transfer = 0x03,
}

pub trait SerializeTx {
    fn serialize(&self, v: &mut Vec<u8>);
}

pub trait DeserializeTx<T> {
    fn deserialize(cur: &mut Cursor<&[u8]>, tx: Tx) -> Option<T>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct TxId(Digest);

impl TxId {
    pub fn from_digest(txid: Digest) -> Self {
        TxId(txid)
    }
}

impl AsRef<[u8]> for TxId {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TxPrecompData<'a> {
    tx: Cow<'a, TxVariant>,
    txid: TxId,
}

impl<'a> TxPrecompData<'a> {
    pub fn from_tx<T>(tx: T) -> Self
    where
        T: Into<Cow<'a, TxVariant>>,
    {
        let tx = tx.into();
        let txid = tx.calc_txid();
        Self { tx, txid }
    }

    #[inline]
    pub fn take(self) -> TxVariant {
        self.tx.into_owned()
    }

    #[inline]
    pub fn tx(&self) -> &TxVariant {
        &self.tx
    }

    #[inline]
    pub fn txid(&self) -> &TxId {
        &self.txid
    }
}

impl<'a> Into<Cow<'a, TxPrecompData<'a>>> for TxPrecompData<'a> {
    fn into(self) -> Cow<'a, TxPrecompData<'a>> {
        Cow::Owned(self)
    }
}

impl<'a> Into<Cow<'a, TxPrecompData<'a>>> for &'a TxPrecompData<'a> {
    fn into(self) -> Cow<'a, TxPrecompData<'a>> {
        Cow::Borrowed(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TxVariant {
    V0(TxVariantV0),
}

impl TxVariant {
    #[inline]
    pub fn precompute(self) -> TxPrecompData<'static> {
        TxPrecompData::from_tx(Cow::Owned(self))
    }

    #[inline]
    pub fn expiry(&self) -> u64 {
        match self {
            TxVariant::V0(tx) => tx.expiry,
        }
    }

    #[inline]
    pub fn sigs(&self) -> &[SigPair] {
        match self {
            TxVariant::V0(tx) => &tx.signature_pairs,
        }
    }

    #[inline]
    pub fn sigs_mut(&mut self) -> &mut Vec<SigPair> {
        match self {
            TxVariant::V0(tx) => &mut tx.signature_pairs,
        }
    }

    #[inline]
    pub fn script(&self) -> Option<&Script> {
        match self {
            TxVariant::V0(var) => match var {
                TxVariantV0::OwnerTx(tx) => unimplemented!(),
                TxVariantV0::MintTx(tx) => unimplemented!(),
                TxVariantV0::CreateAccountTx(tx) => Some(&tx.account.script),
                TxVariantV0::TransferTx(tx) => Some(&tx.script),
            },
        }
    }

    #[inline]
    pub fn calc_txid(&self) -> TxId {
        let mut buf = Vec::with_capacity(4096);
        self.serialize_without_sigs(&mut buf);

        let digest = {
            let mut hasher = DoubleSha256::new();
            hasher.update(&CHAIN_ID);
            hasher.update(&buf);
            hasher.finalize()
        };
        TxId(digest)
    }

    #[inline]
    pub fn sign(&self, key_pair: &KeyPair) -> SigPair {
        let hash = self.calc_txid();
        key_pair.sign(&hash.as_ref())
    }

    #[inline]
    pub fn append_sign(&mut self, key_pair: &KeyPair) {
        let pair = self.sign(key_pair);
        self.sigs_mut().push(pair);
    }

    pub fn serialize(&self, buf: &mut Vec<u8>) {
        self.serialize_without_sigs(buf);
        match self {
            TxVariant::V0(var) => {
                macro_rules! serialize_sigs {
                    ($name:expr) => {{
                        buf.push($name.signature_pairs.len() as u8);
                        for sig in &$name.signature_pairs {
                            buf.push_sig_pair(sig)
                        }
                    }};
                }

                match var {
                    TxVariantV0::OwnerTx(tx) => serialize_sigs!(tx),
                    TxVariantV0::MintTx(tx) => serialize_sigs!(tx),
                    TxVariantV0::CreateAccountTx(tx) => serialize_sigs!(tx),
                    TxVariantV0::TransferTx(tx) => serialize_sigs!(tx),
                }
            }
        };
    }

    pub fn serialize_without_sigs(&self, buf: &mut Vec<u8>) {
        match self {
            TxVariant::V0(var) => {
                // Tx version (2 bytes)
                buf.push_u16(0x00);

                match var {
                    TxVariantV0::OwnerTx(tx) => tx.serialize(buf),
                    TxVariantV0::MintTx(tx) => tx.serialize(buf),
                    TxVariantV0::CreateAccountTx(tx) => tx.serialize(buf),
                    TxVariantV0::TransferTx(tx) => tx.serialize(buf),
                }
            }
        };
    }

    pub fn deserialize(cur: &mut Cursor<&[u8]>) -> Option<TxVariant> {
        let tx_ver = cur.take_u16().ok()?;
        match tx_ver {
            0x00 => {
                let (base, tx_type) = Tx::deserialize_header(cur)?;
                let mut tx = match tx_type {
                    TxType::Owner => TxVariantV0::OwnerTx(OwnerTx::deserialize(cur, base)?),
                    TxType::Mint => TxVariantV0::MintTx(MintTx::deserialize(cur, base)?),
                    TxType::CreateAccount => {
                        TxVariantV0::CreateAccountTx(CreateAccountTx::deserialize(cur, base)?)
                    }
                    TxType::Transfer => {
                        TxVariantV0::TransferTx(TransferTx::deserialize(cur, base)?)
                    }
                };
                tx.signature_pairs = {
                    let len = cur.take_u8().ok()?;
                    let mut sigs = Vec::with_capacity(len as usize);
                    for _ in 0..len {
                        sigs.push(cur.take_sig_pair().ok()?)
                    }
                    sigs
                };
                Some(TxVariant::V0(tx))
            }
            _ => None,
        }
    }
}

impl<'a> Into<Cow<'a, TxVariant>> for TxVariant {
    fn into(self) -> Cow<'a, TxVariant> {
        Cow::Owned(self)
    }
}

impl<'a> Into<Cow<'a, TxVariant>> for &'a TxVariant {
    fn into(self) -> Cow<'a, TxVariant> {
        Cow::Borrowed(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TxVariantV0 {
    OwnerTx(OwnerTx),
    MintTx(MintTx),
    CreateAccountTx(CreateAccountTx),
    TransferTx(TransferTx),
}

impl Deref for TxVariantV0 {
    type Target = Tx;

    fn deref(&self) -> &Self::Target {
        match self {
            TxVariantV0::OwnerTx(tx) => &tx.base,
            TxVariantV0::MintTx(tx) => &tx.base,
            TxVariantV0::CreateAccountTx(tx) => &tx.base,
            TxVariantV0::TransferTx(tx) => &tx.base,
        }
    }
}

impl DerefMut for TxVariantV0 {
    fn deref_mut(&mut self) -> &mut Tx {
        match self {
            TxVariantV0::OwnerTx(tx) => &mut tx.base,
            TxVariantV0::MintTx(tx) => &mut tx.base,
            TxVariantV0::CreateAccountTx(tx) => &mut tx.base,
            TxVariantV0::TransferTx(tx) => &mut tx.base,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tx {
    pub nonce: u32,
    pub expiry: u64,
    pub fee: Asset,
    pub signature_pairs: Vec<SigPair>,
}

impl Tx {
    fn serialize_header(&self, v: &mut Vec<u8>) {
        // The TxType is part of the header and needs to be pushed into the buffer first
        v.push_u32(self.nonce);
        v.push_u64(self.expiry);
        v.push_asset(self.fee);
    }

    fn deserialize_header(cur: &mut Cursor<&[u8]>) -> Option<(Tx, TxType)> {
        let tx_type = match cur.take_u8().ok()? {
            t if t == TxType::Owner as u8 => TxType::Owner,
            t if t == TxType::Mint as u8 => TxType::Mint,
            t if t == TxType::Transfer as u8 => TxType::Transfer,
            _ => return None,
        };
        let nonce = cur.take_u32().ok()?;
        let expiry = cur.take_u64().ok()?;
        let fee = cur.take_asset().ok()?;
        let tx = Tx {
            nonce,
            expiry,
            fee,
            signature_pairs: Vec::new(),
        };

        Some((tx, tx_type))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerTx {
    pub base: Tx,
    pub minter: PublicKey, // Key that signs blocks
    pub wallet: AccountId, // Hot wallet that receives rewards
}

impl SerializeTx for OwnerTx {
    fn serialize(&self, v: &mut Vec<u8>) {
        v.push(TxType::Owner as u8);
        self.serialize_header(v);
        v.push_pub_key(&self.minter);
        v.push_u64(self.wallet);
    }
}

impl DeserializeTx<OwnerTx> for OwnerTx {
    fn deserialize(cur: &mut Cursor<&[u8]>, tx: Tx) -> Option<OwnerTx> {
        let minter = cur.take_pub_key().ok()?;
        let wallet = cur.take_u64().ok()?;
        Some(OwnerTx {
            base: tx,
            minter,
            wallet,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MintTx {
    pub base: Tx,
    pub to: AccountId,
    pub amount: Asset,
    pub attachment: Vec<u8>,
    pub attachment_name: String,
}

impl SerializeTx for MintTx {
    fn serialize(&self, v: &mut Vec<u8>) {
        v.push(TxType::Mint as u8);
        self.serialize_header(v);
        v.push_u64(self.to);
        v.push_asset(self.amount);
        v.push_bytes(&self.attachment);
        v.push_bytes(self.attachment_name.as_bytes());
    }
}

impl DeserializeTx<MintTx> for MintTx {
    fn deserialize(cur: &mut Cursor<&[u8]>, tx: Tx) -> Option<Self> {
        let to = cur.take_u64().ok()?;
        let amount = cur.take_asset().ok()?;
        let attachment = cur.take_bytes().ok()?;
        let attachment_name = {
            let bytes = cur.take_bytes().ok()?;
            String::from_utf8(bytes).ok()?
        };
        Some(Self {
            base: tx,
            to,
            amount,
            attachment,
            attachment_name,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateAccountTx {
    pub base: Tx,
    pub creator: AccountId,
    pub account: Account,
}

impl SerializeTx for CreateAccountTx {
    fn serialize(&self, buf: &mut Vec<u8>) {
        buf.push(TxType::CreateAccount as u8);
        self.serialize_header(buf);
        buf.push_u64(self.creator);
        self.account.serialize(buf);
    }
}

impl DeserializeTx<CreateAccountTx> for CreateAccountTx {
    fn deserialize(cur: &mut Cursor<&[u8]>, tx: Tx) -> Option<Self> {
        let creator = cur.take_u64().ok()?;
        let account = Account::deserialize(cur).ok()?;
        Some(Self {
            base: tx,
            creator,
            account,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferTx {
    pub base: Tx,
    pub from: ScriptHash,
    pub script: Script,
    pub call_fn: u8,
    pub args: Vec<u8>,
    pub amount: Asset,
    pub memo: Vec<u8>,
}

impl SerializeTx for TransferTx {
    fn serialize(&self, v: &mut Vec<u8>) {
        v.push(TxType::Transfer as u8);
        self.serialize_header(v);
        v.push_scripthash(&self.from);
        v.push_bytes(&self.script);
        v.push(self.call_fn);
        v.push_bytes(&self.args);
        v.push_asset(self.amount);
        v.push_bytes(&self.memo);
    }
}

impl DeserializeTx<TransferTx> for TransferTx {
    fn deserialize(cur: &mut Cursor<&[u8]>, tx: Tx) -> Option<TransferTx> {
        let from = ScriptHash(cur.take_digest().ok()?);
        let script = cur.take_bytes().ok()?.into();
        let call_fn = cur.take_u8().ok()?;
        let args = cur.take_bytes().ok()?;
        let amount = cur.take_asset().ok()?;
        let memo = cur.take_bytes().ok()?;
        Some(TransferTx {
            base: tx,
            from,
            script,
            call_fn,
            args,
            amount,
            memo,
        })
    }
}

tx_deref!(OwnerTx);
tx_deref!(MintTx);
tx_deref!(CreateAccountTx);
tx_deref!(TransferTx);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        crypto,
        script::{Arg, Builder, FnBuilder, OpFrame},
    };

    macro_rules! cmp_base_tx {
        ($id:ident, $expiry:expr, $fee:expr) => {
            assert_eq!($id.expiry, $expiry);
            assert_eq!($id.fee.to_string(), $fee);
        };
    }

    #[test]
    fn serialize_tx_with_sigs() {
        let minter = crypto::KeyPair::gen();
        let wallet = crypto::KeyPair::gen();
        let mut owner_tx = TxVariant::V0(TxVariantV0::OwnerTx(OwnerTx {
            base: Tx {
                nonce: 123456789,
                expiry: 1230,
                fee: get_asset("123.00000 TEST"),
                signature_pairs: vec![],
            },
            minter: minter.0.clone(),
            wallet: 0xFF,
        }));

        owner_tx.append_sign(&minter);
        owner_tx.append_sign(&wallet);

        let mut v = vec![];
        owner_tx.serialize(&mut v);

        let mut c = Cursor::<&[u8]>::new(&v);
        let dec = TxVariant::deserialize(&mut c).unwrap();
        assert_eq!(owner_tx, dec);
        assert_eq!(dec.sigs().len(), 2);
        assert_eq!(owner_tx.sigs()[0], dec.sigs()[0]);
        assert_eq!(owner_tx.sigs()[1], dec.sigs()[1]);
    }

    #[test]
    fn serialize_owner() {
        let minter = crypto::KeyPair::gen();
        let wallet = crypto::KeyPair::gen();
        let owner_tx = OwnerTx {
            base: Tx {
                nonce: 123,
                expiry: 1230,
                fee: get_asset("123.00000 TEST"),
                signature_pairs: vec![],
            },
            minter: minter.0,
            wallet: 123,
        };

        let mut v = vec![];
        owner_tx.serialize(&mut v);

        let mut c = Cursor::<&[u8]>::new(&v);
        let (base, tx_type) = Tx::deserialize_header(&mut c).unwrap();
        let dec = OwnerTx::deserialize(&mut c, base).unwrap();
        assert_eq!(owner_tx, dec);

        cmp_base_tx!(dec, 1230, "123.00000 TEST");
        assert_eq!(tx_type, TxType::Owner);
        assert_eq!(owner_tx.minter, dec.minter);
        assert_eq!(owner_tx.wallet, dec.wallet);
    }

    #[test]
    fn serialize_mint() {
        let wallet = crypto::KeyPair::gen();
        let mint_tx = MintTx {
            base: Tx {
                nonce: 123,
                expiry: 1234,
                fee: get_asset("123.00000 TEST"),
                signature_pairs: vec![],
            },
            to: 12345,
            amount: get_asset("10.00000 TEST"),
            attachment: vec![1, 2, 3],
            attachment_name: "abc.pdf".to_owned(),
        };

        let mut v = vec![];
        mint_tx.serialize(&mut v);

        let mut c = Cursor::<&[u8]>::new(&v);
        let (base, tx_type) = Tx::deserialize_header(&mut c).unwrap();
        let dec = MintTx::deserialize(&mut c, base).unwrap();

        cmp_base_tx!(dec, 1234, "123.00000 TEST");
        assert_eq!(tx_type, TxType::Mint);
        assert_eq!(mint_tx.to, dec.to);
        assert_eq!(mint_tx.amount, dec.amount);
        assert_eq!(mint_tx, dec);
    }

    #[test]
    fn serialize_transfer() {
        let from = crypto::KeyPair::gen();
        let transfer_tx = TransferTx {
            base: Tx {
                nonce: 123,
                expiry: 1234567890,
                fee: get_asset("1.23000 TEST"),
                signature_pairs: vec![],
            },
            from: from.0.into(),
            script: vec![1, 2, 3, 4].into(),
            call_fn: 0,
            args: vec![],
            amount: get_asset("1.00456 TEST"),
            memo: Vec::from(String::from("Hello world!").as_bytes()),
        };

        let mut v = vec![];
        transfer_tx.serialize(&mut v);

        let mut c = Cursor::<&[u8]>::new(&v);
        let (base, tx_type) = Tx::deserialize_header(&mut c).unwrap();
        let dec = TransferTx::deserialize(&mut c, base).unwrap();

        cmp_base_tx!(dec, 1234567890, "1.23000 TEST");
        assert_eq!(tx_type, TxType::Transfer);
        assert_eq!(transfer_tx.from, dec.from);
        assert_eq!(transfer_tx.script, vec![1, 2, 3, 4].into());
        assert_eq!(transfer_tx.amount.to_string(), dec.amount.to_string());
        assert_eq!(transfer_tx.memo, dec.memo);
    }

    #[test]
    fn tx_eq() {
        let tx_a = Tx {
            nonce: 123,
            expiry: 1,
            fee: get_asset("10.00000 TEST"),
            signature_pairs: vec![KeyPair::gen().sign(b"hello world")],
        };
        let tx_b = tx_a.clone();
        assert_eq!(tx_a, tx_b);

        let mut tx_b = tx_a.clone();
        tx_b.expiry = tx_b.expiry + 1;
        assert_ne!(tx_a, tx_b);

        let mut tx_b = tx_a.clone();
        tx_b.fee = get_asset("10.00000 TEST");
        assert_eq!(tx_a, tx_b);

        let mut tx_b = tx_a.clone();
        tx_b.fee = get_asset("100.00000 TEST");
        assert_ne!(tx_a, tx_b);

        let mut tx_b = tx_a.clone();
        tx_b.fee = get_asset("1.00000 TEST");
        assert_ne!(tx_a, tx_b);

        let mut tx_b = tx_a.clone();
        tx_b.signature_pairs
            .push(KeyPair::gen().sign(b"hello world"));
        assert_ne!(tx_a, tx_b);
    }

    #[test]
    fn tx_nonce_change_ne() {
        let tx_a = Tx {
            nonce: 123,
            expiry: 1,
            fee: get_asset("10.00000 TEST"),
            signature_pairs: vec![KeyPair::gen().sign(b"hello world")],
        };
        let mut tx_b = tx_a.clone();
        tx_b.nonce = 124;

        let buf_a = {
            let mut buf = Vec::new();
            tx_a.serialize_header(&mut buf);
            buf
        };

        let buf_b = {
            let mut buf = Vec::new();
            tx_b.serialize_header(&mut buf);
            buf
        };

        assert_ne!(buf_a, buf_b);
    }

    #[test]
    fn transfer_tx_eq() {
        let tx_a = TransferTx {
            base: Tx {
                nonce: 123,
                expiry: 1,
                fee: get_asset("10.00000 TEST"),
                signature_pairs: vec![KeyPair::gen().sign(b"hello world")],
            },
            from: KeyPair::gen().0.into(),
            script: Builder::new()
                .push(
                    FnBuilder::new(0, OpFrame::OpDefine(vec![Arg::ScriptHash])).push(OpFrame::True),
                )
                .build()
                .unwrap(),
            call_fn: 0,
            args: vec![],
            amount: get_asset("1.00000 TEST"),
            memo: vec![1, 2, 3],
        };

        let tx_b = tx_a.clone();
        assert_eq!(tx_a, tx_b);

        let mut tx_b = tx_a.clone();
        tx_b.base.fee = get_asset("10.00000 TEST");
        assert_eq!(tx_a, tx_b);

        let mut tx_b = tx_a.clone();
        tx_b.base.fee = get_asset("1.00000 TEST");
        assert_ne!(tx_a, tx_b);

        let mut tx_b = tx_a.clone();
        tx_b.from = KeyPair::gen().0.into();
        assert_ne!(tx_a, tx_b);

        let mut tx_b = tx_a.clone();
        tx_b.script = Builder::new()
            .push(FnBuilder::new(0, OpFrame::OpDefine(vec![Arg::ScriptHash])).push(OpFrame::False))
            .build()
            .unwrap();
        assert_ne!(tx_a, tx_b);

        let mut tx_b = tx_a.clone();
        tx_b.amount = get_asset("10.00000 TEST");
        assert_ne!(tx_a, tx_b);

        let mut tx_b = tx_a.clone();
        tx_b.memo = vec![1, 2, 3, 4];
        assert_ne!(tx_a, tx_b);
    }

    #[test]
    fn precomp_data() {
        let tx = TxVariant::V0(TxVariantV0::TransferTx(TransferTx {
            base: Tx {
                nonce: 123,
                expiry: 1,
                fee: get_asset("10.00000 TEST"),
                signature_pairs: vec![KeyPair::gen().sign(b"hello world")],
            },
            from: KeyPair::gen().0.into(),
            script: Builder::new()
                .push(FnBuilder::new(0, OpFrame::OpDefine(vec![])).push(OpFrame::True))
                .build()
                .unwrap(),
            call_fn: 0,
            args: vec![],
            amount: get_asset("1.00000 TEST"),
            memo: vec![1, 2, 3],
        }));

        let txid = &tx.calc_txid();
        assert_eq!(tx.precompute().txid(), txid);
    }

    fn get_asset(s: &str) -> Asset {
        s.parse().unwrap()
    }
}
