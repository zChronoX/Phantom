#!/usr/bin/env python3
"""Prove della «scatola nera» (telemetria) di phantom-probe su un finto telefono: nessun telefono vero.

Uso: python3 test_telemetry.py <percorso di phantom-probe per questa macchina> [filtro]

Il «telefono» è una cartella con finti /proc, /sys, /dev/kmsg e /proc/sysrq-trigger. Si prova:
  - il flusso: intestazione, campioni completi (T) più volte al secondo, battiti (B), storia a chi si collega dopo;
  - i messaggi del kernel (K): gli indirizzi MAC vengono mascherati, i nomi Wi-Fi e il rumore di audit scartati;
  - il rilevatore di stallo: task bloccati nel kernel → riga S, elenco dei task con stack (D), richiesta sysrq,
    registro su disco; fine dello stallo;
  - un campionatore incastrato in una lettura del kernel (una lettura sysfs che non torna mai) viene segnalato
    senza fermare gli altri e senza fermare la trasmissione;
  - un client che non legge non rallenta gli altri;
  - se il processo della scatola nera muore, viene rilanciato; se muore il demone, muore anche lui.
"""
import os
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request

PROBE = os.path.abspath(sys.argv[1])
FILTER = sys.argv[2] if len(sys.argv) > 2 else ""
FAILURES = []
LAST = None
_port = [47500]


def check(cond, msg):
    print(("  ok   " if cond else "  FAIL ") + msg, flush=True)
    if not cond:
        FAILURES.append(msg)
        if LAST is not None:
            tail = LAST.log().splitlines()[-10:]
            print("       --- log della sonda (ultime righe) ---\n       " + "\n       ".join(tail), flush=True)


def wait_for(cond, timeout, step=0.02):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if cond():
            return True
        time.sleep(step)
    return cond()


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)


STAT_OK = "cpu  1000 0 500 10000 50 0 20 0 0 0\ncpu0 100 0 50 1000 5 0 2 0 0 0\nintr 123456 1 2 3\nctxt 999\nbtime 1\nprocesses 77\nprocs_running 2\nprocs_blocked {blk}\n"
PSI = "some avg10=0.00 avg60=0.00 avg300=0.00 total={t}\nfull avg10=0.00 avg60=0.00 avg300=0.00 total={f}\n"


