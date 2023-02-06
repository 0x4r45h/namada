use std::convert::{TryFrom, TryInto};
use std::hash::{Hash, Hasher};

#[cfg(feature = "ferveo-tpke")]
use ark_ec::AffineCurve;
#[cfg(feature = "ferveo-tpke")]
use ark_ec::PairingEngine;
use borsh::{BorshDeserialize, BorshSchema, BorshSerialize};
use prost::Message;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::generated::types;
#[cfg(any(feature = "tendermint", feature = "tendermint-abcipp"))]
use crate::tendermint_proto::abci::ResponseDeliverTx;
use crate::types::key::*;
use crate::types::time::DateTimeUtc;
#[cfg(feature = "ferveo-tpke")]
use crate::types::token::Transfer;
#[cfg(feature = "ferveo-tpke")]
use crate::types::transaction::encrypted::EncryptedTx;
use crate::types::transaction::hash_tx;
#[cfg(feature = "ferveo-tpke")]
use crate::types::transaction::process_tx;
#[cfg(feature = "ferveo-tpke")]
use crate::types::transaction::DecryptedTx;
#[cfg(feature = "ferveo-tpke")]
use crate::types::transaction::EllipticCurve;
#[cfg(feature = "ferveo-tpke")]
use crate::types::transaction::EncryptionKey;
#[cfg(feature = "ferveo-tpke")]
use crate::types::transaction::TxType;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Error decoding a transaction from bytes: {0}")]
    TxDecodingError(prost::DecodeError),
    #[error("Error deserializing transaction field bytes: {0}")]
    TxDeserializingError(std::io::Error),
    #[error("Error decoding an DkgGossipMessage from bytes: {0}")]
    DkgDecodingError(prost::DecodeError),
    #[error("Dkg is empty")]
    NoDkgError,
    #[error("Timestamp is empty")]
    NoTimestampError,
    #[error("Timestamp is invalid: {0}")]
    InvalidTimestamp(prost_types::TimestampOutOfSystemRangeError),
}

pub type Result<T> = std::result::Result<T, Error>;

/// This can be used to sign an arbitrary tx. The signature is produced and
/// verified on the tx data concatenated with the tx code, however the tx code
/// itself is not part of this structure.
///
/// Because the signature is not checked by the ledger, we don't inline it into
/// the `Tx` type directly. Instead, the signature is attached to the `tx.data`,
/// which can then be checked by a validity predicate wasm.
#[derive(Clone, Debug, BorshSerialize, BorshDeserialize, BorshSchema)]
pub struct SignedTxData {
    /// The original tx data bytes, if any
    pub data: Option<Vec<u8>>,
    /// The signature is produced on the tx data concatenated with the tx code
    /// and the timestamp.
    pub sig: common::Signature,
}

/// A generic signed data wrapper for Borsh encode-able data.
#[derive(
    Clone, Debug, BorshSerialize, BorshDeserialize, Serialize, Deserialize,
)]
pub struct Signed<T: BorshSerialize + BorshDeserialize> {
    /// Arbitrary data to be signed
    pub data: T,
    /// The signature of the data
    pub sig: common::Signature,
}

impl<T> PartialEq for Signed<T>
where
    T: BorshSerialize + BorshDeserialize + PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.data == other.data && self.sig == other.sig
    }
}

impl<T> Eq for Signed<T> where
    T: BorshSerialize + BorshDeserialize + Eq + PartialEq
{
}

impl<T> Hash for Signed<T>
where
    T: BorshSerialize + BorshDeserialize + Hash,
{
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.data.hash(state);
        self.sig.hash(state);
    }
}

impl<T> PartialOrd for Signed<T>
where
    T: BorshSerialize + BorshDeserialize + PartialOrd,
{
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.data.partial_cmp(&other.data)
    }
}

