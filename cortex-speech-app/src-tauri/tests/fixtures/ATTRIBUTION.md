# Test fixture attribution

## `fleurs_ckb_sample.wav` + `fleurs_ckb_sample.txt`

- **Source:** Google **FLEURS** (Few-shot Learning Evaluation of Universal Representations of Speech),
  config `ckb_iq` (Central Kurdish / Sorani), `test` split. One utterance, transcoded to 16 kHz mono WAV.
- **License:** **CC-BY-4.0** — https://creativecommons.org/licenses/by/4.0/
- **Attribution:** FLEURS, Conneau et al., 2022 (Google), dataset `google/fleurs` on the Hugging Face Hub.
- **Use here:** a small, redistributable, Arabic-script Sorani clip for the in-repo real-ASR gate
  (`tests/real_audio.rs::omniasr_on_committed_fleurs_ckb_fixture`). `.txt` is the verified transcript.

This is the only committed audio fixture with non-synthetic speech; all other test audio is generated
(`tests/fixtures/mod.rs`) or supplied out-of-repo. The user's primary corpus remains eval-only and is
never committed (see `DATA_GOVERNANCE.md`).
