use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use eyre::eyre;
use itertools::Itertools;
use namada_core::ledger::storage::{DBIter, StorageHasher, WlStorage, DB};
use namada_core::types::address::Address;
use namada_core::types::storage::BlockHeight;
use namada_core::types::token;
use namada_core::types::voting_power::FractionalVotingPower;
use namada_proof_of_stake::pos_queries::PosQueries;
use namada_proof_of_stake::types::WeightedValidator;

/// Proof of some arbitrary tally whose voters can be queried.
pub(super) trait GetVoters {
    /// Extract all the voters and the block heights at which they voted from
    /// the given proof.
    // TODO(feature = "abcipp"): we do not neet to return block heights
    // anymore. votes will always be from `storage.last_height`.
    fn get_voters(self) -> HashSet<(Address, BlockHeight)>;
}

/// Returns a map whose keys are addresses of validators and the block height at
/// which they signed some arbitrary object, and whose values are the voting
/// powers of these validators at the key's given block height.
pub(super) fn get_voting_powers<D, H, P>(
    wl_storage: &WlStorage<D, H>,
    proof: P,
) -> eyre::Result<HashMap<(Address, BlockHeight), FractionalVotingPower>>
where
    D: 'static + DB + for<'iter> DBIter<'iter> + Sync,
    H: 'static + StorageHasher + Sync,
    P: GetVoters,
{
    let voters = proof.get_voters();
    tracing::debug!(?voters, "Got validators who voted on at least one event");

    let consensus_validators = get_consensus_validators(
        wl_storage,
        voters.iter().map(|(_, h)| h.to_owned()).collect(),
    );
    tracing::debug!(
        n = consensus_validators.len(),
        ?consensus_validators,
        "Got consensus validators"
    );

    let voting_powers =
        get_voting_powers_for_selected(&consensus_validators, voters)?;
    tracing::debug!(
        ?voting_powers,
        "Got voting powers for relevant validators"
    );

    Ok(voting_powers)
}

// TODO: we might be able to remove allocation here
pub(super) fn get_consensus_validators<D, H>(
    wl_storage: &WlStorage<D, H>,
    block_heights: HashSet<BlockHeight>,
) -> BTreeMap<BlockHeight, BTreeSet<WeightedValidator>>
where
    D: 'static + DB + for<'iter> DBIter<'iter> + Sync,
    H: 'static + StorageHasher + Sync,
{
    let mut consensus_validators = BTreeMap::default();
    for height in block_heights.into_iter() {
        let epoch = wl_storage.pos_queries().get_epoch(height).expect(
            "The epoch of the last block height should always be known",
        );
        _ = consensus_validators.insert(
            height,
            wl_storage
                .pos_queries()
                .get_consensus_validators(Some(epoch))
                .iter()
                .collect(),
        );
    }
    consensus_validators
}

/// Gets the voting power of `selected` from `all_consensus`. Errors if a
/// `selected` validator is not found in `all_consensus`.
pub(super) fn get_voting_powers_for_selected(
    all_consensus: &BTreeMap<BlockHeight, BTreeSet<WeightedValidator>>,
    selected: HashSet<(Address, BlockHeight)>,
) -> eyre::Result<HashMap<(Address, BlockHeight), FractionalVotingPower>> {
    let total_voting_powers =
        sum_voting_powers_for_block_heights(all_consensus);
    let voting_powers = selected
        .into_iter()
        .map(
            |(addr, height)| -> eyre::Result<(
                (Address, BlockHeight),
                FractionalVotingPower,
            )> {
                let consensus_validators =
                    all_consensus.get(&height).ok_or_else(|| {
                        eyre!(
                            "No consensus validators found for height {height}"
                        )
                    })?;
                let individual_voting_power = consensus_validators
                    .iter()
                    .find(|&v| v.address == addr)
                    .ok_or_else(|| {
                        eyre!(
                            "No consensus validator found with address {addr} \
                             for height {height}"
                        )
                    })?
                    .bonded_stake;
                let total_voting_power = total_voting_powers
                    .get(&height)
                    .ok_or_else(|| {
                        eyre!(
                            "No total voting power provided for height \
                             {height}"
                        )
                    })?
                    .to_owned();
                Ok((
                    (addr, height),
                    FractionalVotingPower::new(
                        individual_voting_power.into(),
                        total_voting_power.into(),
                    )?,
                ))
            },
        )
        .try_collect()?;
    Ok(voting_powers)
}

pub(super) fn sum_voting_powers_for_block_heights(
    validators: &BTreeMap<BlockHeight, BTreeSet<WeightedValidator>>,
) -> BTreeMap<BlockHeight, token::Amount> {
    validators
        .iter()
        .map(|(h, vs)| (h.to_owned(), sum_voting_powers(vs)))
        .collect()
}

