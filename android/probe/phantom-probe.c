/*
 * phantom-probe — Phantom, fase 0 (prototipo di fattibilità)
 *
 * Un solo binario statico, senza dipendenze, eseguito come root su Android.
 * Serve a misurare tre cose prima di scrivere il sistema vero:
 *
 *   1. Rete    UDP 47800  eco con timestamp: RTT, jitter e perdita misurati dal client.
 *   2. Doze    ogni pacchetto registra quanto il telefono ha dormito (CLOCK_BOOTTIME -
 *              CLOCK_MONOTONIC): dimostra se il telefono si sveglia per un pacchetto.
 *   3. Video   TCP 47802  flusso H.264 grezzo da `screenrecord` (encoder hardware),
 *              per la latenza di base. Spento di default, solo su richiesta (--video).
 *
 * In più, TCP 47801 serve una pagina web per l'iPhone (test di latenza in Safari via
 * WebSocket) e un orologio al millisecondo per le misure con video al rallentatore.
 *
 * Sicurezza: è uno strumento di test. Accetta solo indirizzi privati (LAN, Tailscale,
 * loopback) salvo --allow-any, e il video resta spento finché non lo accendi.
 *
 * Compilazione (statica, arm64): zig cc -target aarch64-linux-musl -O2 -static
 */
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <poll.h>
#include <signal.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <stddef.h>
#include <sys/file.h>
#include <sys/prctl.h>
#include <sys/types.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define PROBE_VERSION "0.5.1"
#define UDP_MAGIC "PHP1"
#define UDP_HDR 32          /* magic(4) seq(4) client_ns(8) server_mono_ns(8) server_suspend_ms(8) */
#define WAKE_GAP_NS (30ull * 1000000000ull)

static int g_udp_port = 47800;
static int g_http_port = 47801;
static int g_video_port = 47802;
static int g_video = 0;
/* Ponte verso il server di scrcpy (progetto open source, Apache 2.0): ogni connessione TCP avvia
 * il suo server via app_process e ne inoltra il flusso. Spento se g_scrcpy_port == 0. */
static int g_scrcpy_port = 0;
static const char *g_scrcpy_jar = NULL;
static const char *g_scrcpy_ver = NULL;
static const char *g_app_process = "/system/bin/app_process";
static const char *g_scrcpy_sock = NULL; /* solo per i test: socket UNIX su file invece dell'astratto */
#define MAX_SCRCPY_OPTS 32
static const char *g_scrcpy_opts[MAX_SCRCPY_OPTS];
static int g_scrcpy_nopts = 0;
static int g_allow_any = 0;
static const char *g_state_dir = "/data/adb/phantom-f0"; /* marker e blocco di sessione */
static const char *g_scrcpy_profile = "";
static int g_stall_client_ms = 1500;  /* l'iPhone non accetta dati da tanto: si chiude la sessione */
static int g_stall_server_ms = 0;     /* 0 = spento: uno schermo fermo non produce frame */
static int g_max_age_ms = 250;        /* un frame più vecchio di così in coda viene scartato */
static int g_first_frame_ms = 8000;   /* tempo massimo per il primo frame dopo l'avvio del server */
static long g_bitrate = 12000000;
static const char *g_size = NULL;
static const char *g_screenrecord = "/system/bin/screenrecord";
static const char *g_getprop = "/system/bin/getprop";
static const char *g_log_path = NULL;
static const char *g_pid_path = NULL;
static int g_log_fd = 2;
static uint64_t g_start_mono;

/* ------------------------------------------------------------------ utilità */

static uint64_t now_ns(clockid_t c) {
    struct timespec ts;
    clock_gettime(c, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
}

/* Millisecondi passati in sospensione dall'avvio: BOOTTIME conta il sonno, MONOTONIC no. */
static uint64_t suspended_ms(void) {
    uint64_t b = now_ns(CLOCK_BOOTTIME), m = now_ns(CLOCK_MONOTONIC);
    return b > m ? (b - m) / 1000000ull : 0;
}

static void logmsg(const char *fmt, ...) {
    char buf[1200];
    time_t t = time(NULL);
    struct tm tm;
    gmtime_r(&t, &tm);
    size_t n = strftime(buf, sizeof buf, "%Y-%m-%dT%H:%M:%SZ ", &tm);
    va_list ap;
    va_start(ap, fmt);
    int w = vsnprintf(buf + n, sizeof buf - n - 1, fmt, ap);
    va_end(ap);
    if (w < 0) return;
    n += (size_t)w;
    if (n > sizeof buf - 2) n = sizeof buf - 2;
    buf[n++] = '\n';
    ssize_t r = write(g_log_fd, buf, n);
    (void)r;
}

static void put_u64(unsigned char *p, uint64_t v) { for (int i = 0; i < 8; i++) p[i] = (unsigned char)(v >> (8 * i)); }
static uint32_t get_u32(const unsigned char *p) { uint32_t v = 0; for (int i = 3; i >= 0; i--) v = (v << 8) | p[i]; return v; }

static pid_t stop_sessions(void);
static pid_t active_session_pid(void);

static int send_all(int fd, const void *buf, size_t len) {
    const char *p = buf;
    while (len) {
        ssize_t w = send(fd, p, len, MSG_NOSIGNAL);
        if (w < 0) { if (errno == EINTR) continue; return -1; }
        p += w; len -= (size_t)w;
    }
    return 0;
}

static int is_private(const struct sockaddr_in *a) {
    uint32_t ip = ntohl(a->sin_addr.s_addr);
    return (ip >> 24) == 10 ||                    /* 10.0.0.0/8      LAN       */
           (ip >> 24) == 127 ||                   /* 127.0.0.0/8     loopback  */
           (ip >> 20) == (172u << 4 | 1) ||       /* 172.16.0.0/12   LAN       */
           (ip >> 16) == (192u << 8 | 168) ||     /* 192.168.0.0/16  LAN       */
           (ip >> 22) == (100u << 2 | 1) ||       /* 100.64.0.0/10   Tailscale */
           (ip >> 16) == (169u << 8 | 254);       /* 169.254.0.0/16  link-local */
}

static int allowed(const struct sockaddr_in *a) { return g_allow_any || is_private(a); }

static const char *addr_str(const struct sockaddr_in *a, char *buf, size_t n) {
    snprintf(buf, n, "%s:%u", inet_ntoa(a->sin_addr), ntohs(a->sin_port));
    return buf;
}

static int listen_tcp(int port) {
    int fd = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (fd < 0) return -1;
    int one = 1;
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    struct sockaddr_in sa = {.sin_family = AF_INET, .sin_port = htons((uint16_t)port), .sin_addr.s_addr = htonl(INADDR_ANY)};
    if (bind(fd, (struct sockaddr *)&sa, sizeof sa) < 0 || listen(fd, 8) < 0) { close(fd); return -1; }
    return fd;
}

/* Esegue un programma senza shell e ne cattura lo stdout (prima riga, senza a capo). */
static void run_capture(const char *path, char *const argv[], char *out, size_t n) {
    out[0] = 0;
    int p[2];
    if (pipe(p) < 0) return;
    pid_t pid = fork();
    if (pid == 0) {
        dup2(p[1], 1);
        int dn = open("/dev/null", O_WRONLY);
        if (dn >= 0) dup2(dn, 2);
        close(p[0]); close(p[1]);
        execv(path, argv);
        _exit(127);
    }
    close(p[1]);
    size_t got = 0;
    ssize_t r;
    while (got + 1 < n && (r = read(p[0], out + got, n - 1 - got)) > 0) got += (size_t)r;
    out[got] = 0;
    char *nl = strchr(out, '\n');
    if (nl) *nl = 0;
    close(p[0]);
    if (pid > 0) waitpid(pid, NULL, 0);
}

static void getprop(const char *key, char *out, size_t n) {
    char *argv[] = {(char *)"getprop", (char *)key, NULL};
    run_capture(g_getprop, argv, out, n);
    if (!out[0]) snprintf(out, n, "sconosciuto");
}

/* -------------------------------------------------------------------- SHA-1 */
/* Serve solo per l'handshake WebSocket (RFC 6455). */

typedef struct { uint32_t h[5]; uint64_t len; unsigned char buf[64]; size_t used; } sha1_ctx;
#define ROL(v, b) (((v) << (b)) | ((v) >> (32 - (b))))

static void sha1_block(sha1_ctx *c, const unsigned char *p) {
    uint32_t w[80];
    for (int i = 0; i < 16; i++) w[i] = (uint32_t)p[4 * i] << 24 | (uint32_t)p[4 * i + 1] << 16 | (uint32_t)p[4 * i + 2] << 8 | p[4 * i + 3];
    for (int i = 16; i < 80; i++) w[i] = ROL(w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16], 1);
    uint32_t a = c->h[0], b = c->h[1], cc = c->h[2], d = c->h[3], e = c->h[4];
    for (int i = 0; i < 80; i++) {
        uint32_t f, k;
        if (i < 20) { f = (b & cc) | (~b & d); k = 0x5A827999; }
        else if (i < 40) { f = b ^ cc ^ d; k = 0x6ED9EBA1; }
        else if (i < 60) { f = (b & cc) | (b & d) | (cc & d); k = 0x8F1BBCDC; }
        else { f = b ^ cc ^ d; k = 0xCA62C1D6; }
        uint32_t t = ROL(a, 5) + f + e + k + w[i];
        e = d; d = cc; cc = ROL(b, 30); b = a; a = t;
    }
    c->h[0] += a; c->h[1] += b; c->h[2] += cc; c->h[3] += d; c->h[4] += e;
}

