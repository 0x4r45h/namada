use namada::core::ledger::slash_fund::ADDRESS as slash_fund_address;
use namada::ledger::events::EventType;
use namada::ledger::governance::{
    storage as gov_storage, ADDRESS as gov_address,
};
use namada::ledger::native_vp::governance::utils::{
    compute_tally, get_proposal_votes, ProposalEvent,
};
use namada::ledger::protocol;
use namada::ledger::storage::types::encode;
use namada::ledger::storage::{DBIter, StorageHasher, DB};
use namada::types::address::Address;
use namada::types::governance::TallyResult;
use namada::types::storage::Epoch;
use namada::types::token;

use super::*;

#[derive(Default)]
pub struct ProposalsResult {
    passed: Vec<u64>,
    rejected: Vec<u64>,
}

pub fn execute_governance_proposals<D, H>(
    shell: &mut Shell<D, H>,
    response: &mut shim::response::FinalizeBlock,
) -> Result<ProposalsResult>
where
    D: DB + for<'iter> DBIter<'iter> + Sync + 'static,
    H: StorageHasher + Sync + 'static,
{
    let mut proposals_result = ProposalsResult::default();

    for id in std::mem::take(&mut shell.proposal_data) {
        println!("Processing proposal {}", id);
        let proposal_funds_key = gov_storage::get_funds_key(id);
        let proposal_end_epoch_key = gov_storage::get_voting_end_epoch_key(id);

        let funds = shell
            .read_storage_key::<token::Amount>(&proposal_funds_key)
            .ok_or_else(|| {
                Error::BadProposal(id, "Invalid proposal funds.".to_string())
            })?;
        let proposal_end_epoch = shell
            .read_storage_key::<Epoch>(&proposal_end_epoch_key)
            .ok_or_else(|| {
                Error::BadProposal(
                    id,
                    "Invalid proposal end_epoch.".to_string(),
                )
            })?;
        println!("Proposal funds: {}", funds);
        println!("Proposal end_epoch: {}", proposal_end_epoch);

        let votes = get_proposal_votes(&shell.storage, proposal_end_epoch, id);
        println!("Proposal votes: {:?}", votes);
        let is_accepted = votes.and_then(|votes| {
            compute_tally(&shell.storage, proposal_end_epoch, votes)
        });

        let transfer_address = match is_accepted {
            Ok(true) => {
                let proposal_author_key = gov_storage::get_author_key(id);
                let proposal_author = shell
                    .read_storage_key::<Address>(&proposal_author_key)
                    .ok_or_else(|| {
                        Error::BadProposal(
                            id,
                            "Invalid proposal author.".to_string(),
                        )
                    })?;

                let proposal_code_key = gov_storage::get_proposal_code_key(id);
                let proposal_code =
                    shell.read_storage_key_bytes(&proposal_code_key);
                match proposal_code {
                    Some(proposal_code) => {
                        let tx = Tx::new(proposal_code, Some(encode(&id)));
                        let tx_type =
                            TxType::Decrypted(DecryptedTx::Decrypted {
                                tx,
                                #[cfg(not(feature = "mainnet"))]
                                has_valid_pow: false,
                            });
                        let pending_execution_key =
                            gov_storage::get_proposal_execution_key(id);
                        shell
                            .storage
                            .write(&pending_execution_key, "")
                            .expect("Should be able to write to storage.");
                        let tx_result = protocol::apply_tx(
                            tx_type,
                            0, /*  this is used to compute the fee
                                * based on the code size. We dont
                                * need it here. */
                            TxIndex::default(),
                            &mut BlockGasMeter::default(),
                            &mut shell.write_log,
                            &shell.storage,
                            &mut shell.vp_wasm_cache,
                            &mut shell.tx_wasm_cache,
                        );
                        shell
                            .storage
                            .delete(&pending_execution_key)
                            .expect("Should be able to delete the storage.");
                        match tx_result {
                            Ok(tx_result) => {
                                if tx_result.is_accepted() {
                                    shell.write_log.commit_tx();
                                    let proposal_event: Event =
                                        ProposalEvent::new(
                                            EventType::Proposal.to_string(),
                                            TallyResult::Passed,
                                            id,
                                            true,
                                            true,
                                        )
                                        .into();
                                    response.events.push(proposal_event);
                                    proposals_result.passed.push(id);

                                    proposal_author
                                } else {
                                    shell.write_log.drop_tx();
                                    let proposal_event: Event =
                                        ProposalEvent::new(
                                            EventType::Proposal.to_string(),
                                            TallyResult::Passed,
                                            id,
                                            true,
                                            false,
                                        )
                                        .into();
                                    response.events.push(proposal_event);
                                    proposals_result.rejected.push(id);

                                    slash_fund_address
                                }
                            }
                            Err(_e) => {
                                shell.write_log.drop_tx();
                                let proposal_event: Event = ProposalEvent::new(
                                    EventType::Proposal.to_string(),
                                    TallyResult::Passed,
                                    id,
                                    true,
                                    false,
                                )
                                .into();
                                response.events.push(proposal_event);
                                proposals_result.rejected.push(id);

                                slash_fund_address
                            }
                        }
                    }
                    None => {
                        let proposal_event: Event = ProposalEvent::new(
                            EventType::Proposal.to_string(),
                            TallyResult::Passed,
                            id,
                            false,
                            false,
                        )
                        .into();
                        response.events.push(proposal_event);
                        proposals_result.passed.push(id);

                        proposal_author
                    }
                }
            }
            Ok(false) => {
                let proposal_event: Event = ProposalEvent::new(
                    EventType::Proposal.to_string(),
                    TallyResult::Rejected,
                    id,
                    false,
                    false,
                )
                .into();
                response.events.push(proposal_event);
                proposals_result.rejected.push(id);

                slash_fund_address
            }
            Err(err) => {
                tracing::error!(
                    "Unexpectedly failed to tally proposal ID {id} with error \
                     {err}"
                );
                let proposal_event: Event = ProposalEvent::new(
                    EventType::Proposal.to_string(),
                    TallyResult::Failed,
                    id,
                    false,
                    false,
                )
                .into();
                response.events.push(proposal_event);

                slash_fund_address
            }
        };

        let native_token = shell.storage.native_token.clone();
        // transfer proposal locked funds
        shell.storage.transfer(
            &native_token,
            funds,
            &gov_address,
            &transfer_address,
        );
    }

    Ok(proposals_result)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use eyre::Result;
    use namada::ledger::events::EventLevel;
    use namada::ledger::native_vp::governance::utils;
    use namada::ledger::storage_api::StorageWrite;

    use super::*;

    /// Tests that if no governance proposals are present in
    /// `shell.proposal_data`, then no proposals are executed.
    #[test]
    fn test_no_governance_proposals() -> Result<()> {
        let (mut shell, _) = test_utils::setup();

        assert!(shell.proposal_data.is_empty());

        let mut resp = shim::response::FinalizeBlock::default();

        let proposals_result =
            execute_governance_proposals(&mut shell, &mut resp)?;

        assert!(
            shell.proposal_data.is_empty(),
            "shell.proposal_data should always be empty after a \
             `execute_governance_proposals` call"
        );
        assert!(proposals_result.passed.is_empty());
        assert!(proposals_result.rejected.is_empty());
        assert!(resp.events.is_empty());
        // TODO: also check expected key changes in `shell.storage` (for this
        // test, that should be no keys changed?)

        Ok(())
    }

    #[test]
    /// Tests that a governance proposal that ends without any votes is
    /// rejected.
    fn test_reject_single_governance_proposal() -> Result<()> {
        let (mut shell, _) = test_utils::setup();

        // we don't bother setting up the shell to be at the right epoch for
        // this test
        // TODO: maybe commit blocks up here in `TestShell` up until just before
        // the first block of Epoch(9), to be more realistic? As governance
        // proposals should only happen at epoch transitions

        // set up validators in storage (no delegations yet)
        utils::testing::setup_storage_with_validators(
            &mut shell.storage,
            HashMap::from([(
                address::testing::established_address_1(),
                token::Amount::from(10_000_000),
            )]),
        );

        // set up a proposal in storage
        // proposals must be in sequence starting from one (or zero?)
        let proposal_id = 1;

        let proposal_funds = token::Amount::from(100_000_000);
        let proposal_funds_key = gov_storage::get_funds_key(proposal_id);
        StorageWrite::write(
            &mut shell.storage,
            &proposal_funds_key,
            proposal_funds,
        )?;

        let proposal_end_epoch = Epoch(9);
        let proposal_end_epoch_key =
            gov_storage::get_voting_end_epoch_key(proposal_id);
        StorageWrite::write(
            &mut shell.storage,
            &proposal_end_epoch_key,
            proposal_end_epoch,
        )?;

        // TODO: more keys need to be set up in storage for this proposal to
        // be realistic - see <https://github.com/anoma/namada/blob/main/tx_prelude/src/governance.rs#L13-L66>

        shell.proposal_data = HashSet::from([proposal_id]);

        let mut resp = shim::response::FinalizeBlock::default();

        let proposals_result =
            execute_governance_proposals(&mut shell, &mut resp)?;

        assert!(
            shell.proposal_data.is_empty(),
            "shell.proposal_data should always be empty after a \
             `execute_governance_proposals` call"
        );
        assert!(proposals_result.passed.is_empty());
        assert_eq!(proposals_result.rejected, vec![proposal_id]);
        assert_eq!(
            resp.events,
            vec![Event {
                event_type: EventType::Proposal,
                level: EventLevel::Block,
                attributes: HashMap::from([
                    ("proposal_id".to_string(), proposal_id.to_string()),
                    (
                        "has_proposal_code".to_string(),
                        (true as u64).to_string()
                    ),
                    (
                        "tally_result".to_string(),
                        TallyResult::Rejected.to_string()
                    ),
                    (
                        "proposal_code_exit_status".to_string(),
                        (true as u64).to_string()
                    ),
                ])
            }]
        );
        // TODO: also check expected key changes in `shell.storage`

        Ok(())
    }
}
