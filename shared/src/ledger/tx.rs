//! SDK functions to construct different types of transactions
use std::borrow::Cow;

use borsh::BorshSerialize;
use itertools::Either::*;
use masp_primitives::transaction::builder;
use namada_core::types::address::{masp, masp_tx_key, Address};
use prost::EncodeError;
use rust_decimal::Decimal;
use thiserror::Error;
use tokio::time::Duration;

use crate::ibc::applications::ics20_fungible_token_transfer::msgs::transfer::MsgTransfer;
use crate::ibc::signer::Signer;
use crate::ibc::timestamp::Timestamp as IbcTimestamp;
use crate::ibc::tx_msg::Msg;
use crate::ibc::Height as IbcHeight;
use crate::ibc_proto::cosmos::base::v1beta1::Coin;
use crate::ledger::args;
use crate::ledger::governance::storage as gov_storage;
use crate::ledger::masp::{ShieldedContext, ShieldedUtils};
use crate::ledger::pos::{BondId, Bonds, CommissionRates, Unbonds};
use crate::ledger::rpc::{self, TxBroadcastData, TxResponse};
use crate::ledger::signing::{find_keypair, sign_tx, tx_signer, TxSigningKey};
use crate::ledger::wallet::{Wallet, WalletUtils};
use crate::proto::Tx;
use crate::tendermint_rpc::endpoint::broadcast::tx_sync::Response;
use crate::tendermint_rpc::error::Error as RpcError;
use crate::types::key::*;
use crate::types::masp::TransferTarget;
use crate::types::storage::{Epoch, RESERVED_ADDRESS_PREFIX};
use crate::types::time::DateTimeUtc;
use crate::types::transaction::{pos, InitAccount, UpdateVp};
use crate::types::{storage, token};
use crate::vm::WasmValidationError;
use crate::{ledger, vm};

/// Default timeout in seconds for requests to the `/accepted`
/// and `/applied` ABCI query endpoints.
const DEFAULT_NAMADA_EVENTS_MAX_WAIT_TIME_SECONDS: u64 = 60;

/// Errors to do with transaction events.
#[derive(Error, Debug)]
pub enum Error {
    /// Expect a dry running transaction
    #[error(
        "Expected a dry-run transaction, received a wrapper transaction \
         instead: {0:?}"
    )]
    ExpectDryRun(Tx),
    /// Expect a wrapped encrypted running transaction
    #[error("Cannot broadcast a dry-run transaction")]
    ExpectWrappedRun(Tx),
    /// Error during broadcasting a transaction
    #[error("Encountered error while broadcasting transaction: {0}")]
    TxBroadcast(RpcError),
    /// Invalid comission rate set
    #[error("Invalid new commission rate, received {0}")]
    InvalidCommisionRate(Decimal),
    /// Invalid validator address
    #[error("The address {0} doesn't belong to any known validator account.")]
    InvalidValidatorAddress(Address),
    /// Rate of epoch change too large for current epoch
    #[error(
        "New rate, {0}, is too large of a change with respect to the \
         predecessor epoch in which the rate will take effect."
    )]
    TooLargeOfChange(Decimal),
    /// Error retrieving from storage
    #[error("Error retrieving from storage")]
    Retrival,
    /// No unbonded bonds ready to withdraw in the current epoch
    #[error(
        "There are no unbonded bonds ready to withdraw in the current epoch \
         {0}."
    )]
    NoUnbondReady(Epoch),
    /// No unbonded bonds found
    #[error("No unbonded bonds found")]
    NoUnbondFound,
    /// No bonds found
    #[error("No bonds found")]
    NoBondFound,
    /// Lower bond amount than the unbond
    #[error(
        "The total bonds of the source {0} is lower than the amount to be \
         unbonded. Amount to unbond is {1} and the total bonds is {2}."
    )]
    LowerBondThanUnbond(Address, token::Amount, token::Amount),
    /// Balance is too low
    #[error(
        "The balance of the source {0} of token {1} is lower than the amount \
         to be transferred. Amount to transfer is {2} and the balance is {3}."
    )]
    BalanceTooLow(Address, Address, token::Amount, token::Amount),
    /// Token Address does not exist on chain
    #[error("The token address {0} doesn't exist on chain.")]
    TokenDoesNotExist(Address),
    /// Source address does not exist on chain
    #[error("The address {0} doesn't exist on chain.")]
    LocationDoesNotExist(Address),
    /// Target Address does not exist on chain
    #[error("The source address {0} doesn't exist on chain.")]
    SourceDoesNotExist(Address),
    /// Source Address does not exist on chain
    #[error("The target address {0} doesn't exist on chain.")]
    TargetLocationDoesNotExist(Address),
    /// No Balance found for token
    #[error("No balance found for the source {0} of token {1}")]
    NoBalanceForToken(Address, Address),
    /// Negative balance after transfer
    #[error(
        "The balance of the source {0} is lower than the amount to be \
         transferred and fees. Amount to transfer is {1} {2} and fees are {3} \
         {4}."
    )]
    NegativeBalanceAfterTransfer(
        Address,
        token::Amount,
        Address,
        token::Amount,
        Address,
    ),
    /// No Balance found for token
    #[error("{0}")]
    MaspError(builder::Error),
    /// Wasm validation failed
    #[error("Validity predicate code validation failed with {0}")]
    WasmValidationFailure(WasmValidationError),
    /// Encoding transaction failure
    #[error("Encoding tx data, {0}, shouldn't fail")]
    EncodeTxFailure(std::io::Error),
    /// Like EncodeTxFailure but for the encode error type
    #[error("Encoding tx data, {0}, shouldn't fail")]
    EncodeFailure(EncodeError),
    /// Encoding public key failure
    #[error("Encoding a public key, {0}, shouldn't fail")]
    EncodeKeyFailure(std::io::Error),
    /// Updating an VP of an implicit account
    #[error(
        "A validity predicate of an implicit address cannot be directly \
         updated. You can use an established address for this purpose."
    )]
    ImplicitUpdate,
    // This should be removed? or rather refactored as it communicates
    // the same information as the ImplicitUpdate
    /// Updating a VP of an internal implicit address
    #[error(
        "A validity predicate of an internal address cannot be directly \
         updated."
    )]
    ImplicitInternalError,
    /// Epoch not in storage
    #[error("Proposal end epoch is not in the storage.")]
    EpochNotInStorage,
    /// Other Errors that may show up when using the interface
    #[error("{0}")]
    Other(String),
}

