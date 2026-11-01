import json, struct, sys, time
from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey, X25519PublicKey
from cryptography.hazmat.primitives.kdf.hkdf import HKDF
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
import base64

cfg = json.load(open("/usr/local/etc/xray/config.json"))
pk = cfg["inbounds"][0]["streamSettings"]["realitySettings"]["privateKey"]
priv = X25519PrivateKey.from_private_bytes(base64.urlsafe_b64decode(pk + "=" * (-len(pk) % 4)))

data = open(sys.argv[1], "rb").read()
off, n = 24, 0
while off + 16 <= len(data) and n < 5:
    incl = struct.unpack("<I", data[off + 8:off + 12])[0]
    pkt = data[off + 16:off + 16 + incl]; off += 16 + incl
    ip = pkt[14:]; ihl = (ip[0] & 15) * 4; tcp = ip[ihl:]; doff = (tcp[12] >> 4) * 4
    rec = tcp[doff:]
    if len(rec) < 6 or rec[0] != 0x16 or rec[5] != 1:
        continue
    n += 1
    hs = bytearray(rec[5:5 + 4 + int.from_bytes(rec[6:9], "big")])
    random = bytes(hs[6:38]); sid_len = hs[38]; sid = bytes(hs[39:39 + sid_len])
    p = 39 + sid_len
    cs_len = int.from_bytes(hs[p:p + 2], "big"); p += 2 + cs_len
    p += 1 + hs[p]
    ext_end = p + 2 + int.from_bytes(hs[p:p + 2], "big"); p += 2
    peer = None
    while p < ext_end:
        et = int.from_bytes(hs[p:p + 2], "big"); el = int.from_bytes(hs[p + 2:p + 4], "big"); body = hs[p + 4:p + 4 + el]
        if et == 0x33:
            q = 2
            while q < len(body):
                g = int.from_bytes(body[q:q + 2], "big"); kl = int.from_bytes(body[q + 2:q + 4], "big")
                if g == 0x1d: peer = bytes(body[q + 4:q + 4 + kl])
                q += 4 + kl
        p += 4 + el
    print(f"hello#{n}: sid_len={sid_len} x25519_share={'yes' if peer else 'no'}")
    if not peer or sid_len != 32:
        continue
    shared = priv.exchange(X25519PublicKey.from_public_bytes(peer))
    key = HKDF(hashes.SHA256(), 32, random[:20], b"REALITY").derive(shared)
    aad = bytearray(hs); aad[39:39 + 32] = b"\x00" * 32
    try:
        pt = AESGCM(key).decrypt(random[20:], sid, bytes(aad))
        ver = ".".join(str(b) for b in pt[:3]); ts = int.from_bytes(pt[4:8], "big")
        print(f"  DECRYPT OK -> client_ver={ver} time_diff={int(time.time()) - ts}s shortId={pt[8:16].hex()}")
    except Exception as e:
        print(f"  DECRYPT FAILED ({type(e).__name__}) -> client public key / x25519 key does not match server")