impl<T> Signed<T>
where
    T: BorshSerialize + BorshDeserialize,
{
    /// Initialize a new signed data.
    pub fn new(keypair: &common::SecretKey, data: T) -> Self {
        let to_sign = data
            .try_to_vec()
            .expect("Encoding data for signing shouldn't fail");
        let sig = common::SigScheme::sign(keypair, to_sign);
        Self { data, sig }
    }

    /// Verify that the data has been signed by the secret key
    /// counterpart of the given public key.
    pub fn verify(
        &self,
        pk: &common::PublicKey,
    ) -> std::result::Result<(), VerifySigError> {
        let bytes = self
            .data
            .try_to_vec()
            .expect("Encoding data for verifying signature shouldn't fail");
        common::SigScheme::verify_signature_raw(pk, &bytes, &self.sig)
    }
}

/// Failed expansion due to hash of supplied code not matching contained hash
#[derive(Debug)]
pub struct InvalidCodeError;

/// Represents either the literal code of a transaction or its hash. Useful for
/// supporting both wallets that need the full transaction code, and those that
/// only need the hash. Also useful for cases when passing full transaction code
/// around separately from their transactions is cumbersome.
#[derive(
    Clone,
    Debug,
    BorshSerialize,
    BorshDeserialize,
    BorshSchema,
    Hash,
    PartialEq,
    Eq,
)]
pub enum TxCode {
    /// A hash of transaction code
    Hash([u8; 32]),
    /// The full transaction code
    Literal(Vec<u8>),
}

impl TxCode {
    /// Get the literal transaction code if available
    pub fn code(&self) -> Option<Vec<u8>> {
        match self {
            Self::Hash(_hash) => None,
            Self::Literal(lit) => Some(lit.clone()),
        }
    }

    /// Return the transaction code hash
    pub fn code_hash(&self) -> [u8; 32] {
        match self {
            Self::Hash(hash) => *hash,
            Self::Literal(lit) => hash_tx(lit).0,
        }
    }

    /// Expand this reduced Tx using the supplied code only if the the code
    /// hashes to the stored code hash
    pub fn expand(
        &mut self,
        code: Vec<u8>,
    ) -> std::result::Result<(), InvalidCodeError> {
        if hash_tx(&code).0 == self.code_hash() {
            *self = TxCode::Literal(code);
            Ok(())
        } else {
            Err(InvalidCodeError)
        }
    }

    /// Replace a literal code with its hash
    pub fn contract(&mut self) {
        *self = TxCode::Hash(self.code_hash());
    }

    /// Indicates that this object contains the full code
    pub fn is_literal(&self) -> bool {
        matches!(self, Self::Literal(_))
    }

    /// Indicates that this object only contains the hash of the code
    pub fn is_hash(&self) -> bool {
        matches!(self, Self::Hash(_))
    }
}

/// A SigningTx but with the full code embedded. This structure will almost
/// certainly be bigger than SigningTxs and contains enough information to
/// execute the transaction.
#[derive(
    Clone, Debug, BorshSerialize, BorshDeserialize, BorshSchema, PartialEq, Eq,
)]
pub struct Tx {
    pub code: TxCode,
    pub data: Option<Vec<u8>>,
    pub timestamp: DateTimeUtc,
    /// the encrypted inner transaction if data contains a WrapperTx
    #[cfg(feature = "ferveo-tpke")]
    pub inner_tx: Option<EncryptedTx>,
    #[cfg(not(feature = "ferveo-tpke"))]
    pub inner_tx: Option<Vec<u8>>,
    /// the encrypted inner transaction code if data contains a WrapperTx
    #[cfg(feature = "ferveo-tpke")]
    pub inner_tx_code: Option<EncryptedTx>,
    #[cfg(not(feature = "ferveo-tpke"))]
    pub inner_tx_code: Option<Vec<u8>>,
}

impl TryFrom<&[u8]> for Tx {
    type Error = Error;

    fn try_from(tx_bytes: &[u8]) -> Result<Self> {
        let tx = types::Tx::decode(tx_bytes).map_err(Error::TxDecodingError)?;
        let timestamp = match tx.timestamp {
            Some(t) => t.try_into().map_err(Error::InvalidTimestamp)?,
            None => return Err(Error::NoTimestampError),
        };
        let inner_tx = tx
            .inner_tx
            .map(|x| {
                BorshDeserialize::try_from_slice(&x)
                    .map_err(Error::TxDeserializingError)
            })
            .transpose()?;
        let inner_tx_code = tx
            .inner_tx_code
            .map(|x| {
                BorshDeserialize::try_from_slice(&x)
                    .map_err(Error::TxDeserializingError)
            })
            .transpose()?;
        let code = if tx.is_code_hash {
            TxCode::Hash(
                tx.code.try_into().expect("Unable to deserialize code hash"),
            )
        } else {
            TxCode::Literal(tx.code)
        };
        Ok(Tx {
            code,
            data: tx.data,
            timestamp,
            inner_tx,
            inner_tx_code,
        })
    }
}

