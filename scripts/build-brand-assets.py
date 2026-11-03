#!/usr/bin/env python3
"""Package the canonical red pixel mascot. Wordmark is used only in the site header."""
from pathlib import Path
import xml.etree.ElementTree as ET
import io,struct,shutil,subprocess,tempfile
from PIL import Image,ImageDraw
root=Path(__file__).resolve().parent.parent;brand=root/'assets/brand'
svg=ET.parse(root/'web/assets/pets/ram.svg').getroot()
sprite=Image.new('RGBA',(48,48));draw=ImageDraw.Draw(sprite)
for r in svg.findall('{http://www.w3.org/2000/svg}rect'):
 x,y,w,h=[int(r.get(k,'1')) for k in ['x','y','width','height']]
 draw.rectangle((x,y,x+w-1,y+h-1),fill=r.get('fill'))
source=Image.new('RGBA',(64,64));source.alpha_composite(sprite,(8,8))
for size in [16,32,48,64,128,256,512,1024]:
 image=source.resize((size,size),Image.Resampling.NEAREST);image.save(brand/f'icon{size}.png')
 if size in [16,48,128]:image.save(root/f'extension/icons/icon{size}.png')
 if size==32:(brand/'icon32.rgba').write_bytes(image.tobytes())
if shutil.which('iconutil'):
 with tempfile.TemporaryDirectory() as tmp:
  iconset=Path(tmp)/'Warden.iconset';iconset.mkdir()
  for size in [16,32,128,256,512]:
   for scale in [1,2]:source.resize((size*scale,size*scale),Image.Resampling.NEAREST).save(iconset/f"icon_{size}x{size}{'@2x' if scale==2 else ''}.png")
  subprocess.run(['iconutil','-c','icns',str(iconset),'-o',str(brand/'RamWarden.icns')],check=True)
else:source.resize((1024,1024),Image.Resampling.NEAREST).save(brand/'RamWarden.icns',format='ICNS')
frames=[]
for size in [16,32,48,64,128,256]:
 buf=io.BytesIO();source.resize((size,size),Image.Resampling.NEAREST).save(buf,format='PNG');frames.append((size,buf.getvalue()))
offset=6+16*len(frames);entries=[]
for size,data in frames:
 entries.append(struct.pack('<BBBBHHII',size%256,size%256,0,0,1,32,len(data),offset));offset+=len(data)
(brand/'ramwarden.ico').write_bytes(struct.pack('<HHH',0,1,len(frames))+b''.join(entries)+b''.join(data for _,data in frames))
for name,size in [('icon.png',256),('ramwarden-mark.png',256),('logo.png',768)]:source.resize((size,size),Image.Resampling.NEAREST).save(root/'web/assets'/name)
# An explicit bitmap alphabet makes the WARDEN lockup match the sprite grid.
glyphs={'W':['10001','10001','10001','10101','10101','11011','10001'],'A':['01110','10001','10001','11111','10001','10001','10001'],'R':['11110','10001','10001','11110','10100','10010','10001'],'D':['11110','10001','10001','10001','10001','10001','11110'],'E':['11111','10000','10000','11110','10000','10000','11111'],'N':['10001','11001','11001','10101','10011','10011','10001']}
word=Image.new('RGBA',(64,64));word.alpha_composite(sprite,(8,0));d=ImageDraw.Draw(word)
for i,c in enumerate('WARDEN'):
 for y,row in enumerate(glyphs[c]):
  for x,on in enumerate(row):
   if on=='1':d.point((14+i*6+x,52+y),fill='#f4e8ef')
word.resize((256,256),Image.Resampling.NEAREST).save(root/'web/assets/warden-wordmark.png')
print('Built red pixel mascot icons and separate site-only WARDEN wordmark.')
