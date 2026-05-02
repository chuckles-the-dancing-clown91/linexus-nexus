//! # Vicinagora — Council Governance Engine

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use linexus_core::errors::LinexusError;
use linexus_core::funnel::{PermissionFunnel, SystemIntent};
use linexus_core::identity::NodeIdentity;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalStatus { Active, Approved, Rejected, Executed }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub proposal_id: Uuid,
    pub district_council_id: Uuid,
    pub author_node_id: Uuid,
    pub summary: String,
    pub required_threshold: f32,
    pub total_eligible_voters: u32,
    pub votes: Vec<VoteRecord>,
    pub status: ProposalStatus,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoteRecord { pub voter_id: Uuid, pub vote: Vote, pub signature: String, pub timestamp: u64 }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Vote { Yea, Nay, Abstain }

impl Proposal {
    pub fn register_vote(&mut self, voter: &NodeIdentity, vote: Vote, signature: String, timestamp: u64) -> Result<(), LinexusError> {
        PermissionFunnel::evaluate_intent(voter, &SystemIntent::VoteOnPolicy(self.proposal_id))?;
        if self.votes.iter().any(|v| v.voter_id == voter.node_id) {
            return Err(LinexusError::GovernanceError("Already voted".into()));
        }
        self.votes.push(VoteRecord { voter_id: voter.node_id, vote, signature, timestamp });
        if self.current_approval_pct() >= self.required_threshold { self.status = ProposalStatus::Approved; }
        Ok(())
    }

    pub fn current_approval_pct(&self) -> f32 {
        if self.total_eligible_voters == 0 { return 0.0; }
        self.votes.iter().filter(|v| v.vote == Vote::Yea).count() as f32 / self.total_eligible_voters as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use linexus_core::identity::{LifecyclePhase, NodeClass};
    use std::collections::HashSet;

    fn make_voter(phase: LifecyclePhase) -> NodeIdentity {
        NodeIdentity { node_id: Uuid::new_v4(), public_key: "k".into(), class: NodeClass::Human { phase, capabilities: HashSet::new(), assigned_homestead: None }, label: "V".into(), commissioned_at: 0 }
    }

    fn make_proposal() -> Proposal {
        Proposal { proposal_id: Uuid::new_v4(), district_council_id: Uuid::new_v4(), author_node_id: Uuid::new_v4(), summary: "Test".into(), required_threshold: 0.66, total_eligible_voters: 3, votes: vec![], status: ProposalStatus::Active, created_at: 0 }
    }

    #[test] fn labor_can_vote() { let mut p = make_proposal(); assert!(p.register_vote(&make_voter(LifecyclePhase::Labor), Vote::Yea, "s".into(), 1).is_ok()); }
    #[test] fn child_cannot_vote() { let mut p = make_proposal(); assert!(p.register_vote(&make_voter(LifecyclePhase::Childhood), Vote::Yea, "s".into(), 1).is_err()); }
    #[test] fn duplicate_rejected() { let mut p = make_proposal(); let v = make_voter(LifecyclePhase::Labor); p.register_vote(&v, Vote::Yea, "s".into(), 1).unwrap(); assert!(p.register_vote(&v, Vote::Nay, "s2".into(), 2).is_err()); }
    #[test] fn threshold_triggers_approval() {
        let mut p = make_proposal();
        p.register_vote(&make_voter(LifecyclePhase::Labor), Vote::Yea, "s".into(), 1).unwrap();
        assert_eq!(p.status, ProposalStatus::Active);
        p.register_vote(&make_voter(LifecyclePhase::Retirement), Vote::Yea, "s".into(), 2).unwrap();
        assert_eq!(p.status, ProposalStatus::Approved);
    }
}
