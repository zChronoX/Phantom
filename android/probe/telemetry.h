/*
 * telemetry.h — la «scatola nera» di Phantom. Incluso da phantom-probe.c (un'unica unità di compilazione).
 *
 * Perché esiste: il POCO si è bloccato in tre prove di cattura dello schermo e dopo il riavvio non resta quasi
 * nulla da leggere. Qui un processo dedicato (phantom-bbox) campiona lo stato del telefono e lo trasmette
 * in tempo reale a chi si collega sulla porta TCP di telemetria (l'app iPhone lo registra: sopravvive al
 * riavvio del POCO). Se il telefono si blocca, le ultime righe dicono cosa stava succedendo e quale parte
 * del sistema ha smesso di rispondere.
 *
 * Come è fatta (e perché):
 *   - Tre thread di campionamento separati: «core» (CPU, memoria, pressione PSI, messaggi del kernel), «hw»
 *     (GPU, temperature, frequenze) e «task» (stato dei processi chiave e dei thread bloccati). Se uno resta
 *     incastrato in una lettura (per esempio sysfs della GPU durante un blocco della GPU) gli altri continuano,
 *     e la riga dice quale lettura è ferma e da quanto: è già un'informazione diagnostica.
 *   - Un thread scrive su disco con O_SYNC (solo durante sessioni e stalli): se il POCO muore, le righe già
 *     scritte restano. Se il disco si blocca, si blocca solo lui: la trasmissione di rete prosegue.
 *   - Le righe passano da un anello in memoria condivisa: ogni connessione di telemetria è un processo
 *     figlio che legge dall'anello, quindi un client lento non rallenta mai il campionamento.
 *   - Rilevatore di stallo: se ci sono task bloccati nel kernel (stato D) a lungo, pressione di I/O o memoria
 *     alta, o un campionatore fermo, la sonda emette una riga «S», elenca i task bloccati con il loro stack del
 *     kernel e chiede al kernel gli stack di tutti i task bloccati (sysrq w/l), che finiscono nel log del kernel
 *     e quindi nel flusso.
 *
 * Formato (una riga di testo per evento, prima lettera = tipo):
 *   H  intestazione alla connessione      T  campione completo       S  evento di stallo
 *   D  task bloccato con stack            K  messaggio del kernel    B  battito del processo che serve il client
 */

#include <dirent.h>
#include <pthread.h>
#include <strings.h>
#include <sys/mman.h>

#define TM_N 512            /* righe nell'anello condiviso */
#define TM_TXT 1008         /* byte massimi per riga */
#define TM_BACKFILL 240     /* righe di storia inviate a chi si collega */

static int g_tm_port = 0;                       /* 0 = telemetria spenta */
static int g_tm_hz = 4;                         /* campioni al secondo con sessione attiva o client collegato */
static int g_tm_idle_ms = 5000;                 /* intervallo a riposo (praticamente gratis) */
static int g_tm_sysrq = 1;                      /* in caso di stallo chiede gli stack al kernel (sysrq w, l) */
static int g_tm_disk = 1;                       /* registro su disco con O_SYNC durante sessioni e stalli */
static const char *g_proc = "/proc";
static const char *g_sys = "/sys";
static const char *g_kmsg_path = "/dev/kmsg";
static const char *g_sysrq_path = "/proc/sysrq-trigger";
static const char *g_sysrq_mask_path = "/proc/sys/kernel/sysrq";

struct tm_slot { volatile uint32_t len; char txt[TM_TXT]; };
struct tm_shared {
    volatile uint64_t head;             /* numero della prossima riga da pubblicare */
    volatile uint64_t publish_ms;       /* ultima pubblicazione dell'anello */
    volatile int32_t tmd_pid;           /* processo della scatola nera */
    volatile uint64_t client_ms;        /* ultimo segno di vita di un client di telemetria */
    /* aggiornati dal ponte video, letti dalla scatola nera */
    volatile int32_t relay_pid, server_pid;
    volatile uint64_t relay_loop_ms, session_start_ms;
    volatile uint64_t bytes_in, bytes_out, frames_in, frames_out, dropped;
    volatile uint32_t queue_kb;
    struct tm_slot slot[TM_N];
};
static struct tm_shared *g_tm = NULL;

static int tm_shared_init(void) {
    void *m = mmap(NULL, sizeof *g_tm, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (m == MAP_FAILED) return -1;
    g_tm = m;
    return 0;
}

/* Il ponte video segnala qui la sessione in corso: la scatola nera passa alla frequenza alta e scrive su disco. */
static void tm_relay_begin(pid_t server_pid) {
    if (!g_tm) return;
    g_tm->session_start_ms = now_ms();
    g_tm->server_pid = (int32_t)server_pid;
    g_tm->relay_loop_ms = now_ms();
    g_tm->bytes_in = g_tm->bytes_out = g_tm->frames_in = g_tm->frames_out = g_tm->dropped = 0;
    g_tm->queue_kb = 0;
    g_tm->relay_pid = (int32_t)getpid();
}
static void tm_relay_end(void) {
    if (!g_tm) return;
    g_tm->relay_pid = 0;
    g_tm->server_pid = 0;
}
static void tm_relay_tick(uint64_t bytes_in, uint64_t bytes_out, uint64_t frames_in, uint64_t frames_out, uint64_t dropped, unsigned queue_kb) {
    if (!g_tm) return;
    g_tm->bytes_in = bytes_in; g_tm->bytes_out = bytes_out; g_tm->frames_in = frames_in; g_tm->frames_out = frames_out;
    g_tm->dropped = dropped; g_tm->queue_kb = queue_kb;
    g_tm->relay_loop_ms = now_ms();
}

/* ------------------------------------------------------------------ letture di procfs/sysfs */

static int tm_rd(const char *path, char *buf, size_t n) {
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0) return -1;
    size_t have = 0;
    for (;;) {
        if (have + 1 >= n) break;
        ssize_t r = read(fd, buf + have, n - 1 - have);
        if (r < 0) { if (errno == EINTR) continue; close(fd); return -1; }
        if (r == 0) break;
        have += (size_t)r;
    }
    close(fd);
    buf[have] = 0;
    return (int)have;
}

/* Valore intero dopo una chiave a inizio riga («MemAvailable:   123 kB»). -1 se manca. */
static long long tm_after(const char *buf, const char *key) {
    size_t kl = strlen(key);
    const char *p = buf;
    while (p && *p) {
        if (strncmp(p, key, kl) == 0) return strtoll(p + kl, NULL, 10);
        p = strchr(p, '\n');
        if (p) p++;
    }
    return -1;
}

