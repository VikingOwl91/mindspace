#!/usr/bin/env python3
"""Embed the audited parser in the public single-file installer, or check parity."""
import argparse
from pathlib import Path
import sys
p=argparse.ArgumentParser(); p.add_argument('--check', action='store_true'); args=p.parse_args()
root=Path(__file__).resolve().parents[1]
module=(root/'scripts/installer_release_contract.py').read_text()
installer=root/'install.sh'; text=installer.read_text()
a=text.index('# BEGIN EMBEDDED RELEASE CONTRACT\n')+len('# BEGIN EMBEDDED RELEASE CONTRACT\n')
b=text.index('# END EMBEDDED RELEASE CONTRACT',a)
expected=text[:a]+module.rstrip()+'\n'+text[b:]
if args.check:
    if expected!=text: sys.exit('Embedded release parser differs; run python3 scripts/embed-installer-contract.py')
else: installer.write_text(expected)
