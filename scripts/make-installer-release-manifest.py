#!/usr/bin/env python3
"""Create an unsigned future installer envelope from existing qualified source assets.

Does not generate signatures, overwrite files, modify input assets, or publish a release.
Sign the resulting envelope independently after reviewing it.
"""
import argparse
import importlib.util
import json
from pathlib import Path
import sys

spec = importlib.util.spec_from_file_location('installer_release_contract', Path(__file__).with_name('installer_release_contract.py'))
contract = importlib.util.module_from_spec(spec)
spec.loader.exec_module(contract)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--archive', required=True, type=Path)
    parser.add_argument('--manifest', required=True, type=Path)
    parser.add_argument('--archive-signature', required=True, type=Path)
    parser.add_argument('--manifest-signature', required=True, type=Path)
    parser.add_argument('--rust-version', required=True)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    try:
        manifest = contract.load_json(args.manifest, contract.MAX_MANIFEST)
        version = contract.normalize(manifest['version'])
        envelope = {'format': 'mindspace-release-v1', 'repository': 'VikingOwl91/mindspace',
                    'tag': 'v'+version, 'version': version, 'platform': 'linux-x86_64',
                    'recipe': 'cargo-source-no-embeddings-v1', 'rust_version': args.rust_version,
                    'source_commit': manifest['baseline']['commit'], 'source_tree': manifest['baseline']['tree'],
                    'source_manifest_format': 'mindspace-source-archive-v2', 'assets': {}}
        for role, path in [('archive', args.archive), ('archive_signature', args.archive_signature),
                           ('source_manifest', args.manifest), ('source_manifest_signature', args.manifest_signature)]:
            size, digest = contract.file_hash(path, contract.MAX_ARCHIVE)
            envelope['assets'][role] = {'filename': path.name, 'bytes': size, 'sha256': digest}
        contract.validate_contract(envelope, version)
        # Validate all source/tar bindings without writing to the intended output until checks pass.
        import tempfile
        with tempfile.TemporaryDirectory(prefix='mindspace-envelope-') as temp:
            path = Path(temp)/'mindspace-release.json'; path.write_text(json.dumps(envelope), encoding='utf-8')
            contract.source(path, args.manifest, args.archive)
        payload = json.dumps(envelope, sort_keys=True, indent=2)+'\n'
        contract.require(len(payload.encode('utf-8')) <= contract.MAX_ENVELOPE, 'release envelope exceeds its byte limit')
        with args.output.open('x', encoding='utf-8') as stream:
            stream.write(payload)
        print(f'Created unsigned envelope: {args.output}. Review and sign it; no assets were published.')
        return 0
    except (contract.ContractError, OSError, ValueError, TypeError, KeyError, EOFError, RecursionError, contract.zlib.error) as error:
        print(f'STOP: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
