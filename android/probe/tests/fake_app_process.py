#!/usr/bin/env python3
"""Finto `app_process` per provare il ponte scrcpy della sonda senza telefono.

Parla come il server di scrcpy 4.x/5.x in modalità tunnel_forward con solo il video
(protocollo: doc/develop.md di scrcpy): byte fittizio, nome del dispositivo da 64 byte, id del
codec, pacchetto di sessione da 12 byte, poi pacchetti media con intestazione da 12 byte.

Comportamento regolabile con variabili d'ambiente:
  FAKE_DIR           cartella per i file di stato (pid, argomenti, tempi)        [/tmp/f0test]
  FAKE_FRAMES        numero di frame da inviare; -1 = senza fine                  [50]
  FAKE_FPS           frame al secondo                                             [100]
  FAKE_FRAME_BYTES   dimensione di ogni frame (minimo 17)                         [100]
  FAKE_KEY_EVERY     un keyframe ogni N frame                                     [10]
  FAKE_SILENT_AFTER  dopo N frame smette di inviare ma tiene il socket aperto     [mai]
  FAKE_EXIT_NOW      termina subito con questo codice, come un server che fallisce
  FAKE_NO_FRAMES     invia l'handshake ma nessun frame
  FAKE_BAD_HEADER    invia un'intestazione con una lunghezza assurda

In ogni frame, subito dopo l'intestazione NAL, ci sono l'ora di invio (double, big endian) e il
numero progressivo: il client dei test li usa per misurare l'età dei frame e cercare i buchi.
Ogni invio è cronometrato: il tempo massimo speso in sendall() finisce in fake.maxsend. Se la sonda
smette di leggere, sendall() si blocca: è esattamente il blocco che farebbe congelare lo schermo.
"""
import os
import socket
import struct
import sys
import time

d = os.environ.get("FAKE_DIR", "/tmp/f0test")
frames = int(os.environ.get("FAKE_FRAMES", "50"))
fps = float(os.environ.get("FAKE_FPS", "100"))
fbytes = max(17, int(os.environ.get("FAKE_FRAME_BYTES", "100")))
key_every = int(os.environ.get("FAKE_KEY_EVERY", "10"))
silent_after = int(os.environ.get("FAKE_SILENT_AFTER", "-1"))
exit_now = os.environ.get("FAKE_EXIT_NOW")
no_frames = os.environ.get("FAKE_NO_FRAMES") == "1"
bad_header = os.environ.get("FAKE_BAD_HEADER") == "1"

with open(f"{d}/fake.pid", "w") as f:
    f.write(str(os.getpid()))
with open(f"{d}/fake.pids", "a") as f:
    f.write(str(os.getpid()) + "\n")
with open(f"{d}/fake.args", "w") as f:
    f.write(" ".join(sys.argv[1:]) + "\nCLASSPATH=" + os.environ.get("CLASSPATH", "") + "\n")
if exit_now is not None:
    print("[server] ERROR: finto errore di avvio")
    sys.exit(int(exit_now))

time.sleep(0.4)  # il vero server impiega circa un secondo ad aprire il socket
path = f"{d}/scrcpy.sock"
if os.path.exists(path):
    os.unlink(path)
srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
srv.bind(path)
srv.listen(1)
conn, _ = srv.accept()

maxsend = 0.0


def send(data: bytes) -> None:
    global maxsend
    t = time.monotonic()
    conn.sendall(data)
    dt = time.monotonic() - t
    if dt > maxsend:
        maxsend = dt
        with open(f"{d}/fake.maxsend", "w") as f:
            f.write(f"{maxsend:.4f}")


send(b"\x00")                                       # byte fittizio (tunnel forward)
send(b"POCO F4 GT".ljust(64, b"\0"))                # nome del dispositivo
send(struct.pack(">I", 0x68323634))                 # codec id "h264"
send(struct.pack(">III", 0x80000000, 1080, 2400))   # pacchetto di sessione
if bad_header:
    send(struct.pack(">QI", 0, 100 * 1024 * 1024))
    time.sleep(60)
cfg = b"\x00\x00\x00\x01\x67\x42\xc0\x1f\xda\x01\x10\x00\x00\x00\x01\x68\xce\x3c\x80"
send(struct.pack(">QI", 1 << 62, len(cfg)) + cfg)   # pacchetto di configurazione

i = 0
period = 1.0 / fps
nxt = time.monotonic()
while not no_frames and (frames < 0 or i < frames):
    if 0 <= silent_after <= i:
        break
    key = key_every > 0 and i % key_every == 0
    nal = b"\x00\x00\x00\x01" + (b"\x65" if key else b"\x41") + struct.pack(">dI", time.monotonic(), i)
    nal += b"\xAB" * (fbytes - len(nal))
    send(struct.pack(">QI", ((1 << 61) if key else 0) | (i * 10000), len(nal)) + nal)
    i += 1
    if i % 20 == 0:
        with open(f"{d}/fake.sent", "w") as f:
            f.write(str(i))
    nxt += period
    delay = nxt - time.monotonic()
    if delay > 0:
        time.sleep(delay)
    else:
        nxt = time.monotonic()  # in ritardo sulla tabella di marcia
with open(f"{d}/fake.sent", "w") as f:
    f.write(str(i))
time.sleep(60)