impl From<Tx> for types::Tx {
    fn from(tx: Tx) -> Self {
        let timestamp = Some(tx.timestamp.into());
        let inner_tx = tx.inner_tx.map(|x| {
            x.try_to_vec()
                .expect("Unable to serialize encrypted transaction")
        });
        let inner_tx_code = tx.inner_tx_code.map(|x| {
            x.try_to_vec()
                .expect("Unable to serialize encrypted transaction code")
        });
        types::Tx {
            code: tx
                .code
                .code()
                .unwrap_or_else(|| tx.code.code_hash().to_vec()),
            is_code_hash: tx.code.is_hash(),
            data: tx.data,
            timestamp,
            inner_tx,
            inner_tx_code,
        }
    }
}

#[cfg(any(feature = "tendermint", feature = "tendermint-abcipp"))]
impl From<Tx> for ResponseDeliverTx {
    #[cfg(not(feature = "ferveo-tpke"))]
    fn from(_tx: Tx) -> ResponseDeliverTx {
        Default::default()
    }

    /// Annotate the Tx with meta-data based on its contents
    #[cfg(feature = "ferveo-tpke")]
    fn from(tx: Tx) -> ResponseDeliverTx {
        use crate::tendermint_proto::abci::{Event, EventAttribute};

        #[cfg(feature = "ABCI")]
        fn encode_str(x: &str) -> Vec<u8> {
            x.as_bytes().to_vec()
        }
        #[cfg(not(feature = "ABCI"))]
        fn encode_str(x: &str) -> String {
            x.to_string()
        }
        #[cfg(feature = "ABCI")]
        fn encode_string(x: String) -> Vec<u8> {
            x.into_bytes()
        }
        #[cfg(not(feature = "ABCI"))]
        fn encode_string(x: String) -> String {
            x
        }
        match process_tx(tx) {
            Ok(TxType::Decrypted(DecryptedTx::Decrypted {
                tx,
                #[cfg(not(feature = "mainnet"))]
                    has_valid_pow: _,
            })) => {
                let empty_vec = vec![];
                let tx_data = tx.data.as_ref().unwrap_or(&empty_vec);
                let signed =
                    if let Ok(signed) = SignedTxData::try_from_slice(tx_data) {
                        signed
                    } else {
                        return Default::default();
                    };
                if let Ok(transfer) = Transfer::try_from_slice(
                    signed.data.as_ref().unwrap_or(&empty_vec),
                ) {
                    let events = vec![Event {
                        r#type: "transfer".to_string(),
                        attributes: vec![
                            EventAttribute {
                                key: encode_str("source"),
                                value: encode_string(transfer.source.encode()),
                                index: true,
                            },
                            EventAttribute {
                                key: encode_str("target"),
                                value: encode_string(transfer.target.encode()),
                                index: true,
                            },
                            EventAttribute {
                                key: encode_str("token"),
                                value: encode_string(transfer.token.encode()),
                                index: true,
                            },
                            EventAttribute {
                                key: encode_str("amount"),
                                value: encode_string(
                                    transfer.amount.to_string(),
                                ),
                                index: true,
                            },
                        ],
                    }];
                    ResponseDeliverTx {
                        events,
                        info: "Transfer tx".to_string(),
                        ..Default::default()
                    }
                } else {
                    Default::default()
                }
            }
            _ => Default::default(),
        }
    }
}

impl Tx {
    pub fn new(code: Vec<u8>, data: Option<Vec<u8>>) -> Self {
        Tx {
            code: TxCode::Literal(code),
            data,
            timestamp: DateTimeUtc::now(),
            inner_tx: None,
            inner_tx_code: None,
        }
    }

