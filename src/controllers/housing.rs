#![allow(clippy::missing_errors_doc)]
#![allow(clippy::unnecessary_struct_initialization)]
#![allow(clippy::unused_async)]

use axum::extract::Path;
use loco_rs::prelude::*;
use serde::Deserialize;
use uuid::Uuid;

use crate::middleware::rbac;
use crate::models::{
    contributions, housing_council_reviews, housing_documents, housing_maintenance, housing_nodes,
    housing_occupancies, housing_queue, housing_units, nodes,
};

// ── DTOs ──

#[derive(Debug, Deserialize)]
pub struct CreateNodeRequest {
    pub name: String,
    pub address: String,
    pub city: Option<String>,
    pub state_province: Option<String>,
    pub postal_code: Option<String>,
    pub property_type: String,
    pub unit_count: i32,
    pub quorum_required: Option<i32>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CastVoteRequest {
    pub vote: String, // approve | reject
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AssignResidentRequest {
    pub resident_node_id: i32,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct VacateUnitRequest {
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateUnitRequest {
    pub unit_number: String,
    pub beds: Option<i32>,
    pub baths: Option<f32>,
    pub sqft: Option<i32>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateMaintenanceRequest {
    pub title: String,
    pub description: Option<String>,
    pub priority: String, // low | medium | high | emergency
    pub assigned_to_node_id: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct CreateDocRequest {
    pub housing_unit_id: Option<i32>,
    pub document_kind: String, // council_resolution | inspection_report | occupancy_agreement | etc
    pub title: String,
    pub url: Option<String>,
    pub content: Option<String>,
}

// ── Handlers ──

/// Create a housing node (and its backing Linexus Node)
pub async fn create_node(
    auth: auth::JWT,
    State(ctx): State<AppContext>,
    Json(req): Json<CreateNodeRequest>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "housing:write").await?;

    // 1. Commission backing Node
    let node_ref = Uuid::new_v4().to_string();
    let commission_params = nodes::CommissionParams {
        class: "housing".to_string(),
        label: req.name.clone(),
        lifecycle_phase: Some("active".to_string()),
        capabilities: None,
        public_key: None,
        owner_user_id: None,
        source: Some("housing_manager".to_string()),
        external_ref: Some(node_ref.clone()),
    };
    let backing_node = nodes::Model::commission(&ctx.db, &commission_params).await?;

    // 2. Create Housing Node
    let params = housing_nodes::CreateParams {
        node_id: backing_node.node_id,
        name: req.name,
        address: req.address,
        city: req.city,
        state_province: req.state_province,
        postal_code: req.postal_code,
        property_type: req.property_type,
        unit_count: req.unit_count,
        quorum_required: req.quorum_required.unwrap_or(2),
        notes: req.notes,
    };
    let hnode = housing_nodes::Model::create(&ctx.db, &params).await?;

    format::json(hnode)
}

/// List all housing nodes
pub async fn list_nodes(auth: auth::JWT, State(ctx): State<AppContext>) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "housing:read").await?;

    let nodes = housing_nodes::Model::find_all(&ctx.db).await?;
    format::json(nodes)
}

/// Get housing node detail
pub async fn get_node(
    auth: auth::JWT,
    State(ctx): State<AppContext>,
    Path(id): Path<i32>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "housing:read").await?;

    let hnode = housing_nodes::Model::find_by_id(&ctx.db, id).await?;
    format::json(hnode)
}

/// Submit for review (draft -> pending_council)
pub async fn submit_node(
    auth: auth::JWT,
    State(ctx): State<AppContext>,
    Path(id): Path<i32>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "housing:write").await?;

    let updated =
        housing_nodes::Model::transition(&ctx.db, id, housing_nodes::HousingStatus::PendingCouncil)
            .await?;
    format::json(updated)
}

/// Archive a node
pub async fn archive_node(
    auth: auth::JWT,
    State(ctx): State<AppContext>,
    Path(id): Path<i32>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "housing:write").await?;

