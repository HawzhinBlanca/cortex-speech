# Owner-product proof inputs

This boundary prepares the three opt-in inputs that cannot be manufactured by the ordinary test
suite: real supported media, a real long-form MP3, and production-sized SQLite characterization
data. It does not certify those gates by itself. It makes their logical authorities hash-bound and
their bundle copies read-only/tamper-evident for a no-retry owner-product proof run. Windows
read-only attributes and deletion-deny ACLs are not WORM storage and are not described as physical
immutability. They make ordinary mutation fail and later tampering detectable; the same Windows
owner or an administrator can deliberately reset the ACLs and therefore must also invalidate the
proof that referenced the bundle. An outer ancestor may also permit the owner to rename the whole
container. Validation and attempt creation retain no-share-delete identity handles for pathname
continuity while they run; a consuming proof supervisor must do the same or revalidate immediately
before use. A returned path by itself is never certification evidence.

The checked-in authority is
[`scripts/owner_proof_input_contract.v1.json`](../scripts/owner_proof_input_contract.v1.json). It
contains logical names, aggregate database facts, and SHA-256 digests, but no private source path,
transcript, reviewer credential, or row content.

## Safety contract

- Every source is a direct regular file. A symlink, Windows reparse point, SQLite WAL/SHM/journal
  sidecar, wrong filename, wrong size, or wrong digest fails before publication.
- Sources under roaming Cortex AppData, the active immutable release tree, or any `snapshots`,
  `pinned`, or `snapshot_*` tree are refused.
- The output parent must already exist. `--output-root` names a new dedicated tool-owned container;
  its fixed evidence child is `bundle.v1`, its disposable work child is `verify-work`, and two
  canonical path-free transaction journals bind crash recovery to the exact release, final-name
  digest, filesystem identities, inventory, and pre-seal protected-DACL fingerprints.
  The whole container is published by one no-replace directory rename outside the Git worktree.
  Existing bytes are never overwritten. A repeated CLI call after a lost response accepts a
  preexisting result only after the full validation path succeeds and then reports
  `already-prepared`.
- Sources are hashed before, during, and after copying. Copied authorities are made read-only and
  rehashed again before the manifest is published. The dedicated container denies top-level
  additions and replacement of `bundle.v1`; content directories, manifest, and baseline files
  receive deletion-deny namespace seals. Validation proves those seals operationally before
  trusting any path. These are effective protected DACL guarantees, not WORM guarantees.
- Preparation and each UUID attempt use one deterministic, contract-reserved staging name plus a
  crash-released Windows named mutex. The owner journal is durable before any payload child is
  created; the exact ACL recovery plan is durable before the first deny ACE is applied. A takeover
  accepts only the recorded identity tree (or a hierarchy-valid subset left by an interrupted
  cleanup), removes only an exact observed cumulative tool-added Everyone deny state, verifies the
  recorded base DACL, and deletes by retained handles deepest-first. Unknown names, aliases,
  hardlinks, altered identities, altered DACLs, invalid journals, or a simultaneous final and
  staging name fail closed. Recovery tests cover empty/pre-plan staging; real subprocess-kill tests
  cover partial sealing, a second kill immediately after recovery deletes the owner journal,
  pre-rename, post-rename, and same-token attempt replay without leaving an unrecoverable orphan.
- The transaction journals are owner-local recovery metadata, not new proof-data authority and not
  WORM storage. Publication hashes and identity-checks them immediately before and after the root
  rename; later corruption makes validation/recovery refuse the tree. There is an explicitly scoped
  same-owner namespace window between releasing descendant locks and renaming the sealed root, so
  preparation still requires the documented trusted, quiescent owner session.
- No copied database authority is ever opened writable. The `owner_proof_db` helper holds stable
  identity-checked handles on the schema-60 authority and its parent, makes a detached in-memory
  copy, calls the real `Database::initialize` migration path in memory, serializes the migrated
  database, and writes those exact bytes through a freshly created exclusive output handle. The
  authority is rehashed and re-characterized afterward; sibling SQLite journals are refused and
  cannot be consulted by the detached immutable opener.
