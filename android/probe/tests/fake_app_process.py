#!/usr/bin/env python3
"""Finto `app_process` per testare il ponte scrcpy della sonda senza telefono.

Si comporta come il server di scrcpy 4.x/5.x in modalità tunnel_forward con solo il video
(protocollo: doc/develop.md di scrcpy): byte fittizio, nome del dispositivo da 64 byte,
id del codec, pacchetto di sessione da 12 byte, poi pacchetti media con intestazione da 12 byte.
Percorsi e comportamento si controllano con le variabili d'ambiente FAKE_DIR e FAKE_FRAMES.
"""
import os, socket, struct, sys, time

d = os.environ.get("FAKE_DIR", "/tmp/f0test")
frames = int(os.environ.get("FAKE_FRAMES", "50"))
open(f"{d}/fake.pid", "w").write(str(os.getpid()))
open(f"{d}/fake.args", "w").write(" ".join(sys.argv[1:]) + "\nCLASSPATH=" + os.environ.get("CLASSPATH", "") + "\n")
time.sleep(0.6)  # il vero server impiega circa un secondo ad aprire il socket
path = f"{d}/scrcpy.sock"
if os.path.exists(path):
    os.unlink(path)
srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
srv.bind(path)
srv.listen(1)
c, _ = srv.accept()

c.sendall(b"\x00")                                        # byte fittizio (tunnel forward)
c.sendall(b"POCO F4 GT".ljust(64, b"\0"))                 # nome del dispositivo
c.sendall(struct.pack(">I", 0x68323634))                  # codec id "h264"
c.sendall(struct.pack(">III", 0x80000000, 1080, 2400))    # pacchetto di sessione
cfg = b"\x00\x00\x00\x01\x67\x42\xc0\x1f\xda\x01\x10\x00\x00\x00\x01\x68\xce\x3c\x80"
c.sendall(struct.pack(">QI", 1 << 62, len(cfg)) + cfg)    # pacchetto di configurazione
for i in range(frames):
    nal = b"\x00\x00\x00\x01\x65" + bytes([i]) * 100
    key = (1 << 61) if i % 10 == 0 else 0
    c.sendall(struct.pack(">QI", key | (i * 8333), len(nal)) + nal)
    time.sleep(0.01)
time.sleep(30)
