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
#include <sys/types.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define PROBE_VERSION "0.3.0"
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
    char body[1024];
    int n = snprintf(body, sizeof body,
                     "{\"probe\":\"%s\",\"uptime_s\":%.1f,\"suspended_ms\":%llu,\"video\":%s,"
                     "\"ports\":{\"udp\":%d,\"http\":%d,\"video\":%d},"
                     "\"device\":{\"model\":\"%s\",\"codename\":\"%s\",\"android\":\"%s\",\"sdk\":\"%s\",\"lineage\":\"%s\",\"soc\":\"%s\"}}\n",
                     PROBE_VERSION, (double)(now_ns(CLOCK_MONOTONIC) - g_start_mono) / 1e9,
                     (unsigned long long)suspended_ms(), g_video ? "true" : "false",
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
    else if (strncmp(path, "/ ", 2) == 0 || strncmp(path, "/clock ", 7) == 0 || strncmp(path, "/?", 2) == 0)
        http_reply(fd, "200 OK", "text/html; charset=utf-8", PAGE_HTML, sizeof(PAGE_HTML) - 1);
    else http_reply(fd, "404 Not Found", "text/plain", "non trovato\n", 12);
}

/* ------------------------------------------------------------------ video */
/* Flusso H.264 Annex-B grezzo da screenrecord, riavviato da solo ogni 3 minuti
 * (limite di screenrecord). È la linea di base: niente bassa latenza, nessuna
 * ottimizzazione. Il nostro server Kotlin dovrà batterla di molto. */

static void serve_video(int fd, const struct sockaddr_in *peer_sa) {
    char peer[64];
    addr_str(peer_sa, peer, sizeof peer);
    signal(SIGCHLD, SIG_DFL);
    int one = 1;
    setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one);
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
            dup2(p[1], 1);
            close(p[0]); close(p[1]); close(fd);
            execv(g_screenrecord, argv);
            _exit(127);
        }
        close(p[1]);
        if (pid < 0) { close(p[0]); break; }

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
            }
        }
        close(p[0]);
        kill(pid, SIGINT);
        int st = 0;
        waitpid(pid, &st, 0);
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
    logmsg("video: client %s disconnesso, %.1f MB inviati", peer, (double)total / 1e6);
}


/* ------------------------------------------------------------ ponte scrcpy */

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