- The migrated scale baseline must retain exact segment and distinct-audio-path counts, reach schema
  70 with the exact release migration prefix/schema contract, pass quick/full/FK integrity, and
  retain zero sequential-campaign or flexible-pool authority.
- The schema-65 campaign-exact authority is inspected through a detached in-memory snapshot while
  stable identity-checked handles deny replacement. A valid sequential campaign must remain
  present. The preparer has no policy sanitization or authority deletion command.
- Each proof run receives new writable copies in a UUIDv4-named attempt. A repeated token returns
  the same attempt only after revalidating the path-bound prepare transaction and while the exact
  read-only attempt manifest plus every database byte still match their untouched initial state;
  once used or changed, it fails closed and can never be reset or overwritten.
  Attempt files remain writable but deletion-sealed so their identities cannot be swapped. The tool
  deliberately has no attempt-deletion command; retention/removal (including an explicit ACL reset)
  is an owner operation outside certification and invalidates any proof that depended on the bytes.

## Build and prepare

Run only from the exact clean release commit. Create an empty parent outside the repository,
AppData, release, and snapshot trees. Then run the preparer with the six exact authority sources.
Paths below are placeholders. Real source paths are necessarily supplied by the local command line,
so keep shell history and redirected CLI output private. They are never persisted in the bundle,
canonical manifest, evidence facts, or documentation. Destination and attempt paths are deliberately
returned to the invoking process because the verifier needs them.

```powershell
python cortex-speech-app/scripts/prepare_owner_proof_inputs.py prepare `
  --output-root <safe-parent>/owner-product-v1 `
  --media-mp4 <A1-0001_PODCAST-001.mp4> `
  --media-mov <A1-0001_PODCAST-001.mov> `
  --media-flac <Lamofull00086400_A01.flac> `
  --audiobook-mp3 <audiobook-long.mp3> `
  --scale-db <isolated-schema60-clone>/cortex-speech.db `
  --campaign-db <isolated-schema65-clone>/cortex-speech.db
```

The command refuses a dirty Git tree, then builds from a separate detached materialization of the
exact commit rather than the live checkout. It pins and hashes the Cargo, Rust, Git, MSVC, and
Windows SDK executable/runtime trees; retains no-write/no-delete handles on those trees while they
are used; removes inherited wrappers/build flags; and proves a real Rust-to-MSVC link before Cargo
runs. It vendors checksum-locked crates offline and builds in fresh isolated Cargo/target
directories. Existing toolchain entries are identity-locked, but this owner-local contract does not
claim to prevent the same owner from adding a previously absent DLL name to an installation tree;
the build therefore requires a trusted, quiescent local toolchain namespace. Every Git, Cargo,
rustc, linker-preflight, and database-helper process tree runs in a Windows kill-on-close Job Object,
so timeout or supervisor death terminates descendants. A deterministic contract-reserved scratch
directory under `verify-work` is safely reconciled after a dead owner; unknown siblings fail closed.
It also proves that the helper source is byte-identical to the Git blob, verifies the
embedded full Git SHA, binds the one tracked Cargo configuration while refusing alternate configs,
pins the schema-60/65/current reference fingerprints, and records the commit tree/build facts. A
caller cannot substitute a prebuilt helper: standalone validation performs a second exact-commit,
pinned-toolchain build and requires the raw executable size and SHA-256 to equal the bundled helper.
The reproducible-build protocol is recorded, but reproducibility is not certified until two real
unrelated-root builds produce identical raw bytes. Publication flushes the final parent after rename; a
flush failure is reported as durability-unknown and is reconciled only when a full repeat validation
succeeds and then flushes the identity-locked final parent immediately before acceptance. Attempt
replay applies the same rule to its locked attempts namespace.
The published bundle contains this shape:

```text
owner-product-v1/
  .owner-proof-owner.v1.json
  .owner-proof-seal-plan.v1.json
  bundle.v1/
    manifest.v1.json
    contract.v1.json
    media/
      A1-0001_PODCAST-001.mp4
      A1-0001_PODCAST-001.mov
      Lamofull00086400_A01.flac
    audiobook/
      audiobook-long.mp3
    db-authorities/
      scale-production-derived-schema60.db
      current-campaign-exact-schema65.db
    db-derived/
      scale-current-schema70.db
    tools/
      owner_proof_db.exe
      owner_proof_db.rs
    attempts/
      <uuid-v4>/
        .owner-proof-owner.v1.json
        .owner-proof-seal-plan.v1.json
        attempt-manifest.v1.json
        scale-work.db
        campaign-observation.db
  verify-work/
