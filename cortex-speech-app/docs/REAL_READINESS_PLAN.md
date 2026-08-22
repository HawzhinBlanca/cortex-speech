# Cortex — Superseded Historical Readiness Snapshot

> [!WARNING]
> **Do not execute the old plan from this document.** It predated the owner’s champion-only canon
> and proposed Scribe/smaller-model production paths that are now explicitly retired. Current
> authority is [`../../docs/OWNER_CANON.md`](../../docs/OWNER_CANON.md), followed by
> [`../CLAUDE.md`](../CLAUDE.md) and [`../ARCHITECTURE.md`](../ARCHITECTURE.md).

This file is retained to preserve the provenance of an early readiness correction: tests alone were
being described as “10/10” even though product flow and real-audio quality had not been observed.
That honesty lesson remains binding; its old engine plan does not.

## Historical evidence (not operational instructions)

- An early snapshot found a weak local CTC path active while the owner’s 7B service was not yet wired.
- A dated live probe found that ElevenLabs Scribe could return Sorani text, with duplicated output.
  That measurement remains historical evidence only. The owner later rejected Scribe; its client,
  key, consent, IPC, UI, and live test are removed from the shipped app.
- OpenRouter refinement and advisory Gemini judging were separately tested. They never authorize
  cloud ASR or a drafting fallback.
- The snapshot correctly required real-audio evidence, confidence intervals for accuracy claims, a
  usable correction flow, visible provenance, crash recovery, and honest failure reporting.

## Current operational readiness law

1. The pinned fine-tuned OmniASR-7B WSL champion drafts every production clip.
2. Wrong identity, unavailability, decode failure, or transcription failure is a hard stop. There is
   no smaller-model, MMS, Scribe, cloud, or “best available” fallback.
3. Production model management exposes only Silero VAD, CAM++ speaker embedding, and the denoiser.
   Optional ASR engines remain explicit offline diagnostics and cannot write production drafts.
4. Cloud LLM refinement or Gemini advisory judging stays default-off and requires its specific
   consent gate. There is no shipped cloud-ASR path.
5. A human reviewer alone accepts, rejects, or corrects a transcript. Tests prove implementation
   contracts; real serving-path probes and real-audio measurements prove operational claims.
6. “10/10” requires the complete current release gate and production evidence. A partial green test
   set is never enough.

## Current next-step sources

- Owner non-negotiables: [`../../docs/OWNER_CANON.md`](../../docs/OWNER_CANON.md)
- Architecture and runtime boundaries: [`../ARCHITECTURE.md`](../ARCHITECTURE.md)
- Release verification: [`RELEASE.md`](RELEASE.md)
