# Nexus as the infrastructure gateway — the v1 extension contract

Nexus already reaches **machines** (agents). This contract makes it the one place
that reaches **everything else we run infrastructure on**: DigitalOcean (compute,
volumes, load balancers), Cloudflare (DNS zones, records, Registrar) and BIND
servers that our own agents host. Solnyxus decides and records; Nexus executes and
holds the write credentials. The Hub never talks to DigitalOcean or Cloudflare with a
write-capable token — Nexus does, from behind the firewall.

Everything here is under `/api/v1`, JSON camelCase, `Authorization: Bearer <key>`
(the root `NEXUS_SYSTEM_TOKEN` or a minted system token) unless a route says
otherwise. Errors are `{"error": "<code>", "detail": "…"}` with:

| Status | `error` | When |
|---|---|---|
| 400 | `invalid` | a missing or malformed field — `detail` names it |
| 401 | `unauthorized` | no key, a wrong key, an agent credential on someone else's route |
| 404 | `not_found` | no such agent / task / zone / record / droplet |
| 409 | `conflict` | already exists (zone name, record that cannot coexist), a `machineId` enrolled in another hostgroup (token path), an `Idempotency-Key` reused for another operation or still in flight, cancelling a finished task |
| 412 | `confirmation_required` | a destructive call without the matching `X-Confirm` header |
| 422 | `provider_rejected` | the provider answered 4xx — `detail` carries its message (credentials scrubbed) |
| 424 | `provider_not_configured` | the provider has no credentials |
| 502 | `provider_unreachable` | the provider timed out / 5xx / bad body |
| 500 | `internal` | a Nexus fault; the cause is in the Nexus log, never in `detail` |

Every **mutating** provider call takes an optional `X-Requested-By` header (the
Hub sends `user:<uuid> <email>`) that is recorded on the operation and shipped to
the Logger, and an optional `Idempotency-Key` header: the same key within 24 h
replays the first response instead of acting twice.

---

## 1. Enrollment tokens and per-agent credentials

