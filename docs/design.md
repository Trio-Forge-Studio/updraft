# updraft design

## Problem

updraft must reach all 299 API-key operations in Roblox's OpenAPI document. It must also run four flows the spec can't express alone. Its callers are agents and Node scripts, so the contract is one JSON document on stdout and an exit code that says whether a repeat is safe. The spec is large (3.9 MB) and inconsistent. 108 operations have no `operationId`. The ids that exist mix three styles. Place publishing declares no body. Asset delivery declares no response. `listAssetVersions` declares a bare array but pages. Two templates collide (`tables/{assetId}` and `tables/{tableId}`). The two endpoints agents use most for checking keys and mapping places (`api-keys/v1/introspect` and `universes/v1/places/{placeId}/universe`) are missing entirely. The design has to survive weekly spec churn without renaming commands that scripts depend on. It also has to keep Fragile Delivery's upload rules: never re-POST, tell rejected from unknown, resume by operation id, and redact the key.

updraft makes the spec data. A build step compiles the spec plus a small hand-written overlay into a checked-in registry. The binary embeds the registry, and one executor interprets it for every request, including the four flows. Downloads use a separate key-free request path and the same outcome table.

## Usage (caller's view)

Install with rokit (`updraft = "Trio-Forge-Studio/updraft@0.1.0"`). Every command prints one JSON document on stdout and writes progress on stderr. `--help` and `--version` are the only plain-text output. `describe` is the machine-readable form of help. Roblox response bodies below are illustrative. Command lines, document shapes, `error.kind` values and exit codes are the contract.

| exit | `error.kind` | meaning | safe to repeat |
|---|---|---|---|
| 0 | | success | |
| 2 | `usage` | bad arguments, unknown operation, unreadable file, malformed JSON, or an unresizable read page. No unsafe request was accepted. | after fixing input |
| 3 | `auth` | no key, or HTTP 401/403. Prints `requiredScopes` and `keySource`. | after fixing the key |
| 4 | `rejected` | another 4xx. The request had no effect. | after fixing input |
| 5 | `outcome_unknown` | a non-idempotent request may have acted, or was accepted without a pollable identity | no. Check first. |
| 6 | `remote_failure` | the work ran and failed: an Operation error, a failed Luau script | depends on the work |
| 7 | `unavailable` | an unaccepted request, rate limit, or failure of a read, an idempotent request, or local output | yes |
| 8 | `pending` | accepted work is unfinished or a later poll or receipt check failed. `error.resume` is the command to continue. | run `resume` |

A `Retry-After` above 300 seconds stops retries and reports the requested value in `error.retryAfter`, preserving an integer outside the supported range as a string with the applicable exit code.

Once an unsafe request gets a 2xx, a command-wide acceptance record maps every later failure to 6 for remote failure, 8 with a GET-only resume when exactly one reader can poll the resource, or 5 with a Creations check when it is not. A GET-only resume keeps exit codes 3, 4, and 7. A classified failure keeps its exit code if stdout cannot be written. A COMPLETE Luau task whose logs cannot be read succeeds with `logsComplete: false` and a stderr warning. Task resource paths are validated before they enter a recovery command. `updraft luau run --resume <task path>` waits for terminal state, reads logs, and exits 6 for FAILED or CANCELLED. Generic `call` readers also wait by default, and `--no-wait` reads once.

updraft never sends an `unsafe` request twice on its own, even after a 429 or a refused connection. Exit 7 tells the caller that repeating is safe, and the caller decides. The one exception is an operation whose overlay entry has a retry policy, which today is only place publishing. If any of its attempts got a 5xx or lost its response and no later attempt succeeded, it exits 5, not 7 or 4.

### Past one-off calls

Read an asset's type and moderation state. Parameters are `name=value`. Names fold, so `assetId`, `asset_id` and `asset-id` all work.

```console
$ updraft call assets.get assetId=1234567 readMask=assetType,displayName,moderationResult,creationContext
{"path":"assets/1234567","assetId":"1234567","assetType":"Model","displayName":"Sofa","moderationResult":{"moderationState":"Approved"},"creationContext":{"creator":{"groupId":"35"}}}
exit 0
```