    let updated =
        housing_nodes::Model::transition(&ctx.db, id, housing_nodes::HousingStatus::Archived)
            .await?;
    format::json(updated)
}

/// Add a unit to a housing node
pub async fn add_unit(
    auth: auth::JWT,
    State(ctx): State<AppContext>,
    Path(id): Path<i32>,
    Json(req): Json<CreateUnitRequest>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "housing:write").await?;

    let hnode = housing_nodes::Model::find_by_id(&ctx.db, id).await?;

    let params = housing_units::CreateUnitParams {
        housing_node_id: id,
        unit_number: req.unit_number,
        beds: req.beds,
        baths: req.baths,
        sqft: req.sqft,
        notes: req.notes,
    };
    let unit = housing_units::Model::create(&ctx.db, &params).await?;

    // Units enter the commons pool only once the council has activated the
    // node; on activation every available unit is enqueued automatically.
    if hnode.status == housing_nodes::HousingStatus::Active.as_str() {
        housing_queue::Model::enqueue(&ctx.db, unit.id, &unit.unit_number, 0).await?;
    }

    format::json(unit)
}

/// List units for a housing node
pub async fn list_units(
    auth: auth::JWT,
    State(ctx): State<AppContext>,
    Path(id): Path<i32>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "housing:read").await?;

    let units = housing_units::Model::find_for_node(&ctx.db, id).await?;
    format::json(units)
}

/// Cast council review vote
pub async fn cast_vote(
    auth: auth::JWT,
    State(ctx): State<AppContext>,
    Path(id): Path<i32>,
    Json(req): Json<CastVoteRequest>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "housing:review").await?;

    // Find the caller's nodes.id - wait, the user's human node.
    // If none exists, we commission one or find it.
    let reviewer_node = match nodes::Model::find_by_owner(&ctx.db, user.id).await? {
        Some(n) => n,
        None => {
            return Err(loco_rs::Error::BadRequest(
                "User has no associated node".to_string(),
            ));
        }
    };

    let params = housing_council_reviews::CastVoteParams {
        housing_node_id: id,
        reviewer_node_id: reviewer_node.id,
        vote: req.vote,
        notes: req.notes,
    };
    let (review, casted) = housing_council_reviews::Model::cast_vote(&ctx.db, &params).await?;

    // Check quorum & trigger activation.
    if casted {
        if let Some(activated) = housing_nodes::Model::check_and_activate(&ctx.db, id).await? {
            // The decisive vote is a civic act of governance — breath, not
            // ownership — so it mints a small labor contribution.
            let at_unix = chrono::Utc::now().timestamp();
            let mint = contributions::RecordParams {
                node_id: reviewer_node.node_id,
                kind: "labor".to_string(),
                minutes: 30,
                essential: false,
                coverage: false,
                week_index: at_unix / 604_800,
                note: Some(format!(
                    "housing council · decisive vote · {}",
                    activated.name
                )),
                at_unix: Some(at_unix),
            };
            if let Err(err) = contributions::Model::record(&ctx.db, &mint).await {
                tracing::warn!(error = %err, housing_node = id, "decisive-vote mint failed");
            }
        }
    }

    format::json(review)
}

/// List reviews for a housing node
pub async fn list_reviews(
    auth: auth::JWT,
    State(ctx): State<AppContext>,
    Path(id): Path<i32>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "housing:read").await?;

    let reviews = housing_council_reviews::Model::find_for_node(&ctx.db, id).await?;
    format::json(reviews)
}

/// Assign resident from queue to a unit
pub async fn assign_resident(
    auth: auth::JWT,
    State(ctx): State<AppContext>,
    Path(unit_id): Path<i32>,
    Json(req): Json<AssignResidentRequest>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "housing:assign").await?;

    let assigner_node = nodes::Model::find_by_owner(&ctx.db, user.id).await?;
    let assigner_node_id = assigner_node.map(|n| n.id);

    let params = housing_occupancies::AssignParams {
        housing_unit_id: unit_id,
        resident_node_id: req.resident_node_id,
        assigned_by_node_id: assigner_node_id,
        notes: req.notes,
    };
    let occupancy = housing_occupancies::Model::assign(&ctx.db, &params).await?;
    format::json(occupancy)
}

