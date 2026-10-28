# updraft

updraft is a Rust CLI for Roblox Open Cloud. Coding agents and Node scripts can call all 299 API-key operations in the pinned Roblox spec, plus API-key introspection and public place-to-universe lookup. Four commands handle asset upload, place publishing, asset download, and Luau execution.

Each command writes one JSON document to stdout. Progress goes to stderr. `--help` and `--version` print plain text. Nothing prompts for input.

## Install with rokit

Add updraft to your project's `rokit.toml`.

```toml
[tools]
updraft = "Trio-Forge-Studio/updraft@0.1.0"
```

Run `rokit install`, then `updraft --version`. The release assets support Windows x86_64, Linux x86_64 with musl, and macOS aarch64. Version 0.1.0 becomes installable after its tag and release are published.

To build from source, use the toolchain pinned in `rust-toolchain.toml`.

```console
cargo build --release --locked --package updraft
```

## Configure a key

For CI, set `UPDRAFT_API_KEY` in the command's environment. Keys never belong in command arguments.

Local profiles live in `%APPDATA%\updraft\config.toml` on Windows. On Unix, the file is `$XDG_CONFIG_HOME/updraft/config.toml`, or `~/.config/updraft/config.toml`. `UPDRAFT_CONFIG` selects another file.

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

A profile requires exactly one of `key_env` and `key_file`. Its optional `group` or `user` sets the default asset creator. Put `default_profile` before the profile tables, as shown above.

The selection order is `--profile`, `UPDRAFT_PROFILE`, `UPDRAFT_API_KEY`, then `default_profile`. A missing or empty selected source fails with exit 3. updraft never falls through to another source or retries with another key. Public calls send no key. Auth errors include the configured profile names and the operation's declared scopes. Missing scope metadata appears as `requiredScopes: null` and `scopeMetadata: "unknown"`.

## Find and describe operations

Find operations with words from their name, path, scopes, or documentation.

```console
updraft find luau logs
updraft describe universes.create-place-version
```

`describe` returns parameters, body media, schema outlines, scopes, safety, retry policy, pagination, completion, stability, and overlay notes. Names remain attached to their operations across spec updates.

## Call an operation

Parameters use `name=value`. `assetId`, `asset_id`, and `asset-id` bind the same parameter. Repeat array parameters as `ids=1 ids=2`. Required parameters and value kinds are checked before sending. JSON bodies are checked for syntax, without schema validation.

```console
updraft call assets.get assetId=1234567 readMask=assetType,displayName
updraft call GET '/assets/v1/assets/1234567?readMask=displayName'
updraft call universes.get-place-universe placeId=1818
updraft call assets.list-versions assetId=1234567 --all --max-items 50
updraft call universes.create-place-version universeId=13058 placeId=1818 versionType=Published --body @build/warehouse.rbxl
updraft call assets.create --part request=@request.json --part 'fileContent=@sofa.fbx;type=model/fbx' --no-wait
```

`--body` accepts inline JSON, `@file`, or `@-` for stdin. Binary operations accept the file's bytes. Multipart operations use repeated `--part name=value` arguments. Use `@@` when a literal starts with `@`. A process can consume stdin once.

`--all` follows pagination and returns one page-shaped document. `--max-pages` defaults to 100 and retains the next token when the cap is reached. `--max-items` bounds its item count and retains the next token when it stops early. updraft keeps the endpoint's default page size and reduces a declared size when the remaining item limit needs it. If a default page is too large, the outcome table permits one smaller-page read. A server that ignores the requested size produces exit 2. An unsafe operation is never re-sent for this adjustment.

Long-running calls poll by default. `--no-wait` returns the first reply. A pending result includes a GET-only command in `error.resume`. GET readers wait for terminal state by default too.

## Check credentials

```console
updraft auth check
updraft auth check --for assets.get
updraft auth profiles
```

