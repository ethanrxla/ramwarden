#!/usr/bin/env python3
"""Assemble review materials; never mark publishing, signing or certification done."""
import argparse,importlib.util,json,pathlib,shutil,hashlib,subprocess
root=pathlib.Path(__file__).resolve().parent.parent
p=argparse.ArgumentParser();p.add_argument('--output',type=pathlib.Path,default=root/'dist/store-submission');p.add_argument('--screenshots',type=pathlib.Path,default=pathlib.Path('/tmp/warden-store-review'));args=p.parse_args()
version=json.loads((root/'extension/manifest.json').read_text())['version']
spec=importlib.util.spec_from_file_location('store_gate',root/'scripts/check-store-package.py');gate=importlib.util.module_from_spec(spec);spec.loader.exec_module(gate)
package=root/f'dist/ramwarden-chrome-{version}.zip';report=gate.validate(package)
chrome=args.output/'Chrome';microsoft=args.output/'Microsoft'
chrome.mkdir(parents=True,exist_ok=True);microsoft.mkdir(parents=True,exist_ok=True)
for file in [package,package.with_suffix('.zip.sha256'),root/'packaging/stores/chrome/listing.md']:
    shutil.copy2(file,chrome/file.name)
for name in ['0-browser-cleanup-1280x800.png','1-threat-pane-1280x800.png','small-promo-440x280.png','marquee-1400x560.png']:
    source=args.screenshots/name
    if not source.is_file():raise SystemExit('Run extension/tests/popup.cjs to generate required synthetic artwork first')
    shutil.copy2(source,chrome/name)
shutil.copy2(root/'extension/icons/icon128.png',chrome/'store-icon-128.png')
shutil.copy2(root/'packaging/stores/microsoft/listing.md',microsoft/'listing.md')
shutil.copy2(root/'packaging/stores/microsoft/product.json',microsoft/'product.json')
(microsoft/'NOT-AN-INSTALLER.txt').write_text('Windows offline installer source and packaging checks exist. No signed production EXE is supplied in this kit. Trusted signing, clean-machine lifecycle tests, feature parity and an immutable public HTTPS download are still required. Do not upload compiler syntax fixtures or unsigned candidates.\n')
(chrome/'UPLOAD-NOTES.txt').write_text('Upload only the ramwarden-chrome ZIP as the extension package. The other files are listing assets/notes. Synthetic demonstration data is labeled. Verify public privacy/support URLs and final disclosures before requesting review.\n')
report.update({'sourceCommit':subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip(),'chromeSubmitted':False,'microsoftSubmitted':False,'microsoftSignedInstallerIncluded':False,'remaining':['live public privacy/support/account URLs','publisher listing review','Windows trusted signing','Windows clean-machine installation/upgrade/uninstall and feature-parity tests','versioned public Windows download']})
(args.output/'submission-status.json').write_text(json.dumps(report,indent=2)+'\n')
(args.output/'README.txt').write_text('RamWarden store submission review kit\nChrome/: extension ZIP, checksum, screenshots, promo art, listing and privacy guidance.\nMicrosoft/: listing and current signing/install gates.\nNo store submission or publication has been performed.\n')
print(args.output)
