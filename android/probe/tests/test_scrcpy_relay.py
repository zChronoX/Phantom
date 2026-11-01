#!/usr/bin/env python3
"""Prove del ponte scrcpy di phantom-probe con un finto server (nessun telefono).

Uso: python3 test_scrcpy_relay.py <percorso di phantom-probe per questa macchina> [filtro]

Il filtro è una parte del nome di un test (es. "stall") per eseguirne solo alcuni.

Cosa si prova, in ordine di importanza per la sicurezza del telefono:
  - se l'iPhone smette di leggere, il server NON resta bloccato e la sessione si chiude presto;
  - se l'iPhone è lento, i frame vecchi vengono scartati e dopo un buco c'è sempre un keyframe;
  - una sola sessione alla volta; /stop la ferma;
  - se la sonda muore di colpo, il server muore con lei e il marker resta come prova;
  - un server che tace (schermo fermo) NON fa chiudere la sessione.
"""
import os
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import time
import urllib.request

PROBE = os.path.abspath(sys.argv[1])
HERE = os.path.dirname(os.path.abspath(__file__))
FILTER = sys.argv[2] if len(sys.argv) > 2 else ""
FAILURES = []
LAST = None
_port = [47100]


def check(cond, msg):
    print(("  ok   " if cond else "  FAIL ") + msg, flush=True)
    if not cond:
        FAILURES.append(msg)
        if LAST is not None:
            tail = LAST.log().splitlines()[-14:]
            print("       --- log della sonda (ultime righe) ---\n       " + "\n       ".join(tail), flush=True)


def wait_for(cond, timeout, step=0.02):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if cond():
            return True
        time.sleep(step)
    return cond()


def pid_alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    try:  # uno zombie non è "vivo"
        with open(f"/proc/{pid}/stat") as f:
            return f.read().rsplit(")", 1)[1].split()[0] != "Z"
    except OSError:
        return False
    return True