struct tm_psi { double some10, full10; uint64_t some_total, full_total; int ok; };

static void tm_psi_read(const char *name, struct tm_psi *o) {
    char p[256], b[512];
    memset(o, 0, sizeof *o);
    snprintf(p, sizeof p, "%s/pressure/%s", g_proc, name);
    if (tm_rd(p, b, sizeof b) <= 0) return;
    o->ok = 1;
    const char *s = strstr(b, "some");
    if (s) {
        const char *a = strstr(s, "avg10="), *t = strstr(s, "total=");
        if (a) o->some10 = atof(a + 6);
        if (t) o->some_total = strtoull(t + 6, NULL, 10);
    }
    const char *f = strstr(b, "full");
    if (f) {
        const char *a = strstr(f, "avg10="), *t = strstr(f, "total=");
        if (a) o->full10 = atof(a + 6);
        if (t) o->full_total = strtoull(t + 6, NULL, 10);
    }
}

/* ------------------------------------------------------------------ anello condiviso e disco */

static pthread_mutex_t g_emit_mu = PTHREAD_MUTEX_INITIALIZER;
static volatile int g_disk_want = 0;

#define TM_DQ 256
static char g_dq[TM_DQ][TM_TXT];
static uint32_t g_dq_len[TM_DQ];
static uint64_t g_dq_head = 0, g_dq_tail = 0, g_dq_dropped = 0;
static pthread_mutex_t g_dq_mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t g_dq_cv;

static void tm_publish(const char *line, size_t n) {
    if (n >= TM_TXT) n = TM_TXT - 1;
    uint64_t h = g_tm->head;
    struct tm_slot *s = &g_tm->slot[h % TM_N];
    s->len = 0;
    __sync_synchronize();
    memcpy(s->txt, line, n);
    __sync_synchronize();
    s->len = (uint32_t)n;
    __sync_synchronize();
    g_tm->head = h + 1;
    g_tm->publish_ms = now_ms();
}

/* Legge la riga numero seq dall'anello; -1 se nel frattempo è stata riscritta. */
static int tm_read_slot(uint64_t seq, char *out, size_t cap) {
    struct tm_slot *s = &g_tm->slot[seq % TM_N];
    uint32_t len = s->len;
    if (len == 0 || len >= TM_TXT || len >= cap) return -1;
    memcpy(out, (const void *)s->txt, len);
    __sync_synchronize();
    if (s->len != len || g_tm->head > seq + TM_N - 8) return -1;
    out[len] = 0;
    return (int)len;
}

static void tm_emit(char kind, const char *fmt, ...) {
    char line[TM_TXT];
    uint64_t ms = now_ms();
    int n = snprintf(line, sizeof line, "%c %llu.%03llu ", kind, (unsigned long long)(ms / 1000), (unsigned long long)(ms % 1000));
    if (n < 0) return;
    va_list ap;
    va_start(ap, fmt);
    int w = vsnprintf(line + n, sizeof line - (size_t)n, fmt, ap);
    va_end(ap);
    if (w < 0) return;
    size_t len = (size_t)n + (size_t)w;
    if (len >= sizeof line) len = sizeof line - 1;
    pthread_mutex_lock(&g_emit_mu);
    tm_publish(line, len);
    if (g_tm_disk) {
        pthread_mutex_lock(&g_dq_mu);
        if (g_dq_head - g_dq_tail >= TM_DQ) { g_dq_tail++; g_dq_dropped++; } /* il disco è fermo: si perde la riga più vecchia */
        memcpy(g_dq[g_dq_head % TM_DQ], line, len);
        g_dq_len[g_dq_head % TM_DQ] = (uint32_t)len;
        g_dq_head++;
        pthread_cond_signal(&g_dq_cv);
        pthread_mutex_unlock(&g_dq_mu);
    }
    pthread_mutex_unlock(&g_emit_mu);
}

static int tm_open_blackbox(void) {
    char p[300], q[320];
    state_path(p, sizeof p, "blackbox.txt");
    struct stat st;
    if (stat(p, &st) == 0 && st.st_size > 3 * 1024 * 1024) {
        snprintf(q, sizeof q, "%s.1", p);
        rename(p, q);
    }
    int fd = open(p, O_WRONLY | O_CREAT | O_APPEND | O_SYNC | O_CLOEXEC, 0644);
    if (fd < 0) return -1;
    char h[160];
    int n = snprintf(h, sizeof h, "# --- scatola nera aperta %lld (sonda %s) ---\n", (long long)time(NULL), PROBE_VERSION);
    if (write(fd, h, (size_t)n) < 0) { /* il disco non risponde: si prosegue senza */ }
    return fd;
}

static void *tm_disk_thread(void *arg) {
    (void)arg;
    char *batch = malloc((size_t)TM_DQ * (TM_TXT + 2) + 256);
    if (!batch) return NULL;
    int fd = -1;
    for (;;) {
        pthread_mutex_lock(&g_dq_mu);
        if (g_dq_head == g_dq_tail) {
            struct timespec ts;
            clock_gettime(CLOCK_MONOTONIC, &ts);
            ts.tv_sec += 1;
            pthread_cond_timedwait(&g_dq_cv, &g_dq_mu, &ts);
        }
        size_t bl = 0;
        while (g_dq_tail < g_dq_head) {
            uint32_t l = g_dq_len[g_dq_tail % TM_DQ];
            memcpy(batch + bl, g_dq[g_dq_tail % TM_DQ], l);
            bl += l;
            batch[bl++] = '\n';
            g_dq_tail++;
        }
        uint64_t dropped = g_dq_dropped;
        g_dq_dropped = 0;
        pthread_mutex_unlock(&g_dq_mu);

        int want = g_disk_want;
        if (!want) {
            if (fd >= 0) { close(fd); fd = -1; }
            continue;
        }
        if (fd < 0) {
            fd = tm_open_blackbox();
            if (fd < 0) continue;
            /* a disco acceso si recuperano le ultime righe già pubblicate, per avere il contesto da prima */
            uint64_t head = g_tm->head;
            uint64_t from = head > 40 ? head - 40 : 0;
            char *pre = malloc(40 * (TM_TXT + 2));
            if (pre) {
                size_t pl = 0;
                char one[TM_TXT];
                for (uint64_t s = from; s < head; s++) {
                    int l = tm_read_slot(s, one, sizeof one);
                    if (l <= 0) continue;
                    memcpy(pre + pl, one, (size_t)l);
                    pl += (size_t)l;
                    pre[pl++] = '\n';
                }
                if (pl && write(fd, pre, pl) < 0) { /* disco fermo */ }
                free(pre);
            }
        }
        if (dropped) {
            char d[96];
            int n = snprintf(d, sizeof d, "# righe perse perché il disco non rispondeva: %llu\n", (unsigned long long)dropped);
            if (write(fd, d, (size_t)n) < 0) { /* disco fermo */ }
        }
        if (bl && write(fd, batch, bl) < 0) { /* disco fermo: si riprova al giro dopo */ }
    }
    return NULL;
}