An agent holding a URL from the docs can skip the name. The same matcher resolves it.

```console
$ updraft call GET '/assets/v1/assets/1234567?readMask=displayName'
{"path":"assets/1234567","displayName":"Sofa"}
exit 0
```

Map a place to its universe. This operation comes from the overlay and sends no key.

```console
$ updraft call universes.get-place-universe placeId=1818
{"universeId":13058}
exit 0
```

Check that a key works and see what it can do. `call api-keys.introspect` and `auth check` use `Auth::BodyKey { field: "apiKey" }`. The executor supplies the selected key and rejects caller bodies before reading them. `auth check` sends the key to `POST /api-keys/v1/introspect`, which answers with scopes such as `{"name":"asset","operations":["read","write"]}` (measured on 2026-10-27). updraft flattens them to `asset:read` and `asset:write`.

```console
$ updraft auth check
{"keySource":"profile assets","name":"fd-assets","enabled":true,"expired":false,"scopes":["asset:read","asset:write"]}
exit 0

$ updraft auth check --for luau.create-task
{"error":{"kind":"auth","status":null,"message":"the key from profile assets lacks universe.place.luau-execution-session:write","requiredScopes":[{"name":"universe.place.luau-execution-session:write"}],"keySource":"profile assets","profiles":["assets","publish","luau"]}}
exit 3
```

Find and describe any operation.

```console
$ updraft find luau logs
[{"name":"luau.list-task-logs","method":"GET","path":"/cloud/v2/universes/{universe_id}/places/{place_id}/versions/{version_id}/luau-execution-sessions/{luau_execution_session_id}/tasks/{task_id}/logs","summary":"List Luau Execution Session Task Logs"}]
exit 0

$ updraft describe universes.create-place-version
{"name":"universes.create-place-version","method":"POST","url":"https://apis.roblox.com/universes/v1/{universeId}/places/{placeId}/versions",
 "params":[{"name":"universeId","in":"path","required":true,"type":"integer"},{"name":"placeId","in":"path","required":true,"type":"integer"},{"name":"versionType","in":"query","type":"string"}],
 "body":{"required":true,"type":"binary","contentType":"application/octet-stream"},
 "response":{"type":"object","fields":[{"name":"versionNumber","type":"integer"}]},
 "requiredScopes":[{"name":"universe-places:write","target":"v1"},{"name":"universe-places:write"}],
 "safety":"unsafe","retry":{"statuses":[409,"5xx"],"attempts":4,"delaySeconds":60},"legacy":false,"stability":"beta",
 "patches":["The spec declares no request body. The endpoint takes the place file as application/octet-stream and answers with JSON despite declaring text/plain."],
 "call":"updraft call universes.create-place-version universeId=<integer> placeId=<integer> --body @<file>"}
exit 0
```

A paged list follows pages with `--all` and prints one page-shaped document. `--max-pages` defaults to 100. Reaching either the page cap or `--max-items` preserves the continuation token. An overfull page that cannot be resized exits 2. A long-running operation waits by default. `--no-wait` prints the first reply.

```console
$ updraft call data-stores.list-entries universe_id=13058 data_store_id=Profiles --all --max-items 500
{"dataStoreEntries":[...500 items...],"nextPageToken":"abc"}
exit 0
```

### The four flows

Upload one file. The display name defaults to the file stem in title case. The group defaults to the profile's `group`.

```console
$ updraft asset upload assets/models/furniture/sofa.fbx --group 35 --description "Model for Fragile Delivery."
{"assetId":"12345678901","operationId":"9f1c","displayName":"Sofa","assetType":"Model"}
exit 0

# HTTP 503 or a reset connection on the POST. One request was sent.
{"error":{"kind":"outcome_unknown","message":"HTTP 503 unavailable","hint":"Check the group's Creations before uploading again. updraft never re-sends an upload."}}
exit 5

# Accepted, still processing after --deadline seconds.
{"error":{"kind":"pending","message":"operation 9f1c is not done after 300 s","resume":"updraft asset upload --resume 9f1c","detail":{"path":"operations/9f1c","done":false}}}
exit 8

$ updraft asset upload --resume 9f1c
{"assetId":"12345678901","operationId":"9f1c","displayName":null,"assetType":null}
exit 0
```

