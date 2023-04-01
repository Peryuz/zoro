use bazuka::core::{Money, MpnWithdraw};
use bazuka::crypto::jubjub;
use bazuka::zk::{MpnAccount, ZkScalar};
use bellman::gadgets::boolean::{AllocatedBit, Boolean};
use bellman::gadgets::num::AllocatedNum;
use bellman::{Circuit, ConstraintSystem, SynthesisError};
use zeekit::common::Number;
use zeekit::common::UnsignedInteger;
use zeekit::eddsa;
use zeekit::eddsa::AllocatedPoint;
use zeekit::merkle;
use zeekit::reveal::{reveal, AllocatedState};
use zeekit::{common, poseidon, BellmanFr};

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Withdraw {
    pub mpn_withdraw: Option<MpnWithdraw>,
    pub index: u64,
    pub token_index: u64,
    pub fee_token_index: u64,
    pub pub_key: jubjub::PointAffine,
    pub fingerprint: ZkScalar,
    pub nonce: u32,
    pub sig: jubjub::Signature,
    pub amount: Money,
    pub fee: Money,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct WithdrawTransition<const LOG4_TREE_SIZE: u8, const LOG4_TOKENS_TREE_SIZE: u8> {
    pub enabled: bool,
    pub tx: Withdraw,
    pub before: MpnAccount,
    pub before_token_balance: Money,
    pub before_fee_balance: Money,
    pub proof: merkle::Proof<LOG4_TREE_SIZE>,
    pub token_balance_proof: merkle::Proof<LOG4_TOKENS_TREE_SIZE>,
    pub before_token_hash: ZkScalar,
    pub fee_balance_proof: merkle::Proof<LOG4_TOKENS_TREE_SIZE>,
}

impl<const LOG4_TREE_SIZE: u8, const LOG4_TOKENS_TREE_SIZE: u8>
    WithdrawTransition<LOG4_TREE_SIZE, LOG4_TOKENS_TREE_SIZE>
{
    pub fn from_bazuka(trans: bazuka::mpn::WithdrawTransition) -> Self {
        Self {
            enabled: true,
            tx: Withdraw {
                mpn_withdraw: Some(trans.tx.clone()),
                index: trans.tx.zk_address_index(LOG4_TREE_SIZE),
                token_index: trans.tx.zk_token_index,
                fee_token_index: trans.tx.zk_fee_token_index,
                pub_key: trans.tx.zk_address.0.decompress(),
                fingerprint: trans.tx.payment.fingerprint(),
                nonce: trans.tx.zk_nonce,
                sig: trans.tx.zk_sig,
                amount: trans.tx.payment.amount,
                fee: trans.tx.payment.fee,
            },
            before: trans.before,
            before_token_hash: trans.before_token_hash,
            before_token_balance: trans.before_token_balance,
            before_fee_balance: trans.before_fee_balance,

            proof: merkle::Proof::<LOG4_TREE_SIZE>(trans.proof),
            token_balance_proof: merkle::Proof::<LOG4_TOKENS_TREE_SIZE>(trans.token_balance_proof),
            fee_balance_proof: merkle::Proof::<LOG4_TOKENS_TREE_SIZE>(trans.fee_balance_proof),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WithdrawTransitionBatch<
    const LOG4_BATCH_SIZE: u8,
    const LOG4_TREE_SIZE: u8,
    const LOG4_TOKENS_TREE_SIZE: u8,
>(Vec<WithdrawTransition<LOG4_TREE_SIZE, LOG4_TOKENS_TREE_SIZE>>);
impl<const LOG4_BATCH_SIZE: u8, const LOG4_TREE_SIZE: u8, const LOG4_TOKENS_TREE_SIZE: u8>
    WithdrawTransitionBatch<LOG4_BATCH_SIZE, LOG4_TREE_SIZE, LOG4_TOKENS_TREE_SIZE>
{
    pub fn new(ts: Vec<bazuka::mpn::WithdrawTransition>) -> Self {
        let mut ts = ts
            .into_iter()
            .map(|t| WithdrawTransition::from_bazuka(t))
            .collect::<Vec<_>>();
        while ts.len() < 1 << (2 * LOG4_BATCH_SIZE) {
            ts.push(WithdrawTransition::default());
        }
        Self(ts)
    }
}
impl<const LOG4_BATCH_SIZE: u8, const LOG4_TREE_SIZE: u8, const LOG4_TOKENS_TREE_SIZE: u8> Default
    for WithdrawTransitionBatch<LOG4_BATCH_SIZE, LOG4_TREE_SIZE, LOG4_TOKENS_TREE_SIZE>
{
    fn default() -> Self {
        Self(
            (0..1 << (2 * LOG4_BATCH_SIZE))
                .map(|_| WithdrawTransition::default())
                .collect::<Vec<_>>(),
        )
    }
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct WithdrawCircuit<
    const LOG4_BATCH_SIZE: u8,
    const LOG4_TREE_SIZE: u8,
    const LOG4_TOKENS_TREE_SIZE: u8,
> {
    pub height: u64,          // Public
    pub state: ZkScalar,      // Public
    pub aux_data: ZkScalar,   // Public
    pub next_state: ZkScalar, // Public
    pub transitions:
        Box<WithdrawTransitionBatch<LOG4_BATCH_SIZE, LOG4_TREE_SIZE, LOG4_TOKENS_TREE_SIZE>>, // Secret :)
}

impl<const LOG4_BATCH_SIZE: u8, const LOG4_TREE_SIZE: u8, const LOG4_TOKENS_TREE_SIZE: u8>
    Circuit<BellmanFr> for WithdrawCircuit<LOG4_BATCH_SIZE, LOG4_TREE_SIZE, LOG4_TOKENS_TREE_SIZE>
{
    fn synthesize<CS: ConstraintSystem<BellmanFr>>(
        self,
        cs: &mut CS,
    ) -> Result<(), SynthesisError> {
        // Contract height feeded as input
        let height_wit = AllocatedNum::alloc(&mut *cs, || Ok(self.height.into()))?;
        height_wit.inputize(&mut *cs)?;

        // Previous state feeded as input
        let mut state_wit = AllocatedNum::alloc(&mut *cs, || Ok(self.state.into()))?;
        state_wit.inputize(&mut *cs)?;

        // Sum of internal tx fees feeded as input
        let aux_wit = AllocatedNum::alloc(&mut *cs, || Ok(self.aux_data.into()))?;
        aux_wit.inputize(&mut *cs)?;

        // Expected next state feeded as input
        let claimed_next_state_wit = AllocatedNum::alloc(&mut *cs, || Ok(self.next_state.into()))?;
        claimed_next_state_wit.inputize(&mut *cs)?;

        let state_model = bazuka::zk::ZkStateModel::List {
            item_type: Box::new(bazuka::zk::ZkStateModel::Struct {
                field_types: vec![
                    bazuka::zk::ZkStateModel::Scalar, // Enabled
                    bazuka::zk::ZkStateModel::Scalar, // Amount token-id
                    bazuka::zk::ZkStateModel::Scalar, // Amount
                    bazuka::zk::ZkStateModel::Scalar, // Fee token-id
                    bazuka::zk::ZkStateModel::Scalar, // fee
                    bazuka::zk::ZkStateModel::Scalar, // Fingerprint
                    bazuka::zk::ZkStateModel::Scalar, // Calldata
                ],
            }),
            log4_size: LOG4_BATCH_SIZE,
        };

        // Uncompress all the Withdraw txs that were compressed inside aux_witness
        let mut tx_wits = Vec::new();
        let mut children = Vec::new();
        for trans in self.transitions.0.iter() {
            // If enabled, transaction is validated, otherwise neglected
            let enabled = AllocatedBit::alloc(&mut *cs, Some(trans.enabled))?;

            let amount_token_id = AllocatedNum::alloc(&mut *cs, || {
                Ok(Into::<ZkScalar>::into(trans.tx.amount.token_id).into())
            })?;

            // Tx amount should always have at most 64 bits
            let amount = UnsignedInteger::alloc_64(&mut *cs, trans.tx.amount.amount.into())?;

            let fee_token_id = AllocatedNum::alloc(&mut *cs, || {
                Ok(Into::<ZkScalar>::into(trans.tx.fee.token_id).into())
            })?;

            // Tx amount should always have at most 64 bits
            let fee = UnsignedInteger::alloc_64(&mut *cs, trans.tx.fee.amount.into())?;

            // Tx amount should always have at most 64 bits
            let fingerprint = AllocatedNum::alloc(&mut *cs, || Ok(trans.tx.fingerprint.into()))?;

            // Pub-key only needs to reside on curve if tx is enabled, which is checked in the main loop
            let pub_key = AllocatedPoint::alloc(&mut *cs, || Ok(trans.tx.pub_key))?;
            let nonce = AllocatedNum::alloc(&mut *cs, || Ok((trans.tx.nonce as u64).into()))?;
            let sig_r = AllocatedPoint::alloc(&mut *cs, || Ok(trans.tx.sig.r))?;
            let sig_s = AllocatedNum::alloc(&mut *cs, || Ok(trans.tx.sig.s.into()))?;

            tx_wits.push((
                Boolean::Is(enabled.clone()),
                amount_token_id.clone(),
                amount.clone(),
                fee_token_id.clone(),
                fee.clone(),
                fingerprint.clone(),
                pub_key.clone(),
                nonce.clone(),
                sig_r.clone(),
                sig_s.clone(),
            ));

            let calldata_hash = poseidon::poseidon(
                &mut *cs,
                &[
                    &pub_key.x.into(),
                    &pub_key.y.into(),
                    &nonce.into(),
                    &sig_r.x.into(),
                    &sig_r.y.into(),
                    &sig_s.into(),
                ],
            )?;

            let calldata = common::mux(
                &mut *cs,
                &enabled.clone().into(),
                &Number::zero(),
                &calldata_hash,
            )?;

            children.push(AllocatedState::Children(vec![
                AllocatedState::Value(enabled.into()),
                AllocatedState::Value(amount_token_id.into()),
                AllocatedState::Value(amount.into()),
                AllocatedState::Value(fee_token_id.into()),
                AllocatedState::Value(fee.into()),
                AllocatedState::Value(fingerprint.into()),
                AllocatedState::Value(calldata.into()),
            ]));
        }
        let tx_root = reveal(&mut *cs, &state_model, &AllocatedState::Children(children))?;
        cs.enforce(
            || "",
            |lc| lc + aux_wit.get_variable(),
            |lc| lc + CS::one(),
            |lc| lc + tx_root.get_lc(),
        );

        for (
            trans,
            (
                enabled_wit,
                tx_amount_token_id_wit,
                tx_amount_wit,
                tx_fee_token_id_wit,
                tx_fee_wit,
                fingerprint_wit,
                tx_pub_key_wit,
                tx_nonce_wit,
                tx_sig_r_wit,
                tx_sig_s_wit,
            ),
        ) in self.transitions.0.iter().zip(tx_wits.into_iter())
        {
            // Tx index should always have at most LOG4_TREE_SIZE * 2 bits
            let tx_index_wit = UnsignedInteger::alloc(
                &mut *cs,
                (trans.tx.index as u64).into(),
                LOG4_TREE_SIZE as usize * 2,
            )?;

            let tx_token_index_wit = UnsignedInteger::alloc(
                &mut *cs,
                (trans.tx.token_index as u64).into(),
                LOG4_TOKENS_TREE_SIZE as usize * 2,
            )?;

            let tx_fee_token_index_wit = UnsignedInteger::alloc(
                &mut *cs,
                (trans.tx.fee_token_index as u64).into(),
                LOG4_TOKENS_TREE_SIZE as usize * 2,
            )?;

            // Check if tx pub-key resides on the curve if tx is enabled
            tx_pub_key_wit.assert_on_curve(&mut *cs, &enabled_wit)?;

            let tx_hash_wit = poseidon::poseidon(
                &mut *cs,
                &[
                    &fingerprint_wit.clone().into(),
                    &tx_nonce_wit.clone().into(),
                ],
            )?;
            // Check if sig_r resides on curve
            tx_sig_r_wit.assert_on_curve(&mut *cs, &enabled_wit)?;
            // Check EdDSA signature
            eddsa::verify_eddsa(
                &mut *cs,
                &enabled_wit,
                &tx_pub_key_wit,
                &tx_hash_wit,
                &tx_sig_r_wit,
                &tx_sig_s_wit,
            )?;

            let src_tx_nonce_wit =
                AllocatedNum::alloc(&mut *cs, || Ok((trans.before.tx_nonce as u64).into()))?;
            let src_withdraw_nonce_wit =
                AllocatedNum::alloc(&mut *cs, || Ok((trans.before.withdraw_nonce as u64).into()))?;

            let src_addr_wit = AllocatedPoint::alloc(&mut *cs, || Ok(trans.before.address))?;
            src_addr_wit.assert_on_curve(&mut *cs, &enabled_wit)?;

            let src_balances_before_token_hash_wit =
                AllocatedNum::alloc(&mut *cs, || Ok(trans.before_token_hash.into()))?;

            let src_token_id_wit = AllocatedNum::alloc(&mut *cs, || {
                Ok(Into::<ZkScalar>::into(trans.before_token_balance.token_id).into())
            })?;

            Number::from(src_token_id_wit.clone())
                .assert_equal(&mut *cs, &tx_amount_token_id_wit.into());

            // We don't need to make sure account balance is 64 bits. If everything works as expected
            // nothing like this should happen.
            let src_balance_wit = AllocatedNum::alloc(&mut *cs, || {
                Ok(Into::<u64>::into(trans.before_token_balance.amount).into())
            })?;

            let src_token_balance_hash_wit = poseidon::poseidon(
                &mut *cs,
                &[
                    &src_token_id_wit.clone().into(),
                    &src_balance_wit.clone().into(),
                ],
            )?;
            let mut src_token_balance_proof_wits = Vec::new();
            for b in trans.token_balance_proof.0.clone() {
                src_token_balance_proof_wits.push([
                    AllocatedNum::alloc(&mut *cs, || Ok(b[0].into()))?,
                    AllocatedNum::alloc(&mut *cs, || Ok(b[1].into()))?,
                    AllocatedNum::alloc(&mut *cs, || Ok(b[2].into()))?,
                ]);
            }
            merkle::check_proof_poseidon4(
                &mut *cs,
                &enabled_wit,
                &tx_token_index_wit.clone().into(),
                &src_token_balance_hash_wit.clone().into(),
                &src_token_balance_proof_wits,
                &src_balances_before_token_hash_wit.clone().into(),
            )?;
            let new_token_balance_hash_wit = poseidon::poseidon(
                &mut *cs,
                &[
                    &src_token_id_wit.clone().into(),
                    &(Number::from(src_balance_wit.clone()) - Number::from(tx_amount_wit.clone())),
                ],
            )?;
            let balance_middle_root = merkle::calc_root_poseidon4(
                &mut *cs,
                &tx_token_index_wit.clone().into(),
                &new_token_balance_hash_wit,
                &src_token_balance_proof_wits,
            )?;

            let src_fee_token_id_wit = AllocatedNum::alloc(&mut *cs, || {
                Ok(Into::<ZkScalar>::into(trans.before_fee_balance.token_id).into())
            })?;

            Number::from(src_fee_token_id_wit.clone())
                .assert_equal(&mut *cs, &tx_fee_token_id_wit.into());

            // We don't need to make sure account balance is 64 bits. If everything works as expected
            // nothing like this should happen.
            let src_fee_balance_wit = AllocatedNum::alloc(&mut *cs, || {
                Ok(Into::<u64>::into(trans.before_fee_balance.amount).into())
            })?;

            let src_fee_token_balance_hash_wit = poseidon::poseidon(
                &mut *cs,
                &[
                    &src_fee_token_id_wit.clone().into(),
                    &src_fee_balance_wit.clone().into(),
                ],
            )?;

            let mut src_fee_token_balance_proof_wits = Vec::new();
            for b in trans.fee_balance_proof.0.clone() {
                src_fee_token_balance_proof_wits.push([
                    AllocatedNum::alloc(&mut *cs, || Ok(b[0].into()))?,
                    AllocatedNum::alloc(&mut *cs, || Ok(b[1].into()))?,
                    AllocatedNum::alloc(&mut *cs, || Ok(b[2].into()))?,
                ]);
            }

            merkle::check_proof_poseidon4(
                &mut *cs,
                &enabled_wit,
                &tx_fee_token_index_wit.clone().into(),
                &src_fee_token_balance_hash_wit.clone().into(),
                &src_fee_token_balance_proof_wits,
                &balance_middle_root,
            )?;

            let new_fee_token_balance_hash_wit = poseidon::poseidon(
                &mut *cs,
                &[
                    &src_fee_token_id_wit.clone().into(),
                    &(Number::from(src_fee_balance_wit.clone()) - Number::from(tx_fee_wit.clone())),
                ],
            )?;

            let src_hash_wit = poseidon::poseidon(
                &mut *cs,
                &[
                    &src_tx_nonce_wit.clone().into(),
                    &src_withdraw_nonce_wit.clone().into(),
                    &src_addr_wit.x.clone().into(),
                    &src_addr_wit.y.clone().into(),
                    &src_balances_before_token_hash_wit.clone().into(),
                ],
            )?;
            let mut proof_wits = Vec::new();
            for b in trans.proof.0.clone() {
                proof_wits.push([
                    AllocatedNum::alloc(&mut *cs, || Ok(b[0].into()))?,
                    AllocatedNum::alloc(&mut *cs, || Ok(b[1].into()))?,
                    AllocatedNum::alloc(&mut *cs, || Ok(b[2].into()))?,
                ]);
            }
            merkle::check_proof_poseidon4(
                &mut *cs,
                &enabled_wit,
                &tx_index_wit.clone().into(),
                &src_hash_wit,
                &proof_wits,
                &state_wit.clone().into(),
            )?;

            // Check tx nonce is equal with account nonce to prevent double spending
            cs.enforce(
                || "",
                |lc| lc + tx_nonce_wit.get_variable(),
                |lc| lc + CS::one(),
                |lc| lc + src_withdraw_nonce_wit.get_variable() + CS::one(),
            );

            let balance_final_root = merkle::calc_root_poseidon4(
                &mut *cs,
                &tx_fee_token_index_wit.clone().into(),
                &new_fee_token_balance_hash_wit,
                &src_fee_token_balance_proof_wits,
            )?;

            // Calculate next-state hash and update state if tx is enabled
            let new_hash_wit = poseidon::poseidon(
                &mut *cs,
                &[
                    &src_tx_nonce_wit.clone().into(),
                    &(Number::from(src_withdraw_nonce_wit)
                        + Number::constant::<CS>(BellmanFr::one())),
                    &tx_pub_key_wit.x.clone().into(),
                    &tx_pub_key_wit.y.clone().into(),
                    &balance_final_root,
                ],
            )?;
            let next_state_wit =
                merkle::calc_root_poseidon4(&mut *cs, &tx_index_wit, &new_hash_wit, &proof_wits)?;
            state_wit = common::mux(&mut *cs, &enabled_wit, &state_wit.into(), &next_state_wit)?;
        }

        // Check if applying txs result in the claimed next state
        cs.enforce(
            || "",
            |lc| lc + state_wit.get_variable(),
            |lc| lc + CS::one(),
            |lc| lc + claimed_next_state_wit.get_variable(),
        );

        Ok(())
    }
}