```

## Revalidate and create a run attempt

Validation rehashes every declared file, checks the exact inventory and canonical manifest, then
independently compares Python immutable-SQLite inspection with the bundled Rust helper. It also
proves the committed namespace seals before opening child paths:

```powershell
python cortex-speech-app/scripts/prepare_owner_proof_inputs.py validate `
  --bundle-root <safe-parent>/owner-product-v1/bundle.v1
```

Create a fresh attempt using a new lowercase UUIDv4:

```powershell
python cortex-speech-app/scripts/prepare_owner_proof_inputs.py attempt `
  --bundle-root <safe-parent>/owner-product-v1/bundle.v1 `
  --run-token <new-uuid-v4>
```

The JSON result supplies `CORTEX_OWNER_REAL_MEDIA_DIR`, `CORTEX_OWNER_AUDIOBOOK_MP3`, and
`CORTEX_OWNER_SCALE_DB`. The attempt also contains `campaign-observation.db`, which is reserved for
restore/migration characterization and proof that the generic exporter refuses campaign-bound data.
It is never substituted for the successful scale-export input, and campaign policy is never removed
to force a green result.

## Exact authorities

| Role | SHA-256 | Locked fact |
|---|---|---|
| MP4 | `34918275905f206a085ff4444422688b3ce849b32781040c9cb1e07187355a5f` | 320,901 bytes |
| MOV | `ab4483a3323624db4d39cae6dcb0cc8262cd066e4739936ea3585486452e2fbe` | 591,784 bytes |
| FLAC | `1358878b4a6d03f368fade40a9f0e5f43f912db075c809a2ab325246d9bbf5e2` | 31,525,964 bytes |
| Long MP3 | `c301c6c8dd09bb81e700f8e22cdf38b5d74f9a2a1420b70b3bf5e05cfaf12f36` | 14,397,549 bytes; 1,799,631 ms |
| Scale DB | `fe312b53386c51b3c725d75b6def86067c049a75ca757a4885a73a46d96c37ea` | schema 60; 30,373 segments; 14,715 distinct audio paths; no campaign authority |
| Campaign DB | `ccbbc66cb464db276d05a4d0d3c83e7baaef4732829eebb021f759a4242295e8` | schema 65; 43,774 segments; 27,921 distinct audio paths; campaign authority required |

The release contract also pins these complete normalized schema-catalog fingerprints (FTS shadow
SQL bodies are intentionally normalized while their catalog/name/table inventory remains bound): schema 60
`80e88a4f9b40ecba46aaee933c98dc7aea54fe8ae58ea3178354409188759cd0`, schema 65
`50b62000b8174323221c206bf747a1507cc5e88459bc16a25064ae09b06ecd66`, and current schema 70
`f542f433eb5f235369ed703d8231c9956f246a1e6470c7d1b46a79c29503257c`.

These digests identify the already discovered disposable inputs. They are not evidence that a future
copy still exists or is healthy; only a successful `prepare`, subsequent `validate`, and hash-bound
owner-product run establish that.