/// Vacate unit (release back into queue)
pub async fn vacate_unit(
    auth: auth::JWT,
    State(ctx): State<AppContext>,
    Path(unit_id): Path<i32>,
    Json(req): Json<VacateUnitRequest>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "housing:vacate").await?;

    let occupancy = housing_occupancies::Model::vacate(&ctx.db, unit_id, req.notes).await?;
    format::json(occupancy)
}

/// View the available units in the housing queue
pub async fn get_queue(auth: auth::JWT, State(ctx): State<AppContext>) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "housing:read").await?;

    let queue = housing_queue::Model::available(&ctx.db).await?;
    format::json(queue)
}

/// Add maintenance ticket
pub async fn add_maintenance(
    auth: auth::JWT,
    State(ctx): State<AppContext>,
    Path(unit_id): Path<i32>,
    Json(req): Json<CreateMaintenanceRequest>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "housing:write").await?;

    let op_node = nodes::Model::find_by_owner(&ctx.db, user.id).await?;
    let op_node_id = op_node.map(|n| n.id);

    let params = housing_maintenance::CreateTicketParams {
        housing_unit_id: unit_id,
        title: req.title,
        description: req.description,
        priority: req.priority,
        opened_by_node_id: op_node_id,
        assigned_to_node_id: req.assigned_to_node_id,
    };
    let ticket = housing_maintenance::Model::create(&ctx.db, &params).await?;
    format::json(ticket)
}

/// List tickets for unit
pub async fn list_maintenance(
    auth: auth::JWT,
    State(ctx): State<AppContext>,
    Path(unit_id): Path<i32>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "housing:read").await?;

    let tickets = housing_maintenance::Model::find_for_unit(&ctx.db, unit_id).await?;
    format::json(tickets)
}

/// List docs for a node
pub async fn list_docs(
    auth: auth::JWT,
    State(ctx): State<AppContext>,
    Path(id): Path<i32>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "housing:read").await?;

    let docs = housing_documents::Model::find_for_node(&ctx.db, id).await?;
    format::json(docs)
}

/// Attach doc to a node
pub async fn add_doc(
    auth: auth::JWT,
    State(ctx): State<AppContext>,
    Path(id): Path<i32>,
    Json(req): Json<CreateDocRequest>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "housing:write").await?;

    let uploader_node = nodes::Model::find_by_owner(&ctx.db, user.id).await?;
    let uploader_node_id = uploader_node.map(|n| n.id);

    let params = housing_documents::CreateDocumentParams {
        housing_node_id: id,
        housing_unit_id: req.housing_unit_id,
        document_kind: req.document_kind,
        title: req.title,
        url: req.url,
        content: req.content,
        uploaded_by_node_id: uploader_node_id,
    };
    let doc = housing_documents::Model::create(&ctx.db, &params).await?;
    format::json(doc)
}

pub fn routes() -> Routes {
    Routes::new()
        .prefix("api/housing")
        .add("/nodes", post(create_node))
        .add("/nodes", get(list_nodes))
        .add("/nodes/{id}", get(get_node))
        .add("/nodes/{id}/submit", patch(submit_node))
        .add("/nodes/{id}/archive", patch(archive_node))
        .add("/nodes/{id}/units", post(add_unit))
        .add("/nodes/{id}/units", get(list_units))
        .add("/nodes/{id}/review", post(cast_vote))
        .add("/nodes/{id}/reviews", get(list_reviews))
        .add("/units/{unit_id}/assign", post(assign_resident))
        .add("/units/{unit_id}/vacate", post(vacate_unit))
        .add("/queue", get(get_queue))
        .add("/units/{unit_id}/maintenance", post(add_maintenance))
        .add("/units/{unit_id}/maintenance", get(list_maintenance))
        .add("/nodes/{id}/documents", get(list_docs))
        .add("/nodes/{id}/documents", post(add_doc))
}