    /// Decrypt the wrapped transaction.
    ///
    /// Will fail if the inner transaction does match the
    /// hash commitment or we are unable to recover a
    /// valid Tx from the decoded byte stream.
    #[cfg(feature = "ferveo-tpke")]
    pub fn decrypt_code(
        &self,
        privkey: <EllipticCurve as PairingEngine>::G2Affine,
        inner_tx: EncryptedTx,
    ) -> Option<Vec<u8>> {
        // decrypt the inner tx
        let decrypted = inner_tx.decrypt(privkey);
        // check that the hash equals commitment
        if hash_tx(&decrypted).0 != self.code.code_hash() {
            None
        } else {
            // convert back to Tx type
            Some(decrypted)
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = vec![];
        let tx: types::Tx = self.clone().into();
        tx.encode(&mut bytes)
            .expect("encoding a transaction failed");
        bytes
    }

    /// Hash this transaction leaving out the inner tx and its code, but instead
    /// of including the transaction code in the hash, include its hash instead
    pub fn partial_hash(&self) -> [u8; 32] {
        let timestamp = Some(self.timestamp.into());
        let mut bytes = vec![];
        types::Tx {
            code: self.code.code_hash().to_vec(),
            is_code_hash: true,
            data: self.data.clone(),
            timestamp,
            inner_tx: None,
            inner_tx_code: None,
        }
        .encode(&mut bytes)
        .expect("encoding a transaction failed");
        hash_tx(&bytes).0
    }

    /// Get the hash of this transaction's code
    pub fn code_hash(&self) -> [u8; 32] {
        self.code.code_hash()
    }

    /// Sign a transaction using [`SignedTxData`].
    pub fn sign(self, keypair: &common::SecretKey) -> Self {
        let to_sign = self.partial_hash();
        let sig = common::SigScheme::sign(keypair, to_sign);
        let signed = SignedTxData {
            data: self.data,
            sig,
        }
        .try_to_vec()
        .expect("Encoding transaction data shouldn't fail");
        Tx {
            code: self.code,
            data: Some(signed),
            timestamp: self.timestamp,
            inner_tx: self.inner_tx,
            inner_tx_code: self.inner_tx_code,
        }
    }

    /// Verify that the transaction has been signed by the secret key
    /// counterpart of the given public key.
    pub fn verify_sig(
        &self,
        pk: &common::PublicKey,
        sig: &common::Signature,
    ) -> std::result::Result<(), VerifySigError> {
        // Try to get the transaction data from decoded `SignedTxData`
        let tx_data = self.data.clone().ok_or(VerifySigError::MissingData)?;
        let signed_tx_data = SignedTxData::try_from_slice(&tx_data[..])
            .expect("Decoding transaction data shouldn't fail");
        let data = signed_tx_data.data;
        let tx = Tx {
            code: self.code.clone(),
            data,
            timestamp: self.timestamp,
            inner_tx: self.inner_tx.clone(),
            inner_tx_code: self.inner_tx_code.clone(),
        };
        let signed_data = tx.partial_hash();
        common::SigScheme::verify_signature_raw(pk, &signed_data, sig)
    }

    /// Attach the given transaction to this one. Useful when the data field
    /// contains a WrapperTx and its tx_hash field needs a witness.
    #[cfg(feature = "ferveo-tpke")]
    pub fn attach_inner_tx(
        mut self,
        tx: &Tx,
        encryption_key: EncryptionKey,
    ) -> Self {
        let inner_tx = EncryptedTx::encrypt(&tx.to_bytes(), encryption_key);
        self.inner_tx = Some(inner_tx);
        self
    }

    /// Attach the given transaction code to this one. Useful when the inner_tx
    /// field contains a Tx and its code field needs a witness.
    #[cfg(feature = "ferveo-tpke")]
    pub fn attach_inner_tx_code(
        mut self,
        tx: &[u8],
        encryption_key: EncryptionKey,
    ) -> Self {
        let inner_tx_code = EncryptedTx::encrypt(tx, encryption_key);
        self.inner_tx_code = Some(inner_tx_code);
        self
    }

    /// A validity check on the ciphertext.
    #[cfg(feature = "ferveo-tpke")]
    pub fn validate_ciphertext(&self) -> bool {
        let mut valid = true;
        // Check the inner_tx ciphertext if it is there
        if let Some(inner_tx) = &self.inner_tx {
            valid = valid &&
                inner_tx.0.check(&<EllipticCurve as PairingEngine>::G1Prepared::from(
                    -<EllipticCurve as PairingEngine>::G1Affine::prime_subgroup_generator(),
                ));
        }
        // Check the inner_tx_code ciphertext if it is there
        if let Some(inner_tx_code) = &self.inner_tx_code {
            valid = valid &&
                inner_tx_code.0.check(&<EllipticCurve as PairingEngine>::G1Prepared::from(
                    -<EllipticCurve as PairingEngine>::G1Affine::prime_subgroup_generator(),
                ));
        }
        valid
    }
}

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub struct DkgGossipMessage {
    pub dkg: Dkg,
}

impl TryFrom<&[u8]> for DkgGossipMessage {
    type Error = Error;