Publish a place. stderr shows `HTTP 409, retrying in 60 s (attempt 2 of 4)`.

```console
$ updraft place publish build/warehouse.rbxl --universe 13058 --place 1818
{"universeId":"13058","placeId":"1818","versionNumber":7,"versionType":"published"}
exit 0
```

Download an asset. The delivery JSON gives a `location`, and updraft fetches it without the key. It follows at most five redirects, all HTTPS, writes to a temporary file beside the target and renames it into place, and refuses to replace an existing file without `--overwrite`. Delivery needs the `legacy-asset:manage` scope. A key with only Assets read and write gets HTTP 403 (measured on 2026-10-27).

```console
$ updraft asset download 1234567 --out build/sofa.rbxm
{"assetId":"1234567","version":null,"file":"build/sofa.rbxm","bytes":48213}
exit 0
```

Run Luau from a file or stdin.

```console
$ updraft luau run scripts/probe.luau --universe 13058 --place 1818
{"task":"universes/13058/places/1818/versions/7/luau-execution-sessions/e1/tasks/t1","state":"COMPLETE","results":[42],"logs":["hello from probe"]}
exit 0

$ echo 'error("boom")' | updraft luau run - --universe 13058 --place 1818
{"error":{"kind":"remote_failure","message":"Script error: boom","detail":{"task":"universes/13058/places/1818/versions/7/luau-execution-sessions/e1/tasks/t2","state":"FAILED","logs":[]}}}
exit 6
```

### Keys

A key never appears in argv. Locally, profiles in `%APPDATA%\updraft\config.toml` name an environment variable or a file.

```toml
default_profile = "assets"

[profiles.assets]
key_env = "ROBLOX_ASSETS_API_KEY"
group = 35

[profiles.publish]
key_env = "ROBLOX_API_KEY"

[profiles.luau]
key_file = "C:/Users/Admin/.secrets/roblox-luau.key"

```

Precedence is `--profile`, then `UPDRAFT_PROFILE`, then `UPDRAFT_API_KEY`, then `default_profile`. A profile named explicitly that is missing or empty fails with exit 3. It never falls through to another key, and updraft never retries with a different key after a 401 or 403. Instead, the auth error lists the operation's required scopes and the configured profile names, so the caller can pick one. When the spec declares no scopes for an operation (7 of the 299), `requiredScopes` is `null` and `scopeMetadata` is `"unknown"`, never an empty list. Introspection requires no scopes, so its body-key auth reports `requiredScopes: []` with `scopeMetadata: "declared"`. CI sets one variable.

```yaml
- run: updraft place publish build/warehouse.rbxl --universe 13058 --place 1818
  env:
    UPDRAFT_API_KEY: ${{ secrets.ROBLOX_API_KEY }}
```

Fragile Delivery's `publish.mjs` and `upload-model.mjs` shrink to `spawnSync("updraft", [...])` and a switch on the exit code. `upload-model.test.mjs` moves into updraft as Rust tests.

## Shape

**Data first.** The registry is the one source every command reads (`crates/registry/src/model.rs`).

- `Registry { source, ops: Vec<Op> sorted by name, schemas: BTreeMap<String, Shape> }`.
- `Op` holds `name`, `key` (`METHOD /template`, the identity across spec updates), `method`, `server`, `path`, `params`, `body: Option<Body>`, `response: Payload`, `auth: Auth::{ApiKey{scopes}, BodyKey{field}, None}`, `stability`, `legacy`, `safety: Safety::{Read, Idempotent, Unsafe}`, `pagination`, `completion: Option<Completion::{Operation, State{field, success, failure}}>`, `retry`, `alternatives` and `patches`.
- `Shape` outlines schemas for `describe`. `allOf` is merged at build time and `$ref` stays a `Ref` into `schemas`, so 527 reachable schemas are stored once.

Each behavior the spec implies becomes a field, and each field has one reader. `safety` drives retries and exit 5. `completion` drives waiting. `pagination` drives `--all`. `retry` carries publish's 409 policy. `auth` drives key choice and the `requiredScopes` in exit 3. This is principle-model-the-domain. Branches on "is this the upload endpoint" don't exist outside the four flows.