pub(super) fn sum_voting_powers(
    validators: &BTreeSet<WeightedValidator>,
) -> token::Amount {
    validators
        .iter()
        .map(|validator| u64::from(validator.bonded_stake))
        .sum::<u64>()
        .into()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use assert_matches::assert_matches;
    use namada_core::types::address;
    use namada_core::types::ethereum_events::testing::arbitrary_bonded_stake;

    use super::*;

    #[test]
    /// Test getting the voting power for the sole consensus validator from the
    /// set of consensus validators
    fn test_get_voting_powers_for_selected_sole_validator() {
        let sole_validator = address::testing::established_address_1();
        let bonded_stake = arbitrary_bonded_stake();
        let weighted_sole_validator = WeightedValidator {
            bonded_stake,
            address: sole_validator.clone(),
        };
        let validators = HashSet::from_iter(vec![(
            sole_validator.clone(),
            BlockHeight(100),
        )]);
        let consensus_validators = BTreeMap::from_iter(vec![(
            BlockHeight(100),
            BTreeSet::from_iter(vec![weighted_sole_validator]),
        )]);

        let result =
            get_voting_powers_for_selected(&consensus_validators, validators);

        let voting_powers = match result {
            Ok(voting_powers) => voting_powers,
            Err(error) => panic!("error: {:?}", error),
        };
        assert_eq!(voting_powers.len(), 1);
        assert_matches!(
            voting_powers.get(&(sole_validator, BlockHeight(100))),
            Some(v) if *v == FractionalVotingPower::new(1, 1).unwrap()
        );
    }

    #[test]
    /// Test that an error is returned if a validator is not found in the set of
    /// consensus validators
    fn test_get_voting_powers_for_selected_missing_validator() {
        let present_validator = address::testing::established_address_1();
        let missing_validator = address::testing::established_address_2();
        let bonded_stake = arbitrary_bonded_stake();
        let weighted_present_validator = WeightedValidator {
            bonded_stake,
            address: present_validator.clone(),
        };
        let validators = HashSet::from_iter(vec![
            (present_validator, BlockHeight(100)),
            (missing_validator, BlockHeight(100)),
        ]);
        let consensus_validators = BTreeMap::from_iter(vec![(
            BlockHeight(100),
            BTreeSet::from_iter(vec![weighted_present_validator]),
        )]);

        let result =
            get_voting_powers_for_selected(&consensus_validators, validators);

        assert!(result.is_err());
    }

    #[test]
    /// Assert we error if we are passed an `(Address, BlockHeight)` but are not
    /// given a corrseponding set of validators for the block height
    fn test_get_voting_powers_for_selected_no_consensus_validators_for_height()
    {
        let all_consensus = BTreeMap::default();
        let selected = HashSet::from_iter(vec![(
            address::testing::established_address_1(),
            BlockHeight(100),
        )]);

        let result = get_voting_powers_for_selected(&all_consensus, selected);

        assert!(result.is_err());
    }

    #[test]
    /// Test getting the voting powers for two consensus validators from the set
    /// of consensus validators
    fn test_get_voting_powers_for_selected_two_validators() {
        let validator_1 = address::testing::established_address_1();
        let validator_2 = address::testing::established_address_2();
        let bonded_stake_1 = token::Amount::from(100);
        let bonded_stake_2 = token::Amount::from(200);
        let weighted_validator_1 = WeightedValidator {
            bonded_stake: bonded_stake_1,
            address: validator_1.clone(),
        };
        let weighted_validator_2 = WeightedValidator {
            bonded_stake: bonded_stake_2,
            address: validator_2.clone(),
        };
        let validators = HashSet::from_iter(vec![
            (validator_1.clone(), BlockHeight(100)),
            (validator_2.clone(), BlockHeight(100)),
        ]);
        let consensus_validators = BTreeMap::from_iter(vec![(
            BlockHeight(100),
            BTreeSet::from_iter(vec![
                weighted_validator_1,
                weighted_validator_2,
            ]),
        )]);

        let result =
            get_voting_powers_for_selected(&consensus_validators, validators);

        let voting_powers = match result {
            Ok(voting_powers) => voting_powers,
            Err(error) => panic!("error: {:?}", error),
        };
        assert_eq!(voting_powers.len(), 2);
        assert_matches!(
            voting_powers.get(&(validator_1, BlockHeight(100))),
            Some(v) if *v == FractionalVotingPower::new(100, 300).unwrap()
        );
        assert_matches!(
            voting_powers.get(&(validator_2, BlockHeight(100))),
            Some(v) if *v == FractionalVotingPower::new(200, 300).unwrap()
        );
    }

    #[test]
    /// Test summing the voting powers for a set of validators containing only
    /// one validator
    fn test_sum_voting_powers_sole_validator() {
        let sole_validator = address::testing::established_address_1();
        let bonded_stake = arbitrary_bonded_stake();
        let weighted_sole_validator = WeightedValidator {
            bonded_stake,
            address: sole_validator,
        };
        let validators = BTreeSet::from_iter(vec![weighted_sole_validator]);

        let total = sum_voting_powers(&validators);

        assert_eq!(total, bonded_stake);
    }

    #[test]
    /// Test summing the voting powers for a set of validators containing two
    /// validators
    fn test_sum_voting_powers_two_validators() {
        let validator_1 = address::testing::established_address_1();
        let validator_2 = address::testing::established_address_2();
        let bonded_stake_1 = token::Amount::from(100);
        let bonded_stake_2 = token::Amount::from(200);
        let weighted_validator_1 = WeightedValidator {
            bonded_stake: bonded_stake_1,
            address: validator_1,
        };
        let weighted_validator_2 = WeightedValidator {
            bonded_stake: bonded_stake_2,
            address: validator_2,
        };
        let validators = BTreeSet::from_iter(vec![
            weighted_validator_1,
            weighted_validator_2,
        ]);

        let total = sum_voting_powers(&validators);

        assert_eq!(total, token::Amount::from(300));
    }
}