/// Submit transaction and wait for result. Returns a list of addresses
/// initialized in the transaction if any. In dry run, this is always empty.
pub async fn process_tx<
    C: crate::ledger::queries::Client + Sync,
    U: WalletUtils,
>(
    client: &C,
    wallet: &mut Wallet<U>,
    args: &args::Tx,
    tx: Tx,
    default_signer: TxSigningKey,
) -> Result<Vec<Address>, Error> {
    let to_broadcast =
        sign_tx::<C, U>(client, wallet, tx, args, default_signer).await?;
    // NOTE: use this to print the request JSON body:

    // let request =
    // tendermint_rpc::endpoint::broadcast::tx_commit::Request::new(
    //     tx_bytes.clone().into(),
    // );
    // use tendermint_rpc::Request;
    // let request_body = request.into_json();
    // println!("HTTP request body: {}", request_body);

    if args.dry_run {
        expect_dry_broadcast(to_broadcast, client, vec![]).await
    } else {
        // Either broadcast or submit transaction and collect result into
        // sum type
        let result = if args.broadcast_only {
            Left(broadcast_tx(client, &to_broadcast).await)
        } else {
            Right(submit_tx(client, to_broadcast).await)
        };
        // Return result based on executed operation, otherwise deal with
        // the encountered errors uniformly
        match result {
            Right(Ok(result)) => Ok(result.initialized_accounts),
            Left(Ok(_)) => Ok(Vec::default()),
            Right(Err(err)) => Err(err),
            Left(Err(err)) => Err(err),
        }
    }
}

/// Submit transaction to reveal public key
pub async fn submit_reveal_pk<
    C: crate::ledger::queries::Client + Sync,
    U: WalletUtils,
>(
    client: &C,
    wallet: &mut Wallet<U>,
    args: args::RevealPk,
) -> Result<(), Error> {
    let args::RevealPk {
        tx: args,
        public_key,
    } = args;
    let public_key = public_key;
    if !reveal_pk_if_needed::<C, U>(client, wallet, &public_key, &args).await? {
        let addr: Address = (&public_key).into();
        println!("PK for {addr} is already revealed, nothing to do.");
        Ok(())
    } else {
        Ok(())
    }
}

/// Submit transaction to rveeal public key if needed
pub async fn reveal_pk_if_needed<
    C: crate::ledger::queries::Client + Sync,
    U: WalletUtils,
