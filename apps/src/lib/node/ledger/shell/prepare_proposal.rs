//! Implementation of the [`PrepareProposal`] ABCI++ method for the Shell

#[cfg(not(feature = "ABCI"))]
mod prepare_block {
    use namada::types::vote_extensions::VoteExtensionDigest;
    use tendermint_proto::abci::{ExtendedCommitInfo, TxRecord};

    use super::super::vote_extensions::{
        iter_protocol_txs, split_vote_extensions,
    };
    use super::super::*;
    use crate::node::ledger::shims::abcipp_shim_types::shim::TxBytes;

    impl<D, H> Shell<D, H>
    where
        D: DB + for<'iter> DBIter<'iter> + Sync + 'static,
        H: StorageHasher + Sync + 'static,
    {
        /// Begin a new block.
        ///
        /// We include half of the new wrapper txs given to us from the mempool
        /// by tendermint. The rest of the block is filled with decryptions
        /// of the wrapper txs from the previously committed block.
        ///
        /// INVARIANT: Any changes applied in this method must be reverted if
        /// the proposal is rejected (unless we can simply overwrite
        /// them in the next block).
        pub fn prepare_proposal(
            &mut self,
            req: RequestPrepareProposal,
        ) -> response::PrepareProposal {
            // We can safely reset meter, because if the block is rejected,
            // we'll reset again on the next proposal, until the
            // proposal is accepted
            self.gas_meter.reset();
            let txs = if let ShellMode::Validator { .. } = self.mode {
                // TODO: add some info logging

                // add ethereum events as protocol txs
                let mut txs =
                    self.build_vote_extensions_txs(req.local_last_commit);

                // add mempool txs
                let mut mempool_txs = self.build_mempool_txs(req.txs);
                txs.append(&mut mempool_txs);

                // decrypt the wrapper txs included in the previous block
                let mut decrypted_txs = self.build_decrypted_txs();
                txs.append(&mut decrypted_txs);

                txs
            } else {
                vec![]
            };

            response::PrepareProposal {
                tx_records: txs,
                ..Default::default()
            }
        }

        /// Builds a batch of vote extension transactions, comprised of Ethereum
        /// events
        // TODO: add `and, optionally, a validator set update` to the docstring,
        // after validator set updates are implemented
        fn build_vote_extensions_txs(
            &mut self,
            local_last_commit: Option<ExtendedCommitInfo>,
        ) -> Vec<TxRecord> {
            // genesis block should not contain vote extensions
            if self.storage.last_height == BlockHeight(0) {
                return vec![];
            }

            let (eth_events, _valset_upds) = split_vote_extensions(
                local_last_commit
                    .expect(
                        "Honest Namada validators will always sign \
                         ethereum_events::Vext instances, even if no Ethereum \
                         events were observed at a given block height. In \
                         fact, a quorum of signed empty ethereum_events::Vext \
                         instances commits the fact no events were observed \
                         by a majority of validators. Therefore, for block \
                         heights greater than zero, we should always have \
                         vote extensions.",
                    )
                    .votes,
            );

            const NOT_ENOUGH_VOTING_POWER_MSG: &str =
                "A Tendermint quorum should never decide on a block including \
                 vote extensions reflecting less than or equal to 2/3 of the \
                 total stake.";

            let ethereum_events = self
                .compress_ethereum_events(eth_events)
                .expect(NOT_ENOUGH_VOTING_POWER_MSG);

            let protocol_key = self
                .mode
                .get_protocol_key()
                .expect("Validators should always have a protocol key");

            iter_protocol_txs(VoteExtensionDigest {
                ethereum_events,
                validator_set_update: None,
            })
            .map(|tx| record::add(tx.sign(protocol_key).to_bytes()))
            .collect()
        }

        /// Builds a batch of mempool transactions
        fn build_mempool_txs(&mut self, txs: Vec<Vec<u8>>) -> Vec<TxRecord> {
            // filter in half of the new txs from Tendermint, only keeping
            // wrappers
            let number_of_new_txs = 1 + txs.len() / 2;
            txs.into_iter()
                .take(number_of_new_txs)
                .map(|tx_bytes| {
                    if let Ok(Ok(TxType::Wrapper(_))) =
                        Tx::try_from(tx_bytes.as_slice()).map(process_tx)
                    {
                        record::keep(tx_bytes)
                    } else {
                        record::remove(tx_bytes)
                    }
                })
                .collect()
        }

        /// Builds a batch of DKG decrypted transactions
        // TODO: we won't have frontrunning protection until V2 of the Anoma
        // protocol; Namada runs V1, therefore this method is
        // essentially a NOOP, and ought to be removed
        //
        // sources:
        // - https://specs.anoma.net/main/releases/v2.html
        // - https://github.com/anoma/ferveo
        fn build_decrypted_txs(&mut self) -> Vec<TxRecord> {
            // TODO: This should not be hardcoded
            let privkey = <EllipticCurve as PairingEngine>::G2Affine::prime_subgroup_generator();

            self.storage
                .tx_queue
                .iter()
                .map(|tx| {
                    Tx::from(match tx.decrypt(privkey) {
                        Ok(tx) => DecryptedTx::Decrypted(tx),
                        _ => DecryptedTx::Undecryptable(tx.clone()),
                    })
                    .to_bytes()
                })
                .map(record::add)
                .collect()
        }
    }