`call api-keys.introspect` and `auth check` use the same executor path. It supplies the selected key as the `apiKey` body field and rejects caller-supplied bodies. `auth check` posts the selected key to `/api-keys/v1/introspect`. It flattens each scope's `name` and `operations` into strings such as `asset:read`. `--for` checks that the returned list contains the operation's declared scope names. Resource restrictions still apply on Roblox. `auth profiles` lists sources and whether their keys are present, without printing their values.

## Upload an asset

```console
updraft asset upload assets/sofa.fbx --group 35 --description 'Model for Fragile Delivery.'
updraft asset upload --resume 9f1c
```

Upload sends one create request, then polls for the asset ID. It never repeats the create request. A 5xx, lost response, malformed success body, or missing operation ID exits 5 and asks you to check the group's Creations. Rejections and unknown outcomes have different exit codes.

`--resume` accepts an operation ID or `operations/<id>` and sends GET requests only. It needs no file or creator. The default poll deadline is 300 seconds. After acceptance, a deadline, poll failure, or invalid asset receipt exits 8 with the resume command. A GET-only resume retains the read failure codes.

The display name defaults to the file stem in title case. `--name` overrides it. `--type` accepts `model`, `decal`, `audio`, or `video`; the default comes from the file extension. Files must be nonempty regular files of at most 20,000,000 bytes. Supported extensions are FBX, glTF, GLB, RBXM, PNG, JPG, JPEG, BMP, TGA, OGG, MP3, WAV, FLAC, and MP4. Use `--user` for individual ownership, or configure a creator in the selected profile.

## Publish a place

```console
updraft --profile publish place publish build/warehouse.rbxl --universe 13058 --place 1818
```

Publishing sends an `application/octet-stream` body. It retries 409 and 5xx responses for at most four attempts, with 60 seconds between attempts by default. A later success resolves earlier uncertainty. If any attempt got a 5xx or lost its response and none later succeeded, the command exits 5, including a 503 followed by a 400. `--version-type saved` saves without publishing.

## Download an asset

```console
updraft asset download 1234567 --out build/sofa.rbxm
updraft asset download 1234567 --version 2 --out build/sofa.rbxm --overwrite
```

Asset delivery needs `legacy-asset:manage`. updraft fetches the returned URL with a separate client that never carries the API key. It accepts HTTPS URLs without credentials and follows at most five redirects. The download goes through a sibling temporary file, then a rename. Existing files require `--overwrite`. Failed transfers preserve the previous target. Downloads have a 30-second idle read timeout and no total duration cap. Upload and publish requests with file bodies get 600 seconds, while polls keep short timeouts.

## Run Luau

```console
updraft luau run scripts/probe.luau --universe 13058 --place 1818
updraft luau run --resume universes/13058/places/1818/versions/7/luau-execution-sessions/e1/tasks/t1
```

Use `-` instead of a file to read UTF-8 Luau from stdin. Scripts are limited to 4 MB. `--version` selects a place version. `--timeout` sets Roblox's execution timeout in seconds.

updraft creates once, polls to COMPLETE, FAILED, or CANCELLED, then reads up to 100 log pages. Results come from `output.results`. FAILED and CANCELLED exit 6 with logs. If reading logs fails or exceeds 100 pages, the result includes `logsComplete: false`, the read error, and a stderr warning. A COMPLETE task still exits 0. An accepted task with an unmatched path exits 5 and retains the path without a resume command. A resume requires exactly one matching GET reader. A missing task identity for unfinished work exits 5. Poll failures retain `updraft luau run --resume <task path>` in `error.resume`. Resume sends GETs only, waits for terminal state, reads logs, and exits 6 on FAILED or CANCELLED.

## Exit codes

| Exit | `error.kind` | Meaning | Next action |
| --- | --- | --- | --- |
| 0 | | Success. | Read the document. |
| 2 | `usage` | Invalid arguments, input, local file, or read-result limits. | Fix the input. |
| 3 | `auth` | Missing key, HTTP 401 or 403, or failed credential check. | Fix the selected key or profile. |
| 4 | `rejected` | Another 4xx response. | Fix the request before repeating it. |
| 5 | `outcome_unknown` | A write may have acted. | Check the remote resource before repeating it. |
| 6 | `remote_failure` | An operation or Luau task failed. | Inspect the error and logs. |
| 7 | `unavailable` | Unprocessed, unavailable, or unreadable response. | Retry or resume a read. |
| 8 | `pending` | Accepted work is unfinished or a later poll or receipt check failed. | Run `error.resume`. |