/* ------------------------------------------------------------------ campionatori */

struct tm_part { char txt[700]; uint64_t t_ms; };
static pthread_mutex_t g_part_mu = PTHREAD_MUTEX_INITIALIZER;
static struct tm_part g_core, g_hw, g_task;
static const char *volatile g_stage_core = "avvio";
static const char *volatile g_stage_hw = "avvio";
static const char *volatile g_stage_task = "avvio";
static volatile int g_tm_interval = 5000;   /* intervallo corrente deciso dal pubblicatore */
static volatile int g_want_deep = 0;        /* richiesta di istantanea dei task bloccati */

/* Valori numerici per il rilevatore di stallo. */
static volatile int g_n_blk = 0;
static volatile double g_n_io_full = 0, g_n_mem_full = 0;
static volatile int g_n_dmain = 0;          /* processi chiave con il thread principale bloccato nel kernel */

static void part_set(struct tm_part *p, const char *txt) {
    pthread_mutex_lock(&g_part_mu);
    snprintf(p->txt, sizeof p->txt, "%s", txt);
    p->t_ms = now_ms();
    pthread_mutex_unlock(&g_part_mu);
}
static uint64_t part_get(struct tm_part *p, char *out, size_t n) {
    pthread_mutex_lock(&g_part_mu);
    snprintf(out, n, "%s", p->txt);
    uint64_t t = p->t_ms;
    pthread_mutex_unlock(&g_part_mu);
    return t;
}

/* Attesa interrompibile: torna quando è passato l'intervallo richiesto, o prima se il pubblicatore lo accorcia
 * (client collegato o sessione avviata) o, per il thread dei task, se serve un'istantanea subito. */
static void tm_nap(int ms, int floor_ms, int wake_on_deep) {
    uint64_t start = now_ms();
    for (;;) {
        uint64_t el = since(now_ms(), start);
        if (el >= (uint64_t)ms) return;
        if (wake_on_deep && g_want_deep) return;
        int iv = g_tm_interval;
        if (iv < floor_ms) iv = floor_ms;
        if (el >= (uint64_t)iv) return;
        usleep(50000);
    }
}

/* ---- messaggi del kernel (/dev/kmsg) */

static int g_kmsg_fd = -1;
static uint64_t g_kmsg_lost = 0;

