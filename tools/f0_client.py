#!/usr/bin/env python3
"""Phantom · fase 0 — client di test da PC (Windows, macOS, Linux). Solo libreria standard.

Comandi:
  status HOST                     stato del telefono e della sonda
  ping   HOST [--seconds 30]      RTT, jitter e perdita verso la sonda (UDP, come il Phantom vero)
  doze   HOST [--every 300]       raggiungibilità del telefono a schermo spento, per ore
  video  HOST                     apre il flusso video di base con ffplay (se installato)

Esempi:
  python f0_client.py ping 192.168.1.50
  python f0_client.py doze 192.168.1.50 --every 300 --hours 2 --csv doze.csv
"""
from __future__ import annotations

import argparse
import csv
import json
import shutil
import socket
import statistics
import struct
import subprocess
import sys
import threading
import time
import urllib.request
from datetime import datetime

UDP_PORT, HTTP_PORT, VIDEO_PORT = 47800, 47801, 47802
MAGIC = b"PHP1"
HDR = struct.Struct("<4sIQQQ")  # magic, seq, client_ns, server_mono_ns, server_suspend_ms


def now_ns() -> int:
    return time.perf_counter_ns()


def make_packet(seq: int, size: int) -> bytes:
    pkt = HDR.pack(MAGIC, seq & 0xFFFFFFFF, now_ns(), 0, 0)
    return pkt + b"\0" * max(0, size - HDR.size)


def parse_packet(data: bytes):
    if len(data) < HDR.size:
        return None
    magic, seq, client_ns, srv_mono, srv_susp = HDR.unpack_from(data)
    return None if magic != MAGIC else (seq, client_ns, srv_mono, srv_susp)


def percentile(sorted_vals: list[float], p: float) -> float:
    if not sorted_vals:
        return float("nan")
    return sorted_vals[min(len(sorted_vals) - 1, int(p / 100 * len(sorted_vals)))]


# ---------------------------------------------------------------- status

def cmd_status(a) -> int:
    url = f"http://{a.host}:{HTTP_PORT}/status"
    try:
        with urllib.request.urlopen(url, timeout=5) as r:
            s = json.load(r)
    except OSError as e:
        print(f"Sonda non raggiungibile su {url}: {e}")
        return 1
    d = s["device"]
    print(f"Telefono : {d['model']} ({d['codename']}), Android {d['android']} (SDK {d['sdk']}), LineageOS {d['lineage']}")
    print(f"SoC      : {d['soc']}")
    print(f"Sonda    : v{s['probe']}, attiva da {s['uptime_s']:.0f} s, video {'acceso' if s['video'] else 'spento'}")
    print(f"Sonno    : {s['suspended_ms'] / 1000:.0f} s di sospensione dall'avvio del telefono")
    return 0


# ------------------------------------------------------------------ ping