>(
    client: &C,
    wallet: &mut Wallet<U>,
    public_key: &common::PublicKey,
    args: &args::Tx,
) -> Result<bool, Error> {
    let addr: Address = public_key.into();
    // Check if PK revealed
    if args.force || !has_revealed_pk(client, &addr).await {
        // If not, submit it
        submit_reveal_pk_aux::<C, U>(client, wallet, public_key, args).await?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Check if the public key for the given address has been revealed
pub async fn has_revealed_pk<C: crate::ledger::queries::Client + Sync>(
    client: &C,
    addr: &Address,
) -> bool {
    rpc::get_public_key(client, addr).await.is_some()
}

/// Submit transaction to reveal the given public key
pub async fn submit_reveal_pk_aux<
    C: crate::ledger::queries::Client + Sync,
    U: WalletUtils,
>(
    client: &C,
    wallet: &mut Wallet<U>,
    public_key: &common::PublicKey,
    args: &args::Tx,
) -> Result<(), Error> {
    let addr: Address = public_key.into();
    println!("Submitting a tx to reveal the public key for address {addr}...");
    let tx_data = public_key.try_to_vec().map_err(Error::EncodeKeyFailure)?;
    let tx_code = args.tx_code_path.clone();
    let tx = Tx::new(tx_code, Some(tx_data));

    // submit_tx without signing the inner tx
    let keypair = if let Some(signing_key) = &args.signing_key {
        Ok(signing_key.clone())
    } else if let Some(signer) = args.signer.as_ref() {
        let signer = signer;
        find_keypair::<C, U>(client, wallet, signer).await
    } else {
        find_keypair::<C, U>(client, wallet, &addr).await
    }?;
    let epoch = rpc::query_epoch(client).await;
    let to_broadcast = if args.dry_run {
        TxBroadcastData::DryRun(tx)
    } else {
        super::signing::sign_wrapper(args, epoch, tx, &keypair).await
    };

    // Logic is the same as process_tx
    if args.dry_run {
        expect_dry_broadcast(to_broadcast, client, ()).await
    } else {
        // Either broadcast or submit transaction and collect result into
        // sum type
        let result = if args.broadcast_only {
            Left(broadcast_tx(client, &to_broadcast).await)
        } else {
            Right(submit_tx(client, to_broadcast).await)
        };
        // Return result based on executed operation, otherwise deal with
        // the encountered errors uniformly
        match result {
            Right(Err(err)) => Err(err),
            Left(Err(err)) => Err(err),
            _ => Ok(()),
        }
    }
}

/// Broadcast a transaction to be included in the blockchain and checks that
/// the tx has been successfully included into the mempool of a validator
///
/// In the case of errors in any of those stages, an error message is returned
pub async fn broadcast_tx<C: crate::ledger::queries::Client + Sync>(
    rpc_cli: &C,
    to_broadcast: &TxBroadcastData,
) -> Result<Response, Error> {
    let (tx, wrapper_tx_hash, decrypted_tx_hash) = match to_broadcast {
        TxBroadcastData::Wrapper {
            tx,
            wrapper_hash,
            decrypted_hash,
        } => Ok((tx, wrapper_hash, decrypted_hash)),
        TxBroadcastData::DryRun(tx) => Err(Error::ExpectWrappedRun(tx.clone())),
    }?;

    tracing::debug!(
        transaction = ?to_broadcast,
        "Broadcasting transaction",
    );

    // TODO: configure an explicit timeout value? we need to hack away at
    // `tendermint-rs` for this, which is currently using a hard-coded 30s
    // timeout.
    let response =
        lift_rpc_error(rpc_cli.broadcast_tx_sync(tx.to_bytes().into()).await)?;

    if response.code == 0.into() {
        println!("Transaction added to mempool: {:?}", response);
        // Print the transaction identifiers to enable the extraction of
        // acceptance/application results later
        {
            println!("Wrapper transaction hash: {:?}", wrapper_tx_hash);
            println!("Inner transaction hash: {:?}", decrypted_tx_hash);
        }
        Ok(response)
    } else {
        Err(Error::TxBroadcast(RpcError::server(
            serde_json::to_string(&response).unwrap(),
        )))
    }
}

/// Broadcast a transaction to be included in the blockchain.
///
/// Checks that
/// 1. The tx has been successfully included into the mempool of a validator
/// 2. The tx with encrypted payload has been included on the blockchain
/// 3. The decrypted payload of the tx has been included on the blockchain.
///
/// In the case of errors in any of those stages, an error message is returned
pub async fn submit_tx<C: crate::ledger::queries::Client + Sync>(
    client: &C,
    to_broadcast: TxBroadcastData,
) -> Result<TxResponse, Error> {
    let (_, wrapper_hash, decrypted_hash) = match &to_broadcast {
        TxBroadcastData::Wrapper {
            tx,
            wrapper_hash,
            decrypted_hash,
        } => Ok((tx, wrapper_hash, decrypted_hash)),
        TxBroadcastData::DryRun(tx) => Err(Error::ExpectWrappedRun(tx.clone())),
    }?;

    // Broadcast the supplied transaction
    broadcast_tx(client, &to_broadcast).await?;

    let deadline =
        Duration::from_secs(DEFAULT_NAMADA_EVENTS_MAX_WAIT_TIME_SECONDS);

    tracing::debug!(
        transaction = ?to_broadcast,
        ?deadline,
        "Awaiting transaction approval",
    );

    let parsed = {
        let wrapper_query =
            crate::ledger::rpc::TxEventQuery::Accepted(wrapper_hash.as_str());
        let event = rpc::query_tx_status(client, wrapper_query, deadline).await;
        let parsed = TxResponse::from_event(event);

        println!(
            "Transaction accepted with result: {}",
            serde_json::to_string_pretty(&parsed).unwrap()
        );
        // The transaction is now on chain. We wait for it to be decrypted
        // and applied
        if parsed.code == 0.to_string() {
            // We also listen to the event emitted when the encrypted
            // payload makes its way onto the blockchain
            let decrypted_query =
                rpc::TxEventQuery::Applied(decrypted_hash.as_str());
            let event =
                rpc::query_tx_status(client, decrypted_query, deadline).await;
            let parsed = TxResponse::from_event(event);
            println!(
                "Transaction applied with result: {}",
                serde_json::to_string_pretty(&parsed).unwrap()
            );
            Ok(parsed)
        } else {
            Ok(parsed)
        }
    };

    tracing::debug!(
        transaction = ?to_broadcast,
        "Transaction approved",
    );

    parsed
}

/// Save accounts initialized from a tx into the wallet, if any.
pub async fn save_initialized_accounts<U: WalletUtils>(
    wallet: &mut Wallet<U>,
    alias: String,
    initialized_accounts: Vec<Address>,
) {
    let len = initialized_accounts.len();
    if len != 0 {
        // Store newly initialized account addresses in the wallet
        println!(
            "The transaction initialized {} new account{}",
            len,
            if len == 1 { "" } else { "s" }
        );
        // Store newly initialized account addresses in the wallet
        for (ix, address) in initialized_accounts.iter().enumerate() {
            let encoded = address.encode();
            let alias: Cow<str> = if len == 1 {
                // If there's only one account, use the
                // alias as is
                Cow::from(&alias)
            } else {
                // If there're multiple accounts, use
                // the alias as prefix, followed by
                // index number
                Cow::from(format!("{}{}", alias, ix))
            };
            let alias = alias.into_owned();
            let added = wallet.add_address(alias.clone(), address.clone());
            match added {
                Some(new_alias) if new_alias != encoded => {
                    println!(
                        "Added alias {} for address {}.",
                        new_alias, encoded
                    );
                }
                _ => println!("No alias added for address {}.", encoded),
            };
        }
    }
}

/// Submit validator comission rate change
pub async fn submit_validator_commission_change<
    C: crate::ledger::queries::Client + Sync,
    U: WalletUtils,
>(
    client: &C,
    wallet: &mut Wallet<U>,
    args: args::TxCommissionRateChange,
) -> Result<(), Error> {
    let epoch = rpc::query_epoch(client).await;

    let tx_code = args.tx_code_path;

    let validator = args.validator.clone();
    if rpc::is_validator(client, &validator).await {
        if args.rate < Decimal::ZERO || args.rate > Decimal::ONE {
            if args.tx.force {
                eprintln!(
                    "Invalid new commission rate, received {}",
                    args.rate
                );
                Ok(())
            } else {
                Err(Error::InvalidCommisionRate(args.rate))
            }
        } else {
            Ok(())
        }?;

        let commission_rate_key =
            ledger::pos::validator_commission_rate_key(&validator);
        let max_commission_rate_change_key =
            ledger::pos::validator_max_commission_rate_change_key(&validator);
        let commission_rates = rpc::query_storage_value::<C, CommissionRates>(
            client,
            &commission_rate_key,
        )
        .await;
        let max_change = rpc::query_storage_value::<C, Decimal>(
            client,
            &max_commission_rate_change_key,
        )
        .await;

        match (commission_rates, max_change) {
            (Some(rates), Some(max_change)) => {
                // Assuming that pipeline length = 2
                let rate_next_epoch = rates.get(epoch.next()).unwrap();
                let epoch_change = (args.rate - rate_next_epoch).abs();
                if epoch_change > max_change {
                    if args.tx.force {
                        eprintln!(
                            "New rate, {epoch_change}, is too large of a \
                             change with respect to the predecessor epoch in \
                             which the rate will take effect."
                        );
                        Ok(())
                    } else {
                        Err(Error::TooLargeOfChange(epoch_change))
                    }
                } else {
                    Ok(())
                }
            }
            _ => {
                if args.tx.force {
                    eprintln!("Error retrieving from storage");
                    Ok(())
                } else {
                    Err(Error::Retrival)
                }
            }
        }?;
        Ok(())
    } else if args.tx.force {
        eprintln!("The given address {validator} is not a validator.");
        Ok(())
    } else {
        Err(Error::InvalidValidatorAddress(validator))
    }?;

    let data = pos::CommissionChange {
        validator: args.validator.clone(),
        new_rate: args.rate,
    };
    let data = data.try_to_vec().map_err(Error::EncodeTxFailure)?;

    let tx = Tx::new(tx_code, Some(data));
    let default_signer = args.validator.clone();
    process_tx::<C, U>(
        client,
        wallet,
        &args.tx,
        tx,
        TxSigningKey::WalletAddress(default_signer),
    )
    .await?;
    Ok(())
}

/// Submit transaction to withdraw an unbond
pub async fn submit_withdraw<
    C: crate::ledger::queries::Client + Sync,
    U: WalletUtils,
>(
    client: &C,
    wallet: &mut Wallet<U>,
    args: args::Withdraw,
) -> Result<(), Error> {
    let epoch = rpc::query_epoch(client).await;

    let validator =
        known_validator_or_err(args.validator.clone(), args.tx.force, client)
            .await?;

    let source = args.source.clone();
    let tx_code = args.tx_code_path;

    // Check the source's current unbond amount
    let bond_source = source.clone().unwrap_or_else(|| validator.clone());
    let bond_id = BondId {
        source: bond_source.clone(),
        validator: validator.clone(),
    };
    let bond_key = ledger::pos::unbond_key(&bond_id);
    let unbonds =
        rpc::query_storage_value::<C, Unbonds>(client, &bond_key).await;
    match unbonds {
        Some(unbonds) => {
            let mut unbonded_amount: token::Amount = 0.into();
            if let Some(unbond) = unbonds.get(epoch) {
                for delta in unbond.deltas.values() {
                    unbonded_amount += *delta;
                }
            }
            if unbonded_amount == 0.into() {
                if args.tx.force {
                    eprintln!(
                        "There are no unbonded bonds ready to withdraw in the \
                         current epoch {}.",
                        epoch
                    );
                    Ok(())
                } else {
                    Err(Error::NoUnbondReady(epoch))
                }
            } else {
                Ok(())
            }
        }
        None => {
            if args.tx.force {
                eprintln!("No unbonded bonds found");
                Ok(())
            } else {
                Err(Error::NoUnbondFound)
            }
        }
    }?;

    let data = pos::Withdraw { validator, source };
    let data = data.try_to_vec().map_err(Error::EncodeTxFailure)?;

    let tx = Tx::new(tx_code, Some(data));
    let default_signer = args.source.unwrap_or(args.validator);
    process_tx::<C, U>(
        client,
        wallet,
        &args.tx,
        tx,
        TxSigningKey::WalletAddress(default_signer),
    )
    .await?;
    Ok(())
}

/// Submit a transaction to unbond
pub async fn submit_unbond<
    C: crate::ledger::queries::Client + Sync,
    U: WalletUtils,
>(
    client: &C,
    wallet: &mut Wallet<U>,
    args: args::Unbond,
) -> Result<(), Error> {
    let validator =
        known_validator_or_err(args.validator.clone(), args.tx.force, client)
            .await?;
    let source = args.source.clone();
    let tx_code = args.tx_code_path;

    // Check the source's current bond amount
    let bond_source = source.clone().unwrap_or_else(|| validator.clone());
    let bond_id = BondId {
        source: bond_source.clone(),
        validator: validator.clone(),
    };
    let bond_key = ledger::pos::bond_key(&bond_id);
    let bonds = rpc::query_storage_value::<C, Bonds>(client, &bond_key).await;
    match bonds {
        Some(bonds) => {
            let mut bond_amount: token::Amount = 0.into();
            for bond in bonds.iter() {
                for delta in bond.pos_deltas.values() {
                    bond_amount += *delta;
                }
            }
            if args.amount > bond_amount {
                if args.tx.force {
                    eprintln!(
                        "The total bonds of the source {} is lower than the \
                         amount to be unbonded. Amount to unbond is {} and \
                         the total bonds is {}.",
                        bond_source, args.amount, bond_amount
                    );
                    Ok(())
                } else {
                    Err(Error::LowerBondThanUnbond(
                        bond_source,
                        args.amount,
                        bond_amount,
                    ))
                }
            } else {
                Ok(())
            }
        }
        None => {
            if args.tx.force {
                eprintln!("No bonds found");
                Ok(())
            } else {
                Err(Error::NoBondFound)
            }
        }
    }?;

    let data = pos::Unbond {
        validator,
        amount: args.amount,
        source,
    };
    let data = data.try_to_vec().map_err(Error::EncodeTxFailure)?;

    let tx = Tx::new(tx_code, Some(data));
    let default_signer = args.source.unwrap_or(args.validator);
    process_tx::<C, U>(
        client,
        wallet,
        &args.tx,
        tx,
        TxSigningKey::WalletAddress(default_signer),
    )
    .await?;
    Ok(())
}

/// Submit a transaction to bond
pub async fn submit_bond<
    C: crate::ledger::queries::Client + Sync,
    U: WalletUtils,
>(
    client: &C,
    wallet: &mut Wallet<U>,
    args: args::Bond,
) -> Result<(), Error> {
    let validator =
        known_validator_or_err(args.validator.clone(), args.tx.force, client)
            .await?;

    // Check that the source address exists on chain
    let source = args.source.clone();
    let source = match args.source.clone() {
        Some(source) => source_exists_or_err(source, args.tx.force, client)
            .await
            .map(Some),
        None => Ok(source),
    }?;
    // Check bond's source (source for delegation or validator for self-bonds)
    // balance
    let bond_source = source.as_ref().unwrap_or(&validator);
    let balance_key = token::balance_key(&args.native_token, bond_source);

    // TODO Should we state the same error message for the native token?
    check_balance_too_low_err(
        &args.native_token,
        bond_source,
        args.amount,
        balance_key,
        args.tx.force,
        client,
    )
    .await?;

    let tx_code = args.tx_code_path;
    let bond = pos::Bond {
        validator,
        amount: args.amount,
        source,
    };
    let data = bond.try_to_vec().map_err(Error::EncodeTxFailure)?;

    let tx = Tx::new(tx_code, Some(data));
    let default_signer = args.source.unwrap_or(args.validator);
    process_tx::<C, U>(
        client,
        wallet,
        &args.tx,
        tx,
        TxSigningKey::WalletAddress(default_signer),
    )
    .await?;
    Ok(())
}

/// Check if current epoch is in the last third of the voting period of the
/// proposal. This ensures that it is safe to optimize the vote writing to
/// storage.
pub async fn is_safe_voting_window<C: crate::ledger::queries::Client + Sync>(
    client: &C,
    proposal_id: u64,
    proposal_start_epoch: Epoch,
) -> Result<bool, Error> {
    let current_epoch = rpc::query_epoch(client).await;

    let proposal_end_epoch_key =
        gov_storage::get_voting_end_epoch_key(proposal_id);
    let proposal_end_epoch =
        rpc::query_storage_value::<C, Epoch>(client, &proposal_end_epoch_key)
            .await;

    match proposal_end_epoch {
        Some(proposal_end_epoch) => {
            Ok(!crate::ledger::native_vp::governance::utils::is_valid_validator_voting_period(
                current_epoch,
                proposal_start_epoch,
                proposal_end_epoch,
            ))
        }
        None => {
            Err(Error::EpochNotInStorage)
        }
    }
}

/// Submit an IBC transfer
pub async fn submit_ibc_transfer<
    C: crate::ledger::queries::Client + Sync,
    U: WalletUtils,
>(
    client: &C,
    wallet: &mut Wallet<U>,
    args: args::TxIbcTransfer,
) -> Result<(), Error> {
    // Check that the source address exists on chain
    let source =
        source_exists_or_err(args.source.clone(), args.tx.force, client)
            .await?;
    // We cannot check the receiver

    let token = token_exists_or_err(args.token, args.tx.force, client).await?;

    // Check source balance
    let (sub_prefix, balance_key) = match args.sub_prefix {
        Some(sub_prefix) => {
            let sub_prefix = storage::Key::parse(sub_prefix).unwrap();
            let prefix = token::multitoken_balance_prefix(&token, &sub_prefix);
            (
                Some(sub_prefix),
                token::multitoken_balance_key(&prefix, &source),
            )
        }
        None => (None, token::balance_key(&token, &source)),
    };

    check_balance_too_low_err(
        &token,
        &source,
        args.amount,
        balance_key,
        args.tx.force,
        client,
    )
    .await?;

    let tx_code = args.tx_code_path;

    let denom = match sub_prefix {
        // To parse IbcToken address, remove the address prefix
        Some(sp) => sp.to_string().replace(RESERVED_ADDRESS_PREFIX, ""),
        None => token.to_string(),
    };
    let token = Some(Coin {
        denom,
        amount: args.amount.to_string(),
    });

    // this height should be that of the destination chain, not this chain
    let timeout_height = match args.timeout_height {
        Some(h) => IbcHeight::new(0, h),
        None => IbcHeight::zero(),
    };

    let now: crate::tendermint::Time = DateTimeUtc::now().try_into().unwrap();
    let now: IbcTimestamp = now.into();
    let timeout_timestamp = if let Some(offset) = args.timeout_sec_offset {
        (now + Duration::new(offset, 0)).unwrap()
    } else if timeout_height.is_zero() {
        // we cannot set 0 to both the height and the timestamp
        (now + Duration::new(3600, 0)).unwrap()
    } else {
        IbcTimestamp::none()
    };

    let msg = MsgTransfer {
        source_port: args.port_id,
        source_channel: args.channel_id,
        token,
        sender: Signer::new(source.to_string()),
        receiver: Signer::new(args.receiver),
        timeout_height,
        timeout_timestamp,
    };
    tracing::debug!("IBC transfer message {:?}", msg);
    let any_msg = msg.to_any();
    let mut data = vec![];
    prost::Message::encode(&any_msg, &mut data)
        .map_err(Error::EncodeFailure)?;

    let tx = Tx::new(tx_code, Some(data));
    process_tx::<C, U>(
        client,
        wallet,
        &args.tx,
        tx,
        TxSigningKey::WalletAddress(args.source),
    )
    .await?;
    Ok(())
}

/// Submit an ordinary transfer
pub async fn submit_transfer<
    C: crate::ledger::queries::Client + Sync,
    V: WalletUtils,
    U: ShieldedUtils<C = C>,
>(
    client: &C,
    wallet: &mut Wallet<V>,
    shielded: &mut ShieldedContext<U>,
    args: args::TxTransfer,
) -> Result<(), Error> {
    // Check that the source address exists on chain
    let force = args.tx.force;
    let transfer_source = args.source.clone();
    let source = source_exists_or_err(
        transfer_source.effective_address(),
        force,
        client,
    )
    .await?;
    // Check that the target address exists on chain
    let transfer_target = args.target.clone();
    let target = target_exists_or_err(
        transfer_target.effective_address(),
        force,
        client,
    )
    .await?;

    // Check that the token address exists on chain
    let token =
        &(token_exists_or_err(args.token.clone(), force, client).await?);

    // Check source balance
    let (sub_prefix, balance_key) = match &args.sub_prefix {
        Some(sub_prefix) => {
            let sub_prefix = storage::Key::parse(sub_prefix).unwrap();
            let prefix = token::multitoken_balance_prefix(token, &sub_prefix);
            (
                Some(sub_prefix),
                token::multitoken_balance_key(&prefix, &source),
            )
        }
        None => (None, token::balance_key(token, &source)),
    };

    check_balance_too_low_err(
        token,
        &source,
        args.amount,
        balance_key,
        args.tx.force,
        client,
    )
    .await?;

    let tx_code = args.tx_code_path.clone();
    let masp_addr = masp();
    // For MASP sources, use a special sentinel key recognized by VPs as default
    // signer. Also, if the transaction is shielded, redact the amount and token
    // types by setting the transparent value to 0 and token type to a constant.
    // This has no side-effect because transaction is to self.
    let (default_signer, amount, token) =
        if source == masp_addr && target == masp_addr {
            // TODO Refactor me, we shouldn't rely on any specific token here.
            (
                TxSigningKey::SecretKey(masp_tx_key()),
                0.into(),
                args.native_token.clone(),
            )
        } else if source == masp_addr {
            (
                TxSigningKey::SecretKey(masp_tx_key()),
                args.amount,
                token.clone(),
            )
        } else {
            (
                TxSigningKey::WalletAddress(source.clone()),
                args.amount,
                token.clone(),
            )
        };
    // If our chosen signer is the MASP sentinel key, then our shielded inputs
    // will need to cover the gas fees.
    let chosen_signer =
        tx_signer::<C, V>(client, wallet, &args.tx, default_signer.clone())
            .await?
            .ref_to();
    let shielded_gas = masp_tx_key().ref_to() == chosen_signer;
    // Determine whether to pin this transaction to a storage key
    let key = match &args.target {
        TransferTarget::PaymentAddress(pa) if pa.is_pinned() => Some(pa.hash()),
        _ => None,
    };

    let stx_result = shielded
        .gen_shielded_transfer(client, args.clone(), shielded_gas)
        .await;
    let shielded = match stx_result {
        Ok(stx) => Ok(stx.map(|x| x.0)),
        Err(builder::Error::ChangeIsNegative(_)) => {
            Err(Error::NegativeBalanceAfterTransfer(
                source.clone(),
                args.amount,
                token.clone(),
                args.tx.fee_amount,
                args.tx.fee_token.clone(),
            ))
        }
        Err(err) => Err(Error::MaspError(err)),
    }?;

    let transfer = token::Transfer {
        source: source.clone(),
        target,
        token,
        sub_prefix,
        amount,
        key,
        shielded,
    };
    tracing::debug!("Transfer data {:?}", transfer);
    let data = transfer.try_to_vec().map_err(Error::EncodeTxFailure)?;

    let tx = Tx::new(tx_code, Some(data));
    let signing_address = TxSigningKey::WalletAddress(source);
    process_tx::<C, V>(client, wallet, &args.tx, tx, signing_address).await?;
    Ok(())
}

/// Submit a transaction to initialize an account
pub async fn submit_init_account<
    C: crate::ledger::queries::Client + Sync,
    U: WalletUtils,
>(
    client: &C,
    wallet: &mut Wallet<U>,
    args: args::TxInitAccount,
) -> Result<(), Error> {
    let public_key = args.public_key;
    let vp_code = args.vp_code_path;
    // Validate the VP code
    validate_untrusted_code_err(&vp_code, args.tx.force)?;

    let tx_code = args.tx_code_path;
    let data = InitAccount {
        public_key,
        vp_code,
    };
    let data = data.try_to_vec().map_err(Error::EncodeTxFailure)?;
    let tx = Tx::new(tx_code, Some(data));
    // TODO Move unwrap to an either
    let initialized_accounts = process_tx::<C, U>(
        client,
        wallet,
        &args.tx,
        tx,
        TxSigningKey::WalletAddress(args.source),
    )
    .await
    .unwrap();
    save_initialized_accounts::<U>(wallet, args.alias, initialized_accounts)
        .await;
    Ok(())
}

/// Submit a transaction to update a VP
pub async fn submit_update_vp<
    C: crate::ledger::queries::Client + Sync,
    U: WalletUtils,
>(
    client: &C,
    wallet: &mut Wallet<U>,
    args: args::TxUpdateVp,
) -> Result<(), Error> {
    let addr = args.addr.clone();

    // Check that the address is established and exists on chain
    match &addr {
        Address::Established(_) => {
            let exists = rpc::known_address::<C>(client, &addr).await;
            if !exists {
                if args.tx.force {
                    eprintln!("The address {} doesn't exist on chain.", addr);
                    Ok(())
                } else {
                    Err(Error::LocationDoesNotExist(addr.clone()))
                }
            } else {
                Ok(())
            }
        }
        Address::Implicit(_) => {
            if args.tx.force {
                eprintln!(
                    "A validity predicate of an implicit address cannot be \
                     directly updated. You can use an established address for \
                     this purpose."
                );
                Ok(())
            } else {
                Err(Error::ImplicitUpdate)
            }
        }
        Address::Internal(_) => {
            if args.tx.force {
                eprintln!(
                    "A validity predicate of an internal address cannot be \
                     directly updated."
                );
                Ok(())
            } else {
                Err(Error::ImplicitInternalError)
            }
        }
    }?;

    let vp_code = args.vp_code_path;
    // Validate the VP code
    if let Err(err) = vm::validate_untrusted_wasm(&vp_code) {
        if args.tx.force {
            eprintln!("Validity predicate code validation failed with {}", err);
            Ok(())
        } else {
            Err(Error::WasmValidationFailure(err))
        }
    } else {
        Ok(())
    }?;

    let tx_code = args.tx_code_path;

    let data = UpdateVp { addr, vp_code };
    let data = data.try_to_vec().map_err(Error::EncodeTxFailure)?;

    let tx = Tx::new(tx_code, Some(data));
    process_tx::<C, U>(
        client,
        wallet,
        &args.tx,
        tx,
        TxSigningKey::WalletAddress(args.addr),
    )
    .await?;
    Ok(())
}

/// Submit a custom transaction
pub async fn submit_custom<
    C: crate::ledger::queries::Client + Sync,
    U: WalletUtils,
>(
    client: &C,
    wallet: &mut Wallet<U>,
    args: args::TxCustom,
) -> Result<(), Error> {
    let tx_code = args.code_path;
    let data = args.data_path;
    let tx = Tx::new(tx_code, data);
    let initialized_accounts =
        process_tx::<C, U>(client, wallet, &args.tx, tx, TxSigningKey::None)
            .await?;
    save_initialized_accounts::<U>(wallet, args.alias, initialized_accounts)
        .await;
    Ok(())
}

async fn expect_dry_broadcast<T, C: crate::ledger::queries::Client + Sync>(
    to_broadcast: TxBroadcastData,
    client: &C,
    ret: T,
) -> Result<T, Error> {
    match to_broadcast {
        TxBroadcastData::DryRun(tx) => {
            rpc::dry_run_tx(client, tx.to_bytes()).await;
            Ok(ret)
        }
        TxBroadcastData::Wrapper {
            tx,
            wrapper_hash: _,
            decrypted_hash: _,
        } => Err(Error::ExpectDryRun(tx)),
    }
}

fn lift_rpc_error<T>(res: Result<T, RpcError>) -> Result<T, Error> {
    res.map_err(Error::TxBroadcast)
}

/// Returns the given validator if the given address is a validator,
/// otherwise returns an error, force forces the address through even
/// if it isn't a validator
async fn known_validator_or_err<C: crate::ledger::queries::Client + Sync>(
    validator: Address,
    force: bool,
    client: &C,
) -> Result<Address, Error> {
    // Check that the validator address exists on chain
    let is_validator = rpc::is_validator(client, &validator).await;
    if !is_validator {
        if force {
            eprintln!(
                "The address {} doesn't belong to any known validator account.",
                validator
            );
            Ok(validator)
        } else {
            Err(Error::InvalidValidatorAddress(validator))
        }
    } else {
        Ok(validator)
    }
}

/// general pattern for checking if an address exists on the chain, or
/// throwing an error if it's not forced. Takes a generic error
/// message and the error type.
async fn address_exists_or_err<C, F>(
    addr: Address,
    force: bool,
    client: &C,
    message: String,
    err: F,
) -> Result<Address, Error>
where
    C: crate::ledger::queries::Client + Sync,
    F: FnOnce(Address) -> Error,
{
    let addr_exists = rpc::known_address::<C>(client, &addr).await;
    if !addr_exists {
        if force {
            eprintln!("{}", message);
            Ok(addr)
        } else {
            Err(err(addr))
        }
    } else {
        Ok(addr)
    }
}

/// Returns the given token if the given address exists on chain
/// otherwise returns an error, force forces the address through even
/// if it isn't on chain
async fn token_exists_or_err<C: crate::ledger::queries::Client + Sync>(
    token: Address,
    force: bool,
    client: &C,
) -> Result<Address, Error> {
    let message =
        format!("The token address {} doesn't exist on chain.", token);
    address_exists_or_err(
        token,
        force,
        client,
        message,
        Error::TokenDoesNotExist,
    )
    .await
}

/// Returns the given source address if the given address exists on chain
/// otherwise returns an error, force forces the address through even
/// if it isn't on chain
async fn source_exists_or_err<C: crate::ledger::queries::Client + Sync>(
    token: Address,
    force: bool,
    client: &C,
) -> Result<Address, Error> {
    let message =
        format!("The source address {} doesn't exist on chain.", token);
    address_exists_or_err(
        token,
        force,
        client,
        message,
        Error::SourceDoesNotExist,
    )
    .await
}

/// Returns the given target address if the given address exists on chain
/// otherwise returns an error, force forces the address through even
/// if it isn't on chain
async fn target_exists_or_err<C: crate::ledger::queries::Client + Sync>(
    token: Address,
    force: bool,
    client: &C,
) -> Result<Address, Error> {
    let message =
        format!("The target address {} doesn't exist on chain.", token);
    address_exists_or_err(
        token,
        force,
        client,
        message,
        Error::TargetLocationDoesNotExist,
    )
    .await
}

/// checks the balance at the given address is enough to transfer the
/// given amount, along with the balance even existing. force
/// overrides this
async fn check_balance_too_low_err<C: crate::ledger::queries::Client + Sync>(
    token: &Address,
    source: &Address,
    amount: token::Amount,
    balance_key: storage::Key,
    force: bool,
    client: &C,
) -> Result<(), Error> {
    match rpc::query_storage_value::<C, token::Amount>(client, &balance_key)
        .await
    {
        Some(balance) => {
            if balance < amount {
                if force {
                    eprintln!(
                        "The balance of the source {} of token {} is lower \
                         than the amount to be transferred. Amount to \
                         transfer is {} and the balance is {}.",
                        source, token, amount, balance
                    );
                    Ok(())
                } else {
                    Err(Error::BalanceTooLow(
                        source.clone(),
                        token.clone(),
                        amount,
                        balance,
                    ))
                }
            } else {
                Ok(())
            }
        }
        None => {
            if force {
                eprintln!(
                    "No balance found for the source {} of token {}",
                    source, token
                );
                Ok(())
            } else {
                Err(Error::NoBalanceForToken(source.clone(), token.clone()))
            }
        }
    }
}

fn validate_untrusted_code_err(
    vp_code: &Vec<u8>,
    force: bool,
) -> Result<(), Error> {
    if let Err(err) = vm::validate_untrusted_wasm(vp_code) {
        if force {
            eprintln!("Validity predicate code validation failed with {}", err);
            Ok(())
        } else {
            Err(Error::WasmValidationFailure(err))
        }
    } else {
        Ok(())
    }
}
