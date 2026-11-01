#!/usr/bin/env python3
"""Prova end-to-end del ponte scrcpy di phantom-probe con un finto server (nessun telefono).

Uso: python3 test_scrcpy_relay.py <percorso di phantom-probe per questa macchina>
"""
import os, signal, socket, struct, subprocess, sys, tempfile, time

probe = os.path.abspath(sys.argv[1])
here = os.path.dirname(os.path.abspath(__file__))
tmp = tempfile.mkdtemp(prefix="f0relay-")
env = dict(os.environ, FAKE_DIR=tmp, FAKE_FRAMES="50")
port = 47993

daemon = subprocess.Popen([
    probe, "--udp-port", "47991", "--http-port", "47992", "--log", f"{tmp}/probe.log",
    "--scrcpy-port", str(port), "--scrcpy-jar", f"{tmp}/fake.jar", "--scrcpy-version", "9.9",
    "--scrcpy-opt", "video_codec=h265", "--scrcpy-opt", "max_fps=120",
    "--app-process", f"{here}/fake_app_process.py", "--scrcpy-socket", f"{tmp}/scrcpy.sock",
], env=env)
failures = []

def check(cond, msg):
    print(("ok   " if cond else "FAIL ") + msg)
    if not cond:
        failures.append(msg)

try:
    for _ in range(50):
        try:
            s = socket.create_connection(("127.0.0.1", port), timeout=1)
            break
        except OSError:
            time.sleep(0.1)
    else:
        raise SystemExit("la sonda non si è avviata")
    s.settimeout(5)

    def rd(n):
        b = b""
        while len(b) < n:
            d = s.recv(n - len(b))
            if not d:
                raise EOFError("flusso chiuso")
            b += d
        return b

    t0 = time.time()
    check(rd(1) == b"\x00", "byte fittizio")
    check(rd(64).rstrip(b"\0") == b"POCO F4 GT", "nome del dispositivo")
    check(struct.unpack(">I", rd(4))[0] == 0x68323634, "id del codec h264")
    flags, w, h = struct.unpack(">III", rd(12))
    check(flags >> 31 == 1 and (w, h) == (1080, 2400), "pacchetto di sessione 1080x2400")
    pts_flags, size = struct.unpack(">QI", rd(12))
    cfg = rd(size)
    check(pts_flags >> 62 & 1 == 1 and pts_flags >> 63 == 0 and cfg[4] == 0x67, "pacchetto di configurazione con SPS")
    keys = 0
    for i in range(50):
        pts_flags, size = struct.unpack(">QI", rd(12))
        body = rd(size)
        if pts_flags >> 63 or pts_flags >> 62 & 1 or body[5] != i:
            check(False, f"frame {i} non valido")
            break
        keys += pts_flags >> 61 & 1
    else:
        check(keys == 5, "50 frame ricevuti in ordine, 5 keyframe")
    check(time.time() - t0 < 5, "tempi")

    args = open(f"{tmp}/fake.args").read()
    check("Server 9.9 scid=" in args and "tunnel_forward=true" in args and "audio=false" in args
          and "control=false" in args and "video_codec=h265" in args and "max_fps=120" in args,
          "argomenti passati al server")
    check(f"CLASSPATH={tmp}/fake.jar" in args, "CLASSPATH impostato sul jar")

    pid = int(open(f"{tmp}/fake.pid").read())
    s.close()
    time.sleep(1.0)
    try:
        os.kill(pid, 0)
        check(False, "il server non viene fermato alla disconnessione")
    except ProcessLookupError:
        check(True, "il server viene fermato alla disconnessione")
finally:
    daemon.send_signal(signal.SIGTERM)
    daemon.wait(timeout=5)

sys.exit(1 if failures else 0)