    /// Functions for creating the appropriate TxRecord given the
    /// numeric code
    pub(super) mod record {
        use tendermint_proto::abci::tx_record::TxAction;

        use super::*;

        /// Keep this transaction in the proposal
        pub fn keep(tx: TxBytes) -> TxRecord {
            TxRecord {
                action: TxAction::Unmodified as i32,
                tx,
            }
        }

        /// A transaction added to the proposal not provided by
        /// Tendermint from the mempool
        pub fn add(tx: TxBytes) -> TxRecord {
            TxRecord {
                action: TxAction::Added as i32,
                tx,
            }
        }

        /// Remove this transaction from the set provided
        /// by Tendermint from the mempool
        pub fn remove(tx: TxBytes) -> TxRecord {
            TxRecord {
                action: TxAction::Removed as i32,
                tx,
            }
        }
    }

    #[cfg(test)]
    // TODO: write a test to check for unreachable code paths in
    // prepare proposals, when processing ethereum events
    mod test_prepare_proposal {
        use std::collections::HashSet;

        use namada::ledger::pos::namada_proof_of_stake::types::{
            VotingPower, WeightedValidator,
        };
        use namada::proto::{Signed, SignedTxData};
        use namada::types::address::xan;
        use namada::types::ethereum_events::EthereumEvent;
        use namada::types::key::common;
        use namada::types::storage::{BlockHeight, Epoch};
        use namada::types::transaction::protocol::ProtocolTxType;
        use namada::types::transaction::{Fee, TxType};
        use namada::types::vote_extensions::ethereum_events::{
            self, MultiSignedEthEvent,
        };
        use namada::types::vote_extensions::VoteExtension;
        use tendermint_proto::abci::tx_record::TxAction;
        use tendermint_proto::abci::{
            ExtendedCommitInfo, ExtendedVoteInfo, TxRecord,
        };

        use super::*;
        use crate::node::ledger::shell::queries::QueriesExt;
        use crate::node::ledger::shell::test_utils::{
            self, gen_keypair, TestShell,
        };
        use crate::node::ledger::shims::abcipp_shim_types::shim::request::FinalizeBlock;
        use crate::wallet;

        /// Test that if a tx from the mempool is not a
        /// WrapperTx type, it is not included in the
        /// proposed block.
        #[test]
        fn test_prepare_proposal_rejects_non_wrapper_tx() {
            let (mut shell, _, _) = TestShell::new();
            let tx = Tx::new(
                "wasm_code".as_bytes().to_owned(),
                Some("transaction_data".as_bytes().to_owned()),
            );
            let req = RequestPrepareProposal {
                txs: vec![tx.to_bytes()],
                max_tx_bytes: 0,
                ..Default::default()
            };
            assert_eq!(
                shell.prepare_proposal(req).tx_records,
                vec![record::remove(tx.to_bytes())]
            );
        }

        /// Check if we are filtering out an invalid vote extension `vext`
        fn check_eth_events_filtering(
            shell: &mut TestShell,
            vext: Signed<ethereum_events::Vext>,
        ) {
            let filtered_votes: Vec<_> =
                shell.filter_invalid_eth_events_vexts(vec![vext]).collect();

            assert_eq!(filtered_votes, vec![]);
        }

        /// Test if we are filtering out Ethereum events with bad
        /// signatures in a prepare proposal.
        #[test]
        fn test_prepare_proposal_filter_out_bad_vext_signatures() {
            const LAST_HEIGHT: BlockHeight = BlockHeight(2);

            let (mut shell, _, _) = test_utils::setup();

            // artificially change the block height
            shell.storage.last_height = LAST_HEIGHT;

            let signed_vote_extension = {
                let (protocol_key, _) = wallet::defaults::validator_keys();
                let validator_addr = wallet::defaults::validator_address();

                // generate a valid signature
                let mut ext = ethereum_events::Vext {
                    validator_addr,
                    block_height: LAST_HEIGHT,
                    ethereum_events: vec![],
                }
                .sign(&protocol_key);
                assert!(ext.verify(&protocol_key.ref_to()).is_ok());

                // modify this signature such that it becomes invalid
                ext.sig = test_utils::invalidate_signature(ext.sig);
                ext
            };

            check_eth_events_filtering(&mut shell, signed_vote_extension);
        }