static void sha1(const unsigned char *data, size_t len, unsigned char out[20]) {
    sha1_ctx c = {{0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0}, 0, {0}, 0};
    c.len = (uint64_t)len * 8;
    while (len >= 64) { sha1_block(&c, data); data += 64; len -= 64; }
    unsigned char tail[128] = {0};
    memcpy(tail, data, len);
    tail[len] = 0x80;
    size_t tl = len < 56 ? 64 : 128;
    for (int i = 0; i < 8; i++) tail[tl - 1 - i] = (unsigned char)(c.len >> (8 * i));
    sha1_block(&c, tail);
    if (tl == 128) sha1_block(&c, tail + 64);
    for (int i = 0; i < 5; i++) { out[4 * i] = (unsigned char)(c.h[i] >> 24); out[4 * i + 1] = (unsigned char)(c.h[i] >> 16); out[4 * i + 2] = (unsigned char)(c.h[i] >> 8); out[4 * i + 3] = (unsigned char)c.h[i]; }
}

static void base64(const unsigned char *in, size_t n, char *out) {
    static const char t[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    size_t o = 0;
    for (size_t i = 0; i < n; i += 3) {
        uint32_t v = (uint32_t)in[i] << 16 | (i + 1 < n ? (uint32_t)in[i + 1] << 8 : 0) | (i + 2 < n ? in[i + 2] : 0);
        out[o++] = t[(v >> 18) & 63];
        out[o++] = t[(v >> 12) & 63];
        out[o++] = i + 1 < n ? t[(v >> 6) & 63] : '=';
        out[o++] = i + 2 < n ? t[v & 63] : '=';
    }
    out[o] = 0;
}

/* ------------------------------------------------------------- pagina web */

#include "webpage.h" /* PAGE_HTML: test di latenza e orologio, generato da webpage.html */

static int recv_headers(int fd, char *buf, size_t n) {
    size_t got = 0;
    while (got + 1 < n) {
        ssize_t r = recv(fd, buf + got, n - 1 - got, 0);
        if (r <= 0) return -1;
        got += (size_t)r;
        buf[got] = 0;
        if (strstr(buf, "\r\n\r\n")) return (int)got;
    }
    return -1;
}

static const char *header_value(const char *req, const char *name, char *out, size_t n) {
    const char *p = req;
    size_t nl = strlen(name);
    while ((p = strstr(p, "\r\n")) != NULL) {
        p += 2;
        if (strncasecmp(p, name, nl) == 0 && p[nl] == ':') {
            p += nl + 1;
            while (*p == ' ') p++;
            size_t i = 0;
            while (p[i] && p[i] != '\r' && i + 1 < n) { out[i] = p[i]; i++; }
            out[i] = 0;
            return out;
        }
    }
    return NULL;
}

static void http_reply(int fd, const char *status, const char *type, const char *body, size_t len) {
    char h[256];
    int n = snprintf(h, sizeof h,
                     "HTTP/1.1 %s\r\nContent-Type: %s\r\nContent-Length: %zu\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
                     status, type, len);
    send_all(fd, h, (size_t)n);
    send_all(fd, body, len);
}

static void serve_status(int fd) {
    char model[96], device[64], release[32], sdk[16], rom[128], soc[64];
    getprop("ro.product.model", model, sizeof model);
    getprop("ro.product.device", device, sizeof device);
    getprop("ro.build.version.release", release, sizeof release);
    getprop("ro.build.version.sdk", sdk, sizeof sdk);
    getprop("ro.lineage.version", rom, sizeof rom);
    getprop("ro.soc.model", soc, sizeof soc);
    char body[1280];
    pid_t sess = g_scrcpy_port > 0 ? active_session_pid() : 0;
    int n = snprintf(body, sizeof body,
                     "{\"probe\":\"%s\",\"uptime_s\":%.1f,\"suspended_ms\":%llu,\"video\":%s,"
                     "\"scrcpy\":{\"enabled\":%s,\"active\":%s,\"pid\":%d,\"profile\":\"%s\"},"
                     "\"ports\":{\"udp\":%d,\"http\":%d,\"video\":%d},"
                     "\"device\":{\"model\":\"%s\",\"codename\":\"%s\",\"android\":\"%s\",\"sdk\":\"%s\",\"lineage\":\"%s\",\"soc\":\"%s\"}}\n",
                     PROBE_VERSION, (double)(now_ns(CLOCK_MONOTONIC) - g_start_mono) / 1e9,
                     (unsigned long long)suspended_ms(), g_video ? "true" : "false",
                     g_scrcpy_port > 0 ? "true" : "false", sess > 0 ? "true" : "false", (int)sess, g_scrcpy_profile ? g_scrcpy_profile : "",
                     g_udp_port, g_http_port, g_video_port, model, device, release, sdk, rom, soc);
    http_reply(fd, "200 OK", "application/json", body, (size_t)n);
}

/* Eco WebSocket: rimanda ogni messaggio così com'è; il browser misura l'RTT. */
static void serve_websocket(int fd, const char *req, const char *peer) {
    char key[128];
    if (!header_value(req, "Sec-WebSocket-Key", key, sizeof key)) {
        http_reply(fd, "400 Bad Request", "text/plain", "manca Sec-WebSocket-Key\n", 24);
        return;
    }
    char cat[256];
    snprintf(cat, sizeof cat, "%s258EAFA5-E914-47DA-95CA-C5AB0DC85B11", key);
    unsigned char dig[20];
    char acc[64];
    sha1((const unsigned char *)cat, strlen(cat), dig);
    base64(dig, 20, acc);
    char h[256];
    int n = snprintf(h, sizeof h,
                     "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: %s\r\n\r\n", acc);
    if (send_all(fd, h, (size_t)n) < 0) return;
    logmsg("ws: test di latenza avviato da %s", peer);

    unsigned char hdr[14], payload[4096];
    unsigned long msgs = 0;
    for (;;) {
        if (recv(fd, hdr, 2, MSG_WAITALL) != 2) break;
        int op = hdr[0] & 0x0F, masked = hdr[1] & 0x80;
        uint64_t len = hdr[1] & 0x7F;
        if (len == 126) { if (recv(fd, hdr + 2, 2, MSG_WAITALL) != 2) break; len = (uint64_t)hdr[2] << 8 | hdr[3]; }
        else if (len == 127) break; /* messaggi enormi non servono a questo test */
        unsigned char mask[4] = {0};
        if (masked && recv(fd, mask, 4, MSG_WAITALL) != 4) break;
        if (len > sizeof payload) break;
        if (len && recv(fd, payload, (size_t)len, MSG_WAITALL) != (ssize_t)len) break;
        for (uint64_t i = 0; i < len; i++) payload[i] ^= mask[i & 3];
        if (op == 8) { unsigned char c[2] = {0x88, 0}; send_all(fd, c, 2); break; }
        int reply_op = (op == 9) ? 10 : op; /* ping -> pong, testo/binario -> eco */
        if (op != 1 && op != 2 && op != 9) continue;
        unsigned char out[4];
        size_t on = 0;
        out[on++] = (unsigned char)(0x80 | reply_op);
        if (len < 126) out[on++] = (unsigned char)len;
        else { out[on++] = 126; out[on++] = (unsigned char)(len >> 8); out[on++] = (unsigned char)len; }
        if (send_all(fd, out, on) < 0 || send_all(fd, payload, (size_t)len) < 0) break;
        msgs++;
    }
    logmsg("ws: test terminato con %s (%lu messaggi)", peer, msgs);
}

static void serve_http(int fd, const struct sockaddr_in *peer_sa) {
    char peer[64], req[4096];
    addr_str(peer_sa, peer, sizeof peer);
    int one = 1;
    setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one);
    if (recv_headers(fd, req, sizeof req) < 0) return;
    if (strncmp(req, "GET ", 4) != 0) { http_reply(fd, "405 Method Not Allowed", "text/plain", "solo GET\n", 9); return; }
    const char *path = req + 4;
    if (strncmp(path, "/ws ", 4) == 0) serve_websocket(fd, req, peer);
    else if (strncmp(path, "/status ", 8) == 0) serve_status(fd);
    else if (strncmp(path, "/stop ", 6) == 0) {
        /* Interruttore d'emergenza: ferma la sessione video scrcpy attiva (libera lo schermo del telefono). */
        pid_t pid = stop_sessions();
        char body[96];
        int bn = snprintf(body, sizeof body, "{\"stopped\":%s,\"pid\":%d}\n", pid > 0 ? "true" : "false", (int)pid);
        http_reply(fd, "200 OK", "application/json", body, (size_t)bn);
        /* Prima si risponde, poi si scrive il log (sincrono su disco): se il disco è fermo, l'iPhone riceve comunque la conferma. */
        logmsg("stop richiesto da %s: %s", peer, pid > 0 ? "sessione fermata" : "nessuna sessione attiva");
    }
    else if (strncmp(path, "/ ", 2) == 0 || strncmp(path, "/clock ", 7) == 0 || strncmp(path, "/?", 2) == 0)
        http_reply(fd, "200 OK", "text/html; charset=utf-8", PAGE_HTML, sizeof(PAGE_HTML) - 1);
    else http_reply(fd, "404 Not Found", "text/plain", "non trovato\n", 12);
}