def cmd_ping(a) -> int:
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(0.2)
    try:
        sock.setsockopt(socket.IPPROTO_IP, socket.IP_TOS, 0xB8)  # DSCP EF, come la sonda
    except OSError:
        pass
    dest = (a.host, UDP_PORT)
    total = int(a.seconds * 1000 / a.interval)
    sent_at: dict[int, int] = {}
    rtts: list[tuple[int, float]] = []
    susp_first = susp_last = None
    stop = threading.Event()

    def receiver() -> None:
        nonlocal susp_first, susp_last
        while not stop.is_set():
            try:
                data, _ = sock.recvfrom(2048)
            except (socket.timeout, OSError):
                continue
            t = now_ns()
            p = parse_packet(data)
            if p is None or p[0] not in sent_at:
                continue
            rtts.append((p[0], (t - p[1]) / 1e6))
            susp_first = p[3] if susp_first is None else susp_first
            susp_last = p[3]

    th = threading.Thread(target=receiver, daemon=True)
    th.start()
    print(f"Ping UDP verso {a.host}:{UDP_PORT}: {total} pacchetti da {a.size} byte ogni {a.interval} ms…")
    start = now_ns()
    for seq in range(total):
        target = start + seq * a.interval * 1_000_000
        delay = (target - now_ns()) / 1e9
        if delay > 0:
            time.sleep(delay)
        sent_at[seq] = now_ns()
        sock.sendto(make_packet(seq, a.size), dest)
        if seq and seq % int(5000 / a.interval) == 0:
            done = [r for _, r in rtts]
            if done:
                print(f"  {seq * a.interval / 1000:5.0f} s  ricevuti {len(done)}  p50 {statistics.median(done):.1f} ms")
    time.sleep(1.0)
    stop.set()
    th.join()

    vals = [r for _, r in sorted(rtts)]
    if not vals:
        print("Nessuna risposta: controlla IP, che la sonda sia attiva e che PC e telefono siano sulla stessa rete.")
        return 1
    order = [r for _, r in sorted(rtts, key=lambda x: x[0])]
    s = sorted(vals)
    jitter = statistics.fmean(abs(order[i] - order[i - 1]) for i in range(1, len(order))) if len(order) > 1 else 0.0
    lost = total - len(vals)
    spikes = sum(1 for v in vals if v > 50)
    print()
    print(f"Risultato  {datetime.now().isoformat(timespec='seconds')}  →  {a.host}")
    print(f"  ricevuti   {len(vals)}/{total}  (persi {lost}, {lost / total * 100:.2f}%)")
    print(f"  RTT        p50 {percentile(s, 50):.1f}  p95 {percentile(s, 95):.1f}  p99 {percentile(s, 99):.1f}  max {s[-1]:.1f} ms")
    print(f"  jitter     {jitter:.2f} ms  ·  picchi >50 ms: {spikes}")
    if susp_first is not None and susp_last is not None:
        print(f"  il telefono ha dormito {susp_last - susp_first} ms durante il test (atteso: 0)")
    print("  istogramma RTT:")
    edges = [1, 2, 5, 10, 20, 50, 100, float("inf")]
    lo = 0.0
    for hi in edges:
        n = sum(1 for v in vals if lo <= v < hi)
        label = f"{lo:g}–{hi:g} ms" if hi != float("inf") else f">{lo:g} ms"
        print(f"    {label:>12}  {'█' * max(0, round(n / len(vals) * 50)):<50} {n}")
        lo = hi
    return 0


# ------------------------------------------------------------------ doze

def probe_once(sock: socket.socket, dest, seq: int, timeout_s: float):
    """Manda un pacchetto al secondo finché arriva una risposta o scade il tempo."""
    deadline = time.monotonic() + timeout_s
    attempts = 0
    first_sent = None
    while time.monotonic() < deadline:
        attempts += 1
        pkt = make_packet(seq * 100 + attempts, 64)
        first_sent = first_sent or now_ns()
        sock.sendto(pkt, dest)
        end = min(deadline, time.monotonic() + 1.0)
        while time.monotonic() < end:
            sock.settimeout(max(0.01, end - time.monotonic()))
            try:
                data, _ = sock.recvfrom(2048)
            except (socket.timeout, OSError):
                continue
            p = parse_packet(data)
            if p and p[0] // 100 == seq:
                return attempts, (now_ns() - first_sent) / 1e6, (now_ns() - p[1]) / 1e6, p[3]
    return attempts, None, None, None


