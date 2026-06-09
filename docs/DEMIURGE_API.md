# Nexus Demiurge API

Real-time, DB-backed Demiurge management. Everything below the API is the
canonical accounting from the white paper: only breath mints, the floor is never
priced, and every lot decays in twenty years.

## Authentication

Service routes are guarded by a **system token** in the
`X-Nexus-System-Token` header. Two ways to get one:

- **Root token** — env `NEXUS_SYSTEM_TOKEN` (dev fallback: `dev-nexus-system-token`),
  carries the `*` scope. Use for local development.
- **Issued token** — an admin (`system:tokens` permission) calls
  `POST /api/nexus/tokens` to mint a scoped token for a named service. The
  plaintext is returned exactly once and stored only as a SHA-256 hash.

Scopes: `nodes:create`, `nodes:read`, `wallet:read`, `wallet:write`,
`demiurge:mint`, `demiurge:redeem`, `payments:process`, `payments:read`.
`*` and `prefix:*` wildcards are honored.

## Endpoints

| Method | Path | Scope | Purpose |
|---|---|---|---|
| GET | `/api/nexus/parameters` | (public) | Canonical rates/multipliers — never hardcode these client-side |
| POST | `/api/nexus/nodes` | `nodes:create` | Commission a node (idempotent on `source`+`external_ref`) |
| GET | `/api/nexus/nodes` | `nodes:read` | List nodes |
| GET | `/api/nexus/nodes/{id}` | `nodes:read` | Get a node |
| GET | `/api/nexus/nodes/{id}/wallet` | `wallet:read` | Live balance + unexpired lots |
| GET | `/api/nexus/nodes/{id}/standing` | `wallet:read` | Node + wallet + recent contributions + floor |
| POST | `/api/nexus/nodes/{id}/wallet/sweep` | `wallet:write` | Sweep decayed lots, report amount lost |
| POST | `/api/nexus/contributions` | `demiurge:mint` | Record a contribution and mint Demiurge |
| POST | `/api/nexus/demiurge/redeem` | `demiurge:redeem` | Spend Demiurge into a sink |
| POST | `/api/nexus/payments` | `payments:process` | Process a Vicinagora payment |
| GET | `/api/nexus/payments` | `payments:read` | List payments |
| POST | `/api/nexus/tokens` | JWT + `system:tokens` | Issue a system token |

## The payment path

A payment settles in `fiat` or `demiurge`:

- **fiat** — the principal clears with the external processor (a seam, simulated
  in dev). The `fee_fiat_minor + tax_fiat_minor` friction is converted to
  Demiurge (`fee_conversion_bps`, default `10000` = 1×) and **minted to the
  payee node**. The skim the old world keeps becomes contribution credit in the
  node.
- **demiurge** — value moves wallet-to-wallet. Resident→resident transfers
  preserve each lot's original mint instant (age is non-transferable); payments
  into a sink simply spend.

## Example

```bash
# Commission a node for a Tea & Madness account
curl -X POST localhost:5150/api/nexus/nodes \
  -H "X-Nexus-System-Token: dev-nexus-system-token" \
  -H 'content-type: application/json' \
  -d '{"label":"@daedalus","source":"tea-and-madness","external_ref":"u-42",
       "capabilities":["writing","mentorship"]}'

# Mint for 20h of labor (-> 100 Demiurge)
curl -X POST localhost:5150/api/nexus/contributions \
  -H "X-Nexus-System-Token: dev-nexus-system-token" \
  -H 'content-type: application/json' \
  -d '{"node_id":"<uuid>","kind":"labor","minutes":1200}'

# A $40 sale with $5 of fees+taxes -> 500 Demiurge minted to the seller
curl -X POST localhost:5150/api/nexus/payments \
  -H "X-Nexus-System-Token: dev-nexus-system-token" \
  -H 'content-type: application/json' \
  -d '{"kind":"sale","pay_currency":"fiat","amount_fiat_minor":4000,
       "fee_fiat_minor":300,"tax_fiat_minor":200,"payee_node_id":"<uuid>"}'
```