/* ------------------------------------------------------------------ video */
/* Flusso H.264 Annex-B grezzo da screenrecord, riavviato da solo ogni 3 minuti
 * (limite di screenrecord). È la linea di base: niente bassa latenza, nessuna
 * ottimizzazione. Il nostro server Kotlin dovrà batterla di molto. */

static void tm_relay_begin(pid_t server_pid);
static void tm_relay_end(void);
static void tm_relay_tick(uint64_t bytes_in, uint64_t bytes_out, uint64_t frames_in, uint64_t frames_out, uint64_t dropped, unsigned queue_kb);

static void serve_video(int fd, const struct sockaddr_in *peer_sa) {
    char peer[64];
    addr_str(peer_sa, peer, sizeof peer);
    signal(SIGCHLD, SIG_DFL);
    int one = 1;
    setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one);
    /* Se l'iPhone smette di leggere, screenrecord si bloccherebbe e con lui lo schermo del telefono:
     * dopo 1,5 s senza progressi l'invio fallisce e la sessione si chiude. */
    struct timeval snd_to = {1, 500000};
    setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &snd_to, sizeof snd_to);
    logmsg("video: client %s connesso (bitrate %ld, dimensione %s)", peer, g_bitrate, g_size ? g_size : "nativa");

    int quick_failures = 0;
    uint64_t total = 0;
    for (;;) {
        int p[2];
        if (pipe(p) < 0) break;
        char br[32];
        snprintf(br, sizeof br, "%ld", g_bitrate);
        char *argv[12];
        int a = 0;
        argv[a++] = (char *)"screenrecord";
        argv[a++] = (char *)"--output-format=h264";
        argv[a++] = (char *)"--bit-rate";
        argv[a++] = br;
        argv[a++] = (char *)"--time-limit";
        argv[a++] = (char *)"180";
        if (g_size) { argv[a++] = (char *)"--size"; argv[a++] = (char *)g_size; }
        argv[a++] = (char *)"-";
        argv[a] = NULL;

        uint64_t t0 = now_ns(CLOCK_MONOTONIC);
        pid_t pid = fork();
        if (pid == 0) {
            prctl(PR_SET_PDEATHSIG, SIGKILL); /* mai uno screenrecord orfano */
            dup2(p[1], 1);
            close(p[0]); close(p[1]); close(fd);
            execv(g_screenrecord, argv);
            _exit(127);
        }
        close(p[1]);
        if (pid < 0) { close(p[0]); break; }
        tm_relay_begin(pid);

        int client_gone = 0;
        unsigned char buf[65536];
        struct pollfd pf[2] = {{.fd = p[0], .events = POLLIN}, {.fd = fd, .events = POLLIN}};
        for (;;) {
            if (poll(pf, 2, -1) < 0) { if (errno == EINTR) continue; break; }
            if (pf[1].revents) { /* il client non manda dati: leggibile = ha chiuso */
                char c;
                if (recv(fd, &c, 1, MSG_DONTWAIT) <= 0) { client_gone = 1; break; }
            }
            if (pf[0].revents) {
                ssize_t r = read(p[0], buf, sizeof buf);
                if (r <= 0) break; /* screenrecord è terminato */
                if (send_all(fd, buf, (size_t)r) < 0) { client_gone = 1; break; }
                total += (uint64_t)r;
                tm_relay_tick(total, total, 0, 0, 0, 0);
            }
        }
        close(p[0]);
        kill(pid, SIGINT);
        int st = 0;
        {
            int done = 0;
            for (int w = 0; w < 100 && !done; w++) { /* fino a 2 s, poi SIGKILL */
                pid_t wr = waitpid(pid, &st, WNOHANG);
                done = (wr == pid) || (wr < 0 && errno == ECHILD);
                if (!done) usleep(20000);
            }
            if (!done) { kill(pid, SIGKILL); waitpid(pid, &st, 0); }
        }
        if (client_gone) break;
        if (now_ns(CLOCK_MONOTONIC) - t0 < 2000000000ull) {
            logmsg("video: screenrecord terminato subito (stato %d)", WIFEXITED(st) ? WEXITSTATUS(st) : -1);
            if (++quick_failures >= 3) { logmsg("video: rinuncio dopo 3 errori"); break; }
            sleep(1);
        } else {
            quick_failures = 0;
            logmsg("video: screenrecord riavviato (limite di 3 minuti)");
        }
    }
    tm_relay_end();
    logmsg("video: client %s disconnesso, %.1f MB inviati", peer, (double)total / 1e6);
}


/* ------------------------------------------------------------ ponte scrcpy */
/*
 * Una sessione = una connessione TCP dell'iPhone, servita da un processo figlio che:
 *   1. prende il blocco di sessione (una sola alla volta: la nuova sostituisce la vecchia);
 *   2. scrive il marker (se il telefono si blocca, al riavvio resta come prova);
 *   3. avvia il server di scrcpy e ne inoltra il flusso SENZA MAI BLOCCARSI: legge sempre dal
 *      server; se l'iPhone non sta al passo scarta i frame vecchi fino al prossimo keyframe; se
 *      smette del tutto di leggere chiude la sessione.
 *
 * Perché: se chi riceve il flusso si ferma, il server smette di svuotare l'encoder, la coda
 * dell'encoder si riempie e SurfaceFlinger (che la alimenta) può restare in attesa: lo schermo di
 * tutto il telefono si congela. Qui lo si impedisce a monte, in modo indipendente da cosa
 * l'abbia causato (app sospesa, Wi-Fi in difficoltà, errore dell'app).
 */

#define SC_HANDSHAKE 69u             /* byte fittizio (1) + nome del dispositivo (64) + id del codec (4) */
#define SC_MAX_RECORD (8u << 20)     /* un pacchetto più grande di 8 MB indica un flusso non valido */

static volatile sig_atomic_t g_stop = 0;
static void on_stop(int sig) { (void)sig; g_stop = 1; }

static uint64_t now_ms(void) { return now_ns(CLOCK_MONOTONIC) / 1000000ull; }
/* Differenza tra due istanti che non va mai sotto zero: un istante letto un attimo dopo "now" non deve
 * trasformarsi in un tempo enorme (con i numeri senza segno farebbe scartare frame e chiudere sessioni sane). */
