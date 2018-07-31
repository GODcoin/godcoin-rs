extern crate sodiumoxide;
extern crate num_bigint;
extern crate num_traits;
extern crate rocksdb;
extern crate bs58;

pub mod asset;
pub use self::asset::{Asset, AssetSymbol};

pub mod crypto;
pub use self::crypto::{KeyPair, PublicKey, PrivateKey, SigPair, Wif};

pub mod serializer;
pub use self::serializer::*;

pub mod tx;
pub use self::tx::*;

pub mod blockchain;

pub fn init() -> Result<(), ()> {
    sodiumoxide::init()
}
