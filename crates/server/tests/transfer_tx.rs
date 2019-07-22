use actix::prelude::*;
use godcoin::{constants::*, prelude::*};
use std::sync::Arc;

mod common;
pub use common::*;

#[test]
fn transfer_from_minter() {
    System::run(|| {
        let minter = TestMinter::new();

        let from_addr = ScriptHash::from(&minter.genesis_info().script);
        let from_bal = minter.chain().get_balance(&from_addr, &[]).unwrap();
        let to_addr = KeyPair::gen();
        let amount = get_asset("1.00000 GRAEL");

        let tx = {
            let mut tx = TxVariant::V0(TxVariantV0::TransferTx(TransferTx {
                base: create_tx_header("1.00000 GRAEL"),
                from: from_addr.clone(),
                to: (&to_addr.0).into(),
                amount,
                memo: vec![],
                script: minter.genesis_info().script.clone(),
            }));
            tx.append_sign(&minter.genesis_info().wallet_keys[3]);
            tx.append_sign(&minter.genesis_info().wallet_keys[0]);
            tx
        };
        let res = minter.request(MsgRequest::Broadcast(tx));
        assert_eq!(res, MsgResponse::Broadcast);
        minter.produce_block().unwrap();

        let chain = minter.chain();
        let cur_bal = chain.get_balance(&to_addr.0.into(), &[]);
        assert_eq!(cur_bal, Some(amount));

        // The fee transfers back to the minter wallet in the form of a reward tx so it
        // must not be subtracted during the assertion
        let cur_bal = chain.get_balance(&from_addr, &[]);
        assert_eq!(cur_bal, from_bal.sub(amount));

        System::current().stop();
    })
    .unwrap();
}

#[test]
fn transfer_from_user() {
    System::run(|| {
        let minter = TestMinter::new();

        let user_1_addr = KeyPair::gen();
        let user_2_addr = KeyPair::gen();

        let res = {
            let tx = {
                let mut tx = TxVariant::V0(TxVariantV0::TransferTx(TransferTx {
                    base: create_tx_header("1.00000 GRAEL"),
                    from: ScriptHash::from(&minter.genesis_info().script),
                    to: (&user_1_addr.0).into(),
                    amount: get_asset("100.00000 GRAEL"),
                    memo: vec![],
                    script: minter.genesis_info().script.clone(),
                }));
                tx.append_sign(&minter.genesis_info().wallet_keys[3]);
                tx.append_sign(&minter.genesis_info().wallet_keys[0]);
                tx
            };
            minter.request(MsgRequest::Broadcast(tx))
        };

        assert_eq!(res, MsgResponse::Broadcast);
        let tx = {
            let mut tx = TxVariant::V0(TxVariantV0::TransferTx(TransferTx {
                base: create_tx_header("1.00000 GRAEL"),
                from: (&user_1_addr.0).into(),
                to: (&user_2_addr.0).into(),
                amount: get_asset("99.00000 GRAEL"),
                memo: vec![],
                script: user_1_addr.0.clone().into(),
            }));
            tx.append_sign(&user_1_addr);
            tx
        };
        let res = minter.request(MsgRequest::Broadcast(tx));
        assert_eq!(res, MsgResponse::Broadcast);
        minter.produce_block().unwrap();

        let user_1_bal = minter.chain().get_balance(&user_1_addr.0.into(), &[]);
        assert_eq!(user_1_bal, Some(get_asset("0.00000 GRAEL")));

        let user_2_bal = minter.chain().get_balance(&user_2_addr.0.into(), &[]);
        assert_eq!(user_2_bal, Some(get_asset("99.00000 GRAEL")));

        let minter_addr = ScriptHash::from(&minter.genesis_info().script);
        let minter_bal = minter.chain().get_balance(&minter_addr, &[]);
        // The fee loops back to the minter in a reward transaction generated by the
        // minter block production
        assert_eq!(minter_bal, Some(get_asset("901.00000 GRAEL")));

        System::current().stop();
    })
    .unwrap();
}