static uint64_t since(uint64_t now, uint64_t t) { return now > t ? now - t : 0; }
static uint32_t be32(const unsigned char *p) { return (uint32_t)p[0] << 24 | (uint32_t)p[1] << 16 | (uint32_t)p[2] << 8 | p[3]; }

static void set_nonblock(int fd) {
    int fl = fcntl(fd, F_GETFL, 0);
    if (fl >= 0) fcntl(fd, F_SETFL, fl | O_NONBLOCK);
}

static void state_path(char *out, size_t n, const char *name) { snprintf(out, n, "%s/%s", g_state_dir, name); }

static void fsync_state_dir(void) {
    int d = open(g_state_dir, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    if (d >= 0) { fsync(d); close(d); }
}

/* ---- una sola sessione alla volta (flock: il kernel lo rilascia anche se il processo muore) */

/* Si controlla il nome del processo (comm), non la riga di comando: leggere /proc/PID/cmdline di un processo
 * incastrato nel kernel può restare in attesa a oltranza e bloccare anche chi vuole fermarlo. */
static int is_probe_pid(pid_t pid) {
    char p[64], buf[64];
    snprintf(p, sizeof p, "/proc/%d/comm", (int)pid);
    int fd = open(p, O_RDONLY | O_CLOEXEC);
    if (fd < 0) return 0;
    ssize_t n = read(fd, buf, sizeof buf - 1);
    close(fd);
    if (n <= 0) return 0;
    buf[n] = 0;
    return strncmp(buf, "phantom-probe", 13) == 0;
}

static pid_t lock_holder_pid(int fd) {
    char b[32] = {0};
    ssize_t r = pread(fd, b, sizeof b - 1, 0);
    return r > 0 ? (pid_t)atoi(b) : 0;
}

/* Restituisce il descrittore del blocco (da tenere per tutta la sessione) oppure -1. */
static int session_lock(void) {
    char p[300];
    state_path(p, sizeof p, "scrcpy.lock");
    int fd = open(p, O_RDWR | O_CREAT | O_CLOEXEC, 0600);
    if (fd < 0) { logmsg("scrcpy: impossibile aprire %s: %s", p, strerror(errno)); return -1; }
    for (int i = 0; i < 100; i++) { /* fino a 5 s */
        if (flock(fd, LOCK_EX | LOCK_NB) == 0) {
            char b[32];
            int n = snprintf(b, sizeof b, "%d\n", (int)getpid());
            if (ftruncate(fd, 0) == 0) { ssize_t w = pwrite(fd, b, (size_t)n, 0); (void)w; }
            return fd;
        }
        if (i == 0 || i == 50) {
            pid_t other = lock_holder_pid(fd);
            if (other > 1 && is_probe_pid(other)) {
                logmsg("scrcpy: sessione precedente ancora attiva (pid %d): %s", (int)other, i == 0 ? "la fermo" : "la termino con la forza");
                kill(other, i == 0 ? SIGTERM : SIGKILL);
            }
        }
        usleep(50000);
    }
    close(fd);
    return -1;
}

/* Ferma la sessione scrcpy attiva (se c'è). Restituisce il pid fermato oppure 0. */
static pid_t stop_sessions(void) {
    char p[300];
    state_path(p, sizeof p, "scrcpy.lock");
    int fd = open(p, O_RDWR | O_CLOEXEC);
    if (fd < 0) return 0;
    pid_t pid = 0;
    if (flock(fd, LOCK_EX | LOCK_NB) != 0) { /* qualcuno la tiene: la sessione è attiva */
        pid = lock_holder_pid(fd);
        if (pid > 1 && is_probe_pid(pid)) kill(pid, SIGTERM); else pid = 0;
    }
    close(fd);
    return pid;
}

static pid_t active_session_pid(void) {
    char p[300];
    state_path(p, sizeof p, "scrcpy.lock");
    int fd = open(p, O_RDWR | O_CLOEXEC);
    if (fd < 0) return 0;
    pid_t pid = 0;
    if (flock(fd, LOCK_EX | LOCK_NB) != 0) pid = lock_holder_pid(fd);
    close(fd);
    return pid;
}

/* ---- marker: esiste solo mentre una sessione è in corso; se resta dopo un riavvio, la sessione è finita male */

static void marker_write(const char *text) {
    char p[300];
    state_path(p, sizeof p, "session.marker");
    int fd = open(p, O_WRONLY | O_CREAT | O_TRUNC | O_SYNC | O_CLOEXEC, 0600);
    if (fd < 0) return;
    ssize_t w = write(fd, text, strlen(text));
    (void)w;
    fsync(fd);
    close(fd);
    fsync_state_dir();
}

static void marker_remove(void) {
    char p[300];
    state_path(p, sizeof p, "session.marker");
    unlink(p);
}

#include "telemetry.h"

/* ---- coda verso l'iPhone */

struct rec {
    struct rec *next;
    size_t len, sent;
    uint64_t t_ms;   /* quando è arrivato dal server */
    int kind;        /* 0 = mai scartare (handshake, sessione, configurazione), 1 = keyframe, 2 = delta */
    unsigned char data[];
};

struct sc_session {
    int fd, up;
    struct rec *head, *tail;
    size_t qbytes;
    int dropping;
    uint64_t last_progress_ms;
    /* parser del flusso del server */
    unsigned char hs[SC_HANDSHAKE];
    size_t hs_have;
    int hs_done;
    unsigned char hdr[12];
    size_t hdr_have;
    struct rec *cur;
    size_t cur_have;
    /* contatori */
    uint64_t bytes_in, bytes_out, frames_in, frames_out, dropped, drop_events, keyframes;
    uint64_t max_age_ms, max_blocked_ms;
    int got_frame;
    uint64_t last_server_ms;
};

static struct rec *rec_new(size_t len, int kind) {
    struct rec *r = malloc(sizeof *r + len);
    if (!r) return NULL;
    r->next = NULL;
    r->len = len;
    r->sent = 0;
    r->t_ms = now_ms();
    r->kind = kind;
    return r;
}

static void q_push(struct sc_session *s, struct rec *r) {
    if (!s->head) s->last_progress_ms = now_ms(); /* la coda era vuota: da qui si misura lo stallo */
    r->next = NULL;
    if (s->tail) s->tail->next = r; else s->head = r;
    s->tail = r;
    s->qbytes += r->len;
}

/* Scarta i frame in coda che non sono ancora iniziati a partire. Restituisce quanti. */
static unsigned q_drop_unsent(struct sc_session *s) {
    unsigned n = 0;
    struct rec *prev = NULL, *r = s->head;
    while (r) {
        struct rec *next = r->next;
        if (r->kind != 0 && r->sent == 0) {
            if (prev) prev->next = next; else s->head = next;
            if (s->tail == r) s->tail = prev;
            s->qbytes -= r->len;
            free(r);
            n++;
        } else {
            prev = r;
        }
        r = next;
    }
    return n;
}

static void q_free_all(struct sc_session *s) {
    while (s->head) { struct rec *r = s->head; s->head = r->next; free(r); }
    s->tail = NULL;
    s->qbytes = 0;
    free(s->cur);
    s->cur = NULL;
}

static void sc_deliver(struct sc_session *s, struct rec *r) {
    if (r->kind == 0) { q_push(s, r); return; }
    s->frames_in++;
    s->got_frame = 1;
    if (r->kind == 1) {
        s->keyframes++;
        if (s->dropping) {
            s->dropping = 0;
            logmsg("scrcpy: keyframe ricevuto, l'invio riprende (frame scartati finora: %llu)", (unsigned long long)s->dropped);
        }
        q_push(s, r);
        return;
    }
    if (s->dropping) { s->dropped++; free(r); return; }
    q_push(s, r);
}

/* Interpreta i byte del server e mette in coda i pacchetti completi. -1 = flusso non valido. */
static int sc_feed(struct sc_session *s, const unsigned char *p, size_t n) {
    while (n > 0) {
        if (!s->hs_done) {
            size_t take = SC_HANDSHAKE - s->hs_have;
            if (take > n) take = n;
            memcpy(s->hs + s->hs_have, p, take);
            s->hs_have += take; p += take; n -= take;
            if (s->hs_have == SC_HANDSHAKE) {
                struct rec *r = rec_new(SC_HANDSHAKE, 0);
                if (!r) return -1;
                memcpy(r->data, s->hs, SC_HANDSHAKE);
                q_push(s, r);
                s->hs_done = 1;
            }
            continue;
        }
        if (!s->cur) {
            size_t take = 12 - s->hdr_have;
            if (take > n) take = n;
            memcpy(s->hdr + s->hdr_have, p, take);
            s->hdr_have += take; p += take; n -= take;
            if (s->hdr_have < 12) continue;
            s->hdr_have = 0;
            if (s->hdr[0] & 0x80) { /* pacchetto di sessione: 12 byte, senza dati */
                struct rec *r = rec_new(12, 0);
                if (!r) return -1;
                memcpy(r->data, s->hdr, 12);
                q_push(s, r);
                continue;
            }
            uint32_t size = be32(s->hdr + 8);
            if (size > SC_MAX_RECORD) { logmsg("scrcpy: pacchetto da %u byte: flusso non valido", size); return -1; }
            int kind = (s->hdr[0] & 0x40) ? 0 : (s->hdr[0] & 0x20) ? 1 : 2;
            struct rec *r = rec_new(12 + (size_t)size, kind);
            if (!r) return -1;
            memcpy(r->data, s->hdr, 12);
            if (size == 0) { sc_deliver(s, r); continue; }
            s->cur = r;
            s->cur_have = 12;
            continue;
        }
        size_t take = s->cur->len - s->cur_have;
        if (take > n) take = n;
        memcpy(s->cur->data + s->cur_have, p, take);
        s->cur_have += take; p += take; n -= take;
        if (s->cur_have == s->cur->len) {
            struct rec *r = s->cur;
            s->cur = NULL;
            r->t_ms = now_ms();
            sc_deliver(s, r);
        }
    }
    return 0;
}

/* Se il primo frame in coda è più vecchio del limite, l'iPhone non sta al passo: via i frame
 * vecchi (un'immagine in ritardo vale meno di nessuna) fino al prossimo keyframe. */
static void sc_check_congestion(struct sc_session *s, uint64_t now) {
    for (struct rec *r = s->head; r; r = r->next) {
        if (r->kind == 0) continue;
        uint64_t age = since(now, r->t_ms);
        if (age > s->max_age_ms) s->max_age_ms = age;
        if (!s->dropping && age > (uint64_t)g_max_age_ms) {
            unsigned n = q_drop_unsent(s);
            s->dropped += n;
            s->drop_events++;
            s->dropping = 1;
            logmsg("scrcpy: l'iPhone non sta al passo (frame in coda da %llu ms): scartati %u frame, attendo un keyframe", (unsigned long long)age, n);
        }
        break; /* la coda è in ordine di arrivo: basta il primo */
    }
}

/* Invia quanto il socket accetta, senza mai attendere. -1 = errore del client. */
static int sc_flush(struct sc_session *s, uint64_t now) {
    while (s->head) {
        struct rec *r = s->head;
        ssize_t w = send(s->fd, r->data + r->sent, r->len - r->sent, MSG_NOSIGNAL | MSG_DONTWAIT);
        if (w < 0) {
            if (errno == EINTR) continue;
            if (errno == EAGAIN || errno == EWOULDBLOCK) return 0;
            return -1;
        }
        r->sent += (size_t)w;
        s->qbytes -= (size_t)w;
        s->bytes_out += (uint64_t)w;
        s->last_progress_ms = now;
        if (r->sent < r->len) return 0; /* il socket è pieno */
        s->head = r->next;
        if (!s->head) s->tail = NULL;
        if (r->kind != 0) s->frames_out++;
        free(r);
    }
    return 0;
}

static int connect_scrcpy_socket(unsigned scid) {
    int s = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (s < 0) return -1;
    struct sockaddr_un sa;
    memset(&sa, 0, sizeof sa);
    sa.sun_family = AF_UNIX;
    socklen_t len;
    if (g_scrcpy_sock) {
        snprintf(sa.sun_path, sizeof sa.sun_path, "%s", g_scrcpy_sock);
        len = (socklen_t)(offsetof(struct sockaddr_un, sun_path) + strlen(sa.sun_path) + 1);
    } else {
        char name[64];
        int n = snprintf(name, sizeof name, "scrcpy_%08x", scid);
        sa.sun_path[0] = 0; /* socket astratto: il nome inizia con un byte nullo */
        memcpy(sa.sun_path + 1, name, (size_t)n);
        len = (socklen_t)(offsetof(struct sockaddr_un, sun_path) + 1 + (size_t)n);
    }
    if (connect(s, (struct sockaddr *)&sa, len) < 0) { close(s); return -1; }
    return s;
}

static pid_t spawn_scrcpy_server(unsigned scid, pid_t session_pid) {
    char scid_arg[32];
    snprintf(scid_arg, sizeof scid_arg, "scid=%08x", scid);
    char *argv[24 + MAX_SCRCPY_OPTS];
    int a = 0;
    argv[a++] = (char *)"app_process";
    argv[a++] = (char *)"/";
    argv[a++] = (char *)"com.genymobile.scrcpy.Server";
    argv[a++] = (char *)(g_scrcpy_ver ? g_scrcpy_ver : "");
    argv[a++] = scid_arg;
    /* Valori di base; le opzioni passate dopo (--scrcpy-opt) li sostituiscono. */
    argv[a++] = (char *)"log_level=debug";           /* l'esito dell'avvio finisce nel log sincronizzato */
    argv[a++] = (char *)"tunnel_forward=true";
    argv[a++] = (char *)"audio=false";
    argv[a++] = (char *)"control=false";
    argv[a++] = (char *)"cleanup=false";             /* niente secondo processo di pulizia: non cambiamo impostazioni */
    argv[a++] = (char *)"downsize_on_error=false";   /* prove ripetibili: niente ridimensionamenti di nascosto */
    for (int i = 0; i < g_scrcpy_nopts; i++) argv[a++] = (char *)g_scrcpy_opts[i];
    argv[a] = NULL;

    pid_t child = fork();
    if (child < 0) return -1;
    if (child == 0) {
        /* Se questo processo muore, il server muore con lui: mai un server orfano che tiene lo schermo agganciato. */
        prctl(PR_SET_PDEATHSIG, SIGKILL);
        if (getppid() != session_pid) _exit(1);
        signal(SIGTERM, SIG_DFL);
        signal(SIGINT, SIG_DFL);
        signal(SIGCHLD, SIG_DFL);
        int dn = open("/dev/null", O_RDONLY);
        if (dn >= 0) { dup2(dn, 0); if (dn > 2) close(dn); }
        dup2(g_log_fd, 1);
        dup2(g_log_fd, 2);
        setenv("CLASSPATH", g_scrcpy_jar, 1);
        execv(g_app_process, argv);
        _exit(127);
    }
    return child;
}

/* Aspetta la fine del processo per un tempo limitato. Restituisce 1 se è terminato. */
static int wait_exit(pid_t pid, int ms) {
    for (int i = 0; i <= ms / 20; i++) {
        int st;
        pid_t r = waitpid(pid, &st, WNOHANG);
        if (r == pid || (r < 0 && errno == ECHILD)) return 1;
        usleep(20000);
    }
    return 0;
}

static void serve_scrcpy(int fd, const struct sockaddr_in *peer_sa) {
    char peer[64];
    addr_str(peer_sa, peer, sizeof peer);
    signal(SIGCHLD, SIG_DFL);
    signal(SIGTERM, on_stop);
    signal(SIGINT, on_stop);

    /* Socket verso l'iPhone: senza Nagle, buffer di invio piccoli (la coda la gestiamo noi) e non bloccante. */
    int one = 1;
    setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one);
    int sb = 131072;
    setsockopt(fd, SOL_SOCKET, SO_SNDBUF, &sb, sizeof sb);
    int lw = 32768;
    setsockopt(fd, IPPROTO_TCP, TCP_NOTSENT_LOWAT, &lw, sizeof lw);
    int ka = 5, ki = 2, kc = 3;
    setsockopt(fd, SOL_SOCKET, SO_KEEPALIVE, &one, sizeof one); /* un iPhone sparito dal Wi-Fi si scopre in circa 11 s */
    setsockopt(fd, IPPROTO_TCP, TCP_KEEPIDLE, &ka, sizeof ka);
    setsockopt(fd, IPPROTO_TCP, TCP_KEEPINTVL, &ki, sizeof ki);
    setsockopt(fd, IPPROTO_TCP, TCP_KEEPCNT, &kc, sizeof kc);
    set_nonblock(fd);

    int lock = session_lock();
    if (lock < 0) { logmsg("scrcpy: non riesco a prendere il blocco di sessione, chiudo %s", peer); return; }

    char marker[1024];
    snprintf(marker, sizeof marker, "pid=%d\nstart=%ld\nprofilo=%s\nversione=%s\nclient=%s\nopzioni=", (int)getpid(), (long)time(NULL),
             g_scrcpy_profile ? g_scrcpy_profile : "", g_scrcpy_ver ? g_scrcpy_ver : "", peer);
    for (int i = 0; i < g_scrcpy_nopts; i++) {
        size_t l = strlen(marker);
        snprintf(marker + l, sizeof marker - l, "%s%s", i ? " " : "", g_scrcpy_opts[i]);
    }
    strncat(marker, "\n", sizeof marker - strlen(marker) - 1);
    marker_write(marker);

    unsigned scid = (unsigned)getpid() & 0x7fffffffu;
    logmsg("scrcpy: client %s connesso, profilo «%s»: avvio il server %s", peer, g_scrcpy_profile ? g_scrcpy_profile : "", g_scrcpy_ver ? g_scrcpy_ver : "?");
    pid_t child = spawn_scrcpy_server(scid, getpid());
    if (child < 0) { logmsg("scrcpy: fork del server fallita"); marker_remove(); return; }
    tm_relay_begin(child);

    const char *reason = NULL;
    int reaped = 0;
    int up = -1;
    uint64_t t0 = now_ms();

    /* Il server impiega circa un secondo ad aprire il suo socket: si riprova fino a 6 s. */
    int early_close = 0;
    while (since(now_ms(), t0) < 6000 && !g_stop) {
        tm_relay_tick(0, 0, 0, 0, 0, 0); /* segno di vita per la scatola nera durante l'avvio del server */
        up = connect_scrcpy_socket(scid);
        if (up >= 0) break;
        int st;
        if (waitpid(child, &st, WNOHANG) == child) {
            logmsg("scrcpy: il server è terminato subito (stato %d); vedi le righe «[server]» qui sopra", WIFEXITED(st) ? WEXITSTATUS(st) : -1);
            reason = "il server è terminato subito";
            reaped = 1;
            break;
        }
        /* Nell'attesa si controlla anche che l'iPhone sia ancora lì: una connessione abbandonata subito non deve tenere acceso il server. */
        struct pollfd pc = {.fd = fd, .events = POLLIN, .revents = 0};
        if (poll(&pc, 1, 50) > 0) {
            char tmp[16];
            ssize_t r = recv(fd, tmp, sizeof tmp, MSG_DONTWAIT);
            if ((pc.revents & (POLLERR | POLLHUP)) || r == 0) {
                reason = "l'iPhone ha chiuso prima dell'avvio del server";
                early_close = 1;
                break;
            }
        }
    }

    struct sc_session s;
    memset(&s, 0, sizeof s);
    s.fd = fd;
    s.up = up;
    uint64_t t_start = now_ms();

    if (up >= 0) {
        set_nonblock(up);
        logmsg("scrcpy: server pronto dopo %llu ms", (unsigned long long)since(t_start, t0));
        s.last_server_ms = t_start;
        s.last_progress_ms = t_start;
        uint64_t last_stats = t_start, prev_out = 0, prev_in = 0, prev_frames = 0;
        unsigned char buf[65536];
        struct pollfd pf[2];

        for (;;) {
            if (g_stop) { reason = "arresto richiesto"; break; }
            pf[0].fd = up; pf[0].events = POLLIN; pf[0].revents = 0;
            pf[1].fd = fd; pf[1].events = POLLIN | (s.head ? POLLOUT : 0); pf[1].revents = 0;
            int pr = poll(pf, 2, 100);
            if (pr < 0 && errno != EINTR) { reason = "errore di poll"; break; }
            uint64_t now = now_ms();

            if (pr > 0) {
                if (pf[1].revents & (POLLERR | POLLHUP)) { reason = "l'iPhone ha chiuso la connessione"; break; }
                if (pf[1].revents & POLLIN) {
                    char tmp[256];
                    ssize_t r = recv(fd, tmp, sizeof tmp, MSG_DONTWAIT); /* l'iPhone non invia nulla: 0 = ha chiuso */
                    if (r == 0 || (r < 0 && errno != EAGAIN && errno != EWOULDBLOCK && errno != EINTR)) { reason = "l'iPhone ha chiuso la connessione"; break; }
                }
                if (pf[0].revents & (POLLIN | POLLHUP | POLLERR)) {
                    ssize_t r = read(up, buf, sizeof buf);
                    if (r == 0) { reason = "il server ha chiuso il flusso"; break; }
                    if (r < 0) {
                        if (errno != EAGAIN && errno != EWOULDBLOCK && errno != EINTR) { reason = "errore di lettura dal server"; break; }
                    } else {
                        s.bytes_in += (uint64_t)r;
                        s.last_server_ms = now;
                        if (sc_feed(&s, buf, (size_t)r) < 0) { reason = "flusso del server non valido"; break; }
                    }
                }
            }

            now = now_ms(); /* dopo la lettura: gli istanti dei frame appena arrivati sono successivi al "now" di prima */
            sc_check_congestion(&s, now);
            if (s.head && sc_flush(&s, now) < 0) { reason = "errore di invio all'iPhone"; break; }
            tm_relay_tick(s.bytes_in, s.bytes_out, s.frames_in, s.frames_out, s.dropped, (unsigned)(s.qbytes / 1024));

            /* Cane da guardia 1: l'iPhone ha dati in attesa ma non li accetta più. È il caso che può
             * congelare lo schermo del telefono: si chiude subito, così il server viene fermato. */
            if (s.head) {
                uint64_t blocked = since(now, s.last_progress_ms);
                if (blocked > s.max_blocked_ms) s.max_blocked_ms = blocked;
                if (blocked > (uint64_t)g_stall_client_ms) { reason = "l'iPhone ha smesso di leggere"; break; }
            }
            /* Cane da guardia 2: nessun frame entro un tempo ragionevole dall'avvio (cattura non partita). */
            if (!s.got_frame && since(now, t_start) > (uint64_t)g_first_frame_ms) { reason = "nessun frame entro il tempo limite"; break; }
            /* Un server silenzioso può essere uno schermo fermo (non produce frame): solo per i test si può forzare la chiusura. */
            if (g_stall_server_ms > 0 && s.got_frame && since(now, s.last_server_ms) > (uint64_t)g_stall_server_ms) { reason = "il server non produce più dati"; break; }

            int st;
            if (waitpid(child, &st, WNOHANG) == child) {
                reaped = 1;
                logmsg("scrcpy: il server è terminato (stato %d)", WIFEXITED(st) ? WEXITSTATUS(st) : -1);
                reason = "il server è terminato";
                break;
            }

            if (since(now, last_stats) >= 5000) {
                double dt = (double)since(now, last_stats) / 1000.0;
                logmsg("scrcpy: %llu s · in %.1f Mbps · out %.1f Mbps · %.0f fps · coda %zu KB · età max %llu ms · client fermo max %llu ms · scartati %llu · silenzio server %llu ms",
                       (unsigned long long)(since(now, t_start) / 1000), (double)(s.bytes_in - prev_in) * 8 / 1e6 / dt,
                       (double)(s.bytes_out - prev_out) * 8 / 1e6 / dt, (double)(s.frames_out - prev_frames) / dt,
                       s.qbytes / 1024, (unsigned long long)s.max_age_ms, (unsigned long long)s.max_blocked_ms,
                       (unsigned long long)s.dropped, (unsigned long long)since(now, s.last_server_ms));
                last_stats = now; prev_in = s.bytes_in; prev_out = s.bytes_out; prev_frames = s.frames_out;
                s.max_age_ms = 0; s.max_blocked_ms = 0;
            }
        }
        close(up);
    } else if (!reaped && !g_stop && !early_close) {
        reason = "il server non ha aperto il socket entro 6 s";
        logmsg("scrcpy: %s", reason);
    }

    /* Chiusura: prima l'iPhone e il flusso (sblocca subito chi scrive), poi il server, con garbo e poi con la forza. */
    shutdown(fd, SHUT_RDWR);
    q_free_all(&s);
    if (!reaped) {
        kill(child, SIGTERM);
        if (!wait_exit(child, 1500)) {
            logmsg("scrcpy: il server non si ferma con SIGTERM: lo uccido");
            kill(child, SIGKILL);
            if (!wait_exit(child, 2000)) logmsg("scrcpy: ATTENZIONE: il server (pid %d) non risponde nemmeno a SIGKILL", (int)child);
            else reaped = 1;
        } else {
            reaped = 1;
        }
    }
    logmsg("scrcpy: sessione con %s conclusa (%s) · frame ricevuti %llu, inviati %llu, scartati %llu in %llu eventi · %.1f MB inviati",
           peer, reason ? reason : "chiusura", (unsigned long long)s.frames_in, (unsigned long long)s.frames_out, (unsigned long long)s.dropped,
           (unsigned long long)s.drop_events, (double)s.bytes_out / 1e6);
    if (reaped) marker_remove(); /* senza marker = chiusura pulita; se il server non muore, il marker resta come segnale */
    tm_relay_end();
    close(lock);
}