**Build time, checked in.** `cargo xtask spec build` turns `spec/openapi.json` plus `spec/overlay.toml` plus the previous `spec/registry.json` into a new `spec/registry.json`. The binary embeds it with `include_str!`. The registry is checked in for three reasons. Names are history, and history can't be derived from the current spec. A PR that updates the spec shows a reviewable registry diff instead of a 3.9 MB JSON diff. The shipped binary contains no OpenAPI code. `cargo xtask spec check` runs in CI and fails when the registry is not what the build would write, which closes the hand-synced-list red flag. There is no runtime spec loading. A pinned rokit version always exposes the same surface, which matters more for scripts than freshness. The weekly spec workflow plus a tag makes freshness cheap.

**Names.** Every operation gets `<namespace>.<action>` from its method and path. Cloud v2 namespaces use the first resource family after universe and place scopes. A small alias table maps instances to `places`, Luau execution collections to `luau`, creator-store products to `creator-store`, and subscriptions below subscription products to `subscriptions`. Other APIs use their API namespace without `legacy-` or service suffixes. Every namespace uses the same action rule. Drop the namespace's own singular or plural noun and parent scopes followed by IDs when another resource follows. Apply the namespace vocabulary in `spec/overlay.toml`, whose `drop` and `rename` lists give each grouping noun one meaning across the namespace. Actions keep the HTTP-derived verb or custom `:verb`. Guarded per-operation overrides correct verbs or explain true irregular paths. Thus assets use `assets.get`, data-store entries use `data-stores.get-entry`, and universe reads use `universes.get`. Actions contain at most four words. Collision groups retain a distinguishing noun in the same position for every member. Only the replaced data-store APIs use `data-stores-v1` and `ordered-data-stores-v1` namespaces. When versions of the same operation coexist elsewhere, the newest keeps the plain action and older versions use an action suffix such as `followings.list-user-universes-v1`. All develop and followings operations share their API namespace. Before v0.1.0, rebuild names once without the previous registry. Later spec updates preserve assigned names. New cloud collections use their own namespace and appear in the build report.

The `operationId` stays searchable and appears in `describe`. Guarded overlay overrides correct misleading verbs, shorten irregular paths, and distinguish real collisions. Final names use meaningful resource distinctions. Older API versions use the explicit action suffix described above. Method, parameter, and numeric suffixes do not distinguish resources. **A name, once assigned, stays with its key** across spec updates. A removed key whose `operationId` reappears at another path keeps its name too. Overrides take priority over history. Before the first release, the resource-name migration rebuilds once to a fresh output file with the pinned source commit, then replaces the registry. Ordinary builds keep reading the previous registry, so this reset does not weaken future name stability. A rebuild with no spec or overlay change is a no-op.

The v1 and v2 `followings` and `develop` APIs share their namespaces. The two followings readers use `followings.list-user-universes-v1` and `followings.list-user-universes`. Develop operations have distinct actions and need no version suffix.

**Search.** `find` requires every query token to match the name, path, scopes, operation ID, tags, or documentation. File-bearing request bodies also match `upload`. Exact token matches rank above substrings, and name matches rank above other fields. Fewer unmatched name tokens and shorter names break ties before alphabetical order. Singular and plural resource words match as tokens. Unknown-operation suggestions still compare names by edit distance.

**The overlay is the fix surface.** `spec/overlay.toml` holds guarded behavior corrections, pre-release name overrides, and 2 additions. Publish gets its binary body, JSON response and retry policy. Delivery gets its `location` response. `listAssetVersions` gets its real page shape. Both Luau create operations and the task GET get a `State` completion. Operation-shaped GET responses get an `Operation` completion. Seven query and history POSTs become `read`. A patch whose key stops matching fails the build. Each patch also states what it expects upstream to still say, such as `expect.body = "absent"` for publish, and the build fails when upstream changes that field, so a stale correction can't silently override a real fix. Every patch carries a note that `describe` prints. Because fixes are data, `updraft call universes.create-place-version --body @file` publishes correctly and retries like `place publish`. The flows and generic calls can't drift.