def cmd_doze(a) -> int:
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    dest = (a.host, UDP_PORT)
    rounds = max(1, int(a.hours * 3600 / a.every))
    out = open(a.csv, "w", newline="") if a.csv else None
    w = csv.writer(out) if out else None
    if w:
        w.writerow(["ora", "raggiunto", "tentativi", "tempo_risposta_ms", "rtt_ultimo_ms", "sonno_dal_giro_prima_ms"])
    print(f"Test Doze verso {a.host}: un controllo ogni {a.every} s per {a.hours} h ({rounds} giri).")
    print("Spegni lo schermo del telefono, scollegalo dal caricatore e non toccarlo. Il PC deve restare acceso.\n")
    ok = 0
    wake_times: list[float] = []
    last_susp = None
    slept_total = 0
    for i in range(rounds):
        t0 = time.monotonic()
        attempts, wake_ms, rtt_ms, susp = probe_once(sock, dest, i + 1, a.timeout)
        slept = None
        if susp is not None:
            slept = susp - last_susp if last_susp is not None else 0
            slept_total += slept
            last_susp = susp
        stamp = datetime.now().strftime("%H:%M:%S")
        if wake_ms is not None:
            ok += 1
            wake_times.append(wake_ms)
            print(f"{stamp}  giro {i + 1:3}/{rounds}  raggiunto in {wake_ms:7.0f} ms ({attempts} tentativi)  "
                  f"il telefono ha dormito {slept / 1000:6.0f} s dal giro prima")
        else:
            print(f"{stamp}  giro {i + 1:3}/{rounds}  NON RAGGIUNTO dopo {attempts} tentativi in {a.timeout:.0f} s")
        if w:
            w.writerow([stamp, wake_ms is not None, attempts, f"{wake_ms:.0f}" if wake_ms is not None else "", f"{rtt_ms:.1f}" if rtt_ms is not None else "", slept if slept is not None else ""])
            out.flush()
        if i + 1 < rounds:
            time.sleep(max(0.0, a.every - (time.monotonic() - t0)))
    print()
    print(f"Raggiungibile {ok}/{rounds} volte ({ok / rounds * 100:.0f}%).")
    if wake_times:
        s = sorted(wake_times)
        print(f"Tempo per svegliarlo e ricevere risposta: mediano {percentile(s, 50):.0f} ms, peggiore {s[-1]:.0f} ms.")
    print(f"Il telefono ha dormito in tutto {slept_total / 1000:.0f} s: se è vicino alla durata del test, il Doze era davvero attivo.")
    print("Criterio F0: 100% raggiungibile, risposta entro 3 s.")
    return 0 if ok == rounds else 1


# ----------------------------------------------------------------- video

def cmd_video(a) -> int:
    url = f"tcp://{a.host}:{VIDEO_PORT}"
    cmd = ["ffplay", "-hide_banner", "-fflags", "nobuffer", "-flags", "low_delay", "-framedrop",
           "-probesize", "32", "-analyzeduration", "0", "-sync", "ext", "-f", "h264", url]
    print("Flusso di base (screenrecord, non ottimizzato). Comando:")
    print("  " + " ".join(cmd))
    if not shutil.which("ffplay"):
        print("\nffplay non trovato: installa FFmpeg (https://ffmpeg.org/download.html) e rilancia, oppure usa il comando sopra.")
        return 1
    return subprocess.call(cmd)


def main() -> int:
    ap = argparse.ArgumentParser(description="Phantom · fase 0 — client di test")
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("status"); p.add_argument("host"); p.set_defaults(fn=cmd_status)
    p = sub.add_parser("ping"); p.add_argument("host")
    p.add_argument("--seconds", type=float, default=30); p.add_argument("--interval", type=float, default=10, help="ms tra i pacchetti")
    p.add_argument("--size", type=int, default=64, help="byte per pacchetto (prova anche 1200)"); p.set_defaults(fn=cmd_ping)
    p = sub.add_parser("doze"); p.add_argument("host")
    p.add_argument("--every", type=float, default=300, help="secondi tra i controlli"); p.add_argument("--hours", type=float, default=2)
    p.add_argument("--timeout", type=float, default=10, help="secondi di tentativi per ogni controllo"); p.add_argument("--csv")
    p.set_defaults(fn=cmd_doze)
    p = sub.add_parser("video"); p.add_argument("host"); p.set_defaults(fn=cmd_video)
    a = ap.parse_args()
    return a.fn(a)


if __name__ == "__main__":
    sys.exit(main())