Onboarding becomes: the Hub mints a one-time token bound to a hostgroup (the
client's slug), an environment and the Hub's own ids → the install command carries
it → the agent enrolls with it → Nexus returns a per-agent credential → the Hub reads
the token back and learns the new agent id.

| Method | Path | Body → answer |
|---|---|---|
| POST | `/enrollment-tokens` | `{hostgroup*, environment?="production", label?, ttlMinutes?=1440 (5…43200), maxUses?=1 (1…1000), metadata?: object}` → `201 {id, token: "nxe_…" (shown once), hostgroup, environment, label, metadata, expiresAt, maxUses, uses: 0, agents: [], revokedAt: null, createdAt}` |
| GET | `/enrollment-tokens` | → `[…the same, without token…]`, newest first |
| GET | `/enrollment-tokens/{id}` | → one, with `agents: [agentId…]` that enrolled with it |
| DELETE | `/enrollment-tokens/{id}` | revoke → `204` |

`POST /agents/enroll` — `{hostname*, hostgroup?, machineId?, enrollmentToken?}`.

- Authenticated **either** by a system key in the bearer (the legacy path) **or** by
  `enrollmentToken` in the body with no bearer. A token that is unknown, expired,
  revoked or used up is `401`.
- With an enrollment token, its `hostgroup` and `environment` win over the body.
- `machineId` (from `/etc/machine-id`) **re-adopts**: an existing agent with the same
  machine id keeps its agent id; its credential is rotated. Without one, a new row.
- → `201 {agentId, id, hostname, hostgroup, environment, agentToken: "nxa_…", enrollmentTokenId, metadata, readopted: bool}`.
  The agent keeps `agentToken` and uses it for every later call.

Agent routes (`/agents/{id}/report`, `/heartbeat`, `/tasks`, `/tasks/{t}/result`,
`POST /agents/{id}/logs`, `GET /agents/{id}/environment`) accept the agent's own
`nxa_` credential **for its own id only**, or a system key. Operator routes refuse
agent credentials (`401`). Credentials are stored as SHA-256 hashes.

## 2. Richer facts

`POST /agents/{id}/report` additionally accepts (all optional):

```json
{
  "machineId": "4c4c…",
  "publicIp": "203.0.113.7",
  "interfaces": [{"name": "eth0", "mac": "…", "up": true, "addresses": ["203.0.113.7/20", "10.10.0.5/16"]}],
  "listening":  [{"proto": "tcp", "address": "0.0.0.0", "port": 443, "process": "nginx"}],
  "services":   [{"name": "nginx.service", "state": "active", "detail": "running", "enabled": true, "description": "…"}],
  "packages":   [{"name": "nginx", "version": "1.24.0", "manager": "apt"}],
  "dnsServer":  {"software": "bind9", "version": "9.18", "running": true, "zones": ["example.com"]}
}
```

The agent reports facts at start and every `AGENT_FACTS_INTERVAL` (default 5 min).
`GET /agents/{id}` adds `machineId, publicIp, interfaces, listening, dnsServer,
factsAt` to the detail. `GET /agents/{id}/services` → `{services: […]}` and
`GET /agents/{id}/packages` → `{packages: […]}` serve the last report (`[]` when none).

## 3. Tasks: retry and cancel

- An `accepted` task (the Orchestrator was unreachable) is re-planned when an agent it
  targets polls, and by a background sweep every 60 s, for up to 24 h; then it is
  `failed` with `result.error = "never planned"`.
- `POST /tasks/{id}/cancel` → `{taskId, status: "cancelled"}`; a cancelled task is
  never handed to an agent (`409` when it is already terminal).
- Hostgroup targets: an entry `hostgroup:<name>` in `targets` resolves to every agent
  in that hostgroup at creation time.

## 4. Providers

| Method | Path | |
|---|---|---|
| GET | `/providers` | `[{key: "digitalocean"|"cloudflare"|"bind", kind: "cloud"|"dns", configured, source: "env"|"stored"|"none"|"builtin", accountId, accountName, checkedAt, state: "ok"|"unauthorized"|"unreachable"|"not_configured"|"unknown", detail}]` |
| PUT | `/providers/{key}/credentials` | `{token*, accountId?}` — write-only; sealed with `NEXUS_SECRET_KEY` (AES-256-GCM). Refused (`400`) outside development when that key is unset. → `204` |
| DELETE | `/providers/{key}/credentials` | → `204` |
| POST | `/providers/{key}/test` | → `{ok, state, accountId, accountName, detail, scopes?: {…}}` (DigitalOcean: `/v2/account`; Cloudflare: `/user/tokens/verify` + `/accounts`) |

Env credentials (`DIGITALOCEAN_TOKEN`, `CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_ACCOUNT_ID`)
win over stored ones. API bases come **only** from env (`DIGITALOCEAN_API_BASE`,
default `https://api.digitalocean.com`; `CLOUDFLARE_API_BASE`, default
`https://api.cloudflare.com/client/v4`) so nobody with API access can redirect a
token. Redirects are never followed; tokens are scrubbed from every error.
`bind` is always `builtin` (it is our agents).

## 5. DNS — one API across Cloudflare and BIND

Zone ids are prefixed by provider: `cf:<cloudflare zone id>` or `bind:<uuid>`.

| Method | Path | |
|---|---|---|
| GET | `/dns/zones` | every zone of every configured provider: `[{id, provider, name, status, nameServers, originalNameServers, primaryAgentId, secondaryAgentIds, serial, applyStatus, lastTaskId, createdAt}]` |
| POST | `/dns/zones` | `{provider*: "cloudflare"|"bind", name*, primaryAgentId? (bind, required), secondaryAgentIds?, defaultTtl?=3600, adminEmail?}` → `201` zone. Cloudflare answers with the **name servers to set at the registrar** |
| GET | `/dns/zones/{zoneId}` | the zone with `records` |
| DELETE | `/dns/zones/{zoneId}` | needs `X-Confirm: <zone name>` → `204` |
| GET | `/dns/zones/{zoneId}/records?type=&name=` | `[{id, type, name, content, ttl, proxied, priority, comment}]` — `name` is the FQDN |
| POST | `/dns/zones/{zoneId}/records` | `{type*, name*, content*, ttl?=1 (auto) / 3600 (bind), proxied?=false, priority? (MX), comment?}` → `201 {record, taskId?}` |
| PATCH | `/dns/zones/{zoneId}/records/{recordId}` | any of the above → `{record, taskId?}` |
| DELETE | `/dns/zones/{zoneId}/records/{recordId}` | → `{taskId?}` |
| POST | `/dns/zones/{zoneId}/records/ensure` | `{type*, name*, content*, ttl?, proxied?}` — find by type+name, update when different, create when absent → `{record, changed, taskId?}` |

Types: `A, AAAA, CNAME, TXT, MX, NS, CAA`. `name` may be relative (`www`, `@`) or the
FQDN; it is stored and answered as the FQDN. Validation: A must parse as IPv4, AAAA
as IPv6, MX needs `priority`, CNAME cannot sit at the apex, TTL 60…86400 (or 1 =
automatic on Cloudflare), at most 2 048 characters of content.

**BIND zones** live in Nexus's database (it is the source of truth for them). Every
change bumps the SOA serial (`YYYYMMDDnn`), renders the zone file and dispatches a
`dns_zone_apply` task (through the Orchestrator) to the primary and each secondary;
`applyStatus` is `pending → applied | failed` from those tasks' results and
`lastTaskId` names the latest. Deleting a zone dispatches `dns_zone_remove`.

| Method | Path | |
|---|---|---|
| GET | `/dns/servers` | agents acting as DNS servers: `[{agentId, hostname, state, software, version, running, zones: n, installTaskId}]` |
| POST | `/dns/servers` | `{agentId*}` → dispatch `install_dns_server` → `{agentId, taskId}` |

## 6. Domains — Cloudflare Registrar

| Method | Path | |
|---|---|---|
| GET | `/domains` | `[{name, registrar: "cloudflare", status, expiresAt, autoRenew, locked, privacy, nameServers, zoneId}]` from `accounts/{account}/registrar/domains` |
| GET | `/domains/{name}` | one |
| PATCH | `/domains/{name}` | `{autoRenew?, locked?, privacy?}` → the domain |

Cloudflare's API cannot **buy** a domain; registration and transfers happen in its
dashboard. Once registered (or once a zone is added and the registrar points at its
name servers) it is managed here.

## 7. Cloud — DigitalOcean

| Method | Path | |
|---|---|---|
| GET | `/cloud/account` | `{uuid, email, teamName, dropletLimit, volumeLimit, status, balance: {monthToDateUsage, accountBalance, monthToDateBalance, generatedAt}}` |
| GET | `/cloud/catalog` | `{regions: [{slug, name, available, features}], sizes: [{slug, description, memoryMb, vcpus, diskGb, transferTb, priceMonthly, priceHourly, regions, available}], images: [{id, slug, name, distribution, description}]}` — cached 1 h |
| GET | `/cloud/droplets?tag=` | `[Droplet]` |
| GET | `/cloud/droplets/{id}` | `Droplet` |
| POST | `/cloud/droplets` | `{name*, region*, size*, image*, tags?, vpcUuid?, sshKeys?, backups?, monitoring?=true, ipv6?, userData?, enrollAgent?: {hostgroup*, environment?, metadata?}}` → `201 {droplet, enrollmentTokenId?}` |
| POST | `/cloud/droplets/{id}/actions` | `{type*: power_on|power_off|shutdown|reboot|power_cycle|resize|snapshot|rebuild|rename|enable_backups|disable_backups, size?, disk?, name?, image?}` → `{action}` |
| GET | `/cloud/droplets/{id}/snapshots` | `[{id, name, sizeGb, createdAt, regions}]` |
| DELETE | `/cloud/droplets/{id}` | needs `X-Confirm: <droplet name>` → `204` |
| GET | `/cloud/actions/{id}` | `{id, type, status: in-progress|completed|errored, startedAt, completedAt, resourceId, resourceType}` |
| GET | `/cloud/volumes` | `[Volume]` |
| POST | `/cloud/volumes` | `{name*, region*, sizeGigabytes*, description?, filesystemType?="ext4" (ext4|xfs), filesystemLabel?, tags?}` → `201 Volume` |
| POST | `/cloud/volumes/{id}/actions` | `{type*: attach|detach|resize, dropletId?, sizeGigabytes?}` → `{action}` |
| POST | `/cloud/volumes/{id}/mount` | `{agentId*, mountPoint*}` → dispatch `mount_volume` → `{taskId}` |
| DELETE | `/cloud/volumes/{id}` | needs `X-Confirm: <volume name>` → `204` |
| GET | `/cloud/load-balancers` | `[LoadBalancer]` |
| GET | `/cloud/load-balancers/{id}` | `LoadBalancer` |
| POST | `/cloud/load-balancers` | `{name*, region*, sizeUnit?=1, forwardingRules*: [{entryProtocol, entryPort, targetProtocol, targetPort, certificateId?, tlsPassthrough?}], healthCheck?: {protocol, port, path, checkIntervalSeconds, responseTimeoutSeconds, healthyThreshold, unhealthyThreshold}, dropletIds? | tag?, vpcUuid?, redirectHttpToHttps?}` → `201` |
| PUT | `/cloud/load-balancers/{id}` | the same body (DigitalOcean replaces the whole definition) |
| POST | `/cloud/load-balancers/{id}/droplets` | `{dropletIds*}` → `204` |
| DELETE | `/cloud/load-balancers/{id}/droplets` | `{dropletIds*}` → `204` |
| DELETE | `/cloud/load-balancers/{id}` | needs `X-Confirm: <lb name>` → `204` |
| GET | `/cloud/firewalls` · `/cloud/vpcs` | read-only lists |

**Droplet** = `{id, name, status, region, size, image, memoryMb, vcpus, diskGb,
publicIpv4, privateIpv4, ipv6, vpcUuid, tags, volumeIds, features, priceMonthly,
createdAt, agentId}`. `agentId` is the agent on that droplet: the droplet tag
`lx-agent-<agentId>` when present, else an agent whose reported addresses include the
droplet's public or private IPv4.

**Volume** = `{id, name, region, sizeGigabytes, description, filesystemType,
filesystemLabel, dropletIds, tags, createdAt}`.

**LoadBalancer** = `{id, name, ip, status, region, sizeUnit, forwardingRules,
healthCheck, dropletIds, tag, vpcUuid, redirectHttpToHttps, createdAt}`.

`enrollAgent` mints an enrollment token (hostgroup, environment, metadata, 1 use,
24 h) and composes cloud-init `user_data` that installs and starts the agent:
`curl -fsSL $NEXUS_PUBLIC_URL/install/agent.sh | NEXUS_URL=$NEXUS_PUBLIC_URL ENROLLMENT_TOKEN=nxe_… sh`.
A caller-supplied `userData` runs after it. The droplet is tagged
`lx-hostgroup-<hostgroup>`. Without `NEXUS_PUBLIC_URL` an `enrollAgent` request is
`400` (the droplet could never call home).

## 8. Agent install

`GET /install/agent.sh` (public) — a POSIX script that downloads the agent for the
machine's architecture from `$NEXUS_PUBLIC_URL/install/rmm-agent-linux-<arch>` (served
from `LINEXUS_AGENT_BINARY_DIR`), writes `/etc/linexus/agent.env`
(`NEXUS_URL`, `ENROLLMENT_TOKEN`, `AGENT_STATE_FILE=/var/lib/linexus/agent-state.json`),
installs `linexus-agent.service` and starts it. Idempotent.

