#!/usr/bin/env python3
"""Separate ambiguous metadata without picking winners or touching original audio.

The unambiguous list is metadata triage, NOT a training/gold manifest. Every input
row appears exactly once in unambiguous_metadata.csv or quarantine_metadata.csv.
Duplicate path groups are quarantined whole, even when their scalar scores agree.
"""
import argparse
from collections import Counter
import csv
import hashlib
import json
from pathlib import Path


def sha256(path):
    value = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            value.update(chunk)
    return value.hexdigest()


def partition(folder: Path, output: Path):
    folder = folder.resolve(strict=True)
    metadata = folder.parent / 'metadata.csv'
    before = sha256(metadata)
    with metadata.open(encoding='utf-8-sig', newline='') as source:
        reader = csv.DictReader(source, delimiter='|')
        columns = reader.fieldnames
        if not columns or 'audio_path' not in columns or len(columns) != len(set(columns)):
            raise ValueError('Missing or duplicate metadata columns')
        rows = list(reader)
    reserved = {'audit_source_line', 'audit_reason', 'audit_audio_sha256', 'audit_gold_eligible'}
    if reserved.intersection(columns) or any(None in r or None in r.values() for r in rows):
        raise ValueError('Malformed metadata or reserved audit columns')
    keys = []
    for row in rows:
        relative = Path(row['audio_path'].replace('\\', '/'))
        keys.append(str((metadata.parent / relative).resolve()).casefold())
    duplicates = Counter(keys)
    accepted, held, stamps = [], [], {}
    for number, (row, key) in enumerate(zip(rows, keys), 2):
        relative = Path(row['audio_path'].replace('\\', '/'))
        path = (metadata.parent / relative).resolve()
        reason = ''
        if not row['audio_path'].strip() or relative.is_absolute() or path.parent != folder:
            reason = 'invalid_or_out_of_scope_audio_path'
        elif duplicates[key] > 1:
            reason = 'duplicate_audio_path_requires_source_adjudication'
        elif not path.is_file():
            reason = 'audio_file_missing'
        audio_hash = ''
        if not reason:
            stat = path.stat()
            audio_hash = sha256(path)
            after = path.stat()
            stamp = (stat.st_size, stat.st_mtime_ns)
            if stamp != (after.st_size, after.st_mtime_ns):
                raise ValueError('Audio changed during metadata binding')
            stamps[path] = stamp
        result = dict(row, audit_source_line=number, audit_reason=reason or 'metadata_unambiguous_NOT_gold',
                      audit_audio_sha256=audio_hash, audit_gold_eligible=False)
        (held if reason else accepted).append(result)
    if before != sha256(metadata):
        raise ValueError('Metadata changed during audit')
    if any((p.stat().st_size, p.stat().st_mtime_ns) != stamp for p, stamp in stamps.items()):
        raise ValueError('Audio changed during audit')
    if output.resolve() == folder or folder in output.resolve().parents:
        raise ValueError('Output must be outside the live audio folder')
    output.mkdir(parents=True, exist_ok=False)
    fieldnames = [*columns, 'audit_source_line', 'audit_reason', 'audit_audio_sha256', 'audit_gold_eligible']
    for name, selected in [('unambiguous_metadata.csv', accepted), ('quarantine_metadata.csv', held)]:
        with (output / name).open('x', encoding='utf-8-sig', newline='') as target:
            writer = csv.DictWriter(target, fieldnames=fieldnames, delimiter='|')
            writer.writeheader()
            writer.writerows(selected)
    assert len(accepted) + len(held) == len(rows)
    report = dict(schema=1, source_metadata=str(metadata), source_metadata_sha256=before,
                  audio_root=str(folder), input_rows=len(rows), unambiguous_rows=len(accepted),
                  quarantine_rows=len(held), gold_qualified_rows=0,
                  reasons=dict(Counter(row['audit_reason'] for row in held)),
                  files={name: sha256(output / name) for name in ['unambiguous_metadata.csv', 'quarantine_metadata.csv']},
                  limits=['No speaker/overlap/transcript or scalar-score correctness certification.',
                          'Original metadata and audio unchanged; this does not alter live pool membership.'])
    with (output / 'manifest.json').open('x', encoding='utf-8') as target:
        json.dump(report, target, indent=2)
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--folder', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(partition(args.folder, args.output), indent=2))
