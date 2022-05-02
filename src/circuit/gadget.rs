//! Gadgets used in the Orchard circuit.

use pasta_curves::pallas;

use super::commit_ivk::CommitIvkChip;
use crate::constants::{
    NullifierK, OrchardCommitDomains, OrchardFixedBases, OrchardFixedBasesFull, OrchardHashDomains,
    ValueCommitV,
};
use halo2_gadgets::{
    ecc::{
        chip::EccChip, EccInstructions, FixedPoint, FixedPointBaseField, FixedPointShort, Point, X,
    },
    poseidon::{Hash as PoseidonHash, PoseidonSpongeInstructions, Pow5Chip as PoseidonChip},
    primitives::poseidon::{self, ConstantLength},
    sinsemilla::{chip::SinsemillaChip, merkle::chip::MerkleChip},
};
use halo2_proofs::{
    arithmetic::FieldExt,
    circuit::{AssignedCell, Chip, Layouter},
    plonk,
};

pub(in crate::circuit) mod add_chip;

impl super::Config {
    pub(super) fn add_chip(&self) -> add_chip::AddChip {
        add_chip::AddChip::construct(self.add_config.clone())
    }

    pub(super) fn commit_ivk_chip(&self) -> CommitIvkChip {
        CommitIvkChip::construct(self.commit_ivk_config.clone())
    }

    pub(super) fn ecc_chip(&self) -> EccChip<OrchardFixedBases> {
        EccChip::construct(self.ecc_config.clone())
    }

    pub(super) fn sinsemilla_chip_1(
        &self,
    ) -> SinsemillaChip<OrchardHashDomains, OrchardCommitDomains, OrchardFixedBases> {
        SinsemillaChip::construct(self.sinsemilla_config_1.clone())
    }

    pub(super) fn sinsemilla_chip_2(
        &self,
    ) -> SinsemillaChip<OrchardHashDomains, OrchardCommitDomains, OrchardFixedBases> {
        SinsemillaChip::construct(self.sinsemilla_config_2.clone())
    }

    pub(super) fn merkle_chip_1(
        &self,
    ) -> MerkleChip<OrchardHashDomains, OrchardCommitDomains, OrchardFixedBases> {
        MerkleChip::construct(self.merkle_config_1.clone())
    }

    pub(super) fn merkle_chip_2(
        &self,
    ) -> MerkleChip<OrchardHashDomains, OrchardCommitDomains, OrchardFixedBases> {
        MerkleChip::construct(self.merkle_config_2.clone())
    }

    pub(super) fn poseidon_chip(&self) -> PoseidonChip<pallas::Base, 3, 2> {
        PoseidonChip::construct(self.poseidon_config.clone())
    }
}

/// An instruction set for adding two circuit words (field elements).
pub(in crate::circuit) trait AddInstruction<F: FieldExt>: Chip<F> {
    /// Constraints `a + b` and returns the sum.
    fn add(
        &self,
        layouter: impl Layouter<F>,
        a: &AssignedCell<F, F>,
        b: &AssignedCell<F, F>,
    ) -> Result<AssignedCell<F, F>, plonk::Error>;
}

/// `ValueCommit^Orchard` from [Section 5.4.8.3 Homomorphic Pedersen commitments (Sapling and Orchard)].
///
/// [Section 5.4.8.3 Homomorphic Pedersen commitments (Sapling and Orchard)]: https://zips.z.cash/protocol/protocol.pdf#concretehomomorphiccommit
pub(in crate::circuit) fn value_commit_orchard<
    EccChip: EccInstructions<
        pallas::Affine,
        FixedPoints = OrchardFixedBases,
        Var = AssignedCell<pallas::Base, pallas::Base>,
    >,
>(
    mut layouter: impl Layouter<pallas::Base>,
    ecc_chip: EccChip,
    v: (
        AssignedCell<pallas::Base, pallas::Base>,
        AssignedCell<pallas::Base, pallas::Base>,
    ),
    rcv: Option<pallas::Scalar>,
) -> Result<Point<pallas::Affine, EccChip>, plonk::Error> {
    // commitment = [v] ValueCommitV
    let (commitment, _) = {
        let value_commit_v = ValueCommitV;
        let value_commit_v = FixedPointShort::from_inner(ecc_chip.clone(), value_commit_v);
        value_commit_v.mul(layouter.namespace(|| "[v] ValueCommitV"), v)?
    };

    // blind = [rcv] ValueCommitR
    let (blind, _rcv) = {
        let value_commit_r = OrchardFixedBasesFull::ValueCommitR;
        let value_commit_r = FixedPoint::from_inner(ecc_chip, value_commit_r);

        // [rcv] ValueCommitR
        value_commit_r.mul(layouter.namespace(|| "[rcv] ValueCommitR"), rcv)?
    };

    // [v] ValueCommitV + [rcv] ValueCommitR
    commitment.add(layouter.namespace(|| "cv"), &blind)
}

/// `DeriveNullifier` from [Section 4.16: Note Commitments and Nullifiers].
///
/// [Section 4.16: Note Commitments and Nullifiers]: https://zips.z.cash/protocol/protocol.pdf#commitmentsandnullifiers
#[allow(clippy::too_many_arguments)]
pub(in crate::circuit) fn derive_nullifier<
    PoseidonChip: PoseidonSpongeInstructions<pallas::Base, poseidon::P128Pow5T3, ConstantLength<2>, 3, 2>,
    AddChip: AddInstruction<pallas::Base>,
    EccChip: EccInstructions<
        pallas::Affine,
        FixedPoints = OrchardFixedBases,
        Var = AssignedCell<pallas::Base, pallas::Base>,
    >,
>(
    mut layouter: impl Layouter<pallas::Base>,
    poseidon_chip: PoseidonChip,
    add_chip: AddChip,
    ecc_chip: EccChip,
    rho: AssignedCell<pallas::Base, pallas::Base>,
    psi: &AssignedCell<pallas::Base, pallas::Base>,
    cm: &Point<pallas::Affine, EccChip>,
    nk: AssignedCell<pallas::Base, pallas::Base>,
) -> Result<X<pallas::Affine, EccChip>, plonk::Error> {
    // hash = poseidon_hash(nk, rho)
    let hash = {
        let poseidon_message = [nk, rho];
        let poseidon_hasher =
            PoseidonHash::init(poseidon_chip, layouter.namespace(|| "Poseidon init"))?;
        poseidon_hasher.hash(
            layouter.namespace(|| "Poseidon hash (nk, rho)"),
            poseidon_message,
        )?
    };

    // Add hash output to psi.
    // `scalar` = poseidon_hash(nk, rho) + psi.
    let scalar = add_chip.add(
        layouter.namespace(|| "scalar = poseidon_hash(nk, rho) + psi"),
        &hash,
        psi,
    )?;

    // Multiply scalar by NullifierK
    // `product` = [poseidon_hash(nk, rho) + psi] NullifierK.
    //
    let product = {
        let nullifier_k = FixedPointBaseField::from_inner(ecc_chip, NullifierK);
        nullifier_k.mul(
            layouter.namespace(|| "[poseidon_output + psi] NullifierK"),
            scalar,
        )?
    };

    // Add cm to multiplied fixed base to get nf
    // cm + [poseidon_output + psi] NullifierK
    cm.add(layouter.namespace(|| "nf"), &product)
        .map(|res| res.extract_p())
}

pub(in crate::circuit) use crate::circuit::commit_ivk::gadgets::commit_ivk;