## 9. Operations log

Every provider mutation is a row (`provider_operations`: id, provider, operation,
target, requester, status `ok|failed`, error, idempotency key, response, created at)
and a Logger line (`source: "nexus.providers"`).

| GET | `/operations?provider=&limit=50` | newest first |
|---|---|---|

## 10. New plan steps (Orchestrator → agent)

| Intent | Steps | Params |
|---|---|---|
| `dns_zone_apply` | `dns.zone.apply` (critical) | `zone`, `content` (the whole zone file), `role` (`primary`/`secondary`), `primaries` (comma-separated IPs, secondaries only), `secondaries` (comma-separated IPs of the secondaries, primary only — for `allow-transfer` / `also-notify`; empty when there are none), `serial` |
| `dns_zone_remove` | `dns.zone.remove` | `zone` |
| `install_dns_server` | `dns.server.ensure` | — (the agent picks `bind9` on apt, `bind` on dnf; writes the include file; starts the service) |
| `mount_volume` | `disk.mount` (critical) | `device` (e.g. `/dev/disk/by-id/scsi-0DO_Volume_<name>`), `mountPoint`, `fsType`, `format` (`if_blank`/`never`) |
| `set_environment` | `agent.environment` (now executed and persisted by the agent) | as before |

`dns.zone.apply` writes `<zones dir>/db.<zone>` atomically, validates it with
`named-checkzone` before swapping it in (a zone that fails validation leaves the old
file in place and fails the step), maintains one `zone "<zone>" { … };` stanza per
zone in `<conf dir>/linexus-zones.conf` (included from `named.conf.local` /
`named.conf`), and runs `rndc reload <zone>` (or `reconfig` for a new zone). It is
idempotent: an unchanged file is `unchanged`. `disk.mount` never formats a device
that already carries a filesystem, writes an `fstab` line by UUID with `nofail`, and
mounts.

