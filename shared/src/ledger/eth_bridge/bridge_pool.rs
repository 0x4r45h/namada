//! Bridge pool SDK functionality.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

use borsh::BorshSerialize;
use ethbridge_bridge_contract::Bridge;
use ethers::providers::Middleware;
use namada_core::types::chain::ChainId;
use owo_colors::OwoColorize;
use serde::{Deserialize, Serialize};

use super::{block_on_eth_sync, eth_sync_or_exit, BlockOnEthSync};
use crate::eth_bridge::ethers::abi::AbiDecode;
use crate::eth_bridge::structs::RelayProof;
use crate::ledger::args;
use crate::ledger::queries::{Client, RPC};
use crate::ledger::signing::TxSigningKey;
use crate::ledger::tx::process_tx;
use crate::ledger::wallet::{Wallet, WalletUtils};
use crate::proto::Tx;
use crate::types::address::Address;
use crate::types::control_flow::time::{Duration, Instant};
use crate::types::control_flow::{
    self, install_shutdown_signal, Halt, TryHalt,
};
use crate::types::eth_abi::Encode;
use crate::types::eth_bridge_pool::{
    GasFee, PendingTransfer, TransferToEthereum,
};
use crate::types::keccak::KeccakHash;
use crate::types::token::Amount;
use crate::types::voting_power::FractionalVotingPower;

/// Craft a transaction that adds a transfer to the Ethereum bridge pool.
pub async fn add_to_eth_bridge_pool<C, U>(
    client: &C,
    wallet: &mut Wallet<U>,
    chain_id: ChainId,
    args: args::EthereumBridgePool,
) where
    C: Client + Sync,
    C::Error: std::fmt::Debug + std::fmt::Display,
    U: WalletUtils,
{
    let args::EthereumBridgePool {
        ref tx,
        asset,
        recipient,
        sender,
        amount,
        gas_amount,
        gas_payer,
        code_path,
    } = args;
    let transfer = PendingTransfer {
        transfer: TransferToEthereum {
            asset,
            recipient,
            sender,
            amount,
        },
        gas_fee: GasFee {
            amount: gas_amount,
            payer: gas_payer,
        },
    };
    let data = transfer.try_to_vec().unwrap();
    let transfer_tx = Tx::new(code_path, Some(data), chain_id, None);
    // this should not initialize any new addresses, so we ignore the result.
    process_tx(
        client,
        wallet,
        tx,
        transfer_tx,
        TxSigningKey::None,
        #[cfg(not(feature = "mainnet"))]
        false,
    )
    .await
    .unwrap();
}

/// A json serializable representation of the Ethereum
/// bridge pool.
#[derive(Serialize, Deserialize)]
struct BridgePoolResponse {
    bridge_pool_contents: HashMap<String, PendingTransfer>,
}

/// Query the contents of the Ethereum bridge pool.
/// Prints out a json payload.
pub async fn query_bridge_pool<C>(client: &C)
where
    C: Client + Sync,
    C::Error: std::fmt::Debug,
{
    let response: Vec<PendingTransfer> = RPC
        .shell()
        .eth_bridge()
        .read_ethereum_bridge_pool(client)
        .await
        .unwrap();
    let pool_contents: HashMap<String, PendingTransfer> = response
        .into_iter()
        .map(|transfer| (transfer.keccak256().to_string(), transfer))
        .collect();
    if pool_contents.is_empty() {
        println!("Bridge pool is empty.");
        return;
    }
    let contents = BridgePoolResponse {
        bridge_pool_contents: pool_contents,
    };
    println!("{}", serde_json::to_string_pretty(&contents).unwrap());
}

/// Query the contents of the Ethereum bridge pool that
/// is covered by the latest signed root.
/// Prints out a json payload.
pub async fn query_signed_bridge_pool<C>(
    client: &C,
) -> Halt<HashMap<String, PendingTransfer>>
where
    C: Client + Sync,
    C::Error: std::fmt::Debug,
{
    let response: Vec<PendingTransfer> = RPC
        .shell()
        .eth_bridge()
        .read_signed_ethereum_bridge_pool(client)
        .await
        .unwrap();
    let pool_contents: HashMap<String, PendingTransfer> = response
        .into_iter()
        .map(|transfer| (transfer.keccak256().to_string(), transfer))
        .collect();
    if pool_contents.is_empty() {
        println!("Bridge pool is empty.");
        return control_flow::halt();
    }
    let contents = BridgePoolResponse {
        bridge_pool_contents: pool_contents.clone(),
    };
    println!("{}", serde_json::to_string_pretty(&contents).unwrap());
    control_flow::proceed(pool_contents)
}