**One matcher, three callers.** `path::Template` matches concrete paths against templates. It serves `call GET /url`, and it serves polling. An `Operation` reply carries `path` (`operations/9f1c`, `universes/1/memory-store/operations/x`), and `Registry::reader_of` finds the GET operation under the same API root whose template matches. A Luau task's `path` resolves the same way. Tied readers return an ambiguity error. The build verifies that each completion reader resolves exactly once to itself. So no poller link is stored, and a new LRO family in the spec works without code. The `one_matcher_serves_urls_and_poll_paths` test proves it against the real registry for assets, memory store, Luau, and the ambiguous localization pair.

**One executor.** `Executor::send(&Call)` owns registry requests. `Executor::download(url, file, overwrite)` delegates to a downloader that holds no key or registry operation. A `Call` has private fields, so it can only be built by `Call::new(op, preset, pairs, body)`. `Call::new` binds folded names to parameters, checks required ones and value kinds, and checks the body against `op.body`. So the executor trusts its input, per principle-boundary-discipline. `send` loops over a pure `judge(op, attempt, n) -> Verdict` whose table is the whole retry and exit-code policy. `send_all` follows pages. `finish` drives a `Completion`. `download` fetches a returned URL with a second client that never holds a key, follows at most five HTTPS redirects, and streams into a temporary file that it renames into place. Downloads have a 30-second idle read timeout and no total duration cap. Their retry sleeps stop at `--deadline`. CDN failures report `rejected` or `unavailable` without API-key metadata. Binary and multipart file requests get 600 seconds, while poll requests get at most 30 seconds and respect the remaining deadline. These methods own transport policy. The flows own input preparation, result receipts, and command-specific recovery. Luau also owns GET-only resumption and log collection.

**Keys.** `Secret` has no `Debug`, `Display` or `Serialize`. `Output` is the only writer to stdout and stderr and replaces every registered secret, so a key echoed by the server can't print. `auth::choose` is pure and picks a `KeySource` once per command from the flags, environment and config, so a create and its polls never use different keys. HTTP errors have a short status message and a parsed JSON `error.detail`, or text detail for a non-JSON body. Poll errors keep the last known resource in `error.last`. An accepted unsafe response with invalid JSON uses `invalid JSON response` as its message and stores the raw text in `error.detail`. Redaction also covers JSON slash escapes, Unicode escapes, and mixed-case percent escapes of every key byte, including letters, and errors strip signed query strings from URLs before they print. The executor disables redirects so the header can't follow a 3xx to another host.

**Interface depth.** The public surface is seven verbs (`find`, `describe`, `call`, `asset`, `place`, `luau`, `auth`) and five global flags. `call` hides parameter location, encoding, body media, multipart assembly, pagination, polling, retries and key choice behind `name=value` pairs. An agent learns the exit table and `find`, and has the whole API. Parameters as `name=value` instead of generated `--flags` mean an operation's parameters can never collide with updraft's own options, by construction.