class Probe:
    def __init__(self, fake_env=None, probe_args=None):
        self.tmp = tempfile.mkdtemp(prefix="f0relay-")
        self.port = _port[0]
        _port[0] += 10
        env = dict(os.environ, FAKE_DIR=self.tmp, **(fake_env or {}))
        args = [
            PROBE, "--udp-port", str(self.port), "--http-port", str(self.port + 1), "--log", f"{self.tmp}/probe.log",
            "--scrcpy-port", str(self.port + 3), "--scrcpy-jar", f"{self.tmp}/fake.jar", "--scrcpy-version", "9.9",
            "--scrcpy-opt", "video_codec=h265", "--scrcpy-opt", "max_fps=120", "--scrcpy-profile", "prova",
            "--app-process", f"{HERE}/fake_app_process.py", "--scrcpy-socket", f"{self.tmp}/scrcpy.sock",
            "--state-dir", self.tmp,
        ] + (probe_args or [])
        # Nuova sessione: il processo è capogruppo, come il demone vero.
        self.proc = subprocess.Popen(args, env=env, start_new_session=True)
        # Si controlla la porta HTTP: collegarsi a quella di scrcpy avvierebbe una sessione.
        for _ in range(100):
            try:
                urllib.request.urlopen(f"http://127.0.0.1:{self.port + 1}/status", timeout=0.5).read()
                break
            except OSError:
                time.sleep(0.05)
        else:
            raise SystemExit("la sonda non si è avviata")
        global LAST
        LAST = self

    def connect(self, rcvbuf=None):
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        if rcvbuf:
            s.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, rcvbuf)
        s.connect(("127.0.0.1", self.port + 3))
        s.settimeout(5)
        return Stream(s)

    def http(self, path):
        with urllib.request.urlopen(f"http://127.0.0.1:{self.port + 1}{path}", timeout=5) as r:
            return r.read().decode()

    def read_file(self, name, default=""):
        try:
            with open(f"{self.tmp}/{name}") as f:
                return f.read()
        except OSError:
            return default

    def log(self):
        return self.read_file("probe.log")

    def fake_pids(self):
        return [int(x) for x in self.read_file("fake.pids").split()]

    def fake_pid(self):
        return int(self.read_file("fake.pid", "0") or 0)

    def maxsend(self):
        return float(self.read_file("fake.maxsend", "0") or 0)

    def marker(self):
        return os.path.exists(f"{self.tmp}/session.marker")

    def close(self):
        try:
            os.killpg(self.proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        self.proc.wait()
        for pid in self.fake_pids():
            if pid_alive(pid):
                os.kill(pid, signal.SIGKILL)


class Stream:
    """Lato iPhone: legge il flusso scrcpy e verifica il formato."""

    def __init__(self, sock):
        self.s = sock
        self.buf = b""
        self.eof = False

    def close(self):
        self.s.close()

    def _fill(self, n):
        while len(self.buf) < n:
            d = self.s.recv(65536)
            if not d:
                self.eof = True
                raise EOFError
            self.buf += d

    def read(self, n):
        self._fill(n)
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def handshake(self):
        ok = self.read(1) == b"\x00"
        name = self.read(64).rstrip(b"\0")
        codec = struct.unpack(">I", self.read(4))[0]
        flags, w, h = struct.unpack(">III", self.read(12))
        return ok and name == b"POCO F4 GT" and codec == 0x68323634 and flags >> 31 == 1 and (w, h) == (1080, 2400)

    def packet(self):
        """Restituisce (kind, seq, send_time, key, size) oppure ('config', ...)."""
        ptsf, size = struct.unpack(">QI", self.read(12))
        body = self.read(size)
        if ptsf >> 63:
            raise ValueError("pacchetto di sessione inatteso")
        if ptsf >> 62 & 1:
            return ("config", None, None, False, size)
        key = bool(ptsf >> 61 & 1)
        t, seq = struct.unpack(">dI", body[5:17])
        return ("frame", seq, t, key, size)


# ------------------------------------------------------------------- i test

def test_normal_flow():
    p = Probe({"FAKE_FRAMES": "50"})
    try:
        s = p.connect()
        check(s.handshake(), "handshake corretto")
        kind, *_ = s.packet()
        check(kind == "config", "pacchetto di configurazione")
        seqs, keys = [], 0
        for _ in range(50):
            _, seq, _, key, _ = s.packet()
            seqs.append(seq)
            keys += key
        check(seqs == list(range(50)) and keys == 5, "50 frame in ordine, 5 keyframe")
        args = p.read_file("fake.args")
        check("Server 9.9 scid=" in args and "tunnel_forward=true" in args and "audio=false" in args and "control=false" in args
              and "cleanup=false" in args and "log_level=debug" in args, "argomenti di base passati al server")
        check("video_codec=h265" in args and "max_fps=120" in args, "opzioni del profilo passate al server")
        check(args.index("log_level=debug") < args.index("video_codec=h265"), "le opzioni del profilo vengono dopo quelle di base")
        check(f"CLASSPATH={p.tmp}/fake.jar" in args, "CLASSPATH impostato sul jar")
        mk = p.read_file("session.marker")
        check("profilo=prova" in mk and "video_codec=h265" in mk and "versione=9.9" in mk, "marker scritto durante la sessione")
        pid = p.fake_pid()
        s.close()
        check(wait_for(lambda: not pid_alive(pid), 3), "il server viene fermato quando l'iPhone si disconnette")
        check(wait_for(lambda: not p.marker(), 2), "marker rimosso a chiusura pulita")
        check(wait_for(lambda: "conclusa" in p.log(), 3), "riepilogo scritto nel log")
    finally:
        p.close()


def test_client_stops_reading():
    """Il caso che può congelare il telefono: l'iPhone smette di leggere (app sospesa, errore, Wi-Fi)."""
    p = Probe({"FAKE_FRAMES": "-1", "FAKE_FPS": "120", "FAKE_FRAME_BYTES": "30000"},
              ["--scrcpy-stall-client-ms", "800"])
    try:
        s = p.connect(rcvbuf=4096)
        check(s.handshake(), "handshake")
        s.packet()
        for _ in range(30):
            s.packet()
        pid = p.fake_pid()
        t_stall = time.monotonic()
        # Da qui l'iPhone non legge più. Il server finto continua a scrivere a 3,6 MB/s.
        time.sleep(0.4)
        check(pid_alive(pid), "durante i primi 0,4 s di stallo il server è ancora attivo (soglia 0,8 s)")
        died = wait_for(lambda: not pid_alive(pid), 4)
        dt = time.monotonic() - t_stall
        check(died, f"la sessione chiude e il server viene fermato (dopo {dt:.2f} s)")
        check(dt < 2.5, "entro 2,5 s dallo stallo")
        check(p.maxsend() < 0.35, f"il server non è mai rimasto bloccato in scrittura (massimo {p.maxsend() * 1000:.0f} ms)")
        check(wait_for(lambda: "l'iPhone ha smesso di leggere" in p.log(), 3), "motivo scritto nel log")
        check(wait_for(lambda: not p.marker(), 2), "marker rimosso (chiusura gestita, non un blocco)")
    finally:
        p.close()


def test_slow_client_drops_frames():
    """iPhone lento ma vivo: si scartano i frame vecchi e dopo ogni buco si riparte da un keyframe."""
    p = Probe({"FAKE_FRAMES": "-1", "FAKE_FPS": "100", "FAKE_FRAME_BYTES": "20000", "FAKE_KEY_EVERY": "25"},
              ["--scrcpy-max-age-ms", "150", "--scrcpy-stall-client-ms", "3000"])
    try:
        s = p.connect(rcvbuf=8192)
        check(s.handshake(), "handshake")
        s.packet()
        got, gaps_ok, last, ages = 0, True, None, []
        t_end = time.monotonic() + 3.0
        while time.monotonic() < t_end:
            kind, seq, t, key, size = s.packet()
            time.sleep(0.012)  # legge più lentamente di quanto il server produce (circa 1,3 MB/s contro 2 MB/s)
            got += 1
            if last is not None and seq != last + 1 and not key:
                gaps_ok = False  # un frame dopo un buco senza essere un keyframe: l'immagine si rovinerebbe
            last = seq
            ages.append(time.monotonic() - t)
        pid = p.fake_pid()
        check(pid_alive(pid), "la sessione è ancora viva (l'iPhone è lento, non fermo)")
        check(got < last, f"alcuni frame sono stati scartati (ricevuti {got} su {last + 1})")
        check(gaps_ok, "dopo ogni buco il primo frame è un keyframe")
        check(max(ages) < 2.0, f"età massima dei frame sotto 2 s (massimo {max(ages) * 1000:.0f} ms)")
        check(p.maxsend() < 0.35, f"il server non è mai rimasto bloccato in scrittura (massimo {p.maxsend() * 1000:.0f} ms)")
        check("non sta al passo" in p.log(), "congestione registrata nel log")
    finally:
        p.close()


def test_short_pause_recovers():
    """Una pausa breve dell'iPhone (sotto la soglia di stallo) non chiude la sessione: si riprende da un keyframe."""
    p = Probe({"FAKE_FRAMES": "-1", "FAKE_FPS": "100", "FAKE_FRAME_BYTES": "20000", "FAKE_KEY_EVERY": "20"},
              ["--scrcpy-max-age-ms", "150", "--scrcpy-stall-client-ms", "2500"])
    try:
        s = p.connect(rcvbuf=8192)
        s.handshake()
        s.packet()
        for _ in range(20):
            s.packet()
        time.sleep(0.9)  # pausa lunga abbastanza per creare congestione ma non per lo stallo
        last, gaps_ok, resumed = None, True, 0
        t_end = time.monotonic() + 1.5
        while time.monotonic() < t_end:
            _, seq, _, key, _ = s.packet()
            if last is not None and seq != last + 1 and not key:
                gaps_ok = False
            if last is not None and seq > last:
                resumed += 1
            last = seq
        check(pid_alive(p.fake_pid()), "la sessione è sopravvissuta alla pausa")
        check(resumed > 50, f"il flusso è ripreso ({resumed} frame dopo la pausa)")
        check(gaps_ok, "dopo il buco si è ripartiti da un keyframe")
    finally:
        p.close()


def test_silent_server_is_not_a_stall():
    """Uno schermo fermo non produce frame: la sessione NON deve chiudersi (vale per l'impostazione predefinita)."""
    p = Probe({"FAKE_FRAMES": "-1", "FAKE_SILENT_AFTER": "20", "FAKE_FPS": "100"})
    try:
        s = p.connect()
        s.handshake()
        s.packet()
        for _ in range(20):
            s.packet()
        pid = p.fake_pid()
        time.sleep(3.5)
        check(pid_alive(pid), "dopo 3,5 s di silenzio la sessione è ancora aperta")
        check("ha smesso di leggere" not in p.log() and "non produce più dati" not in p.log(), "nessun falso allarme nel log")
    finally:
        p.close()


def test_silent_server_forced_stop():
    """Con la soglia esplicita (solo per i test) un server che tace fa chiudere la sessione."""
    p = Probe({"FAKE_FRAMES": "-1", "FAKE_SILENT_AFTER": "20", "FAKE_FPS": "100"}, ["--scrcpy-stall-server-ms", "800"])
    try:
        s = p.connect()
        s.handshake()
        s.packet()
        for _ in range(20):
            s.packet()
        pid = p.fake_pid()
        check(wait_for(lambda: not pid_alive(pid), 4), "il server tace oltre la soglia: sessione chiusa e server fermato")
        check(wait_for(lambda: "il server non produce più dati" in p.log(), 3), "motivo nel log")
    finally:
        p.close()


def test_one_session_at_a_time():
    p = Probe({"FAKE_FRAMES": "-1", "FAKE_FPS": "50"})
    try:
        a = p.connect()
        check(a.handshake(), "prima sessione: handshake")
        a.packet()
        pid_a = p.fake_pid()
        b = p.connect()  # la seconda sostituisce la prima
        check(b.handshake(), "seconda sessione: handshake (sostituisce la prima)")
        check(wait_for(lambda: not pid_alive(pid_a), 3), "il server della prima sessione è stato fermato")
        pid_b = p.fake_pid()
        check(pid_b != pid_a and pid_alive(pid_b), "ne è partito uno nuovo")
        vivi = [x for x in p.fake_pids() if pid_alive(x)]
        check(len(vivi) == 1, f"un solo server attivo alla volta ({len(vivi)})")
        try:
            while True:
                a.s.settimeout(2)
                if not a.s.recv(65536):
                    break
            closed = True
        except OSError:
            closed = True
        check(closed, "la prima connessione è stata chiusa")
        check(wait_for(lambda: "sessione precedente ancora attiva" in p.log(), 3), "sostituzione registrata nel log")
    finally:
        p.close()


def test_stop_endpoint():
    p = Probe({"FAKE_FRAMES": "-1", "FAKE_FPS": "50"})
    try:
        s = p.connect()
        s.handshake()
        s.packet()
        pid = p.fake_pid()
        st = p.http("/status")
        check('"active":true' in st and '"profile":"prova"' in st, "/status mostra la sessione attiva")
        out = p.http("/stop")
        check('"stopped":true' in out, "/stop risponde che ha fermato la sessione")
        check(wait_for(lambda: not pid_alive(pid), 3), "il server è stato fermato")
        check(wait_for(lambda: '"active":false' in p.http("/status"), 3), "/status mostra nessuna sessione attiva")
        check('"stopped":false' in p.http("/stop"), "/stop senza sessioni non fa nulla")
    finally:
        p.close()


def test_probe_killed_abruptly():
    """La sonda (il processo di sessione) muore di colpo: il server non resta orfano e il marker resta."""
    p = Probe({"FAKE_FRAMES": "-1", "FAKE_FPS": "50"})
    try:
        s = p.connect()
        s.handshake()
        s.packet()
        pid_server = p.fake_pid()
        sess = int(p.read_file("scrcpy.lock").split()[0])
        check(p.marker(), "il marker esiste durante la sessione")
        os.kill(sess, signal.SIGKILL)
        check(wait_for(lambda: not pid_alive(pid_server), 2), "il server muore con il processo di sessione (PDEATHSIG)")
        check(p.marker(), "il marker resta: la sessione non è finita in modo pulito")
        s2 = p.connect()
        check(s2.handshake(), "una nuova sessione parte senza problemi (il blocco si è liberato)")
    finally:
        p.close()


def test_server_fails_at_start():
    p = Probe({"FAKE_EXIT_NOW": "3"})
    try:
        s = p.connect()
        try:
            s.read(1)
            closed = False
        except (EOFError, OSError):
            closed = True
        check(closed, "il client vede la connessione chiusa")
        check(wait_for(lambda: "il server è terminato subito" in p.log(), 3), "l'errore del server è nel log")
        check("finto errore di avvio" in p.log(), "l'output del server è nel log")
        check(wait_for(lambda: not p.marker(), 2), "marker rimosso")
    finally:
        p.close()


def test_no_first_frame():
    p = Probe({"FAKE_NO_FRAMES": "1"}, ["--scrcpy-first-frame-ms", "800"])
    try:
        s = p.connect()
        check(s.handshake(), "handshake")
        pid = p.fake_pid()
        check(wait_for(lambda: not pid_alive(pid), 4), "senza frame entro il limite la sessione chiude e il server si ferma")
        check(wait_for(lambda: "nessun frame entro il tempo limite" in p.log(), 3), "motivo nel log")
    finally:
        p.close()


def test_invalid_stream():
    p = Probe({"FAKE_BAD_HEADER": "1"})
    try:
        s = p.connect()
        try:
            s.handshake()
        except EOFError:
            pass
        pid = p.fake_pid()
        check(wait_for(lambda: not pid_alive(pid), 3), "flusso non valido: sessione chiusa e server fermato")
        check(wait_for(lambda: "flusso non valido" in p.log(), 3), "motivo nel log")
    finally:
        p.close()


def test_daemon_terminated():
    p = Probe({"FAKE_FRAMES": "-1", "FAKE_FPS": "50"})
    try:
        s = p.connect()
        s.handshake()
        s.packet()
        pid = p.fake_pid()
        p.proc.send_signal(signal.SIGTERM)
        check(wait_for(lambda: p.proc.poll() is not None, 3), "il demone si ferma con SIGTERM")
        check(wait_for(lambda: not pid_alive(pid), 3), "il server viene fermato insieme")
        check(wait_for(lambda: not p.marker(), 2), "marker rimosso (arresto ordinato)")
    finally:
        p.close()


def test_rapid_reconnects():
    """Quindici connessioni lampo di fila (come un'app che si riapre): nessun server o descrittore resta indietro."""
    p = Probe({"FAKE_FRAMES": "-1", "FAKE_FPS": "50"})
    try:
        for _ in range(15):
            s = p.connect()
            try:
                s.s.settimeout(0.3)
                s.read(1)
            except (EOFError, OSError):
                pass
            s.close()
            time.sleep(0.03)
        time.sleep(2.5)
        vivi = [x for x in p.fake_pids() if pid_alive(x)]
        check(not vivi, f"dopo la serie non resta nessun server attivo ({len(vivi)})")
        fds = len(os.listdir(f"/proc/{p.proc.pid}/fd"))
        check(fds < 20, f"il demone non perde descrittori ({fds})")
        check(not p.marker(), "nessun marker rimasto")
        s = p.connect()
        check(s.handshake(), "dopo la serie, una sessione normale parte")
    finally:
        p.close()


def test_client_leaves_during_startup():
    """L'iPhone si disconnette mentre il server sta ancora partendo: il server non resta acceso."""
    p = Probe({"FAKE_FRAMES": "-1", "FAKE_FPS": "50"})
    try:
        s = p.connect()
        s.close()  # chiude subito, prima che il server abbia aperto il suo socket
        check(wait_for(lambda: "prima dell'avvio del server" in p.log(), 3), "la chiusura anticipata viene rilevata")
        pid = p.fake_pid()
        check(pid == 0 or wait_for(lambda: not pid_alive(pid), 3), "il server avviato per quella connessione viene fermato")
        check(wait_for(lambda: not p.marker(), 2), "marker rimosso")
    finally:
        p.close()


def main():
    tests = [(n, f) for n, f in sorted(globals().items()) if n.startswith("test_") and callable(f)]
    order = [  # dal caso più importante
        "test_client_stops_reading", "test_slow_client_drops_frames", "test_short_pause_recovers",
        "test_silent_server_is_not_a_stall", "test_silent_server_forced_stop", "test_one_session_at_a_time",
        "test_stop_endpoint", "test_probe_killed_abruptly", "test_server_fails_at_start", "test_no_first_frame",
        "test_invalid_stream", "test_daemon_terminated", "test_rapid_reconnects", "test_client_leaves_during_startup", "test_normal_flow",
    ]
    tests.sort(key=lambda t: order.index(t[0]) if t[0] in order else 99)
    for name, fn in tests:
        if FILTER and FILTER not in name:
            continue
        print(f"\n{name}: {(fn.__doc__ or '').strip().splitlines()[0] if fn.__doc__ else ''}", flush=True)
        try:
            fn()
        except Exception as e:  # un errore imprevisto conta come test fallito
            check(False, f"eccezione imprevista: {type(e).__name__}: {e}")
    print(f"\n{len(FAILURES)} test falliti" if FAILURES else "\nTutti i test superati")
    sys.exit(1 if FAILURES else 0)


if __name__ == "__main__":
    main()
