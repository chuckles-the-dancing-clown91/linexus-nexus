# Nexus Housing API — the Hearth commons

The housing domain ported from Acre Nexus, filtered through the Linexus
canon: housing is an element of the Dignity Floor — unconditional and
**never priced**. There is no rent, no screening, no ownership anywhere in
this API. Acre's payment/accounting/screening stack was deliberately not
ported.

## Authentication

Every route requires a **user JWT** (`Authorization: Bearer …`) plus an
RBAC permission. Seeded roles:

- `housing_manager` — `housing:read`, `housing:write` (draft nodes, add
  units, docs, maintenance)
- `council` — `housing:read`, `housing:review`, `housing:assign`,
  `housing:vacate` (sign-off and resident stewardship)
- `admin` / plan `enterprise` — `*`; plan `pro` carries `housing:read`.

## Lifecycle

```
draft ──submit──▶ pending_council ──quorum of approvals──▶ active
                        │                                    │
                        └──rejected──▶ draft        vacating ⇄ active ──▶ archived
```

- A housing node is created in `draft` with a backing `nodes` row
  (`class=housing`). `quorum_required` defaults to 2.
- Council votes are **immutable and idempotent** — one vote per council
  member per node; a re-vote returns the existing record unchanged.
- On quorum the node auto-activates and **every available unit enters the
  housing queue**. Units added before activation stay out of the queue.
- The decisive vote mints a small civic-labor contribution to the
  reviewer's wallet — governance is breath.
- `vacate` closes the occupancy, moves the unit to `make_ready` (its lean)
  and re-enqueues it immediately. The queue is FIFO; there is no
  screening and no priority by wealth.

## Endpoints

All under `/api/housing`.

| Method | Path | Permission | Purpose |
|---|---|---|---|
| POST | `/nodes` | `housing:write` | Draft a housing node (+ backing node) |
| GET | `/nodes` | `housing:read` | List housing nodes |
| GET | `/nodes/{id}` | `housing:read` | Node detail |
| PATCH | `/nodes/{id}/submit` | `housing:write` | draft → pending_council |
| PATCH | `/nodes/{id}/archive` | `housing:write` | Archive |
| POST | `/nodes/{id}/units` | `housing:write` | Add a unit |
| GET | `/nodes/{id}/units` | `housing:read` | List units |
| POST | `/nodes/{id}/review` | `housing:review` | Cast approve/reject vote (quorum auto-activates) |
| GET | `/nodes/{id}/reviews` | `housing:read` | List council reviews |
| POST | `/units/{unit_id}/assign` | `housing:assign` | Assign a resident node from the queue |
| POST | `/units/{unit_id}/vacate` | `housing:vacate` | Close occupancy, lean + re-queue |
| GET | `/queue` | `housing:read` | Available units, FIFO |
| POST | `/units/{unit_id}/maintenance` | `housing:write` | Open a work order |
| GET | `/units/{unit_id}/maintenance` | `housing:read` | List work orders |
| POST | `/nodes/{id}/documents` | `housing:write` | Attach a document (URL or inline) |
| GET | `/nodes/{id}/documents` | `housing:read` | List documents |

## Statuses

- **Housing node**: `draft`, `pending_council`, `active`, `vacating`, `archived`
- **Unit**: `available`, `occupied`, `make_ready`, `maintenance`, `down`
- **Queue entry**: `available`, `reserved`, `assigned`
- **Ticket**: `open`, `in_progress`, `resolved`, `closed` (priority `low` … `emergency`)

Notes for clients: `POST …/review` returns the review row only — re-GET
the node to observe activation. All `*_node_id` request fields are integer
`nodes.id` primary keys; the housing node's `node_id` field is the backing
node UUID.

Request tests live in `tests/requests/housing.rs` (lifecycle, quorum,
assign/vacate/re-queue, permission guards).