    fn try_from(dkg_bytes: &[u8]) -> Result<Self> {
        let message = types::DkgGossipMessage::decode(dkg_bytes)
            .map_err(Error::DkgDecodingError)?;
        match &message.dkg_message {
            Some(types::dkg_gossip_message::DkgMessage::Dkg(dkg)) => {
                Ok(DkgGossipMessage {
                    dkg: dkg.clone().into(),
                })
            }
            None => Err(Error::NoDkgError),
        }
    }
}

impl From<DkgGossipMessage> for types::DkgGossipMessage {
    fn from(message: DkgGossipMessage) -> Self {
        types::DkgGossipMessage {
            dkg_message: Some(types::dkg_gossip_message::DkgMessage::Dkg(
                message.dkg.into(),
            )),
        }
    }
}

#[allow(dead_code)]
impl DkgGossipMessage {
    pub fn new(dkg: Dkg) -> Self {
        DkgGossipMessage { dkg }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = vec![];
        let message: types::DkgGossipMessage = self.clone().into();
        message
            .encode(&mut bytes)
            .expect("encoding a DKG gossip message failed");
        bytes
    }
}

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub struct Dkg {
    pub data: String,
}

impl From<types::Dkg> for Dkg {
    fn from(dkg: types::Dkg) -> Self {
        Dkg { data: dkg.data }
    }
}

impl From<Dkg> for types::Dkg {
    fn from(dkg: Dkg) -> Self {
        types::Dkg { data: dkg.data }
    }
}

#[allow(dead_code)]
impl Dkg {
    pub fn new(data: String) -> Self {
        Dkg { data }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tx() {
        let code = "wasm code".as_bytes().to_owned();
        let data = "arbitrary data".as_bytes().to_owned();
        let tx = Tx::new(code.clone(), Some(data.clone()));

        let bytes = tx.to_bytes();
        let tx_from_bytes =
            Tx::try_from(bytes.as_ref()).expect("decoding failed");
        assert_eq!(tx_from_bytes, tx);

        let types_tx = types::Tx {
            code,
            is_code_hash: false,
            data: Some(data),
            timestamp: None,
            inner_tx: None,
            inner_tx_code: None,
        };
        let mut bytes = vec![];
        types_tx.encode(&mut bytes).expect("encoding failed");
        match Tx::try_from(bytes.as_ref()) {
            Err(Error::NoTimestampError) => {}
            _ => panic!("unexpected result"),
        }
    }

    #[test]
    fn test_dkg_gossip_message() {
        let data = "arbitrary string".to_owned();
        let dkg = Dkg::new(data);
        let message = DkgGossipMessage::new(dkg);

        let bytes = message.to_bytes();
        let message_from_bytes = DkgGossipMessage::try_from(bytes.as_ref())
            .expect("decoding failed");
        assert_eq!(message_from_bytes, message);
    }

    #[test]
    fn test_dkg() {
        let data = "arbitrary string".to_owned();
        let dkg = Dkg::new(data);

        let types_dkg: types::Dkg = dkg.clone().into();
        let dkg_from_types = Dkg::from(types_dkg);
        assert_eq!(dkg_from_types, dkg);
    }
}