        /// Test if we are filtering out Ethereum events seen at
        /// block heights different than the last height.
        #[test]
        fn test_prepare_proposal_filter_out_bad_vext_bheights() {
            const LAST_HEIGHT: BlockHeight = BlockHeight(3);
            const PRED_LAST_HEIGHT: BlockHeight =
                BlockHeight(LAST_HEIGHT.0 - 1);

            let (mut shell, _, _) = test_utils::setup();

            // artificially change the block height
            shell.storage.last_height = LAST_HEIGHT;

            let (protocol_key, _) = wallet::defaults::validator_keys();
            let validator_addr = wallet::defaults::validator_address();

            let signed_vote_extension = {
                let ext = ethereum_events::Vext {
                    validator_addr,
                    block_height: PRED_LAST_HEIGHT,
                    ethereum_events: vec![],
                }
                .sign(&protocol_key);
                assert!(ext.verify(&protocol_key.ref_to()).is_ok());
                ext
            };

            check_eth_events_filtering(&mut shell, signed_vote_extension);
        }

        /// Test if we are filtering out Ethereum events seen by
        /// non-validator nodes.
        #[test]
        fn test_prepare_proposal_filter_out_bad_vext_validators() {
            const LAST_HEIGHT: BlockHeight = BlockHeight(2);

            let (mut shell, _, _) = test_utils::setup();

            // artificially change the block height
            shell.storage.last_height = LAST_HEIGHT;

            let (validator_addr, protocol_key) = {
                let bertha_key = wallet::defaults::bertha_keypair();
                let bertha_addr = wallet::defaults::bertha_address();
                (bertha_addr, bertha_key)
            };

            let signed_vote_extension = {
                let ext = ethereum_events::Vext {
                    validator_addr,
                    block_height: LAST_HEIGHT,
                    ethereum_events: vec![],
                }
                .sign(&protocol_key);
                assert!(ext.verify(&protocol_key.ref_to()).is_ok());
                ext
            };

            check_eth_events_filtering(&mut shell, signed_vote_extension);
        }

        /// Test if we are filtering out duped Ethereum events in
        /// prepare proposals.
        #[test]
        fn test_prepare_proposal_filter_duped_ethereum_events() {
            const LAST_HEIGHT: BlockHeight = BlockHeight(3);

            let (mut shell, _, _) = test_utils::setup();

            // artificially change the block height
            shell.storage.last_height = LAST_HEIGHT;

            let (protocol_key, _) = wallet::defaults::validator_keys();
            let validator_addr = wallet::defaults::validator_address();

            let ethereum_event = EthereumEvent::TransfersToNamada {
                nonce: 1u64.into(),
                transfers: vec![],
            };
            let signed_vote_extension = {
                let ev = ethereum_event;
                let ext = ethereum_events::Vext {
                    validator_addr,
                    block_height: LAST_HEIGHT,
                    ethereum_events: vec![ev.clone(), ev.clone(), ev],
                }
                .sign(&protocol_key);
                assert!(ext.verify(&protocol_key.ref_to()).is_ok());
                ext
            };

            let maybe_digest =
                shell.compress_ethereum_events(vec![signed_vote_extension]);

            // we should be filtering out the vote extension with
            // duped ethereum events; therefore, no valid vote
            // extensions will remain, and we will get no
            // digest from compressing nil vote extensions
            assert!(maybe_digest.is_none());
        }

        /// Creates an Ethereum events digest manually, and encodes it as a
        /// [`TxRecord`].
        fn manually_assemble_digest(
            _protocol_key: &common::SecretKey,
            ext: Signed<ethereum_events::Vext>,
            last_height: BlockHeight,
        ) -> ethereum_events::VextDigest {
            let events = vec![MultiSignedEthEvent {
                event: ext.data.ethereum_events[0].clone(),
                signers: {
                    let mut s = HashSet::new();
                    s.insert(ext.data.validator_addr.clone());
                    s
                },
            }];
            let signatures = {
                let mut s = std::collections::HashMap::new();
                s.insert(ext.data.validator_addr.clone(), ext.sig.clone());
                s
            };

            let vote_extension_digest =
                ethereum_events::VextDigest { events, signatures };

            assert_eq!(
                vec![ext],
                vote_extension_digest.clone().decompress(last_height)
            );

            vote_extension_digest

            // let tx = ProtocolTxType::EthereumEvents(vote_extension_digest)
            //    .sign(&protocol_key)
            //    .to_bytes();
            // super::record::add(tx)
        }