class Phone:
    """Un finto telefono su disco, con i file che la scatola nera legge."""

    def __init__(self):
        self.root = tempfile.mkdtemp(prefix="f0tel-")
        r = self.root
        write(f"{r}/proc/stat", STAT_OK.format(blk=0))
        write(f"{r}/proc/loadavg", "0.50 0.40 0.30 2/300 1234\n")
        write(f"{r}/proc/meminfo", "MemTotal:       12000000 kB\nMemFree:          800000 kB\nMemAvailable:    5000000 kB\nDirty:               100 kB\nWriteback:             0 kB\nSwapFree:        2000000 kB\n")
        write(f"{r}/proc/vmstat", "allocstall_normal 5\nallocstall_movable 0\npgscan_direct 10\n")
        for n in ("cpu", "memory", "io"):
            write(f"{r}/proc/pressure/{n}", PSI.format(t=0, f=0))
        # processi chiave: surfaceflinger (100) e system_server (300)
        for pid, name in ((100, "surfaceflinger"), (300, "system_server")):
            write(f"{r}/proc/{pid}/comm", name + "\n")
            write(f"{r}/proc/{pid}/stat", f"{pid} ({name}) S 1 {pid} {pid} 0 -1 4194560 0 0 0 0 0 0 0 0 20 0 3 0 1 0 0\n")
            write(f"{r}/proc/{pid}/wchan", "do_epoll_wait\n")
            write(f"{r}/proc/{pid}/task/{pid}/stat", f"{pid} ({name}) S 1 {pid} {pid} 0 -1 0 0 0 0 0 0 0 0 0 20 0 3 0 1 0 0\n")
            write(f"{r}/proc/{pid}/task/{pid}/wchan", "do_epoll_wait\n")
        write(f"{r}/proc/1/comm", "init\n")
        # GPU, temperature, frequenze
        write(f"{r}/sys/class/kgsl/kgsl-3d0/gpu_busy_percentage", "35 %\n")
        write(f"{r}/sys/class/kgsl/kgsl-3d0/gpuclk", "818000000\n")
        write(f"{r}/sys/class/kgsl/kgsl/page_alloc", "209715200\n")
        write(f"{r}/sys/class/thermal/thermal_zone0/type", "cpu-0-0-usr\n")
        write(f"{r}/sys/class/thermal/thermal_zone0/temp", "55600\n")
        write(f"{r}/sys/class/thermal/thermal_zone1/type", "gpuss-0-usr\n")
        write(f"{r}/sys/class/thermal/thermal_zone1/temp", "50100\n")
        write(f"{r}/sys/class/thermal/thermal_zone2/type", "wlan-unrelated\n")
        write(f"{r}/sys/class/thermal/thermal_zone2/temp", "20000\n")
        for c, f in ((0, 1804800), (4, 2419200), (7, 2995200)):
            write(f"{r}/sys/devices/system/cpu/cpu{c}/cpufreq/scaling_cur_freq", f"{f}\n")
        os.makedirs(f"{r}/state", exist_ok=True)
        os.mkfifo(f"{r}/kmsg")
        os.mkfifo(f"{r}/sysrq")
        write(f"{r}/sysrqmask", "0\n")
        self.sysrq_rd = os.open(f"{r}/sysrq", os.O_RDONLY | os.O_NONBLOCK)  # il lettore c'è prima della sonda
        self.kmsg_wr = None

    def set_blocked(self, n):
        write(f"{self.root}/proc/stat", STAT_OK.format(blk=n))

    def add_dstate(self):
        r = self.root
        write(f"{r}/proc/200/comm", "kworker/u16:3\n")
        write(f"{r}/proc/200/task/201/stat", "201 (kworker/u16:3) D 2 0 0 0 -1 69238880 0 0 0 0 0 0 0 0 20 0 1 0 1 0 0\n")
        write(f"{r}/proc/200/task/201/wchan", "io_schedule\n")
        write(f"{r}/proc/200/task/201/stack", "[<0>] __switch_to+0x1c/0x28\n[<0>] io_schedule+0x20/0x40\n[<0>] wait_on_page_bit+0x80/0x90\n[<0>] f2fs_sync_file+0x100/0x300\n[<0>] do_fsync+0x40/0x70\n")

    def clear_dstate(self):
        for f in ("stat", "wchan", "stack"):
            try:
                os.remove(f"{self.root}/proc/200/task/201/{f}")
            except OSError:
                pass

    def kmsg(self, text):
        if self.kmsg_wr is None:
            for _ in range(100):
                try:
                    self.kmsg_wr = os.open(f"{self.root}/kmsg", os.O_WRONLY | os.O_NONBLOCK)
                    break
                except OSError:
                    time.sleep(0.05)
        os.write(self.kmsg_wr, text.encode())

    def sysrq_keys(self):
        try:
            return os.read(self.sysrq_rd, 64).decode()
        except BlockingIOError:
            return ""


HERE = os.path.dirname(os.path.abspath(__file__))


