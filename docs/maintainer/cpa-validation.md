[简体中文](cpa-validation.zh-CN.md)

# Evidence for the CPA architecture decision

Date: 2026-10-02. These findings support the [CPA foundation](../architecture.md), not production acceptance or support for every provider. The user has ended further experiments; synchronous integration belongs to implementation.

## Frozen environment and evidence

Official CPA **v8.0.10** Windows amd64 binary, tag commit `6fecc6e5567912661654a4eaf9b8f5436facd1c2`. Release archive SHA256: `d73c3b78faeb9d4c6e80890f0242146557a5032dfbb788474d06cdb64544b325`; actual binary: `01d0523c19e6a659a6b8b89f91c3bc72a8e45d733ac3de6d680346792bf09f0e`.

Two synthetic credentials, isolated configuration/auth directories, and a loopback mock GOAT upstream. Every configured inference destination was the mock; this is not packet capture or proof of zero outbound networking. CPA may fetch official model catalogs.

- [Default-behavior harness](../../scripts/cpa-goat-acceptance.mjs): 22 scenarios, 11 matching expectations and 11 differing. Expectations combine former OCG guarantees and desired GOAT precise quotas; differences are not a CPA defect score.
- [Plugin harness](../../scripts/cpa-plugin-acceptance.mjs), [experimental probe](../../scripts/cpa-plugin-probe.c), and [build script](../../scripts/build-cpa-plugin-probe.ps1): an actually loaded C ABI DLL, with 19 completed assertions including assertions demonstrating unsafe behavior.
- Plugin SHA256: `2224a4cae4878cc1b2936c784eaee0f720faf57cfe5955bc55c525b2d2bbcbc9`; plugin harness: `1f8da7782f14e243fbecb69d6a6afa750e2233cdae645866f995f927d0ee934e`; that raw report: `27902c68f80e44ab8c3d6e85447c81ab0fe0686bd0f25e267005d446bf141c5f`.
- Default-behavior raw report SHA256: `9179c55e6115c2856977d98f96560a03a439ad538366e2ca1a6dd62399c3ada3`. Raw artifacts remain with this task's attachments; private host paths are not product paths.

The probe uses cJSON v1.7.19 (MIT) and Windows build tools, without adding product dependencies. Build output records download sources and dependency hashes. Process exit, endpoint closure, and mock shutdown have receipts; the plugin run used 20 processes including a restart.

## Observed capabilities and gaps

| Boundary | Default CPA / plugin observation | Design implication |
| --- | --- | --- |
| Protocols and streams | Chat SSE and Messages/Responses text conversion were covered; no B replay after visible output | Reuse CPA without extrapolating every protocol/tool/provider combination |
| Declared GOAT reset | Default cooling did not honor short/long declared reset times or preserve that Plan deadline across restart | Precise quotas need trustworthy evidence and explicit admission semantics |
| Credential/model scope | Plan 429 did not block another model on the same Key; ordinary 429 had Key isolation | Plan scope is distinct from default model cooling |
| Plugin learning | Failed usage includes status, body, and AuthID; after recording, A was blocked across models, restored after expiry, and blocked across restart | Lightweight extensions can carry part of the requirement without the former kernel |
| Asynchronous gap | A deliberate 2.5-second usage delay let another model reuse A before publication | Asynchronous observation alone cannot guarantee strict admission; not every normal request necessarily races |
| Uncertain results | Disconnect, lost HTTP 200 body, and pre-output empty stream triggered A→B even with `request-retry: 0` | Selective no-replay belongs inside CPA |
| Coarse limits | `max-retry-credentials: 1` or blocking second attempts stopped some replay but also removed explicit-429 failover | Not a complete product policy |
| Configured stop | HTTP 500 plus matching-body stop worked; the same approach did not stop tested disconnect/body-loss/empty-stream replay | Error rules do not cover all uncertain outcomes |
| Hook failure | Interceptor errors were skipped; an invalid scheduler AuthID fell back to native selection; returned scheduler errors and explicit Reject blocked sending | Mandatory quota enforcement cannot assume plugin failure blocks execution |
| Attempt records | Failed A and successful B had distinct execution/credential information correlated to the client request | Consume events for OCG logs without another sending ledger/retry loop |

## Extension points and unimplemented work

v8.0.10 provides request interception, schedulers, executors, usage, and completion notifications. Interception can participate before sends; completion is asynchronous observation. The SDK also exposes `Hook.OnResult` and `ResultPolicy`, but their suitability for the complete synchronous requirement was not implemented or runtime-tested.

See pinned [plugin interfaces](https://github.com/router-for-me/CLIProxyAPI/blob/v8.0.10/sdk/pluginapi/types.go), [lifecycle example](https://github.com/router-for-me/CLIProxyAPI/blob/v8.0.10/examples/plugin/request-lifecycle/README.md), and [SDK result interfaces](https://github.com/router-for-me/CLIProxyAPI/blob/v8.0.10/sdk/cliproxy/auth/conductor.go).

The target requires actual credential identity and execution facts synchronously before another attempt: publish trusted restrictions first, then decide whether credential failover is allowed. SDK hosting or a small CPA patch remain implementation options. The probe is not adopted as mandatory production policy.

## Coverage limits

No real Go/GOAT accounts, other operating systems, official quota APIs, account-shared quotas, multiprocess state, hot reload, credential rotation, or production storage security were validated. Mock bodies follow a known GOAT format without proving every live response does. Reset timestamps do not establish balances or remaining quota.

These findings support selecting CPA and reducing custom responsibilities. They do not mean migration, synchronous quotas, selective no-replay, or a complete CLI gateway has shipped.