/// Iterates over all ethereum events
/// and returns the amount of voting power
/// backing each `TransferToEthereum` event.
///
/// Prints a json payload.
pub async fn query_relay_progress<C>(client: &C)
where
    C: Client + Sync,
    C::Error: std::fmt::Debug,
{
    let resp = RPC
        .shell()
        .eth_bridge()
        .transfer_to_ethereum_progress(client)
        .await
        .unwrap();
    println!("{}", serde_json::to_string_pretty(&resp).unwrap());
}

/// Internal methdod to construct a proof that a set of transfers are in the
/// bridge pool.
async fn construct_bridge_pool_proof<C>(
    client: &C,
    transfers: &[KeccakHash],
    relayer: Address,
) -> Halt<Vec<u8>>
where
    C: Client + Sync,
    C::Error: std::fmt::Debug,
{
    let in_progress = RPC
        .shell()
        .eth_bridge()
        .transfer_to_ethereum_progress(client)
        .await
        .unwrap();

    let warnings: Vec<_> = in_progress
        .into_iter()
        .filter_map(|(ref transfer, voting_power)| {
            if voting_power > FractionalVotingPower::ONE_THIRD {
                let hash = PendingTransfer::from(transfer).keccak256();
                transfers.contains(&hash).then_some(hash)
            } else {
                None
            }
        })
        .collect();

    if !warnings.is_empty() {
        let warning = "Warning".on_yellow();
        let warning = warning.bold();
        let warning = warning.blink();
        println!(
            "{warning}: The following hashes correspond to transfers that \
             have surpassed the security threshold in Namada, therefore have \
             likely been relayed to Ethereum, but do not yet have a quorum of \
             validator signatures behind them in Namada; thus they are still \
             in the Bridge pool:\n{warnings:?}",
        );
        print!("\nDo you wish to proceed? (y/n): ");
        std::io::stdout().flush().unwrap();
        loop {
            let mut buffer = String::new();
            let stdin = std::io::stdin();
            stdin.read_line(&mut buffer).try_halt(|e| {
                println!("Encountered error reading from STDIN: {e:?}");
            })?;
            match buffer.trim() {
                "y" => break,
                "n" => return control_flow::halt(),
                _ => {
                    print!("Expected 'y' or 'n'. Please try again: ");
                    std::io::stdout().flush().unwrap();
                }
            }
        }
    }

    let data = (transfers, relayer).try_to_vec().unwrap();
    let response = RPC
        .shell()
        .eth_bridge()
        .generate_bridge_pool_proof(client, Some(data), None, false)
        .await;

    response.map(|response| response.data).try_halt(|e| {
        println!("Encountered error constructing proof:\n{:?}", e);
    })
}

/// A response from construction a bridge pool proof.
#[derive(Serialize)]
struct BridgePoolProofResponse {
    hashes: Vec<KeccakHash>,
    relayer_address: Address,
    total_fees: Amount,
    abi_encoded_proof: Vec<u8>,
}

/// Construct a merkle proof of a batch of transfers in
/// the bridge pool and return it to the user (as opposed
/// to relaying it to ethereum).
pub async fn construct_proof<C>(
    client: &C,
    args: args::BridgePoolProof,
) -> Halt<()>
where
    C: Client + Sync,
    C::Error: std::fmt::Debug,
{
    let bp_proof_bytes = construct_bridge_pool_proof(
        client,
        &args.transfers,
        args.relayer.clone(),
    )
    .await?;
    let bp_proof: RelayProof =
        AbiDecode::decode(&bp_proof_bytes).try_halt(|error| {
            println!("Unable to decode the generated proof: {:?}", error);
        })?;
    let resp = BridgePoolProofResponse {
        hashes: args.transfers,
        relayer_address: args.relayer,
        total_fees: bp_proof
            .transfers
            .iter()
            .map(|t| t.fee.as_u64())
            .sum::<u64>()
            .into(),
        abi_encoded_proof: bp_proof_bytes,
    };
    println!("{}", serde_json::to_string(&resp).unwrap());
    control_flow::proceed(())
}

