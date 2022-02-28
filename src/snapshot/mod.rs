pub mod registration;

use registration::{Delegations, MainnetRewardAddress, VotingRegistration};

use chain_addr::{Discrimination, Kind};
use jormungandr_lib::crypto::account::Identifier;
use jormungandr_lib::interfaces::{Address, Initial, InitialUTxO, Value};
use serde::Deserialize;
use std::{collections::BTreeMap, iter::Iterator, num::NonZeroU64};

pub const CATALYST_VOTING_PURPOSE_TAG: u64 = 0;

#[derive(Deserialize, Clone, Debug)]
pub struct RawSnapshot(Vec<VotingRegistration>);

/// Contribution to a voting key for some registration
#[derive(Clone, Debug, PartialEq)]
pub struct KeyContribution {
    pub reward_address: MainnetRewardAddress,
    pub value: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    // a raw public key is preferred so that we don't have to worry about discrimination when deserializing from
    // a CIP-36 compatible encoding
    inner: BTreeMap<Identifier, Vec<KeyContribution>>,
    stake_threshold: Value,
}

impl Snapshot {
    pub fn from_raw_snapshot(raw_snapshot: RawSnapshot, stake_threshold: Value) -> Self {
        Self {
            inner: raw_snapshot
                .0
                .into_iter()
                .filter(|reg| reg.voting_power >= stake_threshold)
                // TODO: add capability to select voting purpose for a snapshot.
                // At the moment Catalyst is the only one in use
                .filter(|reg| reg.voting_purpose == CATALYST_VOTING_PURPOSE_TAG)
                .fold(BTreeMap::new(), |mut acc, reg| {
                    let VotingRegistration {
                        reward_address,
                        delegations,
                        voting_power,
                        ..
                    } = reg;

                    match delegations {
                        Delegations::Legacy(vk) => {
                            acc.entry(vk).or_default().push(KeyContribution {
                                reward_address,
                                value: voting_power.into(),
                            });
                        }
                        Delegations::New(mut vks) => {
                            let voting_power = u64::from(voting_power);
                            let total_weights =
                                vks.iter().map(|(_vk, weight)| *weight as u64).sum::<u64>();

                            let last = vks.pop().expect("CIP36 requires at least 1 delegation");
                            let others_total_vp = vks
                                .into_iter()
                                .filter_map(|(vk, weight)| {
                                    NonZeroU64::new((voting_power * weight as u64) / total_weights)
                                        .map(|value| (vk, value))
                                })
                                .map(|(vk, value)| {
                                    acc.entry(vk).or_default().push(KeyContribution {
                                        reward_address: reward_address.clone(),
                                        value: value.get(),
                                    });
                                    value.get()
                                })
                                .sum::<u64>();
                            if others_total_vp != voting_power {
                                acc.entry(last.0).or_default().push(KeyContribution {
                                    reward_address,
                                    value: voting_power - others_total_vp,
                                });
                            }
                        }
                    };
                    acc
                }),
            stake_threshold,
        }
    }

    pub fn stake_threshold(&self) -> Value {
        self.stake_threshold
    }

    pub fn to_block0_initials(&self, discrimination: Discrimination) -> Initial {
        Initial::Fund(
            self.inner
                .iter()
                .map(|(vk, contribs)| {
                    let value: Value = contribs.iter().map(|c| c.value).sum::<u64>().into();
                    let address: Address =
                        chain_addr::Address(discrimination, Kind::Account(vk.to_inner().into()))
                            .into();
                    InitialUTxO { address, value }
                })
                .collect::<Vec<_>>(),
        )
    }

    pub fn voting_keys(&self) -> impl Iterator<Item = &Identifier> {
        self.inner.keys()
    }

    pub fn contributions_for_voting_key<I: Into<Identifier>>(
        &self,
        voting_public_key: I,
    ) -> Vec<KeyContribution> {
        let voting_public_key: Identifier = voting_public_key.into();
        self.inner
            .get(&voting_public_key)
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quickcheck::{Arbitrary, Gen};
    use quickcheck_macros::*;
    use std::num::NonZeroU64;

    impl Arbitrary for RawSnapshot {
        fn arbitrary<G: Gen>(g: &mut G) -> Self {
            let n_registrations = usize::arbitrary(g);
            Self(
                (0..n_registrations)
                    .map(|_| VotingRegistration::arbitrary(g))
                    .collect::<Vec<_>>(),
            )
        }
    }

    #[quickcheck]
    fn test_threshold(raw: RawSnapshot, stake_threshold: u64) {
        let filtered_snapshot = raw
            .0
            .iter()
            .filter(|reg| u64::from(reg.voting_power) >= stake_threshold)
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            Snapshot::from_raw_snapshot(raw.clone(), stake_threshold.into()),
            Snapshot::from_raw_snapshot(
                RawSnapshot(filtered_snapshot.clone()),
                stake_threshold.into()
            )
        );
        let mut snapshot = Snapshot::from_raw_snapshot(RawSnapshot(filtered_snapshot), 0.into());
        snapshot.stake_threshold = stake_threshold.into();
        assert_eq!(
            Snapshot::from_raw_snapshot(raw, stake_threshold.into()),
            snapshot
        );
    }

    // Test all voting power is distributed among delegated keys
    #[quickcheck]
    fn test_voting_power_all_distributed(reg: VotingRegistration) {
        let snapshot = Snapshot::from_raw_snapshot(vec![reg.clone()].into(), 0.into());
        let total_stake =
            if let Initial::Fund(utxos) = snapshot.to_block0_initials(Discrimination::Test) {
                utxos
                    .into_iter()
                    .map(|utxo| u64::from(utxo.value))
                    .sum::<u64>()
            } else {
                unreachable!()
            };
        assert_eq!(total_stake, u64::from(reg.voting_power))
    }

    #[quickcheck]
    fn test_non_catalyst_regs_are_ignored(mut reg: VotingRegistration) {
        reg.voting_purpose = 1;
        assert_eq!(
            Snapshot::from_raw_snapshot(vec![reg].into(), 0.into()),
            Snapshot::from_raw_snapshot(vec![].into(), 0.into()),
        )
    }

    #[test]
    fn test_distribution() {
        let mut raw_snapshot = Vec::new();
        let voting_pub_key_1 = Identifier::from_hex(&hex::encode([0; 32])).unwrap();
        let voting_pub_key_2 = Identifier::from_hex(&hex::encode([1; 32])).unwrap();

        for i in 1..=10u64 {
            let delegations = Delegations::New(vec![
                (voting_pub_key_1.clone(), 1),
                (voting_pub_key_2.clone(), 1),
            ]);
            raw_snapshot.push(VotingRegistration {
                stake_public_key: String::new(),
                voting_power: i.into(),
                reward_address: String::new(),
                delegations,
                voting_purpose: 0,
            });
        }

        let snapshot = Snapshot::from_raw_snapshot(raw_snapshot.into(), 0.into());
        let vp_1: u64 = snapshot
            .contributions_for_voting_key(voting_pub_key_1)
            .into_iter()
            .map(|c| c.value)
            .sum();
        let vp_2: u64 = snapshot
            .contributions_for_voting_key(voting_pub_key_2)
            .into_iter()
            .map(|c| c.value)
            .sum();
        assert_eq!(vp_2, 30); // last key get the remainder during distributiong
        assert_eq!(vp_1, 25);
    }

    impl Arbitrary for Snapshot {
        fn arbitrary<G: Gen>(g: &mut G) -> Self {
            Self::from_raw_snapshot(
                <_>::arbitrary(g),
                (u64::from(NonZeroU64::arbitrary(g))).into(),
            )
        }
    }

    impl From<Vec<VotingRegistration>> for RawSnapshot {
        fn from(from: Vec<VotingRegistration>) -> Self {
            Self(from)
        }
    }

    // Does not pass as it's unclear why delegations are a map and not a sequence
    #[test]
    fn test_parsing() {
        let raw: RawSnapshot = serde_json::from_str(
            r#"[
            {
                "reward_address": "0xe1ffff2912572257b59dca84c965e4638a09f1524af7a15787eb0d8a46",
                "stake_public_key": "0xe7d6616840734686855ec80ee9658f5ead9e29e494ec6889a5d1988b50eb8d0f",
                "total_voting_power": 177689370111,
                "delegations": {
                    "0xa6a3c0447aeb9cc54cf6422ba32b294e5e1c3ef6d782f2acff4a70694c4d1663": 3,
                    "0x00588e8e1d18cba576a4d35758069fe94e53f638b6faf7c07b8abd2bc5c5cdee": 1}
                }
        ]"#,
        ).unwrap();
        Snapshot::from_raw_snapshot(raw, 0.into());
    }
}
