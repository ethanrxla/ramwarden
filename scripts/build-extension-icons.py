#!/usr/bin/env python3
"""Render the existing pixel-grid SVG into store/toolbar icons; no image dependencies."""
from pathlib import Path
import struct,zlib,xml.etree.ElementTree as ET
ROOT=Path(__file__).resolve().parent.parent
rectangles=[]
for rect in ET.parse(ROOT/'web/assets/pets/ram.svg').getroot().findall('{http://www.w3.org/2000/svg}rect'):
    x,y,w,h=[int(rect.get(k,'1')) for k in ('x','y','width','height')]
    color=bytes.fromhex(rect.get('fill').removeprefix('#'))+b'\xff'
    rectangles.append((x,y,w,h,color))
x0=min(r[0] for r in rectangles);y0=min(r[1] for r in rectangles)
w=max(r[0]+r[2] for r in rectangles)-x0;h=max(r[1]+r[3] for r in rectangles)-y0
pixels={(x-x0,y-y0):color for rx,ry,rw,rh,color in rectangles for x in range(rx,rx+rw) for y in range(ry,ry+rh)}
def chunk(kind,data):return struct.pack('>I',len(data))+kind+data+struct.pack('>I',zlib.crc32(kind+data)&0xffffffff)
def png(size):
    # Store icon gets the recommended 16px inset; small toolbar icons maximize legibility.
    inset=16 if size==128 else (2 if size>=32 else 0)
    scale=(size-2*inset)/max(w,h);width=int(w*scale);height=int(h*scale)
    left=(size-width)//2;top=(size-height)//2;rows=[]
    for y in range(size):
        row=bytearray([0])
        for x in range(size):
            point=(int((x-left)/scale),int((y-top)/scale))
            row.extend(pixels.get(point,b'\0'*4) if left<=x<left+width and top<=y<top+height else b'\0'*4)
        rows.append(row)
    return b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',size,size,8,6,0,0,0))+chunk(b'IDAT',zlib.compress(b''.join(rows),9))+chunk(b'IEND',b'')
for size in (16,48,128):(ROOT/f'extension/icons/icon{size}.png').write_bytes(png(size))
frames=[(size,png(size)) for size in (16,32,48,64,128,256)]
offset=6+16*len(frames);entries=[]
for size,data in frames:
    entries.append(struct.pack('<BBBBHHII',size%256,size%256,0,0,1,32,len(data),offset));offset+=len(data)
(ROOT/'assets/brand/ramwarden.ico').write_bytes(struct.pack('<HHH',0,1,len(frames))+b''.join(entries)+b''.join(data for _,data in frames))
print('Rendered canonical red pixel mascot for Chrome and Windows icons.')