/// Relay a validator set update, signed off for a given epoch.
pub async fn relay_bridge_pool_proof<C, E>(
    eth_client: Arc<E>,
    nam_client: &C,
    args: args::RelayBridgePoolProof,
) -> Halt<()>
where
    C: Client + Sync,
    C::Error: std::fmt::Debug + std::fmt::Display,
    E: Middleware,
    E::Error: std::fmt::Debug + std::fmt::Display,
{
    let _signal_receiver = args.safe_mode.then(install_shutdown_signal);

    if args.sync {
        block_on_eth_sync(
            &*eth_client,
            BlockOnEthSync {
                deadline: Instant::now() + Duration::from_secs(60),
                delta_sleep: Duration::from_secs(1),
            },
        )
        .await?;
    } else {
        eth_sync_or_exit(&*eth_client).await?;
    }

    let bp_proof =
        construct_bridge_pool_proof(nam_client, &args.transfers, args.relayer)
            .await?;
    let bridge = match RPC
        .shell()
        .eth_bridge()
        .read_bridge_contract(nam_client)
        .await
    {
        Ok(address) => Bridge::new(address.address, eth_client),
        Err(err_msg) => {
            let error = "Error".on_red();
            let error = error.bold();
            let error = error.blink();
            println!(
                "{error}: Failed to retrieve the Ethereum Bridge smart \
                 contract address from storage with \
                 reason:\n{err_msg}\n\nPerhaps the Ethereum bridge is not \
                 active.",
            );
            return control_flow::halt();
        }
    };

    let bp_proof: RelayProof =
        AbiDecode::decode(&bp_proof).try_halt(|error| {
            println!("Unable to decode the generated proof: {:?}", error);
        })?;

    // NOTE: this operation costs no gas on Ethereum
    let contract_nonce =
        bridge.transfer_to_erc_20_nonce().call().await.unwrap();

    match bp_proof.batch_nonce.cmp(&contract_nonce) {
        Ordering::Equal => {}
        Ordering::Less => {
            let error = "Error".on_red();
            let error = error.bold();
            let error = error.blink();
            println!(
                "{error}: The Bridge pool nonce in the smart contract is \
                 {contract_nonce}, while the nonce in Namada is still {}. A \
                 relay of the former one has already happened, but a proof \
                 has yet to be crafted in Namada.",
                bp_proof.batch_nonce
            );
            return control_flow::halt();
        }
        Ordering::Greater => {
            let error = "Error".on_red();
            let error = error.bold();
            let error = error.blink();
            println!(
                "{error}: The Bridge pool nonce in the smart contract is \
                 {contract_nonce}, while the nonce in Namada is still {}. \
                 Somehow, Namada's nonce is ahead of the contract's nonce!",
                bp_proof.batch_nonce
            );
            return control_flow::halt();
        }
    }

    let mut relay_op = bridge.transfer_to_erc(bp_proof);
    if let Some(gas) = args.gas {
        relay_op.tx.set_gas(gas);
    }
    if let Some(gas_price) = args.gas_price {
        relay_op.tx.set_gas_price(gas_price);
    }
    if let Some(eth_addr) = args.eth_addr {
        relay_op.tx.set_from(eth_addr.into());
    }

    let pending_tx = relay_op.send().await.unwrap();
    let transf_result = pending_tx
        .confirmations(args.confirmations as usize)
        .await
        .unwrap();

    println!("{transf_result:?}");
    control_flow::proceed(())
}

mod recommendations {
    use super::*;

    pub async fn recommend_batch<C>(
        _: &C,
        _: args::RecommendBatch,
    ) -> Halt<()>
    where
        C: Client + Sync,
        C::Error: std::fmt::Debug,
    {
        todo!()
    }
}

// TODO: fix the code in this module
#[cfg(FALSE)]
mod recommendations {
    use borsh::BorshDeserialize;

    use super::*;
    use crate::eth_bridge::storage::bridge_pool::get_signed_root_key;
    use crate::eth_bridge::storage::proof::BridgePoolRootProof;
    use crate::types::storage::BlockHeight;
    use crate::types::vote_extensions::validator_set_update::{
        EthAddrBook, VotingPowersMap, VotingPowersMapExt,
    };

    const TRANSFER_FEE: i64 = 37_500;
    const SIGNATURE_FEE: u64 = 24_500;
    const VALSET_FEE: u64 = 2000;

    /// The different states while trying to solve
    /// for a recommended batch of transfers.
    struct AlgorithState {
        /// We are scanning transfers that increase
        /// net profits to the relayer. However, we
        /// are not in the feasible region.
        profitable: bool,
        /// We are scanning solutions that satisfy the
        /// requirements of the input.
        feasible_region: bool,
    }

    /// The algorithm exhibits two different remmondation strategies
    /// depending on whether the user is will to accept a positive cost
    /// for relaying.
    #[derive(PartialEq)]
    enum AlgorithmMode {
        /// Only keep profitable transactions
        Greedy,
        /// Allow transactions which are not profitable
        Generous,
    }

    /// Recommend the most economical batch of transfers to relay based
    /// on a conversion rate estimates from NAM to ETH and gas usage
    /// heuristics.
    pub async fn recommend_batch<C>(
        client: &C,
        args: args::RecommendBatch,
    ) -> Halt<()>
    where
        C: Client + Sync,
        C::Error: std::fmt::Debug,
    {
        // get transfers that can already been relayed but are awaiting a quorum
        // of backing votes.
        let in_progress = RPC
            .shell()
            .eth_bridge()
            .transfer_to_ethereum_progress(client)
            .await
            .unwrap()
            .keys()
            .map(PendingTransfer::from)
            .collect::<Vec<_>>();

        // get the signed bridge pool root so we can analyze the signatures
        // the estimate the gas cost of verifying them.
        let (bp_root, height) =
            <(BridgePoolRootProof, BlockHeight)>::try_from_slice(
                &RPC.shell()
                    .storage_value(
                        client,
                        None,
                        Some(0.into()),
                        false,
                        &get_signed_root_key(),
                    )
                    .await
                    .unwrap()
                    .data,
            )
            .unwrap();

        // Get the voting powers of each of validator who signed
        // the above root.
        let voting_powers = RPC
            .shell()
            .eth_bridge()
            .voting_powers_at_height(client, &height)
            .await
            .unwrap();
        let valset_size = voting_powers.len() as u64;

        // This is the gas cost for hashing the validator set and
        // checking a quorum of signatures (in gwei).
        let validator_gas = SIGNATURE_FEE
            * signature_checks(voting_powers, &bp_root.signatures)
            + VALSET_FEE * valset_size;
        // This is the amount of gwei a single name is worth
        let gwei_per_nam =
            (10u64.pow(9) as f64 / args.nam_per_eth).floor() as u64;

        // we don't recommend transfers that have already been relayed
        let mut contents: Vec<(String, i64, PendingTransfer)> =
            query_signed_bridge_pool(client)
                .await?
                .into_iter()
                .filter_map(|(k, v)| {
                    if !in_progress.contains(&v) {
                        Some((
                            k,
                            TRANSFER_FEE
                                - u64::from(v.gas_fee.amount * gwei_per_nam)
                                    as i64,
                            v,
                        ))
                    } else {
                        None
                    }
                })
                .collect();

        // sort transfers in decreasing amounts of profitability
        contents.sort_by_key(|(_, cost, _)| *cost);

        let max_gas = args.max_gas.unwrap_or(u64::MAX);
        let max_cost = args.gas.map(|x| x as i64).unwrap_or_default();
        generate(contents, validator_gas, max_gas, max_cost);

        control_flow::proceed(())
    }