#[test]
fn insufficient_balance_caused_by_fee() {
    System::run(|| {
        let minter = TestMinter::new();

        let from_addr = ScriptHash::from(&minter.genesis_info().script);
        let to_addr = KeyPair::gen();
        let tx = {
            let mut tx = TxVariant::V0(TxVariantV0::TransferTx(TransferTx {
                base: create_tx_header("1001.00000 GRAEL"),
                from: from_addr.clone(),
                to: (&to_addr.0).into(),
                amount: get_asset("0.00000 GRAEL"),
                memo: vec![],
                script: minter.genesis_info().script.clone(),
            }));
            tx.append_sign(&minter.genesis_info().wallet_keys[3]);
            tx.append_sign(&minter.genesis_info().wallet_keys[0]);
            tx
        };
        let res = minter.request(MsgRequest::Broadcast(tx));
        assert_eq!(
            res,
            MsgResponse::Error(net::ErrorKind::TxValidation(
                verify::TxErr::InsufficientBalance
            ))
        );
        minter.produce_block().unwrap();

        let chain = minter.chain();
        let cur_bal = chain.get_balance(&to_addr.0.into(), &[]);
        assert_eq!(cur_bal, Some(get_asset("0.00000 GRAEL")));

        let cur_bal = chain.get_balance(&from_addr, &[]);
        assert_eq!(cur_bal, Some(get_asset("1000.00000 GRAEL")));

        System::current().stop();
    })
    .unwrap();
}

#[test]
fn insufficient_fee() {
    System::run(|| {
        let minter = TestMinter::new();

        let from_addr = ScriptHash::from(&minter.genesis_info().script);
        let info = minter.chain().get_address_info(&from_addr, &[]).unwrap();
        let bad_fee = info
            .total_fee()
            .unwrap()
            .sub(get_asset("0.00001 GRAEL"))
            .unwrap();
        let tx = {
            let mut tx = TxVariant::V0(TxVariantV0::TransferTx(TransferTx {
                base: create_tx_header(&bad_fee.to_string()),
                from: from_addr.clone(),
                to: KeyPair::gen().0.into(),
                amount: get_asset("0.00000 GRAEL"),
                memo: vec![],
                script: minter.genesis_info().script.clone(),
            }));
            tx.append_sign(&minter.genesis_info().wallet_keys[3]);
            tx.append_sign(&minter.genesis_info().wallet_keys[0]);
            tx
        };
        let res = minter.request(MsgRequest::Broadcast(tx));
        assert_eq!(
            res,
            MsgResponse::Error(net::ErrorKind::TxValidation(
                verify::TxErr::InvalidFeeAmount
            ))
        );
        System::current().stop();
    })
    .unwrap();
}

#[test]
fn insufficient_balance_caused_by_amt() {
    System::run(|| {
        let minter = TestMinter::new();

        let from_addr = ScriptHash::from(&minter.genesis_info().script);
        let to_addr = KeyPair::gen();
        let tx = {
            let mut tx = TxVariant::V0(TxVariantV0::TransferTx(TransferTx {
                base: create_tx_header("1.00000 GRAEL"),
                from: from_addr.clone(),
                to: (&to_addr.0).into(),
                amount: get_asset("500000.00000 GRAEL"),
                memo: vec![],
                script: minter.genesis_info().script.clone(),
            }));
            tx.append_sign(&minter.genesis_info().wallet_keys[3]);
            tx.append_sign(&minter.genesis_info().wallet_keys[0]);
            tx
        };
        let res = minter.request(MsgRequest::Broadcast(tx));
        assert_eq!(
            res,
            MsgResponse::Error(net::ErrorKind::TxValidation(
                verify::TxErr::InsufficientBalance
            ))
        );
        minter.produce_block().unwrap();

        let chain = minter.chain();
        let cur_bal = chain.get_balance(&to_addr.0.into(), &[]);
        assert_eq!(cur_bal, Some(get_asset("0.00000 GRAEL")));

        let cur_bal = chain.get_balance(&from_addr, &[]);
        assert_eq!(cur_bal, Some(get_asset("1000.00000 GRAEL")));

        System::current().stop();
    })
    .unwrap();
}

#[test]
fn memo_too_large() {
    System::run(|| {
        let minter = TestMinter::new();

        let from_addr = ScriptHash::from(&minter.genesis_info().script);
        let to_addr = KeyPair::gen();
        let tx = {
            let mut tx = TxVariant::V0(TxVariantV0::TransferTx(TransferTx {
                base: create_tx_header("1.00000 GRAEL"),
                from: from_addr.clone(),
                to: (&to_addr.0).into(),
                amount: get_asset("1.00000 GRAEL"),
                memo: (0..=godcoin::constants::MAX_MEMO_BYTE_SIZE)
                    .map(|_| 0)
                    .collect(),
                script: minter.genesis_info().script.clone(),
            }));
            tx.append_sign(&minter.genesis_info().wallet_keys[3]);
            tx.append_sign(&minter.genesis_info().wallet_keys[0]);
            tx
        };
        let res = minter.request(MsgRequest::Broadcast(tx));
        assert_eq!(
            res,
            MsgResponse::Error(net::ErrorKind::TxValidation(verify::TxErr::TxTooLarge))
        );
        minter.produce_block().unwrap();

        let chain = minter.chain();
        let cur_bal = chain.get_balance(&to_addr.0.into(), &[]);
        assert_eq!(cur_bal, Some(get_asset("0.00000 GRAEL")));

        let cur_bal = chain.get_balance(&from_addr, &[]);
        assert_eq!(cur_bal, Some(get_asset("1000.00000 GRAEL")));

        System::current().stop();
    })
    .unwrap();
}