        /// Test if Ethereum events validation and inclusion in a block
        /// behaves as expected, considering honest validators.
        #[test]
        fn test_prepare_proposal_vext_normal_op() {
            const LAST_HEIGHT: BlockHeight = BlockHeight(3);

            let (mut shell, _, _) = test_utils::setup();

            // artificially change the block height
            shell.storage.last_height = LAST_HEIGHT;

            let (protocol_key, _) = wallet::defaults::validator_keys();
            let validator_addr = wallet::defaults::validator_address();

            let ethereum_event = EthereumEvent::TransfersToNamada {
                nonce: 1u64.into(),
                transfers: vec![],
            };
            let ethereum_events = {
                let ext = ethereum_events::Vext {
                    validator_addr,
                    block_height: LAST_HEIGHT,
                    ethereum_events: vec![ethereum_event],
                }
                .sign(&protocol_key);
                assert!(ext.verify(&protocol_key.ref_to()).is_ok());
                ext
            };
            let vote_extension = VoteExtension {
                ethereum_events,
                validator_set_update: None,
            };
            let vote = ExtendedVoteInfo {
                vote_extension: vote_extension.try_to_vec().unwrap(),
                ..Default::default()
            };

            let mut rsp = shell.prepare_proposal(RequestPrepareProposal {
                local_last_commit: Some(ExtendedCommitInfo {
                    votes: vec![vote],
                    ..Default::default()
                }),
                ..Default::default()
            });
            let rsp_digest = {
                assert_eq!(rsp.tx_records.len(), 1);
                let tx_record = rsp.tx_records.pop().unwrap();

                assert_eq!(tx_record.action(), TxAction::Added);

                let got = Tx::try_from(&tx_record.tx[..]).unwrap();
                let got_signed_tx =
                    SignedTxData::try_from_slice(&got.data.unwrap()[..])
                        .unwrap();
                let protocol_tx =
                    TxType::try_from_slice(&got_signed_tx.data.unwrap()[..])
                        .unwrap();

                let protocol_tx = match protocol_tx {
                    TxType::Protocol(protocol_tx) => protocol_tx.tx,
                    _ => panic!("Test failed"),
                };

                match protocol_tx {
                    ProtocolTxType::EthereumEvents(digest) => digest,
                    _ => panic!("Test failed"),
                }
            };

            let digest = manually_assemble_digest(
                &protocol_key,
                vote_extension.ethereum_events,
                LAST_HEIGHT,
            );

            assert_eq!(rsp_digest, digest);

            // NOTE: this comparison will not work because of timestamps
            // assert_eq!(rsp.tx_records, vec![digest]);
        }

        /// Test if Ethereum events validation and inclusion in a block
        /// behaves as expected, considering <= 2/3 voting power.
        #[test]
        #[should_panic(expected = "A Tendermint quorum should never")]
        fn test_prepare_proposal_vext_insufficient_voting_power() {
            const FIRST_HEIGHT: BlockHeight = BlockHeight(0);
            const LAST_HEIGHT: BlockHeight = BlockHeight(FIRST_HEIGHT.0 + 11);

            // starting the shell like this will contain insufficient voting
            // power
            let (mut shell, _, _) = test_utils::setup();

            // artificially change the voting power of the default validator to
            // zero, change the block height, and commit a dummy block,
            // to move to a new epoch
            let events_epoch = shell
                .storage
                .get_epoch_from_height(FIRST_HEIGHT)
                .expect("Test failed");
            let validator_set = {
                let params = shell.storage.read_pos_params();
                let mut epochs = shell.storage.read_validator_set();
                let mut data =
                    epochs.get(events_epoch).cloned().expect("Test failed");

                data.active = data
                    .active
                    .iter()
                    .cloned()
                    .map(|v| WeightedValidator {
                        voting_power: VotingPower::from(0u64),
                        ..v
                    })
                    .collect();

                epochs.set(data, events_epoch, &params);
                epochs
            };
            shell.storage.write_validator_set(&validator_set);

            let mut req = FinalizeBlock::default();
            req.header.time = namada::types::time::DateTimeUtc::now();
            shell.storage.last_height = LAST_HEIGHT;
            shell.finalize_block(req).expect("Test failed");
            shell.commit();

            // test prepare proposal
            let (protocol_key, _) = wallet::defaults::validator_keys();
            let validator_addr = wallet::defaults::validator_address();

            let ethereum_event = EthereumEvent::TransfersToNamada {
                nonce: 1u64.into(),
                transfers: vec![],
            };
            let ethereum_events = {
                let ext = ethereum_events::Vext {
                    validator_addr,
                    block_height: LAST_HEIGHT,
                    ethereum_events: vec![ethereum_event],
                }
                .sign(&protocol_key);
                assert!(ext.verify(&protocol_key.ref_to()).is_ok());
                ext
            };
            let vote = ExtendedVoteInfo {
                vote_extension: VoteExtension {
                    ethereum_events,
                    validator_set_update: None,
                }
                .try_to_vec()
                .unwrap(),
                ..Default::default()
            };

            // this should panic
            shell.prepare_proposal(RequestPrepareProposal {
                local_last_commit: Some(ExtendedCommitInfo {
                    votes: vec![vote],
                    ..Default::default()
                }),
                ..Default::default()
            });
        }