    /// Given an ordered list of signatures, figure out the size of the first
    /// subset constituting a 2 / 3 majority.
    ///
    /// The function is generic to make unit testing easier (otherwise a dev
    /// dependency needs to be added).
    fn signature_checks<T>(
        voting_powers: VotingPowersMap,
        sigs: &HashMap<EthAddrBook, T>,
    ) -> u64 {
        let voting_powers = voting_powers.get_sorted();
        let total_power = voting_powers
            .iter()
            .map(|(_, y)| u64::from(**y))
            .sum::<u64>();

        // Find the total number of signature checks Ethereum will make
        let mut power = FractionalVotingPower::NULL;
        voting_powers
            .iter()
            .filter_map(|(a, p)| sigs.get(a).map(|_| (a, p)))
            .take_while(|(_, p)| {
                if power <= FractionalVotingPower::TWO_THIRDS {
                    power += FractionalVotingPower::new(
                        u64::from(***p),
                        total_power,
                    )
                    .unwrap();
                    true
                } else {
                    false
                }
            })
            .count() as u64
    }

    /// Generates the actual recommendation from restrictions given by the
    /// input parameters.
    fn generate(
        contents: Vec<(String, i64, PendingTransfer)>,
        validator_gas: u64,
        max_gas: u64,
        max_cost: i64,
    ) -> Option<Vec<String>> {
        let mut state = AlgorithState {
            profitable: true,
            feasible_region: false,
        };

        let mode = if max_cost <= 0 {
            AlgorithmMode::Greedy
        } else {
            AlgorithmMode::Generous
        };

        let mut total_gas = validator_gas;
        let mut total_cost = validator_gas as i64;
        let mut total_fees = 0;
        let mut recommendation = vec![];
        for (hash, cost, transfer) in contents.into_iter() {
            let next_total_gas = total_gas + TRANSFER_FEE as u64;
            let next_total_cost = total_cost + cost;
            let next_total_fees =
                total_fees + u64::from(transfer.gas_fee.amount);
            if cost < 0 {
                if next_total_gas <= max_gas && next_total_cost <= max_cost {
                    state.feasible_region = true;
                } else if state.feasible_region {
                    // once we leave the feasible region, we will never re-enter
                    // it.
                    break;
                }
                recommendation.push(hash);
            } else if mode == AlgorithmMode::Generous {
                state.profitable = false;
                let is_feasible =
                    next_total_gas <= max_gas && next_total_cost <= max_cost;
                // once we leave the feasible region, we will never re-enter it.
                if state.feasible_region && !is_feasible {
                    break;
                } else {
                    recommendation.push(hash);
                }
            } else {
                break;
            }
            total_cost = next_total_cost;
            total_gas = next_total_gas;
            total_fees = next_total_fees;
        }

        if state.feasible_region && !recommendation.is_empty() {
            println!("Recommended batch: {:#?}", recommendation);
            println!(
                "Estimated Ethereum transaction gas (in gwei): {}",
                total_gas
            );
            println!("Estimated net profit (in gwei): {}", -total_cost);
            println!("Total fees (in NAM): {}", total_fees);
            Some(recommendation)
        } else {
            println!(
                "Unable to find a recommendation satisfying the input \
                 parameters."
            );
            None
        }
    }

    #[cfg(test)]
    mod test_recommendations {
        use namada_core::types::address::Address;
        use namada_core::types::ethereum_events::EthAddress;

        use super::*;

        /// An established user address for testing & development
        pub fn bertha_address() -> Address {
            Address::decode(
                "atest1v4ehgw36xvcyyvejgvenxs34g3zygv3jxqunjd6rxyeyys3sxy6rwvfkx4qnj33hg9qnvse4lsfctw",
            )
            .expect("The token address decoding shouldn't fail")
        }

        /// Generate a pending transfer with the specified gas
        /// fee.
        pub fn transfer(gas_amount: u64) -> PendingTransfer {
            PendingTransfer {
                transfer: TransferToEthereum {
                    asset: EthAddress([1; 20]),
                    recipient: EthAddress([2; 20]),
                    sender: bertha_address(),
                    amount: Default::default(),
                },
                gas_fee: GasFee {
                    amount: gas_amount.into(),
                    payer: bertha_address(),
                },
            }
        }

