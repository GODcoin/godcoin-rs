use crate::{
    prelude::{Properties, SignedBlock},
    serializer::*,
};
use std::convert::{TryFrom, TryInto};
use std::io::{self, Cursor, Error};

#[repr(u8)]
pub enum MsgType {
    Error = 0,
    GetProperties = 1,
    GetBlock = 2,
}

pub enum MsgRequest {
    GetProperties,
    GetBlock(u64), // height
}

impl MsgRequest {
    pub fn serialize(self) -> Vec<u8> {
        match self {
            MsgRequest::GetProperties => vec![MsgType::GetProperties as u8],
            MsgRequest::GetBlock(height) => {
                let mut buf = Vec::with_capacity(9);
                buf.push(MsgType::GetBlock as u8);
                buf.push_u64(height);
                buf
            }
        }
    }

    pub fn deserialize(cursor: &mut Cursor<&[u8]>) -> io::Result<Self> {
        let tag = cursor.take_u8()?;
        match tag {
            t if t == MsgType::GetProperties as u8 => Ok(MsgRequest::GetProperties),
            t if t == MsgType::GetBlock as u8 => {
                let height = cursor.take_u64()?;
                Ok(MsgRequest::GetBlock(height))
            }
            _ => Err(Error::new(
                io::ErrorKind::InvalidData,
                "invalid msg request",
            )),
        }
    }
}

#[derive(Copy, Clone, Debug)]
#[repr(u16)]
pub enum ErrorKind {
    UnknownError = 0,
    InvalidHeight = 1,
}

impl TryFrom<u16> for ErrorKind {
    type Error = Error;

    fn try_from(value: u16) -> Result<ErrorKind, Error> {
        match value {
            e if e == ErrorKind::UnknownError as u16 => Ok(ErrorKind::UnknownError),
            e if e == ErrorKind::InvalidHeight as u16 => Ok(ErrorKind::InvalidHeight),
            _ => Err(Error::new(io::ErrorKind::InvalidData, "unknown error kind")),
        }
    }
}

#[derive(Clone, Debug)]
pub enum MsgResponse {
    Error(ErrorKind, Option<String>), // code, message
    GetProperties(Properties),
    GetBlock(SignedBlock),
}

impl MsgResponse {
    pub fn serialize(self) -> Vec<u8> {
        use std::mem;

        match self {
            MsgResponse::Error(code, msg) => match msg {
                Some(msg) => {
                    let mut buf = Vec::with_capacity(3 + msg.len());
                    buf.push(MsgType::Error as u8);
                    buf.push_u16(code as u16);
                    buf.push_bytes(msg.as_bytes());
                    buf
                }
                None => {
                    let mut buf = Vec::with_capacity(7);
                    buf.push(MsgType::Error as u8);
                    buf.push_u16(code as u16);
                    buf.push_bytes(&[]);
                    buf
                }
            },
            MsgResponse::GetProperties(props) => {
                let mut buf = Vec::with_capacity(1 + mem::size_of::<Properties>());
                buf.push(MsgType::GetProperties as u8);
                buf.push_u64(props.height);
                buf.push_balance(&props.network_fee);
                buf.push_balance(&props.token_supply);
                buf
            }
            MsgResponse::GetBlock(block) => {
                let mut buf = Vec::with_capacity(1_048_576);
                buf.push(MsgType::GetBlock as u8);
                block.encode_with_tx(&mut buf);
                buf
            }
        }
    }

    pub fn deserialize(cursor: &mut Cursor<&[u8]>) -> io::Result<Self> {
        let tag = cursor.take_u8()?;
        match tag {
            t if t == MsgType::Error as u8 => {
                let kind = cursor.take_u16()?.try_into()?;
                let msg = {
                    let buf = cursor.take_bytes()?;
                    if buf.is_empty() {
                        None
                    } else {
                        Some(String::from_utf8_lossy(&buf).into_owned())
                    }
                };
                Ok(MsgResponse::Error(kind, msg))
            }
            t if t == MsgType::GetProperties as u8 => {
                let height = cursor.take_u64()?;
                let network_fee = cursor.take_balance()?;
                let token_supply = cursor.take_balance()?;
                Ok(MsgResponse::GetProperties(Properties {
                    height,
                    network_fee,
                    token_supply,
                }))
            }
            t if t == MsgType::GetBlock as u8 => {
                let block = SignedBlock::decode_with_tx(cursor)
                    .ok_or_else(|| Error::from(io::ErrorKind::UnexpectedEof))?;
                Ok(MsgResponse::GetBlock(block))
            }
            _ => Err(Error::new(
                io::ErrorKind::InvalidData,
                "invalid msg response",
            )),
        }
    }
}