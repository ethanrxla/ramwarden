#!/usr/bin/env python3
"""Package the approved artwork without redrawing it. Requires Pillow."""
from pathlib import Path
from PIL import Image
root=Path(__file__).resolve().parent.parent
brand=root/'assets/brand'
source=Image.open(brand/'ramwarden-white-eyes.png').convert('RGBA')
for size in [16,32,48,64,128,256,512,1024]:
    image=source.resize((size,size),Image.Resampling.LANCZOS)
    image.save(brand/f'icon{size}.png')
    if size in [16,48,128]:image.save(root/f'extension/icons/icon{size}.png')
    if size==32:(brand/'icon32.rgba').write_bytes(image.tobytes())
source.save(brand/'ramwarden.ico',sizes=[(16,16),(32,32),(48,48),(64,64),(128,128),(256,256)])
source.save(brand/'RamWarden.icns',format='ICNS')
source.resize((256,256),Image.Resampling.LANCZOS).save(root/'web/assets/icon.png')
source.resize((256,256),Image.Resampling.LANCZOS).save(root/'web/assets/ramwarden-mark.png')
source.resize((768,768),Image.Resampling.LANCZOS).save(root/'web/assets/logo.png')
print('Packaged the approved white-eyed logo for web, extension, Mac, Windows, and Linux.')