#[test]
fn script_too_large() {
    System::run(|| {
        let minter = TestMinter::new();

        let from_script = Script::new(
            (0..=godcoin::constants::MAX_SCRIPT_BYTE_SIZE)
                .map(|_| 0)
                .collect(),
        );
        let from_addr = ScriptHash::from(&from_script);
        let to_addr = KeyPair::gen();
        let tx = {
            let mut tx = TxVariant::V0(TxVariantV0::TransferTx(TransferTx {
                base: create_tx_header("1.00000 GRAEL"),
                from: from_addr,
                to: (&to_addr.0).into(),
                amount: get_asset("1.00000 GRAEL"),
                memo: vec![],
                script: from_script,
            }));
            tx.append_sign(&minter.genesis_info().wallet_keys[3]);
            tx.append_sign(&minter.genesis_info().wallet_keys[0]);
            tx
        };
        let res = minter.request(MsgRequest::Broadcast(tx));
        assert_eq!(
            res,
            MsgResponse::Error(net::ErrorKind::TxValidation(verify::TxErr::TxTooLarge))
        );
        System::current().stop();
    })
    .unwrap();
}

#[test]
fn tx_addr_dynamic_fee_increase_in_pool() {
    System::run(|| {
        let minter = TestMinter::new();
        let from_addr = ScriptHash::from(&minter.genesis_info().script);

        let res = minter.request(MsgRequest::GetAddressInfo(from_addr.clone()));
        let addr_info = match res {
            MsgResponse::GetAddressInfo(info) => info,
            _ => panic!("Expected GetAddressInfo response"),
        };

        let tx = {
            let mut tx = TxVariant::V0(TxVariantV0::TransferTx(TransferTx {
                base: create_tx_header(&addr_info.total_fee().unwrap().to_string()),
                from: from_addr.clone(),
                to: KeyPair::gen().0.into(),
                amount: Asset::new(0),
                memo: vec![],
                script: minter.genesis_info().script.clone(),
            }));
            tx.append_sign(&minter.genesis_info().wallet_keys[3]);
            tx.append_sign(&minter.genesis_info().wallet_keys[0]);
            tx
        };
        let res = minter.request(MsgRequest::Broadcast(tx));
        assert_eq!(res, MsgResponse::Broadcast);

        let res = minter.request(MsgRequest::GetAddressInfo(from_addr.clone()));
        let old_addr_info = addr_info;
        let addr_info = match res {
            MsgResponse::GetAddressInfo(info) => info,
            _ => panic!("Expected GetAddressInfo response"),
        };

        assert!(addr_info.addr_fee > old_addr_info.addr_fee);

        // Transaction count always start at 1, so test it as though two transactions
        // were made.
        let expected_fee = GRAEL_FEE_MIN.mul(GRAEL_FEE_MULT.pow(2).unwrap()).unwrap();
        assert_eq!(addr_info.addr_fee, expected_fee);

        minter.produce_block().unwrap();
        let res = minter.request(MsgRequest::GetAddressInfo(from_addr));
        let addr_info = match res {
            MsgResponse::GetAddressInfo(info) => info,
            _ => panic!("Expected GetAddressInfo response"),
        };
        assert_eq!(addr_info.addr_fee, expected_fee);

        System::current().stop();
    })
    .unwrap();
}

