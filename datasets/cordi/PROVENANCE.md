# CORDI — Central Kurdish dialect corpus

Obtained 2026-08-11 for the `cordi-dialect-fairness` leg (charter item 53).

| field | value |
|---|---|
| source | https://github.com/sinaahmadi/CORDI |
| paper | Language and Speech Technology for Central Kurdish Varieties, LREC-COLING 2024 — https://aclanthology.org/2024.lrec-main.877/ |
| licence | **CC BY-SA 4.0** (attribution + share-alike; NOT non-commercial) |
| file | `cordi_segments.tar.gz` (served with a `.zip` name; it is gzip, magic `1f 8b`) |
| bytes | 4,227,178,003 (3.94 GiB) |
| sha256 | `ff0ea0cb42c3320a0d3f7dde0b5ae5a504164df39f48a49edaec53dbc9363d72` |
| entries | 186,739 (paper reports 186,038 utterances) |
| varieties | Sulaymaniyah, Sanandaj, Mahabad, Erbil + Standard Central Kurdish |
| content | `.ogg` utterance audio + JSON transcriptions, from 311 films/episodes, 100h+ |

**Usage in this repo:** evaluation only, `train_only` in the provenance ledger — the corpus is NOT
redistributed from here. Share-alike attaches to derivative *corpora*, so any published slice must
carry CC BY-SA 4.0 and cite the paper above.

**Why this and not AsoSoft:** AsoSoft publishes no LICENSE file, no terms text and no contact address
— only the phrase "research and non-commercial use" on a web page. There was nothing to verify and
nobody to ask, so the eval-licence leg was descoped (2026-08-11) and CORDI covers the dialect axis
under a real, checkable licence.
