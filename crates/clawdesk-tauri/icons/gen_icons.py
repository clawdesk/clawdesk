import struct, zlib, shutil

def make_png(size):
    def chunk(ctype, data):
        c = ctype + data
        return struct.pack('>I', len(data)) + c + struct.pack('>I', zlib.crc32(c) & 0xffffffff)
    ihdr = struct.pack('>IIBBBBB', size, size, 8, 6, 0, 0, 0)
    row = b'\x00' + bytes([99, 102, 241, 255] * size)
    raw = row * size
    idat = zlib.compress(raw)
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', ihdr) + chunk(b'IDAT', idat) + chunk(b'IEND', b'')

for s in [32, 128]:
    with open(f'{s}x{s}.png', 'wb') as f:
        f.write(make_png(s))

with open('128x128@2x.png', 'wb') as f:
    f.write(make_png(256))

png32 = open('32x32.png', 'rb').read()
ico = struct.pack('<HHH', 0, 1, 1) + struct.pack('<BBBBHHII', 32, 32, 0, 0, 1, 32, len(png32), 22) + png32
with open('icon.ico', 'wb') as f:
    f.write(ico)

shutil.copy('128x128.png', 'icon.icns')
print('Icons created')