class Probe:
    def __init__(self, phone, extra=None, scrcpy=False, fake_env=None):
        global LAST
        self.phone = phone
        self.port = _port[0]
        _port[0] += 10
        r = phone.root
        args = [PROBE, "--udp-port", str(self.port), "--http-port", str(self.port + 1), "--log", f"{r}/probe.log", "--state-dir", f"{r}/state",
                "--telemetry-port", str(self.port + 4), "--tm-hz", "10", "--tm-idle-ms", "300",
                "--proc-root", f"{r}/proc", "--sys-root", f"{r}/sys", "--kmsg", f"{r}/kmsg", "--sysrq-file", f"{r}/sysrq", "--sysrq-mask", f"{r}/sysrqmask"]
        env = dict(os.environ, FAKE_DIR=r, **(fake_env or {}))
        if scrcpy:
            args += ["--scrcpy-port", str(self.port + 3), "--scrcpy-jar", f"{r}/fake.jar", "--scrcpy-version", "9.9", "--scrcpy-profile", "prova",
                     "--app-process", f"{HERE}/fake_app_process.py", "--scrcpy-socket", f"{r}/scrcpy.sock"]
        self.proc = subprocess.Popen(args + (extra or []), env=env, start_new_session=True)
        for _ in range(100):
            try:
                urllib.request.urlopen(f"http://127.0.0.1:{self.port + 1}/status", timeout=0.5).read()
                break
            except OSError:
                time.sleep(0.05)
        else:
            raise SystemExit("la sonda non si è avviata")
        LAST = self

    def log(self):
        try:
            with open(f"{self.phone.root}/probe.log") as f:
                return f.read()
        except OSError:
            return ""

    def read_fake_server_alive(self):
        try:
            with open(f"{self.phone.root}/fake.pid") as f:
                pid = int(f.read().strip())
        except (OSError, ValueError):
            return False
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return False
        try:  # uno zombie non è vivo
            with open(f"/proc/{pid}/stat") as f:
                return f.read().rsplit(")", 1)[1].split()[0] != "Z"
        except OSError:
            return False

    def blackbox(self):
        try:
            with open(f"{self.phone.root}/state/blackbox.txt") as f:
                return f.read()
        except OSError:
            return ""

    def client(self, rcvbuf=None):
        s = socket.socket()
        if rcvbuf:
            s.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, rcvbuf)
        s.connect(("127.0.0.1", self.port + 4))
        return Client(s)

    def children(self):
        """Pid dei processi del gruppo della sonda con il nome dato dal kernel."""
        out = {}
        try:
            for pid in os.listdir("/proc"):
                if not pid.isdigit():
                    continue
                try:
                    if os.getpgid(int(pid)) == self.proc.pid:
                        with open(f"/proc/{pid}/comm") as f:
                            out[int(pid)] = f.read().strip()
                except OSError:
                    pass
        except OSError:
            pass
        return out

    def close(self):
        try:
            os.killpg(self.proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        self.proc.wait()


class Client:
    """Lato iPhone: raccoglie le righe del flusso."""

    def __init__(self, sock):
        self.s = sock
        self.s.settimeout(0.2)
        self.buf = b""
        self.lines = []
        self.t = []
        self.stop = False
        self.th = threading.Thread(target=self._run, daemon=True)
        self.th.start()

    def _run(self):
        while not self.stop:
            try:
                d = self.s.recv(65536)
            except socket.timeout:
                continue
            except OSError:
                break
            if not d:
                break
            self.buf += d
            while b"\n" in self.buf:
                line, self.buf = self.buf.split(b"\n", 1)
                self.lines.append(line.decode(errors="replace"))
                self.t.append(time.monotonic())

    def of(self, kind):
        return [l for l in list(self.lines) if l.startswith(kind + " ")]

    def has(self, kind, text):
        return any(text in l for l in self.of(kind))

    def close(self):
        self.stop = True
        try:
            self.s.close()
        except OSError:
            pass


# ------------------------------------------------------------------- i test

def test_stream():
    ph = Phone()
    p = Probe(ph)
    try:
        c = p.client()
        wait_for(lambda: len(c.of("T")) >= 20, 5)
        t0 = time.monotonic()
        n0 = len(c.of("T"))
        time.sleep(2.0)
        rate = (len(c.of("T")) - n0) / (time.monotonic() - t0)
        check(c.lines and c.lines[0].startswith("H telemetria 1 sonda=0.5.1"), "intestazione H con versione del protocollo e della sonda")
        check(7 <= rate <= 13, f"circa 10 campioni al secondo con un client collegato ({rate:.1f}/s)")
        check(len(c.of("B")) >= 2, "battiti B ogni secondo")
        t = c.of("T")[-1]
        for frag, what in (("e=", "ora"), ("c u", "CPU"), ("l 0.50 0.40 2/300 b0", "carico e task bloccati"), ("m a4882 f781", "memoria disponibile e libera"),
                           ("p c0.0 m0.0/0.0 i0.0/0.0", "pressione"), ("g b35 c818 k200", "GPU"), ("cpu-0-0=55.6", "temperatura CPU"),
                           ("gpuss-0=50.1", "temperatura GPU"), ("f 1804/2419/2995", "frequenze"), ("P sf S/do_epoll_wait t1 D0", "stato di surfaceflinger"),
                           ("ss S/do_epoll_wait t1 D0", "stato di system_server"), ("R -", "nessuna sessione video"), ("a core", "età dei campionatori")):
            check(frag in t, f"il campione contiene {what}  [{frag}]")
        check("wlan-unrelated" not in t, "le temperature non utili non vengono riportate")
        check(len(t) < 1000, f"riga entro il limite ({len(t)} byte)")
        # chi si collega dopo riceve la storia subito
        c2 = p.client()
        time.sleep(0.4)
        check(len(c2.of("T")) >= 20, f"un nuovo client riceve subito la storia ({len(c2.of('T'))} righe)")
        c.close(); c2.close()
    finally:
        p.close()


def test_kmsg():
    ph = Phone()
    p = Probe(ph)
    try:
        c = p.client()
        wait_for(lambda: len(c.of("T")) >= 3, 5)
        ph.kmsg("6,700,5123456,-;kgsl kgsl-3d0: gpu fault detected, ctx 12\n")
        ph.kmsg("6,701,5123999,-;bluetooth: connected to 3c:22:fb:aa:bb:cc on channel 36\n")
        ph.kmsg("6,702,5124000,-;wlan: SSID CasaMia associated\n")
        ph.kmsg("6,703,5124001,-;audit: type=1400 avc: denied { read } for comm=x\n")
        ph.kmsg("6,704,5124002,-;f2fs: checkpoint took long\n SUBSYSTEM=block\n")
        ph.kmsg("6,705,5124003,-;[ FTS ]  fts_enter_pointer_event_handler :  Event 0x13 - Press ID[0] type = 1\n")
        ph.kmsg("6,706,5124004,-;KernelSU: allow root for: 10326\n")
        ph.kmsg("6,707,5124005,-;servicemanager: Caller(pid=1) Could not find android.hardware.graphics.allocator.IAllocator/default in the VINTF manifest.\n")
        ph.kmsg("6,708,5124006,-;[mi_disp:_sde_kms_drm_check_dpms [msm_drm]][2026-11-01 18:23:18.669333] [I]conn32 early_trigger:1 (power mode 2->4, fps 120->60) - 0.78(ms)\n")
        ph.kmsg("6,709,5124007,-;[mi_disp:_sde_kms_drm_check_dpms [msm_drm]][2026-11-01 18:23:18.669400] [I]conn32 early_trigger:0 (power mode 2->4, fps 120->60) - 0.19(ms)\n")
        ph.kmsg("6,710,5124008,-;wlan: [656:I:PMO] pmo_core_enable_wow_in_fw: drv wow is enabled\n")
        ph.kmsg("3,711,5124009,-;wlan: command timeout waiting for firmware\n")
        ph.kmsg("3,712,5124010,-;kgsl kgsl-3d0: |a6xx_gmu_wait_for_idle| GMU idle timeout\n")
        wait_for(lambda: c.has("K", "#712"), 5)
        wait_for(lambda: c.has("K", "checkpoint took long"), 5)
        check(not any("fts_enter_pointer" in l or "[ FTS ]" in l for l in c.lines), "il rumore del tocco dello schermo viene scartato")
        check(not any("KernelSU: allow root" in l for l in c.lines), "il rumore di KernelSU viene scartato")
        check(not any("VINTF" in l for l in c.lines), "il rumore di servicemanager viene scartato")
        check(c.has("K", "#708") and "fps 120->60" in "".join(c.of("K")), "il cambio di frequenza del display (fps 120->60) si tiene")
        check(not c.has("K", "#709"), "ma non la sua copia «early_trigger:0»")
        check(not c.has("K", "#710"), "il rumore Wi-Fi ordinario viene scartato")
        check(c.has("K", "#711") and c.has("K", "#712"), "i timeout (anche del Wi-Fi) e gli errori della GPU si tengono sempre")
        check(c.has("K", "#700 <6> kgsl kgsl-3d0: gpu fault detected"), "messaggio del kernel inoltrato con numero e priorità")
        check(c.has("K", "k=5.123"), "porta il tempo del kernel")
        check(c.has("K", "connected to xx:xx:xx:xx:xx:xx on channel 36"), "indirizzo MAC mascherato")
        check(not any("3c:22:fb" in l for l in c.lines), "il MAC vero non esce mai")
        check(not any("CasaMia" in l for l in c.lines), "i messaggi con il nome della rete Wi-Fi vengono scartati")
        check(not any("avc: denied" in l for l in c.lines), "il rumore di audit viene scartato")
        check(c.has("K", "#704"), "il record con riga di continuazione viene letto")
        check("SUBSYSTEM" not in "\n".join(c.lines), "le righe di continuazione non vengono inoltrate")
        c.close()
    finally:
        p.close()


def test_stall_detection():
    ph = Phone()
    p = Probe(ph)
    try:
        c = p.client()
        wait_for(lambda: len(c.of("T")) >= 3, 5)
        check(not c.of("S"), "nessun falso allarme a telefono sano")
        ph.add_dstate()
        ph.set_blocked(5)
        check(wait_for(lambda: c.has("S", "inizio:"), 5), "stallo rilevato con task bloccati nel kernel")
        check(c.has("S", "5 task bloccati nel kernel"), "il motivo riporta quanti task sono bloccati")
        check(wait_for(lambda: c.has("D", "kworker/u16:3 200/201 wchan=io_schedule"), 5), "elenco dei task bloccati con nome, pid/tid e wchan")
        check(c.has("D", "stack=io_schedule<wait_on_page_bit<f2fs_sync_file<do_fsync"), "stack del kernel del task, senza le funzioni dello scheduler")
        got = []

        def sysrq_seen():
            got.append(ph.sysrq_keys())
            return "l" in "".join(got)

        wait_for(sysrq_seen, 5)
        keys = "".join(got)
        check(keys.startswith("w") and "l" in keys, f"richiesti al kernel gli stack dei task bloccati e delle CPU (sysrq {keys!r})")
        with open(f"{ph.root}/sysrqmask") as f:
            check(f.read().strip() == "8", "abilitate le funzioni di debug di sysrq (maschera 8)")
        check(wait_for(lambda: "inizio:" in p.blackbox(), 5), "lo stallo è scritto su disco")
        check("D " in p.blackbox() or wait_for(lambda: "D " in p.blackbox(), 3), "anche i task bloccati sono su disco")
        check(wait_for(lambda: c.has("S", "continua da"), 5), "durante lo stallo si riprende l'elenco ogni 2 secondi")
        ph.set_blocked(0)
        ph.clear_dstate()
        check(wait_for(lambda: c.has("S", "fine dopo"), 5), "fine dello stallo segnalata")
        c.close()
    finally:
        p.close()


def test_psi_stall():
    ph = Phone()
    p = Probe(ph)
    stop = [False]

    def feed():  # la pressione di I/O «piena» cresce di 50 ms ogni 100 ms = 50 %
        total = 0
        while not stop[0]:
            total += 50000
            write(f"{ph.root}/proc/pressure/io", PSI.format(t=total, f=total))
            time.sleep(0.1)

    try:
        c = p.client()
        wait_for(lambda: len(c.of("T")) >= 3, 5)
        th = threading.Thread(target=feed, daemon=True)
        th.start()
        check(wait_for(lambda: c.has("S", "pressione I/O piena"), 6), "stallo rilevato dalla pressione di I/O")
        stop[0] = True
        c.close()
    finally:
        stop[0] = True
        p.close()


def test_proc_snapshot():
    ph = Phone()
    p = Probe(ph)
    try:
        c = p.client()
        wait_for(lambda: len(c.of("T")) >= 3, 5)
        r = ph.root
        # un thread di surfaceflinger fermo in attesa di una fence nel kernel, e uno che aspetta tranquillo
        write(f"{r}/proc/100/task/101/stat", "101 (RenderEngine) S 1 100 100 0 -1 0 0 0 0 0 0 0 0 0 20 0 3 0 1 0 0\n")
        write(f"{r}/proc/100/task/101/wchan", "dma_fence_default_wait\n")
        write(f"{r}/proc/100/task/101/stack", "[<0>] __switch_to+0x1c/0x28\n[<0>] dma_fence_default_wait+0x120/0x1a0\n[<0>] sync_file_ioctl+0x80/0x1c0\n[<0>] __arm64_sys_ioctl+0x90/0xc0\n")
        write(f"{r}/proc/100/task/102/stat", "102 (binder:100_1) S 1 100 100 0 -1 0 0 0 0 0 0 0 0 0 20 0 3 0 1 0 0\n")
        write(f"{r}/proc/100/task/102/wchan", "binder_thread_read\n")
        ph.add_dstate()
        ph.set_blocked(5)
        check(wait_for(lambda: c.has("D", "proc sf 100/101(RenderEngine) S wchan=dma_fence_default_wait"), 6), "lo stallo include i thread di surfaceflinger che non aspettano tranquilli")
        check(c.has("D", "stack=dma_fence_default_wait<sync_file_ioctl<__arm64_sys_ioctl"), "con lo stack del kernel (dove aspetta la fence)")
        check(not c.has("D", "proc sf 100/102"), "senza i thread che aspettano su binder")
        c.close()
    finally:
        p.close()


def test_key_process_blocked():
    ph = Phone()
    p = Probe(ph)
    try:
        c = p.client()
        wait_for(lambda: len(c.of("T")) >= 3, 5)
        write(f"{ph.root}/proc/100/stat", "100 (surfaceflinger) D 1 100 100 0 -1 4194560 0 0 0 0 0 0 0 0 20 0 3 0 1 0 0\n")
        check(wait_for(lambda: c.has("S", "un processo chiave è bloccato nel kernel"), 6), "surfaceflinger bloccato nel kernel viene segnalato")
        check(wait_for(lambda: any("sf D/do_epoll_wait" in l for l in c.of("T")), 3), "lo stato D di surfaceflinger compare nel campione")
        c.close()
    finally:
        p.close()


def test_sampler_stuck():
    ph = Phone()
    p = Probe(ph)
    try:
        c = p.client()
        wait_for(lambda: len(c.of("T")) >= 3, 5)
        f = f"{ph.root}/sys/class/kgsl/kgsl-3d0/gpu_busy_percentage"
        os.remove(f)
        os.mkfifo(f)  # aprirlo in lettura non torna mai: come una lettura della GPU durante un blocco della GPU
        check(wait_for(lambda: any("FERMO(gpu:gpu_busy_percentage" in l for l in c.of("T")), 10), "il campionatore hw incastrato è indicato con la lettura che non torna")
        check(c.has("S", "campionatore hw fermo in gpu:gpu_busy_percentage"), "e viene segnalato come stallo")
        n = len(c.of("T"))
        time.sleep(1.0)
        check(len(c.of("T")) - n >= 7, "mentre uno è fermo, gli altri campionatori e la trasmissione proseguono")
        check(any("c u" in l and "FERMO(core" not in l for l in c.of("T")[-3:]), "la parte «core» continua ad aggiornarsi")
        c.close()
        # sblocca il lettore perché il thread possa finire
        fd = os.open(f, os.O_WRONLY | os.O_NONBLOCK)
        os.close(fd)
    finally:
        p.close()


def test_slow_client():
    ph = Phone()
    p = Probe(ph)
    try:
        slow = p.client(rcvbuf=2048)
        slow.stop = True  # smette di leggere
        time.sleep(0.3)
        good = p.client()
        wait_for(lambda: len(good.of("T")) >= 10, 5)
        n0 = len(good.of("T"))
        t0 = time.monotonic()
        time.sleep(3.0)
        rate = (len(good.of("T")) - n0) / (time.monotonic() - t0)
        check(7 <= rate <= 13, f"un client che non legge non rallenta gli altri ({rate:.1f} campioni/s)")
        check(p.proc.poll() is None, "la sonda è ancora viva")
        good.close()
        slow.close()
    finally:
        p.close()


def test_blackbox_restart():
    ph = Phone()
    p = Probe(ph)
    try:
        c = p.client()
        wait_for(lambda: len(c.of("T")) >= 3, 5)
        names = p.children()
        bb = [pid for pid, n in names.items() if n == "phantom-bbox"]
        check(len(bb) == 1, f"un solo processo phantom-bbox ({names})")
        os.kill(bb[0], signal.SIGKILL)
        check(wait_for(lambda: "scatola nera rilanciata" in p.log(), 6), "il demone rilancia la scatola nera se muore")
        n = len(c.of("T"))
        check(wait_for(lambda: len(c.of("T")) > n + 5, 6), "il flusso riprende senza riconnettersi")
        bb2 = [pid for pid, nm in p.children().items() if nm == "phantom-bbox"]
        check(len(bb2) == 1 and bb2[0] != bb[0], "il nuovo processo è diverso dal vecchio")
        c.close()
        os.kill(p.proc.pid, signal.SIGTERM)
        check(wait_for(lambda: p.proc.poll() is not None, 5), "il demone si ferma con SIGTERM")
        check(wait_for(lambda: not [1 for n in p.children().values() if n == "phantom-bbox"], 5), "e la scatola nera si ferma con lui")
    finally:
        p.close()


def test_daemon_killed():
    ph = Phone()
    p = Probe(ph)
    try:
        c = p.client()
        wait_for(lambda: len(c.of("T")) >= 3, 5)
        bb = [pid for pid, n in p.children().items() if n == "phantom-bbox"]
        os.kill(p.proc.pid, signal.SIGKILL)  # morte di colpo, senza congedo
        p.proc.wait()
        check(wait_for(lambda: not any(os.path.exists(f"/proc/{b}") and "Z" not in open(f"/proc/{b}/stat").read().rsplit(")", 1)[1][:3] for b in bb), 5),
              "se il demone muore di colpo, anche la scatola nera muore (mai un campionatore orfano)")
        c.close()
    finally:
        p.close()


def test_session_info():
    ph = Phone()
    p = Probe(ph, scrcpy=True, fake_env={"FAKE_FRAMES": "-1", "FAKE_FPS": "60", "FAKE_FRAME_BYTES": "2000"})
    try:
        c = p.client()
        wait_for(lambda: len(c.of("T")) >= 3, 5)
        check(all("R -" in l for l in c.of("T")[-3:]), "senza sessione video il campione dice «R -»")
        s = socket.create_connection(("127.0.0.1", p.port + 3))
        s.settimeout(0.5)
        stop = [False]

        def drain():  # lato iPhone: legge sempre
            while not stop[0]:
                try:
                    if not s.recv(65536):
                        break
                except socket.timeout:
                    pass
                except OSError:
                    break

        th = threading.Thread(target=drain, daemon=True)
        th.start()
        check(wait_for(lambda: any(" R " in l and " R -" not in l and "giro" in l for l in c.of("T")[-4:]), 6), "durante la sessione il campione descrive il ponte (R <pid> giro…)")
        t = [l for l in c.of("T") if "giro" in l][-1]
        check("fr" in t and "q0K" in t, f"con contatori di frame e coda  [{t[t.index(' R '):t.index(' | a ')]}]")
        n1 = len(c.of("T"))
        time.sleep(1.0)
        check(len(c.of("T")) - n1 >= 7, "frequenza piena durante la sessione")
        check(wait_for(lambda: "scatola nera aperta" in p.blackbox(), 4), "durante la sessione il registro su disco si apre da solo")
        check(wait_for(lambda: " T " in "\n" + p.blackbox() or "\nT " in p.blackbox(), 4), "e vi finiscono i campioni")
        check(c.has("E", "sessione video avviata: ponte pid"), "l'inizio della sessione è segnato nel flusso (riga E)")
        stop[0] = True
        s.close()
        check(wait_for(lambda: all("R -" in l for l in c.of("T")[-2:]), 6), "a sessione chiusa il campione torna a «R -»")
        check(wait_for(lambda: c.has("E", "sessione video terminata"), 4), "e anche la fine")
        c.close()
    finally:
        p.close()


def test_no_false_stall_on_connect():
    ph = Phone()
    p = Probe(ph, extra=["--tm-idle-ms", "3000"])  # a riposo si campiona ogni 3 s: al collegamento l'ultimo campione è «vecchio» ma non incastrato
    try:
        time.sleep(2.4)
        c = p.client()
        time.sleep(3.0)
        check(len(c.of("T")) >= 10, "il flusso parte")
        check(not c.of("S"), "nessun falso stallo al passaggio da riposo ad attivo")
        c.close()
    finally:
        p.close()


def test_autostop():
    ph = Phone()
    p = Probe(ph, scrcpy=True, fake_env={"FAKE_FRAMES": "-1", "FAKE_FPS": "60", "FAKE_FRAME_BYTES": "2000"}, extra=["--tm-autostop"])
    try:
        c = p.client()
        wait_for(lambda: len(c.of("T")) >= 3, 5)
        s = socket.create_connection(("127.0.0.1", p.port + 3))
        s.settimeout(0.5)
        state = {"eof": False, "stop": False}

        def drain():
            while not state["stop"]:
                try:
                    if not s.recv(65536):
                        state["eof"] = True
                        return
                except socket.timeout:
                    pass
                except OSError:
                    state["eof"] = True
                    return

        threading.Thread(target=drain, daemon=True).start()
        check(wait_for(lambda: c.has("E", "sessione video avviata"), 6), "sessione avviata")
        time.sleep(1.0)
        check(not state["eof"], "la sessione scorre")
        ph.add_dstate()
        ph.set_blocked(5)  # stallo che dura
        check(wait_for(lambda: c.has("S", "arresto automatico dopo"), 8), "dopo 3 s di stallo la scatola nera ferma la sessione (S arresto automatico)")
        check(wait_for(lambda: state["eof"], 6), "e il flusso verso l'iPhone si chiude")
        check(wait_for(lambda: c.has("E", "sessione video terminata"), 6), "la sessione risulta terminata")
        check(wait_for(lambda: not p.read_fake_server_alive(), 6), "il server di cattura è fermato")
        state["stop"] = True
        s.close()
        c.close()
    finally:
        p.close()


def test_no_autostop_without_option():
    ph = Phone()
    p = Probe(ph, scrcpy=True, fake_env={"FAKE_FRAMES": "-1", "FAKE_FPS": "60", "FAKE_FRAME_BYTES": "2000"})
    try:
        c = p.client()
        wait_for(lambda: len(c.of("T")) >= 3, 5)
        s = socket.create_connection(("127.0.0.1", p.port + 3))
        s.settimeout(0.5)
        state = {"eof": False, "stop": False}

        def drain():
            while not state["stop"]:
                try:
                    if not s.recv(65536):
                        state["eof"] = True
                        return
                except socket.timeout:
                    pass
                except OSError:
                    state["eof"] = True
                    return

        threading.Thread(target=drain, daemon=True).start()
        wait_for(lambda: c.has("E", "sessione video avviata"), 6)
        ph.add_dstate()
        ph.set_blocked(5)
        check(wait_for(lambda: c.has("S", "continua da 4"), 8), "lo stallo dura più di 4 s")
        check(not c.has("S", "arresto automatico") and not state["eof"], "senza --tm-autostop la sessione non viene toccata")
        state["stop"] = True
        s.close()
        c.close()
    finally:
        p.close()


def test_idle_history():
    ph = Phone()
    p = Probe(ph)
    try:
        time.sleep(1.8)  # nessun client: la scatola nera campiona lentamente (300 ms nel test) ma l'anello si riempie
        c = p.client()
        time.sleep(0.5)
        ts = [l.split()[1] for l in c.of("T")]
        check(len(ts) >= 4, f"a riposo l'anello si riempie lo stesso ({len(ts)} righe di storia)")
        c.close()
    finally:
        p.close()


def main():
    tests = [(n, f) for n, f in sorted(globals().items()) if n.startswith("test_") and FILTER in n]
    for name, fn in tests:
        print(name[5:].replace("_", " "), flush=True)
        fn()
    print()
    if FAILURES:
        print(f"{len(FAILURES)} controlli falliti")
        sys.exit(1)
    print("Tutti i test superati")


if __name__ == "__main__":
    main()
