//! Tower-compatible authorization layer.

use linexus_core::funnel::{PermissionFunnel, SystemIntent};
use linexus_core::identity::NodeIdentity;
use linexus_core::errors::LinexusError;

pub struct AuthorizationLayer;

impl AuthorizationLayer {
    pub fn authorize(actor: &NodeIdentity, intent: &SystemIntent) -> Result<(), LinexusError> {
        PermissionFunnel::evaluate_intent(actor, intent)
    }
}