/* -------------------------------------------------------------------- UDP */

static void handle_udp(int fd) {
    static uint64_t last_mono = 0, last_susp = 0;
    static struct sockaddr_in last_peer;
    unsigned char pkt[1500];
    struct sockaddr_in peer;
    socklen_t pl = sizeof peer;
    ssize_t n = recvfrom(fd, pkt, sizeof pkt, 0, (struct sockaddr *)&peer, &pl);
    if (n < UDP_HDR || memcmp(pkt, UDP_MAGIC, 4) != 0 || !allowed(&peer)) return;

    uint64_t mono = now_ns(CLOCK_MONOTONIC), susp = suspended_ms();
    put_u64(pkt + 16, mono);
    put_u64(pkt + 24, susp);
    sendto(fd, pkt, (size_t)n, 0, (struct sockaddr *)&peer, pl);

    char who[64];
    addr_str(&peer, who, sizeof who);
    if (last_mono == 0 || peer.sin_addr.s_addr != last_peer.sin_addr.s_addr) {
        logmsg("udp: nuovo client %s (seq %u)", who, get_u32(pkt + 4));
    } else if (mono - last_mono > WAKE_GAP_NS) {
        logmsg("udp: pacchetto dopo %.0f s di silenzio da %s (seq %u); il telefono ha dormito %llu ms nel frattempo",
               (double)(mono - last_mono) / 1e9, who, get_u32(pkt + 4), (unsigned long long)(susp - last_susp));
    }
    last_mono = mono;
    last_susp = susp;
    last_peer = peer;
}

