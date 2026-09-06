import csv
from pathlib import Path
import tempfile
import unittest
from partition_tts_metadata import partition


class PartitionTests(unittest.TestCase):
    def test_whole_duplicate_group_quarantined_and_originals_unchanged(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            folder=root/'wavs'
            folder.mkdir()
            for name in ['a.wav', 'b.wav']:
                (folder/name).write_bytes(b'opaque bytes, not a quality certificate')
            metadata=root/'metadata.csv'
            metadata.write_text('audio_path|similarity_score|source_file\nwavs/a.wav|.9|one\nwavs/a.wav|.8|two\nwavs/b.wav|.7|three\nwavs/missing.wav|.9|four\n',encoding='utf-8')
            before=metadata.read_bytes()
            result=partition(folder,root/'audit')
            self.assertEqual((result['input_rows'],result['unambiguous_rows'],result['quarantine_rows']),(4,1,3))
            self.assertEqual(result['gold_qualified_rows'],0)
            self.assertEqual(metadata.read_bytes(),before)
            with (root/'audit/quarantine_metadata.csv').open(encoding='utf-8-sig') as source:
                rows=list(csv.DictReader(source,delimiter='|'))
            self.assertEqual([row['audit_source_line'] for row in rows],['2','3','5'])
            self.assertTrue(all(row['audit_gold_eligible']=='False' for row in rows))
            with self.assertRaises(FileExistsError):
                partition(folder,root/'audit')

    def test_alias_duplicate_and_traversal_are_held(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            folder=root/'wavs'
            folder.mkdir()
            (folder/'a.wav').write_bytes(b'test')
            (root/'metadata.csv').write_text('audio_path|similarity_score\nwavs/a.wav|.9\nwavs/../wavs/a.wav|.8\n../outside.wav|.7\n',encoding='utf-8')
            result=partition(folder,root/'audit')
            self.assertEqual(result['quarantine_rows'],3)
            self.assertEqual(result['unambiguous_rows'],0)

    def test_malformed_header_cannot_mint_an_empty_certificate(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            folder=root/'wavs'
            folder.mkdir()
            (root/'metadata.csv').write_text('speaker|speaker\nA|B\n',encoding='utf-8')
            with self.assertRaises(ValueError):
                partition(folder,root/'audit')
            self.assertFalse((root/'audit').exists())


if __name__ == '__main__':
    unittest.main()
