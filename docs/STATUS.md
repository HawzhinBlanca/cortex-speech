# Gate-status authority

This tracked page intentionally contains **no release verdict, gate tally, or Git SHA**.

The authoritative status is the `STATUS.md` inside a completed immutable proof run produced by:

```powershell
python scripts/verify_10.py --profile owner-product
```

That status file is listed by byte count and SHA-256 in the run’s `manifest.json`. The verifier
reconstructs the selected gate set, result order, verdict, exit code, source tree, working-copy
digest, event-journal sequence, registry hash, mandatory evidence classes, migration-catalog
identity, and complete artifact inventory. A separate `ProductAttestationV1` then binds that manifest
hash to the observed executable/release-artifact hashes, schema authority, known-defect digest, and
model-attestation reference. `latest-proof.json` is published last and hashes both roots. A missing,
stale, forged, partial, retried, evidence-omitting, artifact-substituted, or post-finalization-mutated
run is not a release authority.

Why this file is static: a tracked generated file that embeds commit **A** creates commit **B** when
it is committed, so it is stale by construction and makes repeated clean exact-commit proof runs
impossible. Proof-local status avoids that self-reference while keeping every claim reproducible.

Do not infer green status from this notice. Only a validated current-SHA proof manifest plus its
hash-linked product attestation can state a verdict. Required signing, VM, accessibility, usability,
pilot, and field evidence remains explicitly pending until class-specific validators exist and have
produced real artifacts; a hand-written `passed` flag is never accepted.