/* ------------------------------------------------------------------- main */

static volatile sig_atomic_t g_term = 0;
static void on_term(int sig) { (void)sig; g_term = 1; }

static void usage(void) {
    fprintf(stderr,
            "phantom-probe %s — sonda della fase 0 di Phantom\n\n"
            "  --udp-port N       eco UDP (default 47800)\n"
            "  --http-port N      pagina web e WebSocket (default 47801)\n"
            "  --video            accende il flusso H.264 di base su --video-port\n"
            "  --video-port N     default 47802\n"
            "  --bitrate BPS      bitrate del video (default 12000000)\n"
            "  --size WxH         risoluzione del video (default nativa)\n"
            "  --scrcpy-port N    ponte verso il server di scrcpy su questa porta TCP (0 = spento)\n"
            "  --scrcpy-jar FILE  scrcpy-server (il file jar)\n"
            "  --scrcpy-version V versione di scrcpy-server, richiesta come primo argomento\n"
            "  --scrcpy-opt K=V   opzione per il server (ripetibile, es. video_codec=h265)\n"
            "  --scrcpy-profile N nome del profilo (solo per log e marker)\n"
            "  --state-dir DIR    dove tenere marker e blocco di sessione (default /data/adb/phantom-f0)\n"
            "  --scrcpy-stall-client-ms N  chiude se l'iPhone non accetta dati per N ms (default 1500)\n"
            "  --scrcpy-stall-server-ms N  chiude se il server tace per N ms (default 0 = mai)\n"
            "  --scrcpy-max-age-ms N       scarta i frame in coda da più di N ms (default 250)\n"
            "  --scrcpy-first-frame-ms N   attesa massima del primo frame (default 8000)\n"
            "  --telemetry-port N scatola nera: stato del telefono in tempo reale su questa porta TCP (0 = spenta)\n"
            "  --tm-hz N          campioni al secondo con client o sessione attivi (default 4)\n"
            "  --tm-idle-ms N     intervallo a riposo (default 5000)\n"
            "  --no-sysrq         non chiedere al kernel gli stack dei task bloccati\n"
            "  --tm-autostop      se la sessione video è in corso e lo stallo dura 3 s, ferma la cattura\n"
            "  --no-blackbox-disk non scrivere blackbox.txt nella cartella di stato\n"
            "  --proc-root P / --sys-root P / --kmsg P / --sysrq-file P / --sysrq-mask P   percorsi alternativi (per i test)\n"
            "  --allow-any        accetta anche indirizzi pubblici (sconsigliato)\n"
            "  --log FILE         file di log (default stderr)\n"
            "  --pidfile FILE     scrive il PID\n"
            "  --daemon           va in background\n"
            "  --screenrecord P   percorso di screenrecord (per i test)\n"
            "  --getprop P        percorso di getprop (per i test)\n"
            "  --app-process P    percorso di app_process (per i test)\n"
            "  --scrcpy-socket P  socket UNIX su file al posto di quello astratto (per i test)\n",
            PROBE_VERSION);
}