An unsafe request answered with 429, or one that never left the process, is sent once and exits 7. The caller decides whether to repeat it. Read and idempotent operations retry 429, 5xx, and transport failures up to three attempts. Their default delays are 1 and 4 seconds. `Retry-After` in seconds takes precedence up to 300 seconds. A larger value stops retries and appears in `error.retryAfter`. Place publishing uses its overlay policy.

Once an unsafe request gets a 2xx, every later failure maps to 6 for remote failure, 8 with GET-only recovery when the resource is known, or 5 with a Creations check when it is not. Repeating the original write is unsafe. GET-only resumes retain codes 3, 4, and 7. A classified failure keeps its exit code if stdout cannot be written.

The global options are `--profile`, `--deadline <seconds>`, `--poll-interval <seconds>`, `--retry-delay <seconds>`, and `--verbose`. The default poll interval is 2 seconds. `--retry-delay` changes delays, without making an unsafe operation retryable.

## Call from Node

```javascript
import { spawnSync } from "node:child_process";

const result = spawnSync("updraft", ["asset", "upload", "assets/sofa.fbx", "--group", "35"], {
	encoding: "utf8",
	env: process.env,
});
if (result.error) throw result.error;
const document = JSON.parse(result.stdout);
if (result.status === 5) throw new Error(document.error.hint);
if (result.status !== 0) throw new Error(JSON.stringify(document.error));
console.log(document.assetId);
```

## Update the spec

The checked-in source is `spec/openapi.json` from `Roblox/creator-docs` commit `46be165f526ec23305394ac0c56a95edd2d7f7d9`. `spec/overlay.toml` contains guarded corrections and the two added operations. Every command reads the generated `spec/registry.json`. The binary embeds that registry and loads no spec at runtime.

```console
cargo xtask spec update
cargo xtask spec check
```

`spec update` fetches the latest commit that changed Roblox's OpenAPI file, downloads that revision, rebuilds the registry, and prints JSON lists of added, removed, and changed operations. Review both spec files in git. An overlay expectation mismatch fails the build and requires a review of the upstream change.

After an overlay edit, run `cargo xtask spec build`. Keep the previous registry when updating so surviving operations retain their names. The weekly workflow updates, builds, and tests on Ubuntu and Windows with read-only repository permissions and no persisted checkout credentials. A separate write job creates the update PR from the verified files, with the operation diff and both verification results in its body. The commits API uses `GITHUB_TOKEN` when present. Bot PRs made with that token do not trigger CI, so the workflow's own verification job is the check. It opens the PR even if the updated-spec tests fail, then marks the workflow as failed. Overlay build failures stop the update before a PR.

## Verify changes

```console
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo xtask spec check
```

The behavior tests drive the real binary against local HTTP servers. `UPDRAFT_ORIGIN` overrides Roblox origins for these tests and always prints a warning. In that mode, the download client also permits HTTP loopback URLs. It still sends no key to a download URL. API requests never follow redirects.

The live suite is ignored by default. Set `ROBLOX_ASSETS_API_KEY` and run its read-only tests explicitly.

```console
cargo test --test live read_only_ -- --ignored
```

`UPDRAFT_LIVE_ASSET_ID` and `UPDRAFT_LIVE_PLACE_ID` override the read fixtures. The upload test additionally needs `ROBLOX_GROUP_ID` and the user's approval before setting `UPDRAFT_ALLOW_LIVE_UPLOAD=1` and running `cargo test --test live -- --ignored`. That test creates one 1x1 PNG decal and archives it. It records the operation in `build/live-upload-receipt.json`, resolves pending work during cleanup, archives any known created asset after an assertion failure, and verifies the archive. Unknown outcomes or failed cleanup require a manual Creations check or archival of the reported asset ID.