        /// Test that if an error is encountered while
        /// trying to process a tx from the mempool,
        /// we simply exclude it from the proposal
        #[test]
        fn test_error_in_processing_tx() {
            let (mut shell, _, _) = TestShell::new();
            let keypair = gen_keypair();
            let tx = Tx::new(
                "wasm_code".as_bytes().to_owned(),
                Some("transaction_data".as_bytes().to_owned()),
            );
            // an unsigned wrapper will cause an error in processing
            let wrapper = Tx::new(
                "".as_bytes().to_owned(),
                Some(
                    WrapperTx::new(
                        Fee {
                            amount: 0.into(),
                            token: xan(),
                        },
                        &keypair,
                        Epoch(0),
                        0.into(),
                        tx,
                        Default::default(),
                    )
                    .try_to_vec()
                    .expect("Test failed"),
                ),
            )
            .to_bytes();
            let req = RequestPrepareProposal {
                txs: vec![wrapper.clone()],
                max_tx_bytes: 0,
                ..Default::default()
            };
            assert_eq!(
                shell.prepare_proposal(req).tx_records,
                vec![record::remove(wrapper)]
            );
        }

        /// Test that the decrypted txs are included
        /// in the proposal in the same order as their
        /// corresponding wrappers
        #[test]
        fn test_decrypted_txs_in_correct_order() {
            let (mut shell, _, _) = TestShell::new();
            let keypair = gen_keypair();
            let mut expected_wrapper = vec![];
            let mut expected_decrypted = vec![];

            let mut req = RequestPrepareProposal {
                txs: vec![],
                max_tx_bytes: 0,
                ..Default::default()
            };
            // create a request with two new wrappers from mempool and
            // two wrappers from the previous block to be decrypted
            for i in 0..2 {
                let tx = Tx::new(
                    "wasm_code".as_bytes().to_owned(),
                    Some(
                        format!("transaction data: {}", i)
                            .as_bytes()
                            .to_owned(),
                    ),
                );
                expected_decrypted
                    .push(Tx::from(DecryptedTx::Decrypted(tx.clone())));
                let wrapper_tx = WrapperTx::new(
                    Fee {
                        amount: 0.into(),
                        token: xan(),
                    },
                    &keypair,
                    Epoch(0),
                    0.into(),
                    tx,
                    Default::default(),
                );
                let wrapper = wrapper_tx.sign(&keypair).expect("Test failed");
                shell.enqueue_tx(wrapper_tx);
                expected_wrapper.push(wrapper.clone());
                req.txs.push(wrapper.to_bytes());
            }
            // we extract the inner data from the txs for testing
            // equality since otherwise changes in timestamps would
            // fail the test
            expected_wrapper.append(&mut expected_decrypted);
            let expected_txs: Vec<Vec<u8>> = expected_wrapper
                .iter()
                .map(|tx| tx.data.clone().expect("Test failed"))
                .collect();

            let received: Vec<Vec<u8>> = shell
                .prepare_proposal(req)
                .tx_records
                .iter()
                .filter_map(
                    |TxRecord {
                         tx: tx_bytes,
                         action,
                     }| {
                        if *action == (TxAction::Unmodified as i32)
                            || *action == (TxAction::Added as i32)
                        {
                            Some(
                                Tx::try_from(tx_bytes.as_slice())
                                    .expect("Test failed")
                                    .data
                                    .expect("Test failed"),
                            )
                        } else {
                            None
                        }
                    },
                )
                .collect();
            // check that the order of the txs is correct
            assert_eq!(received, expected_txs);
        }
    }
}

#[allow(unused_imports)]
#[cfg(not(feature = "ABCI"))]
pub use prepare_block::*;