static void serve_scrcpy(int fd, const struct sockaddr_in *peer_sa) {
    char peer[64];
    addr_str(peer_sa, peer, sizeof peer);
    signal(SIGCHLD, SIG_DFL);
    int one = 1;
    setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one);

    unsigned scid = (unsigned)getpid() & 0x7fffffffu; /* unico per connessione */
    char scid_arg[32], ver_arg[64];
    snprintf(scid_arg, sizeof scid_arg, "scid=%08x", scid);
    snprintf(ver_arg, sizeof ver_arg, "%s", g_scrcpy_ver ? g_scrcpy_ver : "");

    char *argv[16 + MAX_SCRCPY_OPTS];
    int a = 0;
    argv[a++] = (char *)"app_process";
    argv[a++] = (char *)"/";
    argv[a++] = (char *)"com.genymobile.scrcpy.Server";
    argv[a++] = ver_arg;
    argv[a++] = scid_arg;
    argv[a++] = (char *)"log_level=info";
    argv[a++] = (char *)"tunnel_forward=true";
    argv[a++] = (char *)"audio=false";
    argv[a++] = (char *)"control=false";
    for (int i = 0; i < g_scrcpy_nopts; i++) argv[a++] = (char *)g_scrcpy_opts[i];
    argv[a] = NULL;

    logmsg("scrcpy: client %s connesso, avvio il server %s (%s)", peer, ver_arg, scid_arg);
    pid_t child = fork();
    if (child < 0) { logmsg("scrcpy: fork fallita"); return; }
    if (child == 0) {
        setenv("CLASSPATH", g_scrcpy_jar, 1);
        dup2(g_log_fd, 1);
        dup2(g_log_fd, 2);
        close(fd);
        execv(g_app_process, argv);
        _exit(127);
    }

    /* Il server impiega circa un secondo ad aprire il suo socket: si riprova fino a 6 s. */
    int up = -1;
    uint64_t t0 = now_ns(CLOCK_MONOTONIC);
    while (now_ns(CLOCK_MONOTONIC) - t0 < 6000000000ull) {
        up = connect_scrcpy_socket(scid);
        if (up >= 0) break;
        int st;
        if (waitpid(child, &st, WNOHANG) == child) {
            logmsg("scrcpy: il server è terminato subito (stato %d); vedi il log sopra", WIFEXITED(st) ? WEXITSTATUS(st) : -1);
            return;
        }
        usleep(50000);
    }
    if (up < 0) {
        logmsg("scrcpy: il server non ha aperto il socket entro 6 s");
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        return;
    }
    logmsg("scrcpy: server pronto dopo %.0f ms", (double)(now_ns(CLOCK_MONOTONIC) - t0) / 1e6);

    unsigned char buf[65536];
    uint64_t total = 0;
    struct pollfd pf[2] = {{.fd = up, .events = POLLIN}, {.fd = fd, .events = POLLIN}};
    for (;;) {
        if (poll(pf, 2, -1) < 0) { if (errno == EINTR) continue; break; }
        if (pf[0].revents & (POLLIN | POLLHUP | POLLERR)) {
            ssize_t r = read(up, buf, sizeof buf);
            if (r <= 0) break; /* il server ha chiuso */
            if (send_all(fd, buf, (size_t)r) < 0) break;
            total += (uint64_t)r;
        }
        if (pf[1].revents & (POLLIN | POLLHUP | POLLERR)) {
            char c[256];
            ssize_t r = recv(fd, c, sizeof c, MSG_DONTWAIT);
            if (r == 0 || (r < 0 && errno != EAGAIN && errno != EWOULDBLOCK && errno != EINTR)) break; /* l'iPhone ha chiuso */
            /* i dati dal client non servono (controllo spento): si scartano */
        }
    }
    close(up);
    kill(child, SIGTERM);
    usleep(200000);
    kill(child, SIGKILL);
    waitpid(child, NULL, 0);
    logmsg("scrcpy: client %s disconnesso, %.1f MB inoltrati", peer, (double)total / 1e6);
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
        else if (!strcmp(a, "--version")) { printf("%s\n", PROBE_VERSION); return 0; }
        else { usage(); return 2; }
    }

    if (g_log_path) {
        int fd = open(g_log_path, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0644);
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
    g_start_mono = now_ns(CLOCK_MONOTONIC);
    logmsg("avvio: phantom-probe %s, udp %d, http %d, video %s, scrcpy %s, sospensione finora %llu ms",
           PROBE_VERSION, g_udp_port, g_http_port, g_video ? "acceso" : "spento",
           g_scrcpy_port > 0 ? "acceso" : "spento", (unsigned long long)suspended_ms());

    struct pollfd pf[4] = {{.fd = udp, .events = POLLIN}, {.fd = http, .events = POLLIN}, {.fd = -1, .events = POLLIN}, {.fd = -1, .events = POLLIN}};
    int kind[4] = {0, 1, 0, 0}; /* 1 = http, 2 = video di base, 3 = scrcpy */
    int nfds = 2;
    if (video >= 0) { pf[nfds].fd = video; kind[nfds++] = 2; }
    if (scrcpy >= 0) { pf[nfds].fd = scrcpy; kind[nfds++] = 3; }
    for (;;) {
        if (poll(pf, (nfds_t)nfds, -1) < 0) { if (errno == EINTR) continue; logmsg("errore: poll: %s", strerror(errno)); return 1; }
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
                close(udp); close(http); if (video >= 0) close(video); if (scrcpy >= 0) close(scrcpy);
                if (kind[i] == 1) serve_http(c, &peer);
                else if (kind[i] == 2) serve_video(c, &peer);
                else serve_scrcpy(c, &peer);
                close(c);
                _exit(0);
            }
            close(c);
        }
    }
}