#[test]
fn tx_addr_dynamic_fee_increase() {
    System::run(|| {
        let minter = Arc::new(TestMinter::new());
        let from_addr = ScriptHash::from(&minter.genesis_info().script);

        for num in 1..10 {
            let res = minter.request(MsgRequest::GetAddressInfo(from_addr.clone()));
            let addr_info = match res {
                MsgResponse::GetAddressInfo(info) => info,
                _ => panic!("Expected GetAddressInfo response"),
            };

            let expected_fee = GRAEL_FEE_MIN.mul(GRAEL_FEE_MULT.pow(num).unwrap()).unwrap();
            assert_eq!(addr_info.addr_fee, expected_fee);

            let tx = {
                let mut tx = TxVariant::V0(TxVariantV0::TransferTx(TransferTx {
                    base: create_tx_header(&addr_info.total_fee().unwrap().to_string()),
                    from: from_addr.clone(),
                    to: KeyPair::gen().0.into(),
                    amount: Asset::new(0),
                    memo: vec![],
                    script: minter.genesis_info().script.clone(),
                }));
                tx.append_sign(&minter.genesis_info().wallet_keys[3]);
                tx.append_sign(&minter.genesis_info().wallet_keys[0]);
                tx
            };

            let res = minter.request(MsgRequest::Broadcast(tx));
            assert!(!res.is_err());
            assert_eq!(res, MsgResponse::Broadcast);
            minter.produce_block().unwrap();
        }

        for _ in 0..=FEE_RESET_WINDOW {
            minter.produce_block().unwrap();
        }

        let res = minter.request(MsgRequest::GetAddressInfo(from_addr.clone()));
        let addr_info = match res {
            MsgResponse::GetAddressInfo(info) => info,
            _ => panic!("Expected GetAddressInfo response"),
        };

        // Test the delta reset for address fees
        let expected_fee = GRAEL_FEE_MIN.mul(GRAEL_FEE_MULT).unwrap();
        assert_eq!(addr_info.addr_fee, expected_fee);

        System::current().stop();
    })
    .unwrap();
}

#[test]
fn net_fee_dynamic_increase() {
    System::run(|| {
        let minter = Arc::new(TestMinter::new());
        let from_addr = ScriptHash::from(&minter.genesis_info().script);
        let addrs = Arc::new((0..100).map(|_| KeyPair::gen()).collect::<Vec<_>>());

        for addr_index in 0..addrs.len() {
            let tx = {
                let mut tx = TxVariant::V0(TxVariantV0::TransferTx(TransferTx {
                    base: create_tx_header("1.00000 GRAEL"),
                    from: from_addr.clone(),
                    to: (&addrs[addr_index].0).into(),
                    amount: Asset::new(100000),
                    memo: vec![],
                    script: minter.genesis_info().script.clone(),
                }));
                tx.append_sign(&minter.genesis_info().wallet_keys[3]);
                tx.append_sign(&minter.genesis_info().wallet_keys[0]);
                tx
            };

            let req = MsgRequest::Broadcast(tx);
            let res = minter.request(req.clone());
            let exp = MsgResponse::Error(net::ErrorKind::TxValidation(
                verify::TxErr::InvalidFeeAmount,
            ));
            if res == exp {
                for _ in 0..=FEE_RESET_WINDOW {
                    minter.produce_block().unwrap();
                }
                let res = minter.request(req);
                assert_eq!(res, MsgResponse::Broadcast);
            } else {
                assert_eq!(res, MsgResponse::Broadcast);
            }
        }

        for addr_index in 0..addrs.len() {
            let tx = {
                let addr = &addrs[addr_index];
                let mut tx = TxVariant::V0(TxVariantV0::TransferTx(TransferTx {
                    base: create_tx_header("1.00000 GRAEL"),
                    from: (&addr.0).into(),
                    to: from_addr.clone(),
                    amount: Asset::new(0),
                    memo: vec![],
                    script: addr.0.clone().into(),
                }));
                tx.append_sign(&addr);
                tx
            };

            let res = minter.request(MsgRequest::Broadcast(tx));
            assert_eq!(res, MsgResponse::Broadcast);
        }

        // Ensure the network fee gets updated
        for _ in 0..5 {
            minter.produce_block().unwrap();
        }

        {
            let res = minter.request(MsgRequest::GetProperties);
            let props = match res {
                MsgResponse::GetProperties(props) => props,
                _ => panic!("Expected GetProperties response"),
            };

            let chain = minter.chain();
            let max_height = props.height - (props.height % 5);
            let min_height = max_height - NETWORK_FEE_AVG_WINDOW;
            assert!(min_height < max_height);

            let tx_count = (min_height..=max_height).fold(1u64, |tx_count, height| {
                let block = chain.get_block(height).unwrap();
                tx_count + block.txs().len() as u64
            });
            let tx_count = (tx_count / NETWORK_FEE_AVG_WINDOW) as u16;
            assert!(tx_count > 10);

            let fee = GRAEL_FEE_MIN.mul(GRAEL_FEE_NET_MULT.pow(tx_count).unwrap());
            assert_eq!(Some(props.network_fee), fee);
        }

        for _ in 0..=NETWORK_FEE_AVG_WINDOW {
            minter.produce_block().unwrap();
        }

        // Test network delta fee reset
        let expected_fee = GRAEL_FEE_MIN.mul(GRAEL_FEE_NET_MULT);
        assert_eq!(minter.chain().get_network_fee(), expected_fee);

        System::current().stop();
    })
    .unwrap();
}