static int tm_is_mac(const char *p) {
    for (int i = 0; i < 17; i++) {
        char c = p[i];
        if (!c) return 0;
        if (i % 3 == 2) { if (c != ':') return 0; }
        else if (!((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F'))) return 0;
    }
    return 1;
}

/* Toglie gli indirizzi MAC e scarta i messaggi che citano nomi di reti Wi-Fi. Restituisce 0 se la riga va scartata. */
static int tm_mask_kmsg(char *m) {
    for (char *p = m; *p; p++) {
        if (strncasecmp(p, "ssid", 4) == 0) return 0;
        if (tm_is_mac(p)) memcpy(p, "xx:xx:xx:xx:xx:xx", 17);
    }
    return 1;
}

static void tm_kmsg_drain(void) {
    if (g_kmsg_fd < 0) return;
    static char b[9000]; /* /dev/kmsg rifiuta (EINVAL) un buffer più piccolo del record */
    for (int budget = 80; budget > 0; budget--) {
        ssize_t r = read(g_kmsg_fd, b, sizeof b - 1);
        if (r < 0) {
            if (errno == EPIPE) { g_kmsg_lost++; continue; } /* il kernel ha riscritto record che non avevamo letto */
            if (errno == EINTR) continue;
            break;
        }
        if (r == 0) break;
        b[r] = 0;
        /* Un record è «pri,seq,ts,flags;testo»; le righe che iniziano con uno spazio sono continuazioni. */
        char *save = NULL;
        for (char *line = strtok_r(b, "\n", &save); line; line = strtok_r(NULL, "\n", &save)) {
            if (line[0] == ' ' || line[0] == 0) continue;
            char *semi = strchr(line, ';');
            if (!semi) continue;
            unsigned long long seq = 0, ts = 0;
            int pri = 0;
            if (sscanf(line, "%d,%llu,%llu,", &pri, &seq, &ts) < 3) continue;
            const char *msg = semi + 1;
            if (strstr(msg, "avc:") || strncmp(msg, "audit", 5) == 0) continue; /* rumore: nega mille permessi al secondo */
            char m[400];
            snprintf(m, sizeof m, "%s", msg);
            if (!tm_mask_kmsg(m)) continue;
            tm_emit('K', "k=%llu.%03llu #%llu <%d> %s", ts / 1000000ull, (ts / 1000ull) % 1000ull, seq, pri & 7, m);
        }
    }
}

/* ---- core: CPU, carico, memoria, pressione */

static void tm_core_sample(void) {
    static uint64_t pcpu[8], pms = 0, p_some[3], p_full[3], p_alloc = 0, p_scan = 0;
    static int first = 1;
    static char big[65536];
    char p[256], buf[8192], out[700];
    uint64_t now = now_ms();

    g_stage_core = "stat";
    snprintf(p, sizeof p, "%s/stat", g_proc);
    int n = tm_rd(p, big, sizeof big);
    unsigned long long cpu[8] = {0};
    int blk = 0, runn = 0;
    if (n > 0) {
        sscanf(big, "cpu %llu %llu %llu %llu %llu %llu %llu %llu", &cpu[0], &cpu[1], &cpu[2], &cpu[3], &cpu[4], &cpu[5], &cpu[6], &cpu[7]);
        long long b = tm_after(big, "procs_blocked "), r = tm_after(big, "procs_running ");
        blk = b > 0 ? (int)b : 0;
        runn = r > 0 ? (int)r : 0;
    }
    double dt_us = pms ? (double)(now - pms) * 1000.0 : 0;
    unsigned long long tot = 0, ptot = 0;
    for (int i = 0; i < 8; i++) { tot += cpu[i]; ptot += pcpu[i]; }
    double dtot = (double)(tot - ptot);
    double up = 0, sp = 0, wp = 0, qp = 0;
    if (!first && dtot > 0) {
        up = 100.0 * (double)((cpu[0] + cpu[1]) - (pcpu[0] + pcpu[1])) / dtot;
        sp = 100.0 * (double)(cpu[2] - pcpu[2]) / dtot;
        wp = 100.0 * (double)(cpu[4] - pcpu[4]) / dtot;
        qp = 100.0 * (double)((cpu[5] + cpu[6]) - (pcpu[5] + pcpu[6])) / dtot;
    }
    for (int i = 0; i < 8; i++) pcpu[i] = cpu[i];

    g_stage_core = "loadavg";
    double l1 = 0, l5 = 0;
    int lr = 0, lt = 0;
    snprintf(p, sizeof p, "%s/loadavg", g_proc);
    if (tm_rd(p, buf, sizeof buf) > 0) sscanf(buf, "%lf %lf %*f %d/%d", &l1, &l5, &lr, &lt);

    g_stage_core = "meminfo";
    snprintf(p, sizeof p, "%s/meminfo", g_proc);
    long long avail = -1, mfree = -1, dirty = -1, wb = -1, swf = -1;
    if (tm_rd(p, big, sizeof big) > 0) {
        avail = tm_after(big, "MemAvailable:"); mfree = tm_after(big, "MemFree:");
        dirty = tm_after(big, "Dirty:"); wb = tm_after(big, "Writeback:"); swf = tm_after(big, "SwapFree:");
    }

    g_stage_core = "vmstat";
    snprintf(p, sizeof p, "%s/vmstat", g_proc);
    unsigned long long alloc = 0, scan = 0;
    if (tm_rd(p, big, sizeof big) > 0) {
        const char *q = big;
        while (q && *q) {
            if (strncmp(q, "allocstall", 10) == 0) { const char *sp2 = strchr(q, ' '); if (sp2) alloc += strtoull(sp2, NULL, 10); }
            else if (strncmp(q, "pgscan_direct ", 14) == 0) scan += strtoull(q + 14, NULL, 10);
            q = strchr(q, '\n');
            if (q) q++;
        }
    }

    g_stage_core = "pressure";
    struct tm_psi pc, pm, pi;
    tm_psi_read("cpu", &pc); tm_psi_read("memory", &pm); tm_psi_read("io", &pi);
    /* percentuale di tempo «fermo» nell'ultimo intervallo, dai contatori cumulativi in microsecondi */
    double cs = 0, ms_ = 0, mf = 0, is = 0, if_ = 0;
    if (!first && dt_us > 0) {
        cs = 100.0 * (double)(pc.some_total - p_some[0]) / dt_us;
        ms_ = 100.0 * (double)(pm.some_total - p_some[1]) / dt_us;
        mf = 100.0 * (double)(pm.full_total - p_full[1]) / dt_us;
        is = 100.0 * (double)(pi.some_total - p_some[2]) / dt_us;
        if_ = 100.0 * (double)(pi.full_total - p_full[2]) / dt_us;
    }
    p_some[0] = pc.some_total; p_some[1] = pm.some_total; p_some[2] = pi.some_total;
    p_full[1] = pm.full_total; p_full[2] = pi.full_total;
    unsigned long long d_alloc = first ? 0 : alloc - p_alloc, d_scan = first ? 0 : scan - p_scan;
    p_alloc = alloc; p_scan = scan; pms = now; first = 0;

    snprintf(out, sizeof out,
             "c u%.0f s%.0f w%.0f q%.0f | l %.2f %.2f %d/%d b%d | m a%lld f%lld d%lld w%lld s%lld as%llu pd%llu | p c%.1f m%.1f/%.1f i%.1f/%.1f",
             up, sp, wp, qp, l1, l5, lr ? lr : runn, lt, blk, avail >= 0 ? avail / 1024 : -1, mfree >= 0 ? mfree / 1024 : -1,
             dirty, wb, swf >= 0 ? swf / 1024 : -1, d_alloc, d_scan, cs, ms_, mf, is, if_);
    g_n_blk = blk; g_n_io_full = if_; g_n_mem_full = mf;
    part_set(&g_core, out);

    g_stage_core = "kmsg";
    tm_kmsg_drain();
    g_stage_core = "attesa";
}

static void *tm_core_thread(void *arg) {
    (void)arg;
    for (;;) {
        tm_core_sample();
        tm_nap(g_tm_interval, 0, 0);
    }
    return NULL;
}

/* ---- hw: GPU, temperature, frequenze */

#define TM_MAXZ 12
static char g_zone_path[TM_MAXZ][160];
static char g_zone_name[TM_MAXZ][24];
static int g_nzones = 0;

static void tm_zones_find(void) {
    static const char *want[] = {"cpu-0-0", "cpu-1-0", "cpu-4-0", "cpu-7-0", "cpuss", "gpuss", "skin", "battery", "xo-therm", "quiet", "ddr", "nsp"};
    for (int z = 0; z < 128 && g_nzones < TM_MAXZ; z++) {
        char p[200], t[64];
        snprintf(p, sizeof p, "%s/class/thermal/thermal_zone%d/type", g_sys, z);
        if (tm_rd(p, t, sizeof t) <= 0) continue;
        t[strcspn(t, "\n")] = 0;
        for (size_t i = 0; i < sizeof want / sizeof *want; i++) {
            if (strstr(t, want[i])) {
                snprintf(g_zone_path[g_nzones], sizeof g_zone_path[0], "%s/class/thermal/thermal_zone%d/temp", g_sys, z);
                snprintf(g_zone_name[g_nzones], sizeof g_zone_name[0], "%.23s", t);
                char *u = strstr(g_zone_name[g_nzones], "-usr");
                if (u) *u = 0;
                g_nzones++;
                break;
            }
        }
    }
}

static void tm_hw_sample(void) {
    char p[256], b[256], out[700];
    size_t o = 0;
    out[0] = 0;

    int gb = -1; long long gclk = -1, kpa = -1;
    g_stage_hw = "gpu:gpu_busy_percentage";
    snprintf(p, sizeof p, "%s/class/kgsl/kgsl-3d0/gpu_busy_percentage", g_sys);
    if (tm_rd(p, b, sizeof b) > 0) gb = atoi(b);
    g_stage_hw = "gpu:gpuclk";
    snprintf(p, sizeof p, "%s/class/kgsl/kgsl-3d0/gpuclk", g_sys);
    if (tm_rd(p, b, sizeof b) > 0) gclk = atoll(b) / 1000000;
    g_stage_hw = "gpu:page_alloc";
    snprintf(p, sizeof p, "%s/class/kgsl/kgsl/page_alloc", g_sys);
    if (tm_rd(p, b, sizeof b) > 0) kpa = atoll(b) / (1024 * 1024);
    o += (size_t)snprintf(out + o, sizeof out - o, "g b%d c%lld k%lld", gb, gclk, kpa);

    o += (size_t)snprintf(out + o, sizeof out - o, " | t");
    for (int i = 0; i < g_nzones; i++) {
        g_stage_hw = "thermal";
        if (tm_rd(g_zone_path[i], b, sizeof b) > 0) {
            double t = atof(b);
            if (t > 1000) t /= 1000.0;
            o += (size_t)snprintf(out + o, sizeof out - o, " %s=%.1f", g_zone_name[i], t);
        }
    }

    o += (size_t)snprintf(out + o, sizeof out - o, " | f");
    static const int cpus[] = {0, 4, 7};
    for (size_t i = 0; i < sizeof cpus / sizeof *cpus; i++) {
        g_stage_hw = "cpufreq";
        snprintf(p, sizeof p, "%s/devices/system/cpu/cpu%d/cpufreq/scaling_cur_freq", g_sys, cpus[i]);
        long long f = tm_rd(p, b, sizeof b) > 0 ? atoll(b) / 1000 : -1;
        o += (size_t)snprintf(out + o, sizeof out - o, "%s%lld", i ? "/" : " ", f);
    }
    part_set(&g_hw, out);
    g_stage_hw = "attesa";
}

static void *tm_hw_thread(void *arg) {
    (void)arg;
    tm_zones_find();
    for (;;) {
        tm_hw_sample();
        tm_nap(g_tm_interval < 1000 ? 1000 : g_tm_interval, 1000, 0);
    }
    return NULL;
}

/* ---- task: processi chiave e task bloccati nel kernel */

struct tm_watch { const char *label, *match; int exact; pid_t pid; };
static struct tm_watch g_watch[] = {
    {"sf", "surfaceflinger", 1, 0}, {"ss", "system_server", 1, 0}, {"hwc", "compos", 0, 0},
    {"swc", "media.swcodec", 1, 0}, {"mc", "media.codec", 1, 0},
};
#define TM_NWATCH ((int)(sizeof g_watch / sizeof *g_watch))

static void tm_scan_pids(void) {
    DIR *d = opendir(g_proc);
    if (!d) return;
    for (int i = 0; i < TM_NWATCH; i++) g_watch[i].pid = 0;
    struct dirent *e;
    while ((e = readdir(d))) {
        if (e->d_name[0] < '0' || e->d_name[0] > '9') continue;
        char p[300], c[64];
        snprintf(p, sizeof p, "%s/%s/comm", g_proc, e->d_name);
        if (tm_rd(p, c, sizeof c) <= 0) continue;
        c[strcspn(c, "\n")] = 0;
        for (int i = 0; i < TM_NWATCH; i++) {
            if (g_watch[i].exact ? strcmp(c, g_watch[i].match) == 0 : strstr(c, g_watch[i].match) != NULL) g_watch[i].pid = (pid_t)atoi(e->d_name);
        }
    }
    closedir(d);
}

/* Carattere di stato (e nome) di un task da <tdir>/stat. Restituisce 0 se il task non esiste. */
static int tm_task_state(const char *tdir, char *state, char *comm, size_t cn) {
    char p[480], b[1024];
    snprintf(p, sizeof p, "%s/stat", tdir);
    if (tm_rd(p, b, sizeof b) <= 0) return 0;
    char *rp = strrchr(b, ')');
    if (!rp || !rp[1] || !rp[2]) return 0;
    *state = rp[2];
    if (comm) {
        char *lp = strchr(b, '(');
        size_t l = lp && rp > lp ? (size_t)(rp - lp - 1) : 0;
        if (l >= cn) l = cn - 1;
        if (lp) memcpy(comm, lp + 1, l);
        comm[l] = 0;
    }
    return 1;
}

/* Dove dorme un task nel kernel (nome della funzione); vuoto se non noto. */
static void tm_task_wchan(const char *tdir, char *wchan, size_t wn) {
    char p[480];
    wchan[0] = 0;
    snprintf(p, sizeof p, "%s/wchan", tdir);
    if (tm_rd(p, wchan, wn) > 0) wchan[strcspn(wchan, "\n")] = 0;
    if (wchan[0] == '0' && wchan[1] == 0) wchan[0] = 0;
}

/* Funzioni dello stack del kernel di un task, dalla più recente: «a<b<c». */
static void tm_task_stack(const char *tdir, char *out, size_t n) {
    char p[480], b[2048];
    out[0] = 0;
    snprintf(p, sizeof p, "%s/stack", tdir);
    if (tm_rd(p, b, sizeof b) <= 0) return;
    size_t o = 0;
    int cnt = 0;
    for (char *l = strtok(b, "\n"); l && cnt < 8; l = strtok(NULL, "\n")) {
        char *s = strstr(l, "] ");
        s = s ? s + 2 : l;
        char *plus = strchr(s, '+');
        if (plus) *plus = 0;
        if (strncmp(s, "__switch_to", 11) == 0 || strncmp(s, "__schedule", 10) == 0 || strncmp(s, "schedule", 8) == 0) continue;
        o += (size_t)snprintf(out + o, n - o, "%s%s", cnt ? "<" : "", s);
        cnt++;
        if (o + 40 >= n) break;
    }
}

/* Elenca i thread di un processo: conta quelli in stato D e ne riporta fino a due. */
static void tm_proc_threads(pid_t pid, int *nthreads, int *ndisk, char *detail, size_t dn) {
    char tp[300];
    snprintf(tp, sizeof tp, "%s/%d/task", g_proc, (int)pid);
    DIR *d = opendir(tp);
    *nthreads = 0; *ndisk = 0; detail[0] = 0;
    if (!d) return;
    struct dirent *e;
    size_t o = 0;
    while ((e = readdir(d))) {
        if (e->d_name[0] < '0' || e->d_name[0] > '9') continue;
        (*nthreads)++;
        char td[400], st, wc[64], cm[32];
        snprintf(td, sizeof td, "%s/%.20s", tp, e->d_name);
        if (!tm_task_state(td, &st, cm, sizeof cm) || st != 'D') continue;
        (*ndisk)++;
        if (*ndisk <= 2) {
            tm_task_wchan(td, wc, sizeof wc);
            o += (size_t)snprintf(detail + o, dn - o, "%s%s:%s", *ndisk > 1 ? "," : "", cm, wc[0] ? wc : "-");
        }
    }
    closedir(d);
}

/* Istantanea di tutti i task bloccati nel kernel (stato D) nel sistema, con stack. */
static void tm_deep_snapshot(const char *why) {
    DIR *d = opendir(g_proc);
    if (!d) return;
    int found = 0, with_stack = 0;
    struct dirent *e;
    g_stage_task = "deep:scan";
    while ((e = readdir(d)) && found < 40) {
        if (e->d_name[0] < '0' || e->d_name[0] > '9') continue;
        char tp[300];
        snprintf(tp, sizeof tp, "%s/%s/task", g_proc, e->d_name);
        DIR *t = opendir(tp);
        if (!t) continue;
        struct dirent *te;
        while ((te = readdir(t)) && found < 40) {
            if (te->d_name[0] < '0' || te->d_name[0] > '9') continue;
            char td[400], st, wc[64], cm[32], pc[32] = "?";
            snprintf(td, sizeof td, "%s/%.20s", tp, te->d_name);
            if (!tm_task_state(td, &st, cm, sizeof cm) || st != 'D') continue;
            tm_task_wchan(td, wc, sizeof wc);
            char pp[400], pcb[64];
            snprintf(pp, sizeof pp, "%s/%s/comm", g_proc, e->d_name);
            if (tm_rd(pp, pcb, sizeof pcb) > 0) { pcb[strcspn(pcb, "\n")] = 0; snprintf(pc, sizeof pc, "%.31s", pcb); }
            char stack[300] = "";
            if (with_stack < 12) { g_stage_task = "deep:stack"; tm_task_stack(td, stack, sizeof stack); with_stack++; }
            tm_emit('D', "%s %s %s/%s wchan=%s stack=%s", why, pc, e->d_name, te->d_name, wc[0] ? wc : "-", stack[0] ? stack : "-");
            found++;
        }
        closedir(t);
    }
    closedir(d);
    if (found == 0) tm_emit('D', "%s nessun task in stato D trovato", why);
}

static void tm_task_sample(void) {
    static uint64_t last_scan = 0;
    uint64_t now = now_ms();
    char out[700];
    size_t o = 0;
    out[0] = 0;
    if (!last_scan || since(now, last_scan) > 3000) { g_stage_task = "scan"; tm_scan_pids(); last_scan = now; }
    int dmain = 0;
    pid_t extra[2] = {g_tm ? (pid_t)g_tm->server_pid : 0, g_tm ? (pid_t)g_tm->relay_pid : 0};
    const char *extra_label[2] = {"srv", "rl"};
    for (int i = 0; i < TM_NWATCH + 2; i++) {
        const char *label = i < TM_NWATCH ? g_watch[i].label : extra_label[i - TM_NWATCH];
        pid_t pid = i < TM_NWATCH ? g_watch[i].pid : extra[i - TM_NWATCH];
        if (pid <= 0) continue;
        char td[300], st = '?', wc[64], det[160];
        int nt, nd;
        g_stage_task = "proc";
        snprintf(td, sizeof td, "%s/%d", g_proc, (int)pid);
        if (!tm_task_state(td, &st, NULL, 0)) { o += (size_t)snprintf(out + o, sizeof out - o, "%s%s -", o ? " " : "", label); continue; }
        tm_task_wchan(td, wc, sizeof wc);
        g_stage_task = "threads";
        tm_proc_threads(pid, &nt, &nd, det, sizeof det);
        if (st == 'D') dmain++;
        o += (size_t)snprintf(out + o, sizeof out - o, "%s%s %c/%s t%d D%d%s%s", o ? " " : "", label, st, wc[0] ? wc : "-", nt, nd, nd ? "[" : "", nd ? det : "");
        if (nd) o += (size_t)snprintf(out + o, sizeof out - o, "]");
        if (o + 120 >= sizeof out) break;
    }
    g_n_dmain = dmain;
    part_set(&g_task, out);
    if (g_want_deep) { g_want_deep = 0; tm_deep_snapshot("stallo"); }
    g_stage_task = "attesa";
}

static void *tm_task_thread(void *arg) {
    (void)arg;
    for (;;) {
        tm_task_sample();
        tm_nap(g_tm_interval < 1000 ? 1000 : g_tm_interval, 1000, 1);
    }
    return NULL;
}

/* ---- sysrq: stack di tutti i task bloccati e delle CPU, nel log del kernel (da un thread a parte: può essere lento) */

static pthread_t g_sysrq_th;
static volatile int g_sysrq_busy = 0;

static void *tm_sysrq_thread(void *arg) {
    (void)arg;
    const char keys[] = {'w', 'l'};
    for (size_t i = 0; i < sizeof keys; i++) {
        int fd = open(g_sysrq_path, O_WRONLY | O_CLOEXEC);
        if (fd >= 0) { if (write(fd, &keys[i], 1) < 0) { /* sysrq non permesso */ } close(fd); }
        sleep(1);
    }
    g_sysrq_busy = 0;
    return NULL;
}

static void tm_sysrq_enable(void) {
    char b[32];
    if (tm_rd(g_sysrq_mask_path, b, sizeof b) <= 0) return;
    int v = atoi(b);
    if (v == 1 || (v & 8)) return;
    int fd = open(g_sysrq_mask_path, O_WRONLY | O_CLOEXEC);
    if (fd < 0) return;
    char w[16];
    int n = snprintf(w, sizeof w, "%d\n", v | 8);
    if (write(fd, w, (size_t)n) < 0) { /* non permesso */ }
    close(fd);
}

static void tm_sysrq_request(void) {
    if (!g_tm_sysrq || g_sysrq_busy) return;
    g_sysrq_busy = 1;
    pthread_attr_t a;
    pthread_attr_init(&a);
    pthread_attr_setdetachstate(&a, PTHREAD_CREATE_DETACHED);
    pthread_attr_setstacksize(&a, 65536);
    if (pthread_create(&g_sysrq_th, &a, tm_sysrq_thread, NULL) != 0) g_sysrq_busy = 0;
    pthread_attr_destroy(&a);
}

/* ------------------------------------------------------------------ pubblicatore (processo phantom-blackbox) */

static int tm_active(uint64_t now) {
    return (g_tm->client_ms && since(now, g_tm->client_ms) < 4000) || g_tm->relay_pid != 0;
}

static void tm_start_thread(void *(*fn)(void *)) {
    pthread_attr_t a;
    pthread_attr_init(&a);
    pthread_attr_setdetachstate(&a, PTHREAD_CREATE_DETACHED);
    pthread_attr_setstacksize(&a, 131072);
    pthread_t t;
    pthread_create(&t, &a, fn, NULL);
    pthread_attr_destroy(&a);
}

static void tm_main(pid_t parent) {
    prctl(PR_SET_NAME, "phantom-bbox");
    g_tm->tmd_pid = (int32_t)getpid();
    if (getppid() != parent) _exit(0);
    pthread_condattr_t ca;
    pthread_condattr_init(&ca);
    pthread_condattr_setclock(&ca, CLOCK_MONOTONIC);
    pthread_cond_init(&g_dq_cv, &ca);
    if (g_tm_sysrq) tm_sysrq_enable();
    g_kmsg_fd = open(g_kmsg_path, O_RDONLY | O_NONBLOCK | O_CLOEXEC);
    if (g_kmsg_fd >= 0) lseek(g_kmsg_fd, 0, SEEK_END); /* si parte da ora: la storia vecchia non serve */
    g_tm_interval = g_tm_idle_ms;
    tm_start_thread(tm_core_thread);
    tm_start_thread(tm_hw_thread);
    tm_start_thread(tm_task_thread);
    if (g_tm_disk) tm_start_thread(tm_disk_thread);

    int stalled = 0, cnt_blk3 = 0, cnt_blk1 = 0, cnt_dmain = 0;
    uint64_t stall_start = 0, last_snap = 0, last_sysrq = 0, last_end = 0;
    char reason[96] = "";
    for (;;) {
        uint64_t now = now_ms();
        if (g_tm->relay_pid && kill((pid_t)g_tm->relay_pid, 0) != 0 && errno == ESRCH) { g_tm->relay_pid = 0; g_tm->server_pid = 0; } /* ponte ucciso senza congedo */
        int active = tm_active(now);
        int iv = active ? 1000 / (g_tm_hz > 0 ? g_tm_hz : 4) : g_tm_idle_ms;
        g_tm_interval = iv;
        g_disk_want = g_tm_disk && (g_tm->relay_pid != 0 || stalled || since(now, last_end) < 10000);

        char core[700], hw[700], task[700], rl[200], line[TM_TXT];
        uint64_t tc = part_get(&g_core, core, sizeof core), th = part_get(&g_hw, hw, sizeof hw), tt = part_get(&g_task, task, sizeof task);

        /* campionatori fermi: chi non aggiorna da troppo è incastrato in una lettura del kernel */
        uint64_t lim_core = (uint64_t)iv * 3 + 1000, lim_slow = (uint64_t)(iv < 1000 ? 1000 : iv) * 3 + 750;
        char agebuf[200];
        size_t ao = 0;
        const char *stuck_name = NULL;
        const char *stuck_stage = "";
        uint64_t stuck_age = 0;
        struct { const char *n; uint64_t t; const char *volatile *st; } ps[3] = {{"core", tc, &g_stage_core}, {"hw", th, &g_stage_hw}, {"task", tt, &g_stage_task}};
        for (int i = 0; i < 3; i++) {
            uint64_t age = ps[i].t ? since(now, ps[i].t) : 0;
            ao += (size_t)snprintf(agebuf + ao, sizeof agebuf - ao, "%s%s%llu", i ? " " : "", ps[i].n, (unsigned long long)age);
            if (ps[i].t && age > (i == 0 ? lim_core : lim_slow) && !stuck_name) { stuck_name = ps[i].n; stuck_stage = *ps[i].st; stuck_age = age; }
        }
        if (stuck_name) {
            char *which = strcmp(stuck_name, "core") == 0 ? core : strcmp(stuck_name, "hw") == 0 ? hw : task;
            snprintf(which, 700, "FERMO(%s %llums)", stuck_stage, (unsigned long long)stuck_age);
        }

        if (g_tm->relay_pid) {
            snprintf(rl, sizeof rl, "%d giro%llums in%.1fM out%.1fM fr%llu/%llu dr%llu q%uK", (int)g_tm->relay_pid,
                     (unsigned long long)since(now, g_tm->relay_loop_ms), (double)g_tm->bytes_in / 1e6, (double)g_tm->bytes_out / 1e6,
                     (unsigned long long)g_tm->frames_in, (unsigned long long)g_tm->frames_out, (unsigned long long)g_tm->dropped, (unsigned)g_tm->queue_kb);
        } else {
            snprintf(rl, sizeof rl, "-");
        }
        snprintf(line, sizeof line, "e=%lld | %.250s | %.190s | P %.300s | R %.130s | a %.60s", (long long)time(NULL), core[0] ? core : "-", hw[0] ? hw : "-", task[0] ? task : "-", rl, agebuf);

        /* rilevatore di stallo */
        int blk = g_n_blk;
        cnt_blk3 = blk >= 2 ? cnt_blk3 + 1 : 0;
        cnt_blk1 = blk >= 1 ? cnt_blk1 + 1 : 0;
        cnt_dmain = g_n_dmain > 0 ? cnt_dmain + 1 : 0;
        const char *why = NULL;
        char why_buf[96];
        if (stuck_name) { snprintf(why_buf, sizeof why_buf, "campionatore %s fermo in %s", stuck_name, stuck_stage); why = why_buf; }
        else if (g_n_io_full >= 40) { snprintf(why_buf, sizeof why_buf, "pressione I/O piena %.0f%%", g_n_io_full); why = why_buf; }
        else if (g_n_mem_full >= 40) { snprintf(why_buf, sizeof why_buf, "pressione memoria piena %.0f%%", g_n_mem_full); why = why_buf; }
        else if (g_tm->relay_pid && since(now, g_tm->relay_loop_ms) > 2500 && since(now, g_tm->session_start_ms) > 4000) {
            snprintf(why_buf, sizeof why_buf, "il ponte video non gira da %llu ms", (unsigned long long)since(now, g_tm->relay_loop_ms));
            why = why_buf;
        }
        else if (cnt_dmain >= 3) { why = "un processo chiave è bloccato nel kernel"; }
        else if (cnt_blk3 >= 3) { snprintf(why_buf, sizeof why_buf, "%d task bloccati nel kernel", blk); why = why_buf; }
        else if (cnt_blk1 >= 12) { why = "task bloccato nel kernel da oltre 3 secondi"; }

        if (why && !stalled) {
            stalled = 1; stall_start = now; last_snap = now; last_sysrq = now;
            snprintf(reason, sizeof reason, "%s", why);
            g_disk_want = g_tm_disk;
            tm_emit('S', "inizio: %s", why);
            g_want_deep = 1;
            tm_sysrq_request();
        } else if (why && stalled) {
            if (since(now, last_snap) >= 2000) {
                last_snap = now;
                tm_emit('S', "continua da %llu ms: %s", (unsigned long long)since(now, stall_start), why);
                g_want_deep = 1;
            }
            if (since(now, last_sysrq) >= 20000) { last_sysrq = now; tm_sysrq_request(); }
        } else if (!why && stalled) {
            stalled = 0;
            last_end = now;
            tm_emit('S', "fine dopo %llu ms (motivo iniziale: %s)", (unsigned long long)since(now, stall_start), reason);
        }

        tm_emit('T', "%s", line);

        /* attende il prossimo giro; se diventa attivo (client o sessione) si accorcia subito */
        uint64_t next = now + (uint64_t)iv;
        while (now_ms() < next) {
            usleep(20000);
            if (!active && tm_active(now_ms())) break;
        }
    }
}

/* Avvia (e rilancia) il processo della scatola nera. close_fds: socket di ascolto da chiudere nel figlio. */
static pid_t tm_spawn(const int *close_fds, int nfds) {
    pid_t parent = getpid();
    pid_t c = fork();
    if (c < 0) return -1;
    if (c == 0) {
        prctl(PR_SET_PDEATHSIG, SIGKILL);
        signal(SIGTERM, SIG_DFL);
        signal(SIGINT, SIG_DFL);
        signal(SIGPIPE, SIG_IGN);
        for (int i = 0; i < nfds; i++) if (close_fds[i] >= 0) close(close_fds[i]);
        tm_main(parent);
        _exit(0);
    }
    return c;
}

/* ------------------------------------------------------------------ client: un processo per ogni connessione */

static void serve_telemetry(int fd, const struct sockaddr_in *peer_sa) {
    char peer[64];
    addr_str(peer_sa, peer, sizeof peer);
    int one = 1;
    setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one);
    int sb = 65536;
    setsockopt(fd, SOL_SOCKET, SO_SNDBUF, &sb, sizeof sb);
    int ka = 5, ki = 2, kc = 3;
    setsockopt(fd, SOL_SOCKET, SO_KEEPALIVE, &one, sizeof one);
    setsockopt(fd, IPPROTO_TCP, TCP_KEEPIDLE, &ka, sizeof ka);
    setsockopt(fd, IPPROTO_TCP, TCP_KEEPINTVL, &ki, sizeof ki);
    setsockopt(fd, IPPROTO_TCP, TCP_KEEPCNT, &kc, sizeof kc);
    set_nonblock(fd);
    logmsg("telemetria: client %s connesso", peer);

    static char out[65536];
    size_t olen = 0;
    uint64_t lost = 0, last_beat = 0;
    char hdr[256];
    int hn = snprintf(hdr, sizeof hdr, "H telemetria 1 sonda=%s monotonic=%llu epoch=%lld hz=%d\n", PROBE_VERSION,
                      (unsigned long long)now_ms(), (long long)time(NULL), g_tm_hz);
    memcpy(out, hdr, (size_t)hn);
    olen = (size_t)hn;
    uint64_t head = g_tm->head;
    uint64_t seq = head > TM_BACKFILL ? head - TM_BACKFILL : 0;

    for (;;) {
        uint64_t now = now_ms();
        g_tm->client_ms = now;
        struct pollfd pf = {.fd = fd, .events = POLLIN | (olen ? POLLOUT : 0), .revents = 0};
        int pr = poll(&pf, 1, 50);
        if (pr > 0) {
            if (pf.revents & (POLLERR | POLLHUP)) break;
            if (pf.revents & POLLIN) {
                char tmp[256];
                ssize_t r = recv(fd, tmp, sizeof tmp, MSG_DONTWAIT);
                if (r == 0 || (r < 0 && errno != EAGAIN && errno != EWOULDBLOCK && errno != EINTR)) break;
            }
        }
        now = now_ms();
        /* nuove righe dall'anello */
        uint64_t h = g_tm->head;
        if (h > seq + TM_N - 16) { lost += h - TM_N / 2 - seq; seq = h - TM_N / 2; }
        while (seq < h) {
            char l[TM_TXT + 2];
            int n = tm_read_slot(seq, l, sizeof l - 2);
            seq++;
            if (n <= 0) { lost++; continue; }
            l[n++] = '\n';
            if (olen + (size_t)n > sizeof out) { lost++; continue; } /* client lento: si scarta la riga, non si rallenta nessuno */
            memcpy(out + olen, l, (size_t)n);
            olen += (size_t)n;
        }
        /* battito: prova che questo processo (e quindi rete e kernel) è vivo anche se il campionatore è fermo */
        if (since(now, last_beat) >= 1000 && olen + 160 < sizeof out) {
            last_beat = now;
            char b[160];
            int bn = snprintf(b, sizeof b, "B %llu.%03llu head=%llu pub_age=%llu tmd=%d perse=%llu\n", (unsigned long long)(now / 1000),
                              (unsigned long long)(now % 1000), (unsigned long long)g_tm->head,
                              (unsigned long long)since(now, g_tm->publish_ms), (int)g_tm->tmd_pid, (unsigned long long)lost);
            memcpy(out + olen, b, (size_t)bn);
            olen += (size_t)bn;
        }
        if (olen) {
            ssize_t w = send(fd, out, olen, MSG_DONTWAIT | MSG_NOSIGNAL);
            if (w < 0) {
                if (errno != EAGAIN && errno != EWOULDBLOCK && errno != EINTR) break;
            } else if ((size_t)w < olen) {
                memmove(out, out + w, olen - (size_t)w);
                olen -= (size_t)w;
            } else {
                olen = 0;
            }
        }
    }
    logmsg("telemetria: client %s uscito (righe perse %llu)", peer, (unsigned long long)lost);
}
