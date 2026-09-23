# `workflows[]` parity fixtures (WP-31 · Round-29 review fix)

`manifest_v5_parity.rs` walks the contract's own fixture folders under
`contract/src/__fixtures__/manifest-v5/` (or `$IKENGA_CONTRACT_DIR`) for the §2
/ §4 blocks. As of `@ikenga/contract` #43 that fixture set has **no
`workflows/` folder** — nothing there declares a §10 `workflows[]` block — so
these local fixtures stand in.

Verdict by folder, same contract as the contract-side set:

| folder     | expected verdict                                            |
|------------|-------------------------------------------------------------|
| `valid/`   | deserializes **and** `Package::validate` accepts            |
| `invalid/` | deserializes (serde is shape-only) **and** `validate` errors |

`workflows_fixtures_match_contract_verdicts` prefers the contract-side
`workflows/` folders when they appear, and falls back to these. **When
contract-side fixtures land, delete this folder** and let the parity test read
the canonical set.

The `valid/` fixture is the real
`ikenga-pkgs/packages/sidecars/local-store-etl/manifest.json` with a §10
`workflows[]` block whose handler addresses that manifest's own real
`POST /pkg/com.ikenga.local-store-etl/etl` route.