        /// Convert transfers into a format that the `generate` function
        /// understands.
        fn process_transfers(
            transfers: Vec<PendingTransfer>,
        ) -> Vec<(String, i64, PendingTransfer)> {
            transfers
                .into_iter()
                .map(|t| {
                    (
                        t.keccak256().to_string(),
                        TRANSFER_FEE - u64::from(t.gas_fee.amount) as i64,
                        t,
                    )
                })
                .collect()
        }

        fn address_book(i: u8) -> EthAddrBook {
            EthAddrBook {
                hot_key_addr: EthAddress([i; 20]),
                cold_key_addr: EthAddress([i; 20]),
            }
        }

        #[test]
        fn test_signature_count() {
            let voting_powers = VotingPowersMap::from([
                (address_book(1), Amount::from(5)),
                (address_book(2), Amount::from(1)),
                (address_book(3), Amount::from(1)),
            ]);
            let signatures = HashMap::from([
                (address_book(1), 0),
                (address_book(2), 0),
                (address_book(3), 0),
            ]);
            let checks = signature_checks(voting_powers, &signatures);
            assert_eq!(checks, 1)
        }

        #[test]
        fn test_signature_count_with_skips() {
            let voting_powers = VotingPowersMap::from([
                (address_book(1), Amount::from(5)),
                (address_book(2), Amount::from(5)),
                (address_book(3), Amount::from(1)),
                (address_book(4), Amount::from(1)),
            ]);
            let signatures = HashMap::from([
                (address_book(1), 0),
                (address_book(3), 0),
                (address_book(4), 0),
            ]);
            let checks = signature_checks(voting_powers, &signatures);
            assert_eq!(checks, 3)
        }

        #[test]
        fn test_only_profitable() {
            let profitable = vec![transfer(100_000); 17];
            let hash = profitable[0].keccak256().to_string();
            let expected = vec![hash; 17];
            let recommendation =
                generate(process_transfers(profitable), 800_000, u64::MAX, 0)
                    .expect("Test failed");
            assert_eq!(recommendation, expected);
        }

        #[test]
        fn test_non_profitable_removed() {
            let mut transfers = vec![transfer(100_000); 17];
            let hash = transfers[0].keccak256().to_string();
            transfers.push(transfer(0));
            let expected: Vec<_> = vec![hash; 17];
            let recommendation =
                generate(process_transfers(transfers), 800_000, u64::MAX, 0)
                    .expect("Test failed");
            assert_eq!(recommendation, expected);
        }

        #[test]
        fn test_max_gas() {
            let transfers = vec![transfer(75_000); 4];
            let hash = transfers[0].keccak256().to_string();
            let expected = vec![hash; 2];
            let recommendation = generate(
                process_transfers(transfers),
                50_000,
                150_000,
                i64::MAX,
            )
            .expect("Test failed");
            assert_eq!(recommendation, expected);
        }

        #[test]
        fn test_net_loss() {
            let mut transfers = vec![transfer(75_000); 4];
            transfers.extend([transfer(17_500), transfer(17_500)]);
            let expected: Vec<_> = transfers
                .iter()
                .map(|t| t.keccak256().to_string())
                .take(5)
                .collect();
            let recommendation = generate(
                process_transfers(transfers),
                150_000,
                u64::MAX,
                20_000,
            )
            .expect("Test failed");
            assert_eq!(recommendation, expected);
        }

        #[test]
        fn test_net_loss_max_gas() {
            let mut transfers = vec![transfer(75_000); 4];
            let hash = transfers[0].keccak256().to_string();
            let expected = vec![hash; 4];
            transfers.extend([transfer(17_500), transfer(17_500)]);
            let recommendation = generate(
                process_transfers(transfers),
                150_000,
                330_000,
                20_000,
            )
            .expect("Test failed");
            assert_eq!(recommendation, expected);
        }

        #[test]
        fn test_wholly_infeasible() {
            let transfers = vec![transfer(75_000); 4];
            let recommendation = generate(
                process_transfers(transfers),
                300_000,
                u64::MAX,
                20_000,
            );
            assert!(recommendation.is_none())
        }
    }
}

pub use recommendations::recommend_batch;