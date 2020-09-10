// LNP/BP Rust Library
// Written in 2020 by
//     Dr. Maxim Orlovsky <orlovsky@pandoracore.com>
//
// To the extent possible under law, the author(s) have dedicated all
// copyright and related and neighboring rights to this software to
// the public domain worldwide. This software is distributed without
// any warranty.
//
// You should have received a copy of the MIT License
// along with this software.
// If not, see <https://opensource.org/licenses/MIT>.

use amplify::AsAny;
use core::fmt::Debug;
use core::option::NoneError;
use std::collections::{BTreeMap, BTreeSet};

use super::{
    super::schema, amount, data, seal, Amount, AutoConceal, NodeId, SealDefinition, SECP256K1_ZKP,
};
use crate::bp::blind::OutpointHash;
use crate::client_side_validation::{commit_strategy, CommitEncodeWithStrategy, Conceal};
use crate::strict_encoding::{Error as EncodingError, StrictDecode, StrictEncode};

use bitcoin_hashes::core::cmp::Ordering;

pub type Assignments = BTreeMap<schema::AssignmentsType, AssignmentsVariant>;

impl CommitEncodeWithStrategy for Assignments {
    type Strategy = commit_strategy::Merklization;
}

pub type Ancestors = BTreeMap<NodeId, BTreeMap<schema::AssignmentsType, Vec<u16>>>;

impl CommitEncodeWithStrategy for Ancestors {
    type Strategy = commit_strategy::Merklization;
}

#[derive(Clone, Debug, Display, PartialEq)]
#[display_from(Debug)]
pub enum AssignmentsVariant {
    Declarative(BTreeSet<Assignment<DeclarativeStrategy>>),
    DiscreteFiniteField(BTreeSet<Assignment<PedersenStrategy>>),
    CustomData(BTreeSet<Assignment<HashStrategy>>),
}

impl AssignmentsVariant {
    pub fn zero_balanced(
        inputs: Vec<amount::Revealed>,
        allocations_ours: Vec<(SealDefinition, Amount)>,
        allocations_theirs: Vec<(OutpointHash, Amount)>,
    ) -> Self {
        // Generate random blinding factors
        let mut rng = rand::thread_rng();
        // We will compute the last blinding factors from all others so they
        // sum up to 0, so we need to generate only n - 1 random factors
        let count = allocations_theirs.len() + allocations_ours.len() - 1;
        let mut blinding_factors = Vec::<_>::with_capacity(count + 1);
        for _ in 0..count {
            blinding_factors.push(amount::BlindingFactor::new(&SECP256K1_ZKP, &mut rng));
        }

        // We need the last factor to be equal to the difference
        let mut blinding_inputs: Vec<_> = inputs.iter().map(|inp| inp.blinding.clone()).collect();
        if blinding_inputs.is_empty() {
            blinding_inputs.push(secp256k1zkp::key::ONE_KEY);
        }

        // remove one output blinding factor and replace it with the correction factor
        let blinding_correction = SECP256K1_ZKP
            .blind_sum(blinding_inputs.clone(), blinding_factors.clone())
            .expect("SECP256K1_ZKP failure has negligible probability");
        blinding_factors.push(blinding_correction);

        let mut blinding_iter = blinding_factors.into_iter();
        let mut set: BTreeSet<Assignment<_>> = allocations_ours
            .into_iter()
            .map(|(seal_definition, amount)| Assignment::Revealed {
                seal_definition,
                assigned_state: amount::Revealed {
                    amount,
                    blinding: blinding_iter
                        .next()
                        .expect("Internal inconsistency in `AssignmentsVariant::zero_balanced`"),
                },
            })
            .collect();
        set.extend(
            allocations_theirs
                .into_iter()
                .map(|(seal_definition, amount)| Assignment::ConfidentialSeal {
                    seal_definition,
                    assigned_state: amount::Revealed {
                        amount,
                        blinding: blinding_iter.next().expect(
                            "Internal inconsistency in `AssignmentsVariant::zero_balanced`",
                        ),
                    },
                }),
        );

        Self::DiscreteFiniteField(set)
    }

    #[inline]
    pub fn is_declarative(&self) -> bool {
        match self {
            AssignmentsVariant::Declarative(_) => true,
            _ => false,
        }
    }

    #[inline]
    pub fn is_field(&self) -> bool {
        match self {
            AssignmentsVariant::DiscreteFiniteField(_) => true,
            _ => false,
        }
    }

    #[inline]
    pub fn is_data(&self) -> bool {
        match self {
            AssignmentsVariant::CustomData(_) => true,
            _ => false,
        }
    }

    #[inline]
    pub fn declarative(&self) -> Option<&BTreeSet<Assignment<DeclarativeStrategy>>> {
        match self {
            AssignmentsVariant::Declarative(set) => Some(set),
            _ => None,
        }
    }

    #[inline]
    pub fn declarative_mut(&mut self) -> Option<&mut BTreeSet<Assignment<DeclarativeStrategy>>> {
        match self {
            AssignmentsVariant::Declarative(set) => Some(set),
            _ => None,
        }
    }

    #[inline]
    pub fn field(&self) -> Option<&BTreeSet<Assignment<PedersenStrategy>>> {
        match self {
            AssignmentsVariant::DiscreteFiniteField(set) => Some(set),
            _ => None,
        }
    }

    #[inline]
    pub fn field_mut(&mut self) -> Option<&mut BTreeSet<Assignment<PedersenStrategy>>> {
        match self {
            AssignmentsVariant::DiscreteFiniteField(set) => Some(set),
            _ => None,
        }
    }

    #[inline]
    pub fn data(&self) -> Option<&BTreeSet<Assignment<HashStrategy>>> {
        match self {
            AssignmentsVariant::CustomData(set) => Some(set),
            _ => None,
        }
    }

    #[inline]
    pub fn data_mut(&mut self) -> Option<&mut BTreeSet<Assignment<HashStrategy>>> {
        match self {
            AssignmentsVariant::CustomData(set) => Some(set),
            _ => None,
        }
    }

    pub fn seal(&self, index: u16) -> Result<Option<&seal::Revealed>, NoneError> {
        // NB: Seal indexes are part of the consensus commitment, so we have to use
        // deterministic ordering of the seals. This is currently done by using
        // `sort` vector method and `Ord` implementation for the `Assignment` type
        Ok(match self {
            AssignmentsVariant::Declarative(set) => {
                let mut vec = set.into_iter().collect::<Vec<_>>();
                vec.sort();
                vec.get(index as usize)?.seal_definition()
            }
            AssignmentsVariant::DiscreteFiniteField(set) => {
                let mut vec = set.into_iter().collect::<Vec<_>>();
                vec.sort();
                vec.get(index as usize)?.seal_definition()
            }
            AssignmentsVariant::CustomData(set) => {
                let mut vec = set.into_iter().collect::<Vec<_>>();
                vec.sort();
                vec.get(index as usize)?.seal_definition()
            }
        })
    }

    pub fn known_seals(&self) -> Vec<&seal::Revealed> {
        match self {
            AssignmentsVariant::Declarative(s) => s
                .into_iter()
                .filter_map(Assignment::<_>::seal_definition)
                .collect(),
            AssignmentsVariant::DiscreteFiniteField(s) => s
                .into_iter()
                .filter_map(Assignment::<_>::seal_definition)
                .collect(),
            AssignmentsVariant::CustomData(s) => s
                .into_iter()
                .filter_map(Assignment::<_>::seal_definition)
                .collect(),
        }
    }

    pub fn all_seals(&self) -> Vec<seal::Confidential> {
        match self {
            AssignmentsVariant::Declarative(s) => s
                .into_iter()
                .map(Assignment::<_>::seal_definition_confidential)
                .collect(),
            AssignmentsVariant::DiscreteFiniteField(s) => s
                .into_iter()
                .map(Assignment::<_>::seal_definition_confidential)
                .collect(),
            AssignmentsVariant::CustomData(s) => s
                .into_iter()
                .map(Assignment::<_>::seal_definition_confidential)
                .collect(),
        }
    }

    pub fn known_state_homomorphic(&self) -> Vec<&amount::Revealed> {
        match self {
            AssignmentsVariant::Declarative(_) => vec![],
            AssignmentsVariant::DiscreteFiniteField(s) => s
                .into_iter()
                .filter_map(Assignment::<_>::assigned_state)
                .collect(),
            AssignmentsVariant::CustomData(_) => vec![],
        }
    }

    pub fn known_state_data(&self) -> Vec<&data::Revealed> {
        match self {
            AssignmentsVariant::Declarative(_) => vec![],
            AssignmentsVariant::DiscreteFiniteField(_) => vec![],
            AssignmentsVariant::CustomData(s) => s
                .into_iter()
                .filter_map(Assignment::<_>::assigned_state)
                .collect(),
        }
    }

    pub fn all_state_pedersen(&self) -> Vec<amount::Confidential> {
        match self {
            AssignmentsVariant::Declarative(_) => vec![],
            AssignmentsVariant::DiscreteFiniteField(s) => s
                .into_iter()
                .map(Assignment::<_>::assigned_state_confidential)
                .collect(),
            AssignmentsVariant::CustomData(_) => vec![],
        }
    }

    pub fn all_state_hashed(&self) -> Vec<data::Confidential> {
        match self {
            AssignmentsVariant::Declarative(_) => vec![],
            AssignmentsVariant::DiscreteFiniteField(_) => vec![],
            AssignmentsVariant::CustomData(s) => s
                .into_iter()
                .map(Assignment::<_>::assigned_state_confidential)
                .collect(),
        }
    }

    pub fn len(&self) -> usize {
        match self {
            AssignmentsVariant::Declarative(set) => set.len(),
            AssignmentsVariant::DiscreteFiniteField(set) => set.len(),
            AssignmentsVariant::CustomData(set) => set.len(),
        }
    }
}

impl AutoConceal for AssignmentsVariant {
    fn conceal_except(&mut self, seals: &Vec<seal::Confidential>) -> usize {
        match self {
            AssignmentsVariant::Declarative(data) => data as &mut dyn AutoConceal,
            AssignmentsVariant::DiscreteFiniteField(data) => data as &mut dyn AutoConceal,
            AssignmentsVariant::CustomData(data) => data as &mut dyn AutoConceal,
        }
        .conceal_except(seals)
    }
}

impl CommitEncodeWithStrategy for AssignmentsVariant {
    type Strategy = commit_strategy::UsingStrict;
}

pub trait ConfidentialState:
    StrictEncode<Error = EncodingError> + StrictDecode<Error = EncodingError> + Debug + Clone + AsAny
{
}

pub trait RevealedState:
    StrictEncode<Error = EncodingError>
    + StrictDecode<Error = EncodingError>
    + Debug
    + Conceal
    + Clone
    + AsAny
{
}

pub trait StateTypes: Debug {
    type Confidential: ConfidentialState;
    type Revealed: RevealedState;
}

#[derive(Clone, Debug, PartialOrd, Ord, PartialEq, Eq)]
pub struct DeclarativeStrategy;
impl StateTypes for DeclarativeStrategy {
    type Confidential = data::Void;
    type Revealed = data::Void;
}

#[derive(Clone, Debug, PartialOrd, Ord, PartialEq, Eq)]
pub struct PedersenStrategy;
impl StateTypes for PedersenStrategy {
    type Confidential = amount::Confidential;
    type Revealed = amount::Revealed;
}

#[derive(Clone, Debug, PartialOrd, Ord, PartialEq, Eq)]
pub struct HashStrategy;
impl StateTypes for HashStrategy {
    type Confidential = data::Confidential;
    type Revealed = data::Revealed;
}

#[derive(Clone, Debug, Display)]
#[display_from(Debug)]
pub enum Assignment<STATE>
where
    STATE: StateTypes,
    // Deterministic ordering requires Eq operation, so the confidential
    // state must have it
    STATE::Confidential: PartialEq + Eq,
    STATE::Confidential: From<<STATE::Revealed as Conceal>::Confidential>,
    EncodingError: From<<STATE::Confidential as StrictEncode>::Error>
        + From<<STATE::Confidential as StrictDecode>::Error>
        + From<<STATE::Revealed as StrictEncode>::Error>
        + From<<STATE::Revealed as StrictDecode>::Error>,
{
    Confidential {
        seal_definition: seal::Confidential,
        assigned_state: STATE::Confidential,
    },
    Revealed {
        seal_definition: seal::Revealed,
        assigned_state: STATE::Revealed,
    },
    ConfidentialSeal {
        seal_definition: seal::Confidential,
        assigned_state: STATE::Revealed,
    },
    ConfidentialAmount {
        seal_definition: seal::Revealed,
        assigned_state: STATE::Confidential,
    },
}

// Consensus-critical!
// Assignment indexes are part of the transition ancestor's commitment, so
// here we use deterministic ordering based on hash values of the concealed
// seal data contained within the assignment
impl<STATE> PartialOrd for Assignment<STATE>
where
    STATE: StateTypes,
    STATE::Confidential: PartialEq + Eq,
    STATE::Confidential: From<<STATE::Revealed as Conceal>::Confidential>,
    EncodingError: From<<STATE::Confidential as StrictEncode>::Error>
        + From<<STATE::Confidential as StrictDecode>::Error>
        + From<<STATE::Revealed as StrictEncode>::Error>
        + From<<STATE::Revealed as StrictDecode>::Error>,
{
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.seal_definition_confidential()
            .partial_cmp(&other.seal_definition_confidential())
    }
}

impl<STATE> Ord for Assignment<STATE>
where
    STATE: StateTypes,
    STATE::Confidential: PartialEq + Eq,
    STATE::Confidential: From<<STATE::Revealed as Conceal>::Confidential>,
    EncodingError: From<<STATE::Confidential as StrictEncode>::Error>
        + From<<STATE::Confidential as StrictDecode>::Error>
        + From<<STATE::Revealed as StrictEncode>::Error>
        + From<<STATE::Revealed as StrictDecode>::Error>,
{
    fn cmp(&self, other: &Self) -> Ordering {
        self.seal_definition_confidential()
            .cmp(&other.seal_definition_confidential())
    }
}

impl<STATE> PartialEq for Assignment<STATE>
where
    STATE: StateTypes,
    STATE::Confidential: PartialEq + Eq,
    STATE::Confidential: From<<STATE::Revealed as Conceal>::Confidential>,
    EncodingError: From<<STATE::Confidential as StrictEncode>::Error>
        + From<<STATE::Confidential as StrictDecode>::Error>
        + From<<STATE::Revealed as StrictEncode>::Error>
        + From<<STATE::Revealed as StrictDecode>::Error>,
{
    fn eq(&self, other: &Self) -> bool {
        self.seal_definition_confidential() == other.seal_definition_confidential()
            && self.assigned_state_confidential() == other.assigned_state_confidential()
    }
}

impl<STATE> Eq for Assignment<STATE>
where
    STATE: StateTypes,
    STATE::Confidential: PartialEq + Eq,
    STATE::Confidential: From<<STATE::Revealed as Conceal>::Confidential>,
    EncodingError: From<<STATE::Confidential as StrictEncode>::Error>
        + From<<STATE::Confidential as StrictDecode>::Error>
        + From<<STATE::Revealed as StrictEncode>::Error>
        + From<<STATE::Revealed as StrictDecode>::Error>,
{
}

impl<STATE> Assignment<STATE>
where
    STATE: StateTypes,
    STATE::Confidential: PartialEq + Eq,
    STATE::Confidential: From<<STATE::Revealed as Conceal>::Confidential>,
    EncodingError: From<<STATE::Confidential as StrictEncode>::Error>
        + From<<STATE::Confidential as StrictDecode>::Error>
        + From<<STATE::Revealed as StrictEncode>::Error>
        + From<<STATE::Revealed as StrictDecode>::Error>,
{
    pub fn seal_definition_confidential(&self) -> seal::Confidential {
        match self {
            Assignment::Revealed {
                seal_definition, ..
            }
            | Assignment::ConfidentialAmount {
                seal_definition, ..
            } => seal_definition.conceal(),
            Assignment::Confidential {
                seal_definition, ..
            }
            | Assignment::ConfidentialSeal {
                seal_definition, ..
            } => *seal_definition,
        }
    }

    pub fn seal_definition(&self) -> Option<&seal::Revealed> {
        match self {
            Assignment::Revealed {
                seal_definition, ..
            }
            | Assignment::ConfidentialAmount {
                seal_definition, ..
            } => Some(seal_definition),
            Assignment::Confidential { .. } | Assignment::ConfidentialSeal { .. } => None,
        }
    }

    pub fn assigned_state_confidential(&self) -> STATE::Confidential {
        match self {
            Assignment::Revealed { assigned_state, .. }
            | Assignment::ConfidentialSeal { assigned_state, .. } => {
                assigned_state.conceal().into()
            }
            Assignment::Confidential { assigned_state, .. }
            | Assignment::ConfidentialAmount { assigned_state, .. } => assigned_state.clone(),
        }
    }

    pub fn assigned_state(&self) -> Option<&STATE::Revealed> {
        match self {
            Assignment::Revealed { assigned_state, .. }
            | Assignment::ConfidentialSeal { assigned_state, .. } => Some(assigned_state),
            Assignment::Confidential { .. } | Assignment::ConfidentialAmount { .. } => None,
        }
    }
}

impl<STATE> Conceal for Assignment<STATE>
where
    Self: Clone,
    STATE: StateTypes,
    STATE::Confidential: PartialEq + Eq,
    STATE::Confidential: From<<STATE::Revealed as Conceal>::Confidential>,
    EncodingError: From<<STATE::Confidential as StrictEncode>::Error>
        + From<<STATE::Confidential as StrictDecode>::Error>
        + From<<STATE::Revealed as StrictEncode>::Error>
        + From<<STATE::Revealed as StrictDecode>::Error>,
{
    type Confidential = Assignment<STATE>;

    fn conceal(&self) -> Self {
        match self {
            Assignment::Confidential { .. } | Assignment::ConfidentialAmount { .. } => self.clone(),
            Assignment::Revealed {
                seal_definition,
                assigned_state,
            } => Self::ConfidentialAmount {
                seal_definition: seal_definition.clone(),
                assigned_state: assigned_state.conceal().into(),
            },
            Assignment::ConfidentialSeal {
                seal_definition,
                assigned_state,
            } => Self::Confidential {
                seal_definition: seal_definition.clone(),
                assigned_state: assigned_state.conceal().into(),
            },
        }
    }
}

impl<STATE> AutoConceal for Assignment<STATE>
where
    STATE: StateTypes,
    STATE::Revealed: Conceal,
    STATE::Confidential: PartialEq + Eq,
    <STATE as StateTypes>::Confidential: From<<STATE::Revealed as Conceal>::Confidential>,
    EncodingError: From<<STATE::Confidential as StrictEncode>::Error>
        + From<<STATE::Confidential as StrictDecode>::Error>
        + From<<STATE::Revealed as StrictEncode>::Error>
        + From<<STATE::Revealed as StrictDecode>::Error>,
{
    fn conceal_except(&mut self, seals: &Vec<seal::Confidential>) -> usize {
        match self {
            Assignment::Confidential { .. } | Assignment::ConfidentialAmount { .. } => 0,
            Assignment::ConfidentialSeal {
                seal_definition,
                assigned_state,
            } => {
                if seals.contains(&seal_definition) {
                    0
                } else {
                    *self = Assignment::<STATE>::Confidential {
                        assigned_state: assigned_state.conceal().into(),
                        seal_definition: seal_definition.clone(),
                    };
                    1
                }
            }
            Assignment::Revealed {
                seal_definition,
                assigned_state,
            } => {
                if seals.contains(&seal_definition.conceal()) {
                    0
                } else {
                    *self = Assignment::<STATE>::ConfidentialAmount {
                        assigned_state: assigned_state.conceal().into(),
                        seal_definition: seal_definition.clone(),
                    };
                    1
                }
            }
        }
    }
}

impl<STATE> CommitEncodeWithStrategy for Assignment<STATE>
where
    STATE: StateTypes,
    STATE::Confidential: PartialEq + Eq,
    STATE::Confidential: From<<STATE::Revealed as Conceal>::Confidential>,
    EncodingError: From<<STATE::Confidential as StrictEncode>::Error>
        + From<<STATE::Confidential as StrictDecode>::Error>
        + From<<STATE::Revealed as StrictEncode>::Error>
        + From<<STATE::Revealed as StrictDecode>::Error>,
{
    type Strategy = commit_strategy::UsingConceal;
}

mod strict_encoding {
    use super::*;
    use crate::strict_encoding::Error;
    use data::strict_encoding::EncodingTag;
    use std::io;

    impl StrictEncode for AssignmentsVariant {
        type Error = Error;

        fn strict_encode<E: io::Write>(&self, mut e: E) -> Result<usize, Self::Error> {
            Ok(match self {
                AssignmentsVariant::Declarative(tree) => {
                    strict_encode_list!(e; schema::StateType::Declarative, tree)
                }
                AssignmentsVariant::DiscreteFiniteField(tree) => {
                    strict_encode_list!(e; schema::StateType::DiscreteFiniteField, EncodingTag::U64, tree)
                }
                AssignmentsVariant::CustomData(tree) => {
                    strict_encode_list!(e; schema::StateType::CustomData, tree)
                }
            })
        }
    }

    impl StrictDecode for AssignmentsVariant {
        type Error = Error;

        fn strict_decode<D: io::Read>(mut d: D) -> Result<Self, Self::Error> {
            let format = schema::StateType::strict_decode(&mut d)?;
            Ok(match format {
                schema::StateType::Declarative => {
                    AssignmentsVariant::Declarative(BTreeSet::strict_decode(d)?)
                }
                schema::StateType::DiscreteFiniteField => match EncodingTag::strict_decode(&mut d)?
                {
                    EncodingTag::U64 => {
                        AssignmentsVariant::DiscreteFiniteField(BTreeSet::strict_decode(&mut d)?)
                    }
                    _ => Err(Error::UnsupportedDataStructure(
                        "We support only homomorphic commitments to U64 data".to_string(),
                    ))?,
                },
                schema::StateType::CustomData => {
                    AssignmentsVariant::CustomData(BTreeSet::strict_decode(d)?)
                }
            })
        }
    }

    impl<STATE> StrictEncode for Assignment<STATE>
    where
        STATE: StateTypes,
        STATE::Confidential: PartialEq + Eq,
        STATE::Confidential: From<<STATE::Revealed as Conceal>::Confidential>,
        EncodingError: From<<STATE::Confidential as StrictEncode>::Error>
            + From<<STATE::Confidential as StrictDecode>::Error>
            + From<<STATE::Revealed as StrictEncode>::Error>
            + From<<STATE::Revealed as StrictDecode>::Error>,
    {
        type Error = Error;

        fn strict_encode<E: io::Write>(&self, mut e: E) -> Result<usize, Self::Error> {
            Ok(match self {
                Assignment::Confidential {
                    seal_definition,
                    assigned_state,
                } => strict_encode_list!(e; 0u8, seal_definition, assigned_state),
                Assignment::Revealed {
                    seal_definition,
                    assigned_state,
                } => strict_encode_list!(e; 1u8, seal_definition, assigned_state),
                Assignment::ConfidentialSeal {
                    seal_definition,
                    assigned_state,
                } => strict_encode_list!(e; 2u8, seal_definition, assigned_state),
                Assignment::ConfidentialAmount {
                    seal_definition,
                    assigned_state,
                } => strict_encode_list!(e; 3u8, seal_definition, assigned_state),
            })
        }
    }

    impl<STATE> StrictDecode for Assignment<STATE>
    where
        STATE: StateTypes,
        STATE::Confidential: PartialEq + Eq,
        STATE::Confidential: From<<STATE::Revealed as Conceal>::Confidential>,
        EncodingError: From<<STATE::Confidential as StrictEncode>::Error>
            + From<<STATE::Confidential as StrictDecode>::Error>
            + From<<STATE::Revealed as StrictEncode>::Error>
            + From<<STATE::Revealed as StrictDecode>::Error>,
    {
        type Error = Error;

        fn strict_decode<D: io::Read>(mut d: D) -> Result<Self, Self::Error> {
            let format = u8::strict_decode(&mut d)?;
            Ok(match format {
                0u8 => Assignment::Confidential {
                    seal_definition: seal::Confidential::strict_decode(&mut d)?,
                    assigned_state: STATE::Confidential::strict_decode(&mut d)?,
                },
                1u8 => Assignment::Revealed {
                    seal_definition: seal::Revealed::strict_decode(&mut d)?,
                    assigned_state: STATE::Revealed::strict_decode(&mut d)?,
                },
                2u8 => Assignment::ConfidentialSeal {
                    seal_definition: seal::Confidential::strict_decode(&mut d)?,
                    assigned_state: STATE::Revealed::strict_decode(&mut d)?,
                },
                3u8 => Assignment::ConfidentialAmount {
                    seal_definition: seal::Revealed::strict_decode(&mut d)?,
                    assigned_state: STATE::Confidential::strict_decode(&mut d)?,
                },
                invalid => Err(Error::EnumValueNotKnown("Assignment".to_string(), invalid))?,
            })
        }
    }
}

#[cfg(test)]
mod test {
    use super::super::testutils::*;
    use super::*;
    use crate::bp::blind::OutpointReveal;
    use crate::paradigms::client_side_validation::Conceal;
    use crate::rgb::contract::seal::Revealed;
    use crate::rgb::data;
    use bitcoin::blockdata::transaction::OutPoint;
    use bitcoin_hashes::{
        hex::{FromHex, ToHex},
        sha256,
    };
    use rand::{thread_rng, Rng};
    use secp256k1zkp::{key::SecretKey, pedersen::Commitment, Secp256k1};
    use std::num::Wrapping;

    // Hard coded test vectors of Assignment Variants
    // Each Variant contains 4 types of Assignments
    // [Revealed, Confidential, ConfidentialSeal, ConfidentialState]
    static HASH_VARIANT: [u8; 267] = [
        0x2, 0x4, 0x0, 0x2, 0x3a, 0xa, 0x34, 0xc8, 0xd, 0xdb, 0x3e, 0xac, 0x5c, 0xd5, 0x92, 0x38,
        0x30, 0x81, 0x4d, 0x72, 0xf9, 0xde, 0x9, 0x6b, 0xca, 0x74, 0x87, 0x79, 0xda, 0x39, 0x7a,
        0xa3, 0xb7, 0x71, 0xfe, 0x7e, 0x40, 0xc6, 0x41, 0x1a, 0xea, 0x8, 0x2e, 0x2c, 0x5d, 0x74,
        0x34, 0x73, 0x68, 0x67, 0x7d, 0xb6, 0x95, 0x45, 0x12, 0x62, 0x37, 0xd5, 0xed, 0x78, 0xfa,
        0xa0, 0x84, 0x63, 0x52, 0xf5, 0x38, 0x3f, 0x95, 0x3, 0x0, 0xfd, 0xb7, 0x19, 0xd1, 0x24,
        0xce, 0xff, 0x58, 0x6, 0xe, 0xf5, 0x8d, 0x94, 0xa, 0x75, 0xe4, 0x3d, 0x13, 0x9d, 0x55,
        0xa5, 0xe4, 0xd3, 0x26, 0x4d, 0xc9, 0xeb, 0x4f, 0x77, 0x3b, 0xff, 0xc5, 0x72, 0x90, 0x19,
        0xe4, 0x7e, 0xd2, 0x7e, 0xf5, 0x1, 0x0, 0x0, 0x0, 0x64, 0x20, 0xcc, 0x42, 0x1e, 0x11, 0x89,
        0x80, 0x5c, 0x8c, 0xec, 0x8, 0x9d, 0x74, 0xc1, 0x98, 0xf, 0x79, 0xc0, 0x69, 0x0, 0x5a,
        0x21, 0xae, 0x40, 0xa7, 0xe5, 0x8e, 0x68, 0x77, 0xa8, 0x10, 0x7b, 0x4, 0x9, 0x1a, 0x9a,
        0x97, 0x1, 0x2, 0x90, 0xe5, 0x10, 0xa2, 0x10, 0x60, 0xad, 0xa3, 0x39, 0x71, 0xd, 0xd, 0xdc,
        0x43, 0xe4, 0x46, 0x0, 0x6c, 0x5b, 0xc9, 0x38, 0x64, 0xda, 0xfb, 0x3, 0xcf, 0x4b, 0xa4,
        0x72, 0xbe, 0xdf, 0x5c, 0xa7, 0x1, 0x0, 0x47, 0xe7, 0xd3, 0x5d, 0x93, 0xe4, 0xb5, 0x62,
        0x8e, 0xaf, 0xd3, 0x36, 0xd, 0x65, 0x25, 0x89, 0x52, 0xf4, 0xd9, 0x57, 0x5e, 0xac, 0x1b,
        0x1f, 0x18, 0xee, 0x18, 0x51, 0x29, 0x71, 0x82, 0x93, 0xb6, 0xd7, 0x62, 0x2b, 0x1e, 0xdd,
        0x1f, 0x20, 0x1, 0x0, 0x0, 0x0, 0x40, 0xe7, 0xa, 0x36, 0xe2, 0xce, 0x51, 0xd3, 0x1d, 0x4c,
        0xf5, 0xd6, 0x73, 0x1f, 0xa6, 0x37, 0x38, 0x64, 0x81, 0x27, 0xdb, 0x83, 0x37, 0x15, 0xd3,
        0x96, 0x52, 0xd8, 0x6d, 0x92, 0x7d, 0x48, 0x88,
    ];

    static PEDERSAN_VARIANT: [u8; 1672] = [
        0x1, 0x3, 0x4, 0x0, 0x3, 0x0, 0x6e, 0x5a, 0x76, 0xca, 0xd0, 0x21, 0x63, 0xa5, 0x6, 0xe,
        0xf5, 0x8d, 0x94, 0xa, 0x75, 0xe4, 0x3d, 0x13, 0x9d, 0x55, 0xa5, 0xe4, 0xd3, 0x26, 0x4d,
        0xc9, 0xeb, 0x4f, 0x77, 0x3b, 0xff, 0xc5, 0x72, 0x90, 0x19, 0xe4, 0x7e, 0xd2, 0x7e, 0xf5,
        0x1, 0x0, 0x0, 0x0, 0x21, 0x0, 0x8, 0xcc, 0x48, 0xfa, 0x5e, 0x5c, 0xb1, 0xd2, 0xd2, 0x46,
        0x5b, 0xd8, 0xc4, 0x37, 0xc0, 0xe0, 0x5, 0x14, 0xab, 0xd8, 0x13, 0xf9, 0xa7, 0xdd, 0x50,
        0x6a, 0x77, 0x84, 0x5, 0xa2, 0xc4, 0x3b, 0xc0, 0xa3, 0x2, 0xdd, 0x8a, 0x81, 0xc2, 0xb1,
        0x62, 0xe7, 0xb9, 0xc8, 0xec, 0xe9, 0x64, 0xfc, 0x4f, 0x67, 0x56, 0xdb, 0x85, 0x34, 0x43,
        0x97, 0x3c, 0x84, 0xf9, 0x32, 0x45, 0x5e, 0x8c, 0x4c, 0x93, 0xd9, 0x19, 0xb, 0x68, 0x4e,
        0x5a, 0x15, 0xc7, 0x31, 0xb, 0x33, 0xa4, 0xc0, 0xbe, 0xa6, 0x11, 0xc, 0x64, 0xa0, 0x24,
        0x72, 0x79, 0xec, 0x12, 0x49, 0xc6, 0x9f, 0x94, 0xeb, 0x5, 0x71, 0x7d, 0x81, 0x0, 0xe,
        0x3f, 0x84, 0x8e, 0x9f, 0xe9, 0x68, 0x2f, 0xa6, 0xa, 0xd8, 0x59, 0x57, 0xcf, 0x64, 0xb9,
        0x56, 0xb5, 0xfc, 0xcc, 0x2b, 0xdc, 0x9e, 0x4d, 0xdd, 0x78, 0x60, 0x63, 0x12, 0x57, 0x12,
        0xcd, 0xf3, 0x6f, 0xe2, 0xca, 0x1e, 0x19, 0x3a, 0xb, 0x10, 0xc, 0x59, 0x97, 0xc, 0xde,
        0xa8, 0x62, 0x42, 0x4a, 0x2f, 0x1e, 0xeb, 0x89, 0x98, 0xc6, 0x31, 0x82, 0xc9, 0x4f, 0xf,
        0xf1, 0xa5, 0x1a, 0x37, 0x2d, 0x92, 0x86, 0x8c, 0xe5, 0x37, 0x3a, 0x86, 0xc4, 0x89, 0x9f,
        0xf4, 0xcf, 0x10, 0x7b, 0x9a, 0x30, 0xc0, 0x0, 0x97, 0x1e, 0x44, 0x9b, 0xb2, 0x92, 0x1d,
        0x38, 0x6e, 0x3a, 0xce, 0xea, 0x95, 0xcd, 0xcd, 0x63, 0x74, 0x5e, 0x43, 0xf, 0xd3, 0xdd,
        0x21, 0x2, 0xca, 0x91, 0xc5, 0x2d, 0x9f, 0x21, 0x7b, 0x4d, 0x14, 0x9f, 0xf1, 0x88, 0xe5,
        0x3a, 0x98, 0x6d, 0x3, 0xdd, 0x64, 0x90, 0x73, 0x5a, 0x87, 0x1f, 0x53, 0x64, 0xe4, 0x9e,
        0x48, 0xfc, 0x1e, 0x3e, 0xcc, 0xeb, 0x5, 0xd3, 0xfd, 0x9a, 0x56, 0x5f, 0x71, 0x51, 0x39,
        0xe3, 0x10, 0xa8, 0xae, 0x20, 0xa8, 0xba, 0xca, 0x7b, 0x91, 0x6, 0xe9, 0x61, 0x45, 0x69,
        0x91, 0x94, 0xe0, 0xec, 0x50, 0xa4, 0x12, 0x58, 0xe1, 0x64, 0xc2, 0x4c, 0x3c, 0x7f, 0x69,
        0x7a, 0x7e, 0x4a, 0xee, 0xed, 0xb, 0x91, 0x3e, 0x63, 0x71, 0x96, 0x99, 0x78, 0xf0, 0x3e,
        0x40, 0x96, 0x58, 0x9a, 0xd, 0xb9, 0x77, 0x79, 0xa2, 0xb7, 0xa3, 0x67, 0xcf, 0xc2, 0x45,
        0x27, 0xf0, 0x86, 0x3f, 0x8f, 0x60, 0xec, 0x17, 0x54, 0x4, 0xa8, 0xf1, 0xce, 0x5f, 0xa2,
        0x5b, 0xe9, 0xba, 0xb0, 0xac, 0x4f, 0x5b, 0x47, 0xbb, 0xb6, 0xd8, 0x1, 0x7, 0x73, 0x24,
        0xc4, 0x8b, 0xc8, 0xe1, 0x15, 0xe4, 0xd2, 0x9f, 0xc5, 0x4, 0xed, 0x13, 0xc3, 0x17, 0xb8,
        0xc9, 0xdd, 0x84, 0x78, 0xea, 0x92, 0x4c, 0x41, 0x98, 0xc, 0x38, 0xc8, 0x2, 0x20, 0xeb,
        0xf2, 0x93, 0x75, 0x8f, 0xd7, 0x9d, 0x76, 0xfa, 0xfa, 0xbb, 0x5e, 0xa1, 0x98, 0x51, 0xd6,
        0xbd, 0x6, 0xa2, 0x37, 0x2, 0x89, 0x10, 0xb9, 0x84, 0x69, 0xc7, 0xb7, 0xee, 0xec, 0xca,
        0x2d, 0x13, 0xbb, 0x8f, 0xb, 0xa5, 0x9f, 0x17, 0x6a, 0xb2, 0xef, 0x51, 0x39, 0x1f, 0xce,
        0x69, 0x8c, 0xc, 0x67, 0x67, 0x6f, 0x29, 0x29, 0x5, 0x5a, 0xcb, 0x17, 0x6a, 0x8f, 0x1b,
        0xe6, 0x1c, 0x32, 0xad, 0xf2, 0xda, 0xb3, 0xb6, 0xb8, 0x6e, 0xae, 0x28, 0x9e, 0x7b, 0x12,
        0x3f, 0x52, 0x26, 0xfd, 0x9c, 0xad, 0x2b, 0x18, 0xb2, 0x6f, 0x33, 0xf, 0xf5, 0xab, 0x53,
        0x8c, 0x9b, 0xbf, 0xca, 0xe2, 0x1f, 0xfd, 0x91, 0xaa, 0x41, 0x26, 0x81, 0xdc, 0x1c, 0x9a,
        0xd4, 0x1d, 0xec, 0xd9, 0x48, 0x60, 0xc9, 0x7, 0x1c, 0xf8, 0x4d, 0x41, 0xfc, 0x4, 0xe,
        0xf0, 0x7d, 0xe3, 0x31, 0x7f, 0xc5, 0xcd, 0x5e, 0x84, 0x3d, 0xda, 0x92, 0xfb, 0x71, 0xc3,
        0x77, 0x2a, 0xae, 0x39, 0x65, 0x16, 0x24, 0x7d, 0x7c, 0x61, 0xcd, 0xdd, 0xe3, 0x50, 0x54,
        0x44, 0xc4, 0x30, 0x98, 0xfc, 0x62, 0xb9, 0xad, 0x20, 0x7b, 0x2b, 0x5b, 0xf1, 0xf6, 0xe5,
        0x3e, 0xf4, 0xe0, 0xaf, 0x7a, 0xeb, 0xe6, 0xee, 0xe7, 0x21, 0xc, 0xf1, 0x54, 0xbc, 0xe7,
        0xe4, 0x19, 0xd9, 0xfd, 0x1d, 0x1b, 0x2f, 0xad, 0xeb, 0xe4, 0x27, 0x73, 0xd, 0xcd, 0xb8,
        0x7a, 0x7e, 0xe7, 0x4b, 0x8d, 0xce, 0x83, 0x91, 0x1, 0x82, 0x62, 0xb1, 0xb0, 0xad, 0x32,
        0x6f, 0xb6, 0xe2, 0xff, 0x10, 0x5c, 0x83, 0x13, 0xa4, 0x6f, 0xe7, 0xaa, 0x7, 0xf0, 0xc4,
        0x3c, 0x42, 0x51, 0xd9, 0xc7, 0x70, 0x4, 0xf, 0x6e, 0x2c, 0x5c, 0x67, 0x2d, 0xd2, 0x3,
        0x69, 0xa, 0x45, 0x9b, 0xa9, 0x6e, 0xd0, 0x6c, 0x7e, 0xfb, 0xf3, 0x15, 0xa0, 0x8d, 0x31,
        0xb0, 0x7d, 0x83, 0xc, 0xa9, 0xbf, 0xa8, 0xcc, 0x13, 0x33, 0x61, 0xdf, 0x2f, 0x7e, 0x4d,
        0xd3, 0xe, 0x94, 0x0, 0xa4, 0x49, 0xcc, 0xf, 0x32, 0x93, 0x1, 0xdc, 0xf1, 0x56, 0xfe, 0x14,
        0xa0, 0x95, 0x96, 0xf6, 0xe5, 0x23, 0x2, 0xb7, 0xce, 0x71, 0x2c, 0xa6, 0x7e, 0x67, 0x7a,
        0x59, 0x84, 0x5c, 0xc5, 0xbe, 0x66, 0xd4, 0x73, 0x3a, 0xbd, 0xf9, 0xa3, 0xd4, 0x7a, 0x66,
        0xaf, 0xe, 0x46, 0x2d, 0x6d, 0x2c, 0x5b, 0x31, 0xf9, 0x51, 0x5, 0xa6, 0xa4, 0x49, 0xbd,
        0xf3, 0x5, 0x6d, 0x98, 0x56, 0xa6, 0xce, 0xea, 0x15, 0x1, 0x0, 0xa, 0xa2, 0x7f, 0x61, 0x42,
        0xef, 0x52, 0xbc, 0x8e, 0xaf, 0xd3, 0x36, 0xd, 0x65, 0x25, 0x89, 0x52, 0xf4, 0xd9, 0x57,
        0x5e, 0xac, 0x1b, 0x1f, 0x18, 0xee, 0x18, 0x51, 0x29, 0x71, 0x82, 0x93, 0xb6, 0xd7, 0x62,
        0x2b, 0x1e, 0xdd, 0x1f, 0x20, 0x1, 0x0, 0x0, 0x0, 0x3, 0xa, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
        0x0, 0x20, 0x0, 0xde, 0xbb, 0xbe, 0xfd, 0x16, 0x83, 0xe3, 0x35, 0x29, 0x6a, 0xc, 0x86,
        0xf1, 0xc8, 0x82, 0xa2, 0xea, 0x37, 0x59, 0xf1, 0x14, 0x22, 0xb, 0xb, 0x2c, 0xf8, 0x69,
        0xe3, 0x7d, 0xec, 0x24, 0xc8, 0x0, 0xb3, 0x2a, 0x56, 0xa6, 0xc7, 0x2, 0x56, 0x33, 0x79,
        0xad, 0x65, 0xd0, 0x7a, 0x2c, 0x80, 0xe0, 0x46, 0x73, 0xaf, 0x85, 0x59, 0x52, 0x58, 0xfc,
        0x15, 0x60, 0xff, 0xd8, 0x47, 0x1a, 0xd7, 0x32, 0x21, 0x0, 0x8, 0x97, 0x75, 0xf8, 0x29,
        0xc8, 0xad, 0xad, 0x92, 0xad, 0xa1, 0x7b, 0x59, 0x31, 0xed, 0xf6, 0x30, 0x64, 0xd5, 0x46,
        0x78, 0xf4, 0xeb, 0x9a, 0x6f, 0xdf, 0xe8, 0xe4, 0xcb, 0x5d, 0x95, 0xf6, 0xf4, 0xa3, 0x2,
        0x3b, 0x4d, 0x5a, 0x83, 0xe1, 0x98, 0x44, 0x23, 0xd7, 0xc8, 0xe3, 0xc8, 0x35, 0xf, 0x42,
        0xc, 0xca, 0x90, 0xc5, 0xa4, 0xa7, 0x3f, 0xea, 0x62, 0x69, 0xe4, 0x78, 0xbc, 0x63, 0x94,
        0x76, 0xfb, 0xb6, 0x64, 0xa9, 0xf7, 0x81, 0x70, 0xc6, 0xd, 0xa3, 0x70, 0x5, 0xc1, 0xc8,
        0x94, 0x7f, 0x20, 0xff, 0x83, 0x8c, 0xc4, 0x7a, 0x26, 0xd3, 0x84, 0xab, 0x51, 0x15, 0xfc,
        0xb1, 0x57, 0x2d, 0xb3, 0x9, 0x1b, 0xc8, 0x6d, 0x32, 0xfd, 0x52, 0x70, 0xa, 0xf1, 0x47,
        0xe2, 0xc4, 0xa8, 0x5c, 0x5e, 0x95, 0x18, 0xc8, 0x46, 0xa9, 0xa3, 0xd0, 0xda, 0x76, 0xce,
        0xbf, 0xdb, 0x31, 0x27, 0x4b, 0x68, 0x58, 0xe5, 0x36, 0x13, 0x54, 0x29, 0x94, 0x4c, 0x30,
        0x23, 0x4c, 0xc, 0x6a, 0x3a, 0x5, 0x15, 0x70, 0x97, 0x8a, 0x3e, 0xc5, 0x82, 0x96, 0x65,
        0x56, 0x69, 0xc0, 0x4d, 0x58, 0x5b, 0x8a, 0x85, 0x39, 0x84, 0x29, 0x9b, 0xa5, 0x9b, 0xe4,
        0xc4, 0x79, 0x5b, 0x8b, 0xe1, 0x17, 0x25, 0x10, 0x22, 0x4, 0x5, 0x44, 0xe5, 0x68, 0x72,
        0x80, 0x88, 0xd5, 0x23, 0x22, 0x19, 0x8d, 0xca, 0xa5, 0x26, 0xc4, 0x73, 0xdb, 0x6, 0x96,
        0xb7, 0xe, 0x28, 0xc6, 0xa, 0xc3, 0x65, 0x5c, 0x9c, 0x3, 0xf3, 0x1d, 0xc9, 0x53, 0x34,
        0x6a, 0x85, 0x1, 0xe2, 0x3c, 0x91, 0x6d, 0x70, 0xe1, 0x4d, 0xa2, 0xa, 0x67, 0x50, 0xb0,
        0xe2, 0x12, 0x1f, 0xba, 0x68, 0xdc, 0xd, 0x35, 0x3b, 0x32, 0xa7, 0x2b, 0xe7, 0x91, 0x6d,
        0xb2, 0xe0, 0xf4, 0xb8, 0xb1, 0x6d, 0xab, 0xa6, 0x46, 0xd5, 0x4, 0x5a, 0x5d, 0xf1, 0x8f,
        0x2d, 0x52, 0x6a, 0xb8, 0x50, 0xf3, 0x22, 0x4e, 0xb1, 0x24, 0xa8, 0xa1, 0x15, 0x34, 0xbc,
        0x3f, 0xda, 0x8c, 0xc6, 0xc8, 0x53, 0x2b, 0xd0, 0x9f, 0xa8, 0x72, 0x3e, 0xc1, 0x6a, 0x3a,
        0x51, 0xb1, 0x99, 0x80, 0x1b, 0xae, 0x2d, 0x4c, 0x79, 0xa0, 0x10, 0x2b, 0x7, 0x4a, 0xa,
        0x65, 0x3a, 0x82, 0xe4, 0x1f, 0xbb, 0x9c, 0x6e, 0x20, 0xa5, 0x1b, 0x17, 0xdc, 0xa7, 0x6f,
        0x77, 0x22, 0xd, 0xb9, 0xc2, 0xf6, 0xa7, 0xe1, 0x8d, 0x88, 0x88, 0xdc, 0x44, 0x68, 0xbd,
        0x25, 0x42, 0x5f, 0x20, 0x1b, 0x84, 0x15, 0x56, 0x5, 0x95, 0x9c, 0x40, 0xef, 0xa1, 0x71,
        0xaa, 0xc7, 0x82, 0x8, 0x39, 0xf4, 0x58, 0xae, 0x39, 0x50, 0xac, 0xc7, 0x53, 0xff, 0x5,
        0xb0, 0x29, 0x9d, 0x54, 0x4f, 0x8d, 0x1a, 0x81, 0x61, 0xc2, 0x71, 0xc, 0x2f, 0xdb, 0x1b,
        0x1b, 0xa7, 0x4f, 0x1a, 0x4a, 0xa2, 0xa9, 0x8c, 0x2c, 0x1, 0xe7, 0xf9, 0xf, 0x85, 0xc1,
        0x33, 0xe7, 0x39, 0x8f, 0x43, 0x40, 0x30, 0x27, 0xeb, 0xad, 0x7e, 0xef, 0x22, 0xf8, 0xb5,
        0x51, 0xe5, 0xb3, 0x7c, 0x2a, 0x45, 0x88, 0x93, 0xac, 0xea, 0x6a, 0x51, 0x63, 0x79, 0x45,
        0x35, 0xfd, 0x9d, 0xd4, 0x55, 0x98, 0xd, 0xf4, 0x29, 0x7c, 0xfc, 0x93, 0x52, 0xa4, 0x61,
        0x6c, 0x1a, 0xcf, 0x5, 0x5a, 0x3e, 0x44, 0x82, 0x6c, 0x44, 0x7e, 0x6e, 0xb2, 0xad, 0x5a,
        0x3, 0x72, 0x2f, 0xed, 0x77, 0x44, 0x16, 0xd1, 0x59, 0xa8, 0x10, 0x2d, 0x8, 0x6c, 0xd6,
        0xb2, 0x38, 0x95, 0x4c, 0x37, 0x54, 0x2e, 0x8d, 0xdc, 0xd6, 0x34, 0xe5, 0xe2, 0x64, 0x9b,
        0x57, 0x26, 0x38, 0x28, 0xd, 0x46, 0x7e, 0xc3, 0x1, 0xcc, 0x36, 0x48, 0xe9, 0xd1, 0x9a,
        0x9f, 0x29, 0xa1, 0xac, 0x53, 0xdd, 0xf, 0x8a, 0x51, 0x5d, 0xe3, 0x18, 0x19, 0xcf, 0x93,
        0x82, 0x95, 0x5b, 0x69, 0x8e, 0xf, 0xab, 0x2, 0x17, 0xfa, 0xa7, 0x9, 0x35, 0xf2, 0x9, 0x39,
        0xe2, 0x5b, 0x36, 0x90, 0xa8, 0x46, 0x9c, 0xf3, 0x58, 0x29, 0x0, 0xb1, 0xb0, 0xdd, 0xdc,
        0x41, 0xf6, 0xa, 0x99, 0xe1, 0xff, 0x2b, 0xe8, 0x1d, 0x3c, 0x86, 0x8e, 0xff, 0x9f, 0xed,
        0x3e, 0x98, 0x5d, 0x24, 0xfc, 0x58, 0xd7, 0x13, 0x12, 0xa7, 0x74, 0x5e, 0x3e, 0x44, 0x68,
        0x7d, 0x11, 0x0, 0x44, 0xb1, 0x28, 0x4f, 0x85, 0x1e, 0x92, 0x5a, 0x3c, 0xc6, 0x77, 0x70,
        0x4, 0x43, 0x1c, 0x81, 0x41, 0x65, 0xd2, 0x33, 0x77, 0x91, 0xd1, 0xab, 0xe5, 0x97, 0x90,
        0x1f, 0x7b, 0xe6, 0xbb, 0xcc, 0xb3, 0x65, 0x61, 0x57, 0x6d, 0x60, 0xa6, 0x93, 0x79, 0x3d,
        0x70, 0x43, 0x92, 0x5, 0x4b, 0x2, 0x67, 0xea, 0x78, 0x8b, 0x12, 0xba, 0x85, 0x9c, 0x2b,
        0xda, 0x7b, 0xb, 0xed, 0x3c, 0xe8, 0xca, 0xa4, 0x64, 0xe4, 0x9b, 0x9c, 0xa8, 0x5c, 0x5c,
        0xe2, 0xa7, 0x82, 0xea, 0x4c, 0x79, 0x77, 0x4, 0xf1, 0x0, 0x5, 0xad, 0x2f, 0x72, 0x3d,
        0x95, 0xe5, 0x8, 0x50, 0x48, 0x2e, 0x80, 0x5d, 0x54, 0x67, 0xf9, 0x41, 0xf1, 0x1d, 0xb6,
        0x86, 0x6, 0x73, 0xa, 0xaf, 0x99, 0x7d, 0x2c, 0x30, 0xa6, 0xc9, 0xbc, 0x7d, 0x39, 0x16,
        0x3, 0x55, 0x85, 0x63, 0xe8, 0x69, 0x36, 0x2a, 0xc2, 0xba, 0x5b, 0xf, 0x49, 0x1d, 0x2,
        0xb4, 0xe1, 0x12, 0xf1, 0xe6, 0x9b, 0xaf, 0xd4, 0x78, 0xd9, 0xaf, 0x7b, 0x5f, 0x50, 0xa5,
        0x86, 0x32, 0xbc, 0x36, 0xe4, 0x96, 0x11, 0xef, 0xf8, 0xb4, 0xd4, 0x91, 0xf7, 0xd7, 0x43,
        0x15, 0x28, 0x3, 0x1e, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x20, 0x0, 0x5d, 0x35, 0x74,
        0xc4, 0xd9, 0x9c, 0x8, 0xef, 0x95, 0x6, 0x19, 0xbe, 0x72, 0xbf, 0xa1, 0xd5, 0xa, 0xe3,
        0xc1, 0x53, 0xd1, 0xf3, 0xf, 0x64, 0xbc, 0x1a, 0xc0, 0x8d, 0xe9, 0x9e, 0xa5, 0x56,
    ];

    static DECLARATIVE_VARIANT: [u8; 161] = [
        0x0, 0x4, 0x0, 0x2, 0x24, 0xa5, 0xd4, 0xe1, 0xd0, 0x55, 0xa0, 0xc, 0x15, 0xc6, 0x61, 0xcd,
        0x6d, 0x6a, 0x55, 0xb8, 0x51, 0xaf, 0xfd, 0x90, 0x98, 0x9, 0x6c, 0x3e, 0xf5, 0x31, 0xd4,
        0xb, 0xee, 0x1b, 0x3c, 0x6b, 0x0, 0x53, 0x20, 0xe, 0x17, 0xc, 0x8, 0xf2, 0x24, 0x2a, 0xd7,
        0xba, 0xa0, 0x22, 0x55, 0xb, 0x91, 0x8a, 0xd1, 0x4e, 0xe, 0xcc, 0x64, 0x12, 0x19, 0x71,
        0xe3, 0x7a, 0x19, 0x6b, 0xac, 0x43, 0xc8, 0x3, 0x0, 0xae, 0xe9, 0xa8, 0xc3, 0x4c, 0x5d,
        0x4f, 0x87, 0x6, 0xe, 0xf5, 0x8d, 0x94, 0xa, 0x75, 0xe4, 0x3d, 0x13, 0x9d, 0x55, 0xa5,
        0xe4, 0xd3, 0x26, 0x4d, 0xc9, 0xeb, 0x4f, 0x77, 0x3b, 0xff, 0xc5, 0x72, 0x90, 0x19, 0xe4,
        0x7e, 0xd2, 0x7e, 0xf5, 0x1, 0x0, 0x0, 0x0, 0x1, 0x0, 0xad, 0xb1, 0x39, 0x64, 0xb, 0xb6,
        0xb9, 0x36, 0x8e, 0xaf, 0xd3, 0x36, 0xd, 0x65, 0x25, 0x89, 0x52, 0xf4, 0xd9, 0x57, 0x5e,
        0xac, 0x1b, 0x1f, 0x18, 0xee, 0x18, 0x51, 0x29, 0x71, 0x82, 0x93, 0xb6, 0xd7, 0x62, 0x2b,
        0x1e, 0xdd, 0x1f, 0x20, 0x1, 0x0, 0x0, 0x0,
    ];

    // Real data used for creation of above variants
    // Used in tests to ensure operations of AssignmentVariants gives deterministic results

    // Txids to generate seals
    static TXID_VEC: [&str; 4] = [
        "201fdd1e2b62d7b6938271295118ee181f1bac5e57d9f4528925650d36d3af8e",
        "f57ed27ee4199072c5ff3b774febc94d26d3e4a5559d133de4750a948df50e06",
        "12072893d951c633dcafb4d3074d1fc41c5e6e64b8d53e3b0705c41bc6679d54",
        "8f75db9f89c7c75f0a54322f18cd4d557ae75c24a8e5a95eae13fe26edc2d789",
    ];

    // State data used in CustomData type Assignments
    static STATE_DATA: [&str; 4] = [
        "e70a36e2ce51d31d4cf5d6731fa63738648127db833715d39652d86d927d4888",
        "408e331ebce96ca98cfb7b8a6286a79300379eed6395636e6d103017d474039f",
        "c6411aea082e2c5d74347368677db69545126237d5ed78faa0846352f5383f95",
        "277fb00655e2523424677686c24d90fba6b70869050ae204782e8ef0ab8049c6",
    ];

    // Confidential seals for Declarative Assignments
    static DECLARATIVE_OUTPOINT_HASH: [&str; 4] = [
        "58f3ea4817a12aa6f1007d5b3d24dd2940ce40f8498029e05f1dc6465b3d65b4",
        "6b3c1bee0bd431f53e6c099890fdaf51b8556a6dcd61c6150ca055d0e1d4a524",
        "9a17566abc006cf335fd96d8f8a4136526d85493a85ebe875abbbee19795c496",
        "c843ac6b197ae371191264cc0e4ed18a910b5522a0bad72a24f2080c170e2053",
    ];

    // Confidential seals for Pedersan type Assignments
    static PEDERSAN_OUTPOINT_HASH: [&str; 4] = [
        "281543d7f791d4b4f8ef1196e436bc3286a5505f7bafd978d4af9be6f112e1b4",
        "32d71a47d8ff6015fc58525985af7346e0802c7ad065ad79335602c7a6562ab3",
        "68955a27e1ffde810fcfdd18697eb59aa4f7b0afde2a8193cd28184b729b5195",
        "698c43d973bec68540e6df67137785e40be6d29def4888ada3cd7b7884b37f62",
    ];

    // Confidential seals for CustomData type Assignments
    static HASH_OUTPOINT_HASH: [&str; 4] = [
        "7efe71b7a37a39da798774ca6b09def9724d81303892d55cac3edb0dc8340a3a",
        "9565d29461c863e013c26d176a9929307286963322849a1dc6c978e5c70c8d52",
        "9b64a3024632f0517d8a608cb29902f7083eab0ac25d2827a5ef27e9a68b18f9",
        "dc0d0d7139a3ad6010a210e5900201979a1a09047b10a877688ee5a740ae215a",
    ];

    // Generic encode-decode testing
    #[test]
    fn test_encoded_data() {
        test_encode!((HASH_VARIANT, AssignmentsVariant));
        test_encode!((PEDERSAN_VARIANT, AssignmentsVariant));
        test_encode!((DECLARATIVE_VARIANT, AssignmentsVariant));
    }

    // Generic garbage value testing
    #[test]
    #[should_panic(expected = "EnumValueNotKnown")]
    fn test_garbage_dec() {
        test_garbage!((DECLARATIVE_VARIANT, AssignmentsVariant));
    }

    #[test]
    #[should_panic(expected = "EnumValueNotKnown")]
    fn test_garbage_ped() {
        test_garbage!((PEDERSAN_VARIANT, AssignmentsVariant));
    }

    #[test]
    #[should_panic(expected = "EnumValueNotKnown")]
    fn test_garbage_hash() {
        test_garbage!((HASH_VARIANT, AssignmentsVariant));
    }

    #[test]
    #[should_panic(expected = "UnsupportedDataStructure")]
    fn test_garbage_ped_2() {
        let mut bytes = PEDERSAN_VARIANT.clone();
        bytes[1] = 0x02;

        AssignmentsVariant::strict_decode(&bytes[..]).unwrap();
    }

    fn compute_zero_balance(
        input_amounts: &[u64],
        output_amounts: &[u64],
        partition: usize,
    ) -> bool {
        let mut rng = thread_rng();

        // Create revealed amount from input amounts
        let input_revealed: Vec<amount::Revealed> = input_amounts[..]
            .into_iter()
            .map(|amount| amount::Revealed::with_amount(*amount, &mut rng))
            .collect();

        // Allocate Txid vector of size of the output vector
        let mut txid_vec: Vec<bitcoin::Txid> = Vec::with_capacity(output_amounts.len());

        // Fill the txid vector with random txids.
        for _ in 0..output_amounts.len() {
            let mut bytes: [u8; 32] = [0; 32];
            rng.fill(&mut bytes[..]);
            let txid = bitcoin::Txid::from_hex(&bytes.to_vec().to_hex()[..]).unwrap();
            txid_vec.push(txid);
        }

        // Take first two amounts to create our allocations
        let zip_data = txid_vec[..partition]
            .iter()
            .zip(output_amounts[..partition].iter());

        // Create our allocations
        let ours: Vec<(SealDefinition, Amount)> = zip_data
            .map(|(txid, amount)| {
                (
                    Revealed::TxOutpoint(OutpointReveal::from(OutPoint::new(
                        *txid,
                        rng.gen_range(0, 10),
                    ))),
                    amount.clone(),
                )
            })
            .collect();

        // Take next two amounts for their allocations
        let zip_data2 = txid_vec[partition..]
            .iter()
            .zip(output_amounts[partition..].iter());

        // Create their allocations
        let theirs: Vec<(OutpointHash, Amount)> = zip_data2
            .map(|(txid, amount)| {
                (
                    Revealed::TxOutpoint(OutpointReveal::from(OutPoint::new(
                        *txid,
                        rng.gen_range(0, 10),
                    )))
                    .conceal(),
                    amount.clone(),
                )
            })
            .collect();

        // Balance both the allocations against input amounts
        let balanced = AssignmentsVariant::zero_balanced(input_revealed.clone(), ours, theirs);

        // Extract balanced confidential output amounts
        let outputs: Vec<Commitment> = balanced
            .all_state_pedersen()
            .iter()
            .map(|confidential| confidential.commitment)
            .collect();

        // Create confidential input amounts
        let inputs: Vec<Commitment> = input_revealed
            .iter()
            .map(|revealed| revealed.conceal().commitment)
            .collect();

        // Check sum verification and return the result
        amount::Confidential::verify_commit_sum(inputs, outputs)
    }

    #[test]
    #[allow(arithmetic_overflow)]
    fn test_zero_balance() {
        let mut rng = thread_rng();

        // test equal inputs and outputs
        let single_amounts = vec![
            [0u64],
            [1u64],
            [u16::MAX as u64],
            [u32::MAX as u64],
            [u64::MAX - 1u64],
            [u64::MAX],
        ];
        let double_amounts = vec![
            [(u32::MAX - 1) as u64, (u32::MAX - 1) as u64],
            [u32::MAX as u64, u32::MAX as u64],
        ];
        let multiple_amounts = vec![
            [0u64, 0u64, 0u64, 0u64],
            [0u64, 1u64, 0u64, 1u64],
            [1u64, 2u64, 3u64, u64::MAX],
            [10u64, 20u64, 30u64, 40u64],
            [0u64, 197642u64, u64::MAX, 476543u64],
            [u64::MAX, u64::MAX, u64::MAX, u64::MAX],
        ];

        for vec in single_amounts.iter() {
            assert!(compute_zero_balance(vec, vec, 0));
            assert!(compute_zero_balance(vec, vec, 1));
            assert!(compute_zero_balance(vec, vec, vec.len() / 2));
            assert!(compute_zero_balance(vec, vec, vec.len() / 2 + 1));
        }

        for vec in double_amounts.iter() {
            assert!(compute_zero_balance(vec, vec, 0));
            assert!(compute_zero_balance(vec, vec, 1));
            assert!(compute_zero_balance(vec, vec, vec.len() / 2));
            assert!(compute_zero_balance(vec, vec, vec.len() / 2 + 1));
        }

        for vec in multiple_amounts.iter() {
            assert!(compute_zero_balance(vec, vec, 0));
            assert!(compute_zero_balance(vec, vec, 1));
            assert!(compute_zero_balance(vec, vec, vec.len() / 2));
            assert!(compute_zero_balance(vec, vec, vec.len() / 2 + 1));
        }

        // Test when ours is empty
        assert!(compute_zero_balance(
            &multiple_amounts[2],
            &multiple_amounts[2],
            0
        ));

        // Test when theirs is empty
        assert!(compute_zero_balance(
            &multiple_amounts[4],
            &multiple_amounts[4],
            multiple_amounts[4].len()
        ));

        // Test when input.sum() != output.sum()
        // When they only differ by 1
        // When they differ by u64::MAX
        assert!(!compute_zero_balance(
            &multiple_amounts[1],
            &multiple_amounts[2],
            2
        ));
        assert!(!compute_zero_balance(
            &multiple_amounts[2],
            &multiple_amounts[3],
            2
        ));
        assert!(!compute_zero_balance(
            &multiple_amounts[3],
            &multiple_amounts[4],
            2
        ));
        assert!(!compute_zero_balance(
            &multiple_amounts[4],
            &multiple_amounts[5],
            2
        ));
        assert!(!compute_zero_balance(&[1, 2, 3, 4], &[1, 2, 3, 5], 2));
        assert!(!compute_zero_balance(
            &[1, 2, 3, 0],
            &[1, 2, 3, u64::MAX],
            2
        ));

        // Test random inputs and outputs
        // Randomly distributed between ours and theirs allocation
        for _ in 0..5 {
            // Randomly generate number of amounts between 1 to 20
            let input_length = rng.gen_range(1, 20);

            // Randomly fill the amount vector
            let mut input_amounts = vec![0; input_length];
            for index in 0..input_length {
                // keep the amount value low for faster testing
                input_amounts[index] = rng.gen_range::<u64>(100_000, 100_000_000_000);
            }
            let input_sum: u64 = input_amounts.iter().sum();

            // Create an output amount vector such that
            // input.sum() = output.sum(), but
            // input.count() != output.count()

            let mut output_amounts = vec![0u64; rng.gen_range(1, 20)];
            let output_length = output_amounts.len();

            // Add random values to output amounts until the last element
            for index in 0..output_length - 1 {
                output_amounts[index] = rng.gen_range::<u64>(100_000, 100_000_000_000);
            }
            let output_sum: u64 = output_amounts.iter().sum();

            // Balance input and output amount vector based on their sums
            if output_sum > input_sum {
                input_amounts[input_length - 1] += output_sum - input_sum;
            } else {
                output_amounts[output_length - 1] += input_sum - output_sum;
            }

            // Check if test passes
            assert!(compute_zero_balance(
                &input_amounts[..],
                &output_amounts[..],
                rng.gen_range(0, output_length)
            ));

            //Check non-equivalent amounts do not verify
            assert!(!compute_zero_balance(
                &input_amounts[..(input_length - 1)],
                &output_amounts[..(output_length - 1)],
                rng.gen_range(0, output_length)
            ));
        }

        // Test Overflow conditions
        assert!(compute_zero_balance(&[u64::MAX + 1], &[0], 1));
        assert!(compute_zero_balance(
            &[Wrapping::<u64>(u64::MAX + u64::MAX).0],
            &[Wrapping::<u64>(u64::MAX - 1).0],
            1
        ));
        assert!(compute_zero_balance(
            &[Wrapping::<u64>((u32::MAX * u32::MAX - 1) as u64).0],
            &[0u64],
            1
        ));
        assert!(compute_zero_balance(
            &[Wrapping::<u64>((u32::MAX * u32::MAX) as u64).0],
            &[1u64],
            1
        ));

        assert!(compute_zero_balance(
            &[Wrapping::<u64>((u64::MAX * u64::MAX + 1) as u64).0],
            &[2u64],
            1
        ));
    }

    #[test]
    fn test_identification() {
        let declarative_type = AssignmentsVariant::strict_decode(&DECLARATIVE_VARIANT[..]).unwrap();
        let pedersan_type = AssignmentsVariant::strict_decode(&PEDERSAN_VARIANT[..]).unwrap();
        let hash_type = AssignmentsVariant::strict_decode(&HASH_VARIANT[..]).unwrap();

        // Check correct types are being identified
        // and wrong types return false
        assert!(declarative_type.is_declarative());
        assert!(pedersan_type.is_field());
        assert!(hash_type.is_data());
        assert!(!declarative_type.is_data());
        assert!(!declarative_type.is_field());
        assert!(!pedersan_type.is_declarative());
        assert!(!pedersan_type.is_data());
        assert!(!hash_type.is_declarative());
        assert!(!hash_type.is_field());
    }

    #[test]
    fn test_extraction() {
        let mut declarative_type =
            AssignmentsVariant::strict_decode(&DECLARATIVE_VARIANT[..]).unwrap();
        let mut pedersan_type = AssignmentsVariant::strict_decode(&PEDERSAN_VARIANT[..]).unwrap();
        let mut hash_type = AssignmentsVariant::strict_decode(&HASH_VARIANT[..]).unwrap();

        // Check Correct type extraction works
        assert!(declarative_type.declarative().is_some());
        assert!(pedersan_type.field().is_some());
        assert!(hash_type.data().is_some());

        // Check wrong type extraction doesn't work
        assert!(declarative_type.field().is_none());
        assert!(declarative_type.data().is_none());
        assert!(pedersan_type.declarative().is_none());
        assert!(pedersan_type.data().is_none());
        assert!(hash_type.declarative().is_none());
        assert!(hash_type.field().is_none());

        // Check correct mutable type extraction works
        assert!(declarative_type.declarative_mut().is_some());
        assert!(pedersan_type.field_mut().is_some());
        assert!(hash_type.data_mut().is_some());

        // Check wrong mutable type extraction doesn't work
        assert!(declarative_type.field_mut().is_none());
        assert!(declarative_type.data_mut().is_none());
        assert!(pedersan_type.declarative_mut().is_none());
        assert!(pedersan_type.data_mut().is_none());
        assert!(hash_type.declarative_mut().is_none());
        assert!(hash_type.field_mut().is_none());
    }

    #[test]
    fn test_seal_extraction() {
        let declarative_type = AssignmentsVariant::strict_decode(&DECLARATIVE_VARIANT[..]).unwrap();
        let pedersan_type = AssignmentsVariant::strict_decode(&PEDERSAN_VARIANT[..]).unwrap();
        let hash_type = AssignmentsVariant::strict_decode(&HASH_VARIANT[..]).unwrap();

        // Extract a specific Txid from each variants
        let txid_1 = match declarative_type.seal(2).unwrap().unwrap() {
            Revealed::TxOutpoint(outpoint) => Some(outpoint.txid),
            _ => None,
        }
        .unwrap()
        .to_hex();

        let txid_2 = match pedersan_type.seal(0).unwrap().unwrap() {
            Revealed::TxOutpoint(outpoint) => Some(outpoint.txid),
            _ => None,
        }
        .unwrap()
        .to_hex();

        let txid_3 = match hash_type.seal(1).unwrap().unwrap() {
            Revealed::TxOutpoint(outpoint) => Some(outpoint.txid),
            _ => None,
        }
        .unwrap()
        .to_hex();

        // Check extracted Txids matches with predetermined values
        assert_eq!(txid_1, TXID_VEC[1]);
        assert_eq!(txid_2, TXID_VEC[1]);
        assert_eq!(txid_3, TXID_VEC[1]);
    }

    #[test]
    fn test_known_seals() {
        let declarative_type = AssignmentsVariant::strict_decode(&DECLARATIVE_VARIANT[..]).unwrap();
        let pedersan_type = AssignmentsVariant::strict_decode(&PEDERSAN_VARIANT[..]).unwrap();
        let hash_type = AssignmentsVariant::strict_decode(&HASH_VARIANT[..]).unwrap();

        // Extract known Txids from each variants
        let mut dec_txids: Vec<String> = declarative_type
            .known_seals()
            .iter()
            .map(|revealed| {
                match revealed {
                    Revealed::TxOutpoint(outpoint) => Some(outpoint.txid),
                    _ => None,
                }
                .unwrap()
                .to_hex()
            })
            .collect();

        let mut ped_txids: Vec<String> = pedersan_type
            .known_seals()
            .iter()
            .map(|revealed| {
                match revealed {
                    Revealed::TxOutpoint(outpoint) => Some(outpoint.txid),
                    _ => None,
                }
                .unwrap()
                .to_hex()
            })
            .collect();

        let mut hash_txids: Vec<String> = hash_type
            .known_seals()
            .iter()
            .map(|revealed| {
                match revealed {
                    Revealed::TxOutpoint(outpoint) => Some(outpoint.txid),
                    _ => None,
                }
                .unwrap()
                .to_hex()
            })
            .collect();

        // Sort the extracted Txids
        dec_txids.sort();
        ped_txids.sort();
        hash_txids.sort();

        // Predetermined values
        let mut sorted_txid = TXID_VEC[..2].to_vec().clone();
        sorted_txid.sort();

        // Check extracted values matches with predetermined values
        assert_eq!(dec_txids, sorted_txid);
        assert_eq!(ped_txids, sorted_txid);
        assert_eq!(hash_txids, sorted_txid);
    }

    #[test]
    fn test_all_seals() {
        let declarative_type = AssignmentsVariant::strict_decode(&DECLARATIVE_VARIANT[..]).unwrap();
        let pedersan_type = AssignmentsVariant::strict_decode(&PEDERSAN_VARIANT[..]).unwrap();
        let hash_type = AssignmentsVariant::strict_decode(&HASH_VARIANT[..]).unwrap();

        // Extract seals from all variants and conceal them
        let mut dec_hashes: Vec<String> = declarative_type
            .all_seals()
            .iter()
            .map(|hash| hash.to_hex())
            .collect();

        let mut ped_hashes: Vec<String> = pedersan_type
            .all_seals()
            .iter()
            .map(|hash| hash.to_hex())
            .collect();

        let mut hash_hashes: Vec<String> = hash_type
            .all_seals()
            .iter()
            .map(|hash| hash.to_hex())
            .collect();

        // Sort the concealed seals
        dec_hashes.sort();
        ped_hashes.sort();
        hash_hashes.sort();

        // Check extracted values matches with precomputed values
        assert_eq!(dec_hashes, DECLARATIVE_OUTPOINT_HASH);
        assert_eq!(ped_hashes, PEDERSAN_OUTPOINT_HASH);
        assert_eq!(hash_hashes, HASH_OUTPOINT_HASH);
    }

    #[test]
    fn test_known_state_homomorphic() {
        let declarative_type = AssignmentsVariant::strict_decode(&DECLARATIVE_VARIANT[..]).unwrap();
        let pedersan_type = AssignmentsVariant::strict_decode(&PEDERSAN_VARIANT[..]).unwrap();
        let hash_type = AssignmentsVariant::strict_decode(&HASH_VARIANT[..]).unwrap();

        // Extract known states from pedersan type variant
        let states = pedersan_type.known_state_homomorphic();

        // Check the amounts matches with precomputed values
        assert_eq!(states[0].amount, 10);
        assert_eq!(states[1].amount, 30);

        // Precomputed blinding factors
        let blind_1: Vec<u8> =
            Vec::from_hex("debbbefd1683e335296a0c86f1c882a2ea3759f114220b0b2cf869e37dec24c8")
                .unwrap();
        let blind_2: Vec<u8> =
            Vec::from_hex("5d3574c4d99c08ef950619be72bfa1d50ae3c153d1f30f64bc1ac08de99ea556")
                .unwrap();

        // Check blinding factor matches with precomputed values
        assert_eq!(
            states[0].blinding,
            SecretKey::from_slice(&Secp256k1::new(), &blind_1[..]).unwrap()
        );
        assert_eq!(
            states[1].blinding,
            SecretKey::from_slice(&Secp256k1::new(), &blind_2[..]).unwrap()
        );

        // Check no values returned for declarative and custom data type variants
        assert_eq!(declarative_type.known_state_homomorphic().len(), 0);
        assert_eq!(hash_type.known_state_homomorphic().len(), 0);
    }

    #[test]
    fn test_known_state_data() {
        let declarative_type = AssignmentsVariant::strict_decode(&DECLARATIVE_VARIANT[..]).unwrap();
        let pedersan_type = AssignmentsVariant::strict_decode(&PEDERSAN_VARIANT[..]).unwrap();
        let hash_type = AssignmentsVariant::strict_decode(&HASH_VARIANT[..]).unwrap();

        // Extract known states from custom data type variant
        let data_set = hash_type.known_state_data();

        // Create state data from precomputed values
        let data_1 = data::Revealed::Sha256(sha256::Hash::from_hex(STATE_DATA[2]).unwrap());
        let data_2 = data::Revealed::Sha256(sha256::Hash::from_hex(STATE_DATA[0]).unwrap());

        // Check extracted data matches with precomputed values
        assert_eq!(data_set[0].to_owned(), data_1);
        assert_eq!(data_set[1].to_owned(), data_2);

        // Check no values returned for declarative and pedersan type variants
        assert_eq!(declarative_type.known_state_data().len(), 0);
        assert_eq!(pedersan_type.known_state_data().len(), 0);
    }

    #[test]
    fn test_all_state_pedersan() {
        let declarative_type = AssignmentsVariant::strict_decode(&DECLARATIVE_VARIANT[..]).unwrap();
        let pedersan_type = AssignmentsVariant::strict_decode(&PEDERSAN_VARIANT[..]).unwrap();
        let hash_type = AssignmentsVariant::strict_decode(&HASH_VARIANT[..]).unwrap();

        // Extract state data for pedersan type and conceal them
        let conf_amounts = pedersan_type.all_state_pedersen();

        // Check extracted values matches with precomputed values
        assert_eq!(
            conf_amounts[0].commitment,
            Commitment::from_vec(
                Vec::from_hex("08cc48fa5e5cb1d2d2465bd8c437c0e00514abd813f9a7dd506a778405a2c43bc0")
                    .unwrap()
            )
        );
        assert_eq!(
            conf_amounts[1].commitment,
            Commitment::from_vec(
                Vec::from_hex("091e1b9e7605fc214806f3af3eba13947b91f47bac729f5def5e8fbd530112bed1")
                    .unwrap()
            )
        );
        assert_eq!(
            conf_amounts[2].commitment,
            Commitment::from_vec(
                Vec::from_hex("089775f829c8adad92ada17b5931edf63064d54678f4eb9a6fdfe8e4cb5d95f6f4")
                    .unwrap()
            )
        );

        // Check no values returned for declarative and hash type
        assert_eq!(declarative_type.all_state_pedersen().len(), 0);
        assert_eq!(hash_type.all_state_pedersen().len(), 0);
    }

    #[test]
    fn test_all_state_hashed() {
        let declarative_type = AssignmentsVariant::strict_decode(&DECLARATIVE_VARIANT[..]).unwrap();
        let pedersan_type = AssignmentsVariant::strict_decode(&PEDERSAN_VARIANT[..]).unwrap();
        let hash_type = AssignmentsVariant::strict_decode(&HASH_VARIANT[..]).unwrap();

        // Extract state data from hash type variant and conceal them
        let extracted_states = hash_type.all_state_hashed();

        // Precomputed concealed state data
        let expected: [&str; 4] = [
            "fa6eae3e74de3e5dd92f58ac753d02b613daaaab",
            "6420cc421e1189805c8cec089d74c1980f79c069",
            "43e446006c5bc93864dafb03cf4ba472bedf5ca7",
            "69b01b4d96d00ceff2599eb089e4c7b979961fec",
        ];

        // Check extracted values matches with precomputed values
        assert_eq!(
            extracted_states
                .iter()
                .map(|hash| hash.to_hex())
                .collect::<Vec<String>>(),
            expected
        );

        // Check no values returned for declarative and pedersan types
        assert_eq!(declarative_type.all_state_hashed().len(), 0);
        assert_eq!(pedersan_type.all_state_hashed().len(), 0);
    }

    #[test]
    fn test_conceal() {
        // Only hash type is considered for concealment operations because
        // Declarative type has void state data
        // Pedersan type has very large concealed state data which slows down the test
        let mut hash_type = AssignmentsVariant::strict_decode(&HASH_VARIANT[..]).unwrap();

        // Conceal all without any exception
        // This will create 2 Confidential and 2 ConfidentialState type Assignments
        hash_type.conceal_except(&Vec::<OutpointHash>::new());

        // Precomputed values of revealed seals in 2 ConfidentialState type Assignments
        let known_txid: [&str; 2] = [
            "f57ed27ee4199072c5ff3b774febc94d26d3e4a5559d133de4750a948df50e06",
            "201fdd1e2b62d7b6938271295118ee181f1bac5e57d9f4528925650d36d3af8e",
        ];

        // Extracted seal values
        let extracted_txid: Vec<String> = hash_type
            .known_seals()
            .iter()
            .map(|revealed| {
                match revealed {
                    Revealed::TxOutpoint(outpoint) => Some(outpoint.txid),
                    _ => None,
                }
                .unwrap()
                .to_hex()
            })
            .collect();

        // Check extracted values matches with precomputed values
        assert_eq!(known_txid.to_vec(), extracted_txid);

        // Precomputed of concealed seals of all 4 assignments
        let all_seals_confidential: [&str; 4] = [
            "7efe71b7a37a39da798774ca6b09def9724d81303892d55cac3edb0dc8340a3a",
            "9565d29461c863e013c26d176a9929307286963322849a1dc6c978e5c70c8d52",
            "dc0d0d7139a3ad6010a210e5900201979a1a09047b10a877688ee5a740ae215a",
            "9b64a3024632f0517d8a608cb29902f7083eab0ac25d2827a5ef27e9a68b18f9",
        ];

        // Extract concealed seals
        let extracted_seals_confidential: Vec<String> = hash_type
            .all_seals()
            .iter()
            .map(|hash| hash.to_hex())
            .collect();

        // Check extracted values matches with precomputed values
        assert_eq!(
            all_seals_confidential.to_vec(),
            extracted_seals_confidential
        );

        // Precomputed concealed state data of all 4 assignments
        let all_state_confidential = [
            "fa6eae3e74de3e5dd92f58ac753d02b613daaaab",
            "6420cc421e1189805c8cec089d74c1980f79c069",
            "43e446006c5bc93864dafb03cf4ba472bedf5ca7",
            "69b01b4d96d00ceff2599eb089e4c7b979961fec",
        ];

        // Extract concealed state data
        let extracted_state_confidential: Vec<String> = hash_type
            .all_state_hashed()
            .iter()
            .map(|confidential| confidential.to_hex())
            .collect();

        // Check extracted values matches with precomputed values
        assert_eq!(
            all_state_confidential.to_vec(),
            extracted_state_confidential
        );
    }

    #[test]
    fn test_len() {
        let declarative_type = AssignmentsVariant::strict_decode(&DECLARATIVE_VARIANT[..]).unwrap();
        let pedersan_type = AssignmentsVariant::strict_decode(&PEDERSAN_VARIANT[..]).unwrap();
        let hash_type = AssignmentsVariant::strict_decode(&HASH_VARIANT[..]).unwrap();

        // All variants have 4 assignments in them
        assert_eq!(declarative_type.len(), 4);
        assert_eq!(pedersan_type.len(), 4);
        assert_eq!(hash_type.len(), 4);
    }
}