## 11. Implementation notes (Nexus)

What the Nexus implementation decided where the sections above leave room.
The Hub can rely on these.

**Wire conventions.** Absent strings are `""`, absent lists `[]`, absent
objects `{}`; absent *timestamps* are `null` (never `""`, so a Go `time.Time`
decodes): `revokedAt`, `factsAt`, `checkedAt`, `expiresAt` (domains),
`startedAt`/`completedAt` (actions). An absent DNS server on an agent is
`dnsServer: null`. `taskId` is **omitted** (not `null`) when a DNS change
dispatched nothing (Cloudflare). DigitalOcean ids keep DigitalOcean's types:
droplet, action, snapshot and image ids are **numbers**; volume, load balancer,
firewall and VPC ids are **strings** (UUIDs). Record `priority` is `null`
unless the record is an MX. Firewalls and VPCs are DigitalOcean's objects with
their keys camelCased.

**Enrollment (§1).** A used-up, expired, revoked or unknown token is `401`.
The `201` body is the full agent detail (as `GET /agents/{id}`) plus
`agentId`, `agentToken`, `enrollmentTokenId` (`""` on the system-key path),
`metadata` (the token's, `{}` without one) and `readopted`. The system-key path
also returns an `agentToken`. When the body carries `enrollmentToken`, the
bearer is ignored. On the token path, a `machineId` that belongs to an agent in
**another hostgroup** is `409` (a client's token cannot take over another
client's machine; re-enroll with a system key to move it) and spends no use.
Re-adoption picks the oldest agent with that machine id (cloned images can
share one). With an agent credential, a result for a task that does not target
that agent is `404`.

**Facts (§2).** A field missing from a report keeps its previous value;
non-array `interfaces` / `listening` / `services` / `packages`, a non-object
`dnsServer` and an unparseable `publicIp` are ignored. `factsAt` is stamped on
every report.

**Tasks (§3).** `POST /tasks/{id}/cancel` on an unknown id is `404`. A
`hostgroup:<name>` target that matches no agent contributes nothing (the task
is still created). Re-planning and cancelling are conditional updates, so a
cancelled task is never revived by a late plan.

**Providers (§4).** `POST /providers/{key}/test` is `424` without credentials;
a provider that refuses the token or cannot be reached answers `200` with
`ok: false` and `state` `unauthorized` / `unreachable` (and `detail`).
`scopes` (Cloudflare only) is `{tokenStatus, expiresOn, notBefore, accounts}`.
`bind` takes no credentials (`PUT` is `400`). When a Cloudflare token sees
several accounts and no `accountId` / `CLOUDFLARE_ACCOUNT_ID` is set, calls
that need an account are `424` rather than guessing. Outbound calls to the
providers use no proxy unless `NEXUS_PROVIDER_PROXY` names one.

**Errors from providers.** A provider `404` is our `404`, a provider "already
exists" (HTTP 409, Cloudflare codes 1061 / 81053 / 81057 / 81058) is our `409`,
and every other provider 4xx — including a refused token — is `422`; `401`
always means *our* caller's key.

**DNS (§5).** `ensure` with several records of that type and name, none of
which already has the content, is `409` (say which one to change by id).
`ensure` compares `ttl` / `proxied` only when the body carries them. For BIND
a CNAME cannot share its name with any other record (`409`), and `ttl: 1` is
`400`; a BIND record's default TTL is the zone's `defaultTtl`. TXT content is
stored without surrounding quotes; CAA is normalized to `flags tag "value"`.
A new change cancels the zone's previous `dns_zone_apply` tasks that no agent
has picked up yet (each apply carries the whole zone; a stale one must never
land after a newer one). `applyStatus` is `applied` when every task of the
latest change completed, `failed` when one failed or was cancelled, else
`pending`. `DELETE /dns/zones/{id}` answers `204`; the `dns_zone_remove` tasks
are visible in `/operations` and `GET /tasks`. In `GET /dns/servers`, `zones`
is the number of zones the agent **reported** serving.

**Idempotency (§ top).** A replay carries `Idempotent-Replayed: true`. Only
successful operations replay; a failed one can be retried with the same key.
Keys are scoped by provider + operation (same key, other operation → `409`).

**Cloud (§7).** Volume names are lowercase letters, digits and dashes
(starting with a letter) because they become the device path. `mountPoint`
must be absolute, plain characters, and not a system directory (`/`, `/etc`,
`/usr`, `/var`, `/proc/…`, …). `userData` is capped at 62 KiB (the enrollment
part needs the rest of DigitalOcean's 64 KiB). Caller `userData` is appended as
a second MIME part (so `#cloud-config` keeps working); without it `user_data`
is the plain enrollment script. If droplet creation fails after the token was
minted, the token is revoked. `GET /cloud/account` answers `balance: null`
when the token cannot read billing.

**Install (§8).** `NEXUS_URL` in the script defaults to `NEXUS_PUBLIC_URL`
when that is set. Only `rmm-agent-linux-amd64` and `-arm64` are served;
anything else is `404`.

**Operations (§9).** `GET /operations` rows are `{id, provider, operation,
target, requester, status, error, idempotencyKey, createdAt}` (`limit`
1…500). Operations: `credentials.put|delete`, `zone.create|delete`,
`record.create|update|delete|ensure`, `server.install`, `domain.update`,
`droplet.create|action|delete`, `volume.create|action|mount|delete`,
`load_balancer.create|update|add_droplets|remove_droplets|delete`.