int main(int argc, char **argv) {
    int daemonize = 0;
    for (int i = 1; i < argc; i++) {
        const char *a = argv[i];
        const char *v = i + 1 < argc ? argv[i + 1] : NULL;
#define NEEDV() do { if (!v) { usage(); return 2; } i++; } while (0)
        if (!strcmp(a, "--udp-port")) { NEEDV(); g_udp_port = atoi(v); }
        else if (!strcmp(a, "--http-port")) { NEEDV(); g_http_port = atoi(v); }
        else if (!strcmp(a, "--video-port")) { NEEDV(); g_video_port = atoi(v); }
        else if (!strcmp(a, "--video")) g_video = 1;
        else if (!strcmp(a, "--bitrate")) { NEEDV(); g_bitrate = atol(v); }
        else if (!strcmp(a, "--size")) { NEEDV(); g_size = v; }
        else if (!strcmp(a, "--allow-any")) g_allow_any = 1;
        else if (!strcmp(a, "--log")) { NEEDV(); g_log_path = v; }
        else if (!strcmp(a, "--pidfile")) { NEEDV(); g_pid_path = v; }
        else if (!strcmp(a, "--daemon")) daemonize = 1;
        else if (!strcmp(a, "--screenrecord")) { NEEDV(); g_screenrecord = v; }
        else if (!strcmp(a, "--getprop")) { NEEDV(); g_getprop = v; }
        else if (!strcmp(a, "--scrcpy-port")) { NEEDV(); g_scrcpy_port = atoi(v); }
        else if (!strcmp(a, "--scrcpy-jar")) { NEEDV(); g_scrcpy_jar = v; }
        else if (!strcmp(a, "--scrcpy-version")) { NEEDV(); g_scrcpy_ver = v; }
        else if (!strcmp(a, "--scrcpy-opt")) { NEEDV(); if (g_scrcpy_nopts < MAX_SCRCPY_OPTS) g_scrcpy_opts[g_scrcpy_nopts++] = v; }
        else if (!strcmp(a, "--app-process")) { NEEDV(); g_app_process = v; }
        else if (!strcmp(a, "--scrcpy-socket")) { NEEDV(); g_scrcpy_sock = v; }
        else if (!strcmp(a, "--scrcpy-profile")) { NEEDV(); g_scrcpy_profile = v; }
        else if (!strcmp(a, "--state-dir")) { NEEDV(); g_state_dir = v; }
        else if (!strcmp(a, "--scrcpy-stall-client-ms")) { NEEDV(); g_stall_client_ms = atoi(v); }
        else if (!strcmp(a, "--scrcpy-stall-server-ms")) { NEEDV(); g_stall_server_ms = atoi(v); }
        else if (!strcmp(a, "--scrcpy-max-age-ms")) { NEEDV(); g_max_age_ms = atoi(v); }
        else if (!strcmp(a, "--scrcpy-first-frame-ms")) { NEEDV(); g_first_frame_ms = atoi(v); }
        else if (!strcmp(a, "--telemetry-port")) { NEEDV(); g_tm_port = atoi(v); }
        else if (!strcmp(a, "--tm-hz")) { NEEDV(); g_tm_hz = atoi(v) > 0 ? atoi(v) : 4; }
        else if (!strcmp(a, "--tm-idle-ms")) { NEEDV(); g_tm_idle_ms = atoi(v) > 0 ? atoi(v) : 5000; }
        else if (!strcmp(a, "--no-sysrq")) g_tm_sysrq = 0;
        else if (!strcmp(a, "--tm-autostop")) g_tm_autostop = 1;
        else if (!strcmp(a, "--no-blackbox-disk")) g_tm_disk = 0;
        else if (!strcmp(a, "--proc-root")) { NEEDV(); g_proc = v; }
        else if (!strcmp(a, "--sys-root")) { NEEDV(); g_sys = v; }
        else if (!strcmp(a, "--kmsg")) { NEEDV(); g_kmsg_path = v; }
        else if (!strcmp(a, "--sysrq-file")) { NEEDV(); g_sysrq_path = v; }
        else if (!strcmp(a, "--sysrq-mask")) { NEEDV(); g_sysrq_mask_path = v; }
        else if (!strcmp(a, "--version")) { printf("%s\n", PROBE_VERSION); return 0; }
        else { usage(); return 2; }
    }

    if (g_log_path) {
        int fd = open(g_log_path, O_WRONLY | O_CREAT | O_APPEND | O_SYNC | O_CLOEXEC, 0644); /* O_SYNC: se il telefono si blocca, le righe già scritte restano */
        if (fd < 0) { perror("log"); return 1; }
        g_log_fd = fd;
    }

    int udp = socket(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, 0);
    struct sockaddr_in sa = {.sin_family = AF_INET, .sin_port = htons((uint16_t)g_udp_port), .sin_addr.s_addr = htonl(INADDR_ANY)};
    if (udp < 0 || bind(udp, (struct sockaddr *)&sa, sizeof sa) < 0) { logmsg("errore: porta UDP %d: %s", g_udp_port, strerror(errno)); return 1; }
    int tos = 0xB8; /* DSCP EF: priorità alta sul Wi-Fi (WMM voce), se il router la rispetta */
    setsockopt(udp, IPPROTO_IP, IP_TOS, &tos, sizeof tos);
    int http = listen_tcp(g_http_port);
    if (http < 0) { logmsg("errore: porta TCP %d: %s", g_http_port, strerror(errno)); return 1; }
    int video = -1;
    if (g_video && (video = listen_tcp(g_video_port)) < 0) { logmsg("errore: porta video %d: %s", g_video_port, strerror(errno)); return 1; }

    int scrcpy = -1;
    if (g_scrcpy_port > 0) {
        if (!g_scrcpy_jar || !g_scrcpy_ver) { logmsg("errore: --scrcpy-port richiede --scrcpy-jar e --scrcpy-version"); return 1; }
        if ((scrcpy = listen_tcp(g_scrcpy_port)) < 0) { logmsg("errore: porta scrcpy %d: %s", g_scrcpy_port, strerror(errno)); return 1; }
    }

    int telem = -1;
    if (g_tm_port > 0 && (telem = listen_tcp(g_tm_port)) < 0) { logmsg("errore: porta telemetria %d: %s", g_tm_port, strerror(errno)); return 1; }

    if (daemonize) {
        pid_t p = fork();
        if (p < 0) return 1;
        if (p > 0) return 0;
        setsid();
        int dn = open("/dev/null", O_RDWR);
        if (dn >= 0) { dup2(dn, 0); dup2(dn, 1); if (g_log_path) dup2(dn, 2); }
    }
    if (g_pid_path) {
        FILE *f = fopen(g_pid_path, "w");
        if (f) { fprintf(f, "%d\n", (int)getpid()); fclose(f); }
    }

    signal(SIGPIPE, SIG_IGN);
    signal(SIGCHLD, SIG_IGN); /* i figli che servono le connessioni vengono raccolti dal kernel */
    signal(SIGTERM, on_term);
    signal(SIGINT, on_term);
    g_start_mono = now_ns(CLOCK_MONOTONIC);
    logmsg("avvio: phantom-probe %s, udp %d, http %d, video %s, scrcpy %s, sospensione finora %llu ms",
           PROBE_VERSION, g_udp_port, g_http_port, g_video ? "acceso" : "spento",
           g_scrcpy_port > 0 ? "acceso" : "spento", (unsigned long long)suspended_ms());

    struct pollfd pf[5] = {{.fd = udp, .events = POLLIN}, {.fd = http, .events = POLLIN}, {.fd = -1, .events = POLLIN}, {.fd = -1, .events = POLLIN}, {.fd = -1, .events = POLLIN}};
    int kind[5] = {0, 1, 0, 0, 0}; /* 1 = http, 2 = video di base, 3 = scrcpy, 4 = telemetria */
    int nfds = 2;
    if (video >= 0) { pf[nfds].fd = video; kind[nfds++] = 2; }
    if (scrcpy >= 0) { pf[nfds].fd = scrcpy; kind[nfds++] = 3; }
    if (telem >= 0) { pf[nfds].fd = telem; kind[nfds++] = 4; }

    /* Scatola nera: un processo a parte che campiona lo stato del telefono (vedi telemetry.h). */
    pid_t blackbox = -1;
    int listen_fds[5] = {udp, http, video, scrcpy, telem};
    int respawns = 0;
    uint64_t respawn_window = now_ms();
    if (telem >= 0) {
        if (tm_shared_init() < 0) { logmsg("telemetria: memoria condivisa non disponibile, la spengo"); close(telem); telem = -1; nfds--; }
        else {
            blackbox = tm_spawn(listen_fds, 5);
            logmsg("telemetria: scatola nera avviata (pid %d), porta %d", (int)blackbox, g_tm_port);
        }
    }
    for (;;) {
        if (blackbox > 0 && kill(blackbox, 0) != 0 && errno == ESRCH) { /* il processo della scatola nera è morto: lo si rilancia (al massimo 5 volte al minuto) */
            if (since(now_ms(), respawn_window) > 60000) { respawn_window = now_ms(); respawns = 0; }
            if (respawns++ < 5) { blackbox = tm_spawn(listen_fds, 5); logmsg("telemetria: scatola nera rilanciata (pid %d)", (int)blackbox); }
            else blackbox = -1;
        }
        if (poll(pf, (nfds_t)nfds, 2000) < 0) {
            if (errno == EINTR) {
                if (g_term) break;
                continue;
            }
            logmsg("errore: poll: %s", strerror(errno));
            return 1;
        }
        if (pf[0].revents & POLLIN) handle_udp(udp);
        for (int i = 1; i < nfds; i++) {
            if (!(pf[i].revents & POLLIN)) continue;
            struct sockaddr_in peer;
            socklen_t pl = sizeof peer;
            int c = accept4(pf[i].fd, (struct sockaddr *)&peer, &pl, SOCK_CLOEXEC);
            if (c < 0) continue;
            if (!allowed(&peer)) {
                char who[64];
                logmsg("rifiutato %s: indirizzo pubblico", addr_str(&peer, who, sizeof who));
                close(c);
                continue;
            }
            pid_t child = fork();
            if (child == 0) {
                signal(SIGTERM, SIG_DFL);
                signal(SIGINT, SIG_DFL);
                close(udp); close(http); if (video >= 0) close(video); if (scrcpy >= 0) close(scrcpy); if (telem >= 0) close(telem);
                if (kind[i] == 1) serve_http(c, &peer);
                else if (kind[i] == 2) serve_video(c, &peer);
                else if (kind[i] == 4) serve_telemetry(c, &peer);
                else serve_scrcpy(c, &peer);
                close(c);
                _exit(0);
            }
            close(c);
        }
    }

    /* Arresto richiesto (phantomctl stop): ferma la sessione scrcpy in modo ordinato e, se siamo il capogruppo,
     * anche gli screenrecord del video di base. */
    logmsg("arresto richiesto: fermo le sessioni");
    signal(SIGTERM, SIG_IGN);
    stop_sessions();
    if (getpgrp() == getpid()) kill(0, SIGTERM);
    usleep(300000);
    return 0;
}
