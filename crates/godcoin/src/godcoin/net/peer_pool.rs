use std::sync::{Arc, atomic::{Ordering, AtomicBool}};
use std::net::SocketAddr;
use parking_lot::Mutex;
use tokio::prelude::*;

use blockchain::Blockchain;
use producer::Minter;

use super::peer::PeerType;
use super::client::*;
use super::rpc::*;

pub struct PeerPool {
    peer_addresses: Vec<SocketAddr>,
    peers: Mutex<Vec<PeerState>>
}

impl PeerPool {
    pub fn new(addrs: &[&str]) -> PeerPool {
        let peer_addresses = addrs.into_iter().map(|s| {
            (*s).parse().map_err(|e| {
                format!("failed to parse address: {} {}", *s, e)
            }).unwrap()
        }).collect::<Vec<SocketAddr>>();
        let peers = Mutex::new(Vec::with_capacity(peer_addresses.len()));

        PeerPool {
            peer_addresses,
            peers
        }
    }

    pub fn start(&self, blockchain: &Arc<Blockchain>, minter: &Arc<Option<Minter>>) {
        assert!(self.peers.lock().is_empty(), "peer pool already started");
        for addr in self.peer_addresses.clone() {
            let (tx, rx) = connect_loop(addr, PeerType::NODE);
            let state = PeerState {
                tx,
                rx,
                connected: Arc::new(AtomicBool::new(false))
            };
            let blockchain = Arc::clone(blockchain);
            let minter = Arc::clone(minter);
            self.handle_client_peer(addr, blockchain, minter, state);
        }
    }

    fn handle_client_peer(&self,
                            addr: SocketAddr,
                            blockchain: Arc<Blockchain>,
                            minter: Arc<Option<Minter>>,
                            state: PeerState) {
        macro_rules! quick_send {
            ($state:expr, $id:expr, $msg:expr) => {
                $state.tx.send(ClientEvent::Message(Box::new(RpcPayload {
                    id: $id,
                    msg: Some($msg)
                })))
            };
            ($state:expr, $id:expr) => {
                $state.tx.send(ClientEvent::Message(Box::new(RpcPayload {
                    id: $id,
                    msg: None
                })))
            };
        }

        self.peers.lock().push(state.clone());
        ::tokio::spawn(state.rx.clone().for_each(move |evt| {
            match evt {
                ClientEvent::Connect => {
                    state.connected.store(true, Ordering::Release);
                },
                ClientEvent::Disconnect => {
                    state.connected.store(false, Ordering::Release);
                },
                ClientEvent::Message(rpc) => {
                    let id = rpc.id;
                    let msg = match rpc.msg {
                        Some(msg) => msg,
                        None => return Ok(())
                    };
                    match msg {
                        RpcMsg::Handshake(_) => {
                            warn!("[{}] Invalid handshake message sent from peer", addr);
                        }
                        RpcMsg::Error(_) => {},
                        RpcMsg::Event(evt) => {
                            if let Some(minter) = &*minter {
                                match *evt {
                                    RpcEvent::Block(block) => {
                                        let _ = minter.add_block(block);
                                    },
                                    RpcEvent::Tx(tx) => {
                                        let _ = minter.add_tx(tx);
                                    }
                                }
                            }
                        },
                        RpcMsg::Broadcast(tx) => {
                            if let Some(minter) = &*minter {
                                match minter.add_tx(tx) {
                                    Ok(_) => {
                                        quick_send!(state, id).wait().unwrap();
                                    },
                                    Err(s) => {
                                        quick_send!(state, id, RpcMsg::Error(s)).wait().unwrap();
                                    }
                                }
                            }
                        },
                        RpcMsg::Properties(var) => {
                            if var.req().is_some() {
                                let props = blockchain.get_properties();
                                let var = RpcVariant::Res(props);
                                quick_send!(state, id, RpcMsg::Properties(var)).wait().unwrap();
                            }
                        },
                        RpcMsg::Block(var) => {
                            if let Some(height) = var.req() {
                                let block = match blockchain.get_block(height) {
                                    Some(block) => Some((&*block).clone()),
                                    None => None
                                };
                                let var = Box::new(RpcVariant::Res(block));
                                quick_send!(state, id, RpcMsg::Block(var)).wait().unwrap();
                            }
                        },
                        RpcMsg::Balance(var) => {
                            if let Some(addr) = var.req() {
                                let bal = blockchain.get_balance(&addr);
                                let var = RpcVariant::Res(bal);
                                quick_send!(state, id, RpcMsg::Balance(var)).wait().unwrap();
                            }
                        },
                        RpcMsg::TotalFee(var) => {
                            if let Some(addr) = var.req() {
                                let fee = blockchain.get_total_fee(&addr);
                                match fee {
                                    Some(fee) => {
                                        let var = RpcVariant::Res(fee);
                                        quick_send!(state, id, RpcMsg::TotalFee(var)).wait().unwrap();
                                    },
                                    None => {
                                        let err = "failed to retrieve total fee".to_string();
                                        quick_send!(state, id, RpcMsg::Error(err)).wait().unwrap();
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Ok(())
        }));
    }
}

#[derive(Clone)]
struct PeerState {
    tx: ClientSender,
    rx: ClientReceiver,
    connected: Arc<AtomicBool>
}