**What it deliberately does not do.** It does not validate bodies against schemas (non-goal), load a spec at runtime, resend an `Unsafe` request (publish's overlay retry policy is the one exception), choose a key by matching scopes, or follow redirects with a key.

**Measured** (`build-report.json`). Reproduce with `rm spec/registry.json && cargo xtask spec build --spec <openapi.json> --commit 46be165f`, then `cargo test --release -p updraft -- --nocapture` for the parse time.

| fact | value |
|---|---|
| operations in spec / API-key / registry | 994 / 299 / 301 (2 added) |
| with / without `operationId` | 191 / 108 |
| older-version action suffixes | 1, `followings.list-user-universes-v1`, verified by a fresh registry build |
| templates ambiguous for URL matching | 1 pair (`localization-table/tables/{assetId}` vs `{tableId}`) |
| detected paginated / long-running | 38 / 18 (after overlay) |
| reachable schemas | 527 of 1412 |
| registry size, compact / pretty / gzip | 597 KB / 970 KB / 89 KB |
| build time | 0.23 s |
| embedded registry parse, release | 3.8 ms |
| resource name length median / max | 30 / 61 chars across all 301 operations |

An action suffix distinguishes the v1 followings reader from the v2 reader within the shared namespace. Guarded names distinguish query-based `toolbox.search-assets` from `toolbox.search-assets-body`, and `localization-tables.get-table` from `localization-tables.get-asset-table`. The localization URL templates still require an explicit operation name when their shapes tie. No action exceeds four words. The name lengths come from the rebuilt registry; the startup and size numbers remain measurements of the original sketch. The binary is 2.2 MB with todo bodies, so the shipped size is a guess of 3 to 4 MB.

**Release and CI.** `ci.yml` runs fmt, clippy with `-D warnings`, `cargo xtask spec check` and `cargo test` on Ubuntu and Windows, because both developers are on Windows and paths and config directories differ. `release.yml` triggers on `v*`, checks the tag equals the Cargo version, and builds `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-musl` (static, so any CI image works) and `aarch64-apple-darwin`. Each target runner executes the built binary with `--version` and `find asset` before packaging. It zips each as `updraft-<version>-<os>-<arch>.zip`, which rokit's keyword matching accepts. `spec.yml` updates, builds, and tests weekly on Ubuntu and Windows with read-only repository permissions and `persist-credentials: false`. Its actions are pinned to commit SHAs. The commits API receives `GITHUB_TOKEN`. A separate write job creates the PR from identical verified files and records the added, removed, and changed list plus both verification results. The PR body states that token-created bot PRs do not trigger CI and that this workflow's verification job is the check.

**Tests.** Pure units cover the naming rule, the overlay, pagination and completion detection, `judge`'s table, `Call::new` binding, `choose` precedence and redaction. One test checks every flow key resolves in the embedded registry. Behavior tests run the real binary against `httpmock` through `UPDRAFT_ORIGIN`, with `--poll-interval 0 --retry-delay 0` so retries and polls take no wall time. They port `upload-model.test.mjs` case by case: hit counts prove the POST is never re-sent, stdout and stderr are searched for the key, and each lost-response shape asserts exit 5. Publish asserts 4 calls on persistent 409. Download asserts the second request has no `x-api-key`. Luau asserts exit 6 with logs on FAILED. Each defect the design review found gets a test that fails on the defect: an unsafe POST answered with 429 or a refused connection is sent once and exits 7; publish answered 503 then 400 exits 5; an operation with no declared scopes reports `requiredScopes: null`; a patch whose `expect` no longer matches upstream fails the build. A coverage test fails when an operation whose response names an `Operation`-shaped schema has neither a completion nor an overlay note saying why it has none. A live smoke suite (`cargo test --test live -- --ignored`, local only) runs `auth check`, `assets.get` and `universes.get-place-universe`, uploads a 1x1 PNG decal, then archives it with `assets.archive`. Cleanup runs even when an assertion fails once an asset id is known, polls a pending operation first, and verifies the archive. If the upload outcome is unknown or cleanup fails, the test fails and prints the asset id or the Creations check to do by hand.

## Synthesis decision

Two runners designed opposite shapes from the same brief. Candidate A (Claude Opus) made operations data: a checked-in registry and one executor. Candidate B (Codex) generated Rust per operation: 24,201 checked-in lines with typed parameters and flows that call typed operations. This document is A, with B's strongest parts folded in.

The cross-judge (Codex) recommended B. It scored A higher on reader load (4 against 3) and B higher on write safety (4 against 2), agent contract and testability. The write-safety gap came from four concrete defects in A's sketch: the retry table resent unsafe POSTs after 429 or a refused connection, `Call` had public fields, overlay patches checked only that their operation existed, and a missing scope list became an empty one. Each defect sits in one place in A, so each fix is local, and each now has a test in the plan above. B's protection comes from typed generated code, at the cost of a generated diff on every spec update and two wait paths (generic `--wait` and the flows' own polling), which the red-flag list calls two ways to do one task. A keeps one executor and one policy table for every request. So A is the base.

Grafted from B:

- Publish keeps uncertainty across attempts. A 5xx or lost response with no later success exits 5.
- Downloads use a client that never holds a key, follow at most five HTTPS redirects, write through a temporary file and rename, and need `--overwrite` to replace a file.
- Key choice is explicit with no fallthrough, and no retry with another key. Missing scope metadata is `null` with `scopeMetadata: "unknown"`.
- Overlay patches carry `expect` guards so a stale correction fails the build.
- Redaction covers escaped forms of the key and strips signed query strings.
- The live smoke test's cleanup contract.

Rejected from B: per-location flags such as `--path-asset-id` (A's `name=value` can't collide with updraft's own flags), JSON-wrapped `--help`, and the transactional `spec update` with a journal and rollback (the update runs in a git checkout, and git is the rollback).

Rejected from A: choosing a profile by matching declared scopes. It depends on whether `x-roblox-scopes` means all-of or any-of, which nothing settles, and declared scopes drift from real keys.

## Tradeoffs accepted

- We accept resource-family aliases and guarded overrides in exchange for short operation names that agents can infer. Names omit scopes already required as parameters, and spec updates preserve assigned names.
- We accept a checked-in 970 KB generated file in exchange for stable names, reviewable surface diffs, and a binary with no OpenAPI parser.
- We accept that a new endpoint needs an updraft release in exchange for every pinned version exposing the same commands on every machine.
- We accept a curated `safety = "read"` list for query POSTs in exchange for retries that never repeat a write. An unlisted read POST just doesn't retry.
- We accept `name=value` parameters over per-operation `--flags` in exchange for zero collisions with updraft's options and no per-operation clap tree. `describe` replaces per-operation `--help`.
- We accept a one-file `asset upload` in exchange for one exit code per upload. Batch upload is a shell loop.
- We accept a 3.8 ms registry parse on every start in exchange for embedding plain JSON. A build script could cut it with a binary encoding if it ever shows up.
- We accept naming a profile for non-default keys in exchange for key choice that never guesses.
- We accept plain-text `--help` in exchange for clap's standard help. `describe` is the machine-readable form.

## Alternatives considered

- **Generated Rust per operation.** It catches flow-to-operation mismatches at compile time and gives typed bodies. It loses here because it exposes 299 generated commands and their types to every reader, and the generator still needs the same overlay and naming lock. A spec update then becomes a code diff, and the generic behaviors (retries, paging, waiting) are spread across generated call sites or a runtime anyway.
- **Embed the raw spec and interpret it at runtime.** It skips the build step. It loses because names would shift whenever a collision appears, overlay fixes would be applied on every start, and startup would parse 3.9 MB with `$ref` resolution instead of 0.6 MB of resolved data.
- **Names from `operationId` where present, derived otherwise.** Shorter for `cloud` names. It loses because it is two naming rules, `__Using_Universes_DataStores_Scopes` suffixes still need a collision scheme, and the 108 operations without ids would read differently from their neighbors.
- **A runtime spec cache (`updraft spec pull`).** It loses for the pinned-surface reason above.

## Open questions and risks

- Does the Luau create response's `path` always use the long form with `versions/` and `luau-execution-sessions/`? Only that form matches a GET template in the spec. The resource schema lists three other forms. An accepted task with an unmatched path exits 5 without a resume command. A resume is emitted only when exactly one GET reader matches the task path.
- Which hosts does asset delivery's `location` point to? A key with `legacy-asset:manage` is needed to see. The download client accepts any HTTPS host because it never holds a key.
- `UPDRAFT_ORIGIN` stays in release builds so tests drive the real binary, and a warning prints on stderr when it is set. Anyone who can set it can also read the key variables, so it grants nothing new.
- `describe` shows `legacy` and `stability` (125 of the 299 operations are experimental). `call` prints no warning for them.

Settled during synthesis:

- Publish keeps retrying 409. Fragile Delivery added the retry in commit `1c48505a9` because Roblox returns 409 while it is still saving a back-to-back upload, which the spec's description of 409 doesn't cover.
- The introspect response shape was measured on 2026-10-27. It returns `{name, operations, groupIds, userIds}` per scope.

## Next implementation step

Copy the sketch's registry crate, xtask and checked-in registry into the repo with the overlay `expect` guards and the scope `null` fix, then implement `outcome::judge` and `Executor::send` with the upload tests running, since every flow and generic call depends on that pair.
