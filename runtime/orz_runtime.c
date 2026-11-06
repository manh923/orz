// orz のランタイム（x86-64 Linux）。
//
// C ライブラリは使わず、システムコールだけを使う（-ffreestanding でコンパイルする）。
// コンパイラが出したアセンブリから、C と同じ呼び出し方（System V ABI）で呼ばれる。
//
// 値の持ち方：
//   string  先頭のバイトを指すアドレス。長さはその手前の 8 バイトにあり、最後に 0 が付いている
//   array   先頭の要素を指すアドレス。要素数はその手前の 8 バイトにある
//
// メモリは確保するだけで、プログラムの終わりにまとめて解放する（GC は実装の手順 7 で入れる）。

typedef unsigned long usize;

#define SYS_read 0
#define SYS_write 1
#define SYS_open 2
#define SYS_close 3
#define SYS_mmap 9
#define SYS_rt_sigaction 13
#define SYS_sigaltstack 131
#define SYS_exit_group 231

#define O_RDONLY 0
#define O_WRONLY 1
#define O_CREAT 0100
#define O_TRUNC 01000
#define O_CLOEXEC 02000000

#define EINTR 4
#define PANIC_EXIT_CODE 101

static long sys3(long n, long a, long b, long c) {
    long r;
    __asm__ volatile("syscall" : "=a"(r) : "a"(n), "D"(a), "S"(b), "d"(c) : "rcx", "r11", "memory");
    return r;
}

static long sys4(long n, long a, long b, long c, long d) {
    long r;
    register long r10 __asm__("r10") = d;
    __asm__ volatile("syscall" : "=a"(r) : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10) : "rcx", "r11", "memory");
    return r;
}

static long sys6(long n, long a, long b, long c, long d, long e, long f) {
    long r;
    register long r10 __asm__("r10") = d;
    register long r8 __asm__("r8") = e;
    register long r9 __asm__("r9") = f;
    __asm__ volatile("syscall"
                     : "=a"(r)
                     : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8), "r"(r9)
                     : "rcx", "r11", "memory");
    return r;
}

// gcc は構造体のコピーなどで、これらを勝手に呼ぶことがある。
void *memcpy(void *d, const void *s, usize n) {
    char *dp = d;
    const char *sp = s;
    while (n--) *dp++ = *sp++;
    return d;
}

void *memset(void *d, int c, usize n) {
    char *dp = d;
    while (n--) *dp++ = (char)c;
    return d;
}

int memcmp(const void *a, const void *b, usize n) {
    const unsigned char *x = a, *y = b;
    for (; n; n--, x++, y++)
        if (*x != *y) return *x - *y;
    return 0;
}

static long cstrlen(const char *s) {
    long n = 0;
    while (s[n]) n++;
    return n;
}

// ---------------- 長さの付いた文字列 ----------------

#define STATIC_STR(name, lit) \
    static struct { long len; char s[sizeof(lit)]; } __attribute__((aligned(8))) name = {sizeof(lit) - 1, lit}

STATIC_STR(str_empty, "");
STATIC_STR(str_true, "true");
STATIC_STR(str_false, "false");

static struct { long len; long data[1]; } __attribute__((aligned(8))) empty_array = {0, {0}};

static long str_len(const char *s) { return ((const long *)s)[-1]; }

// ---------------- 出力（バッファ付き） ----------------

static char outbuf[8192];
static long outlen;

static void write_all(int fd, const char *p, long n) {
    while (n > 0) {
        long r = sys3(SYS_write, fd, (long)p, n);
        if (r == -EINTR) continue;
        if (r <= 0) return;
        p += r;
        n -= r;
    }
}

void orz_rt_flush(void) {
    write_all(1, outbuf, outlen);
    outlen = 0;
}

static void out(const char *p, long n) {
    if (n > (long)sizeof outbuf - outlen) {
        orz_rt_flush();
        if (n >= (long)sizeof outbuf) {
            write_all(1, p, n);
            return;
        }
    }
    memcpy(outbuf + outlen, p, n);
    outlen += n;
}

static void outs(const char *s) { out(s, cstrlen(s)); }

static void err(const char *s) { write_all(2, s, cstrlen(s)); }

// 数を 10 進の文字列にする。buf は 21 バイト以上。最小値も正しく扱う。
static char *fmt_long(char *buf, long v) {
    char *p = buf + 21;
    *--p = 0;
    unsigned long u = v < 0 ? -(unsigned long)v : (unsigned long)v;
    do {
        *--p = (char)('0' + u % 10);
        u /= 10;
    } while (u);
    if (v < 0) *--p = '-';
    return p;
}

static char *fmt_hex(char *buf, unsigned long v) {
    char *p = buf + 21;
    *--p = 0;
    do {
        *--p = "0123456789abcdef"[v & 15];
        v >>= 4;
    } while (v);
    *--p = 'x';
    *--p = '0';
    return p;
}

// ---------------- 終了と実行時エラー ----------------

__attribute__((noreturn)) static void sys_exit(long code) {
    for (;;) sys3(SYS_exit_group, code, 0, 0);
}

__attribute__((noreturn)) void orz_rt_exit(long code) {
    orz_rt_flush();
    sys_exit(code);
}

static void panic_begin(const char *site) {
    orz_rt_flush();
    err("実行時エラー: ");
    if (site) {
        err(site);
        err(": ");
    }
}

__attribute__((noreturn)) static void panic_end(void) {
    err("\n");
    sys_exit(PANIC_EXIT_CODE);
}

__attribute__((noreturn)) void orz_rt_panic(const char *site, const char *reason) {
    panic_begin(site);
    err(reason);
    panic_end();
}

__attribute__((noreturn)) void orz_rt_panic_index(const char *site, long index, long len) {
    char a[24], b[24];
    panic_begin(site);
    err("配列の範囲外です（添字 ");
    err(fmt_long(a, index));
    err("、長さ ");
    err(fmt_long(b, len));
    err("）");
    panic_end();
}

// 変換で範囲外。reason は「8 の範囲（0〜255）に入りません」のような文。
__attribute__((noreturn)) void orz_rt_panic_convert(const char *site, long value, const char *reason) {
    char a[24];
    panic_begin(site);
    err("変換する値 ");
    err(fmt_long(a, value));
    err(" が ");
    err(reason);
    panic_end();
}

// ---------------- メモリ ----------------

#define CHUNK (4L << 20)

static char *heap_cur, *heap_end;

static void *map(long n, const char *site) {
    long p = sys6(SYS_mmap, 0, n, 3 /* 読み書き */, 0x22 /* MAP_PRIVATE | MAP_ANONYMOUS */, -1, 0);
    if (p < 0 && p > -4096) orz_rt_panic(site, "メモリが足りません");
    return (void *)p;
}

// n バイトを確保する。中身は 0。
void *orz_rt_alloc(long n, const char *site) {
    if (n < 0) {
        char a[24];
        panic_begin(site);
        err("allocate に負の大きさ ");
        err(fmt_long(a, n));
        err(" が渡されました");
        panic_end();
    }
    if (n > (1L << 46)) orz_rt_panic(site, "メモリが足りません");
    n = (n + 15) & ~15L;
    if (n == 0) n = 16;
    if (n > heap_end - heap_cur) {
        if (n >= CHUNK / 4) return map(n, site);
        heap_cur = map(CHUNK, site);
        heap_end = heap_cur + CHUNK;
    }
    void *r = heap_cur;
    heap_cur += n;
    return r;
}

static char *new_str(long len) {
    long *h = orz_rt_alloc(8 + len + 1, 0);
    h[0] = len;
    return (char *)(h + 1);
}

static char *str_from(const char *p, long len) {
    char *s = new_str(len);
    memcpy(s, p, len);
    return s;
}

// 配列を作る。kind：0 = 中身は 0、1 = 空の文字列、2 = 空の配列。
void *orz_rt_array_new(long count, long elem_size, long kind, const char *site) {
    if (count < 0) {
        char a[24];
        panic_begin(site);
        err("配列の長さ ");
        err(fmt_long(a, count));
        err(" が負です");
        panic_end();
    }
    if (count > (1L << 40) / elem_size) orz_rt_panic(site, "メモリが足りません");
    long *h = orz_rt_alloc(8 + count * elem_size, site);
    h[0] = count;
    void **data = (void **)(h + 1);
    if (kind == 1)
        for (long i = 0; i < count; i++) data[i] = str_empty.s;
    else if (kind == 2)
        for (long i = 0; i < count; i++) data[i] = empty_array.data;
    return data;
}

// ---------------- 文字列 ----------------

char *orz_rt_str_concat(const char *a, const char *b) {
    long la = str_len(a), lb = str_len(b);
    char *s = new_str(la + lb);
    memcpy(s, a, la);
    memcpy(s + la, b, lb);
    return s;
}

// 中身が同じか。どちらかが null（0）のときは、両方 null なら同じ。
long orz_rt_str_eq(const char *a, const char *b) {
    if (a == b) return 1;
    if (!a || !b) return 0;
    long la = str_len(a);
    return la == str_len(b) && memcmp(a, b, la) == 0;
}

char *orz_rt_str_from_int(long v) {
    char buf[24];
    char *p = fmt_long(buf, v);
    return str_from(p, cstrlen(p));
}

char *orz_rt_str_from_bool(long v) { return v ? str_true.s : str_false.s; }

// ---------------- 表示 ----------------

void orz_rt_put_int(long v) {
    char buf[24];
    outs(fmt_long(buf, v));
}

void orz_rt_put_addr(long v) {
    char buf[24];
    outs(fmt_hex(buf, (unsigned long)v));
}

void orz_rt_put_bool(long v) { outs(v ? "true" : "false"); }

void orz_rt_put_str(const char *s) {
    if (!s)
        outs("null");
    else
        out(s, str_len(s));
}

void orz_rt_newline(void) { out("\n", 1); }

static long elem_size(const char *desc) {
    switch (desc[0]) {
    case 'b':
    case 'B':
        return 1;
    case 'i':
        return 4;
    default:
        return 8;
    }
}

// 型の説明 desc に従って 1 つ表示する。
//   b = 8、i = 32、l = 64、B = bool、s = string、p = pointer・address、m = maybe、[ = array
static void put_value(const char *desc, const void *p, int quote) {
    switch (desc[0]) {
    case 'b':
        orz_rt_put_int(*(const unsigned char *)p);
        break;
    case 'B':
        orz_rt_put_bool(*(const unsigned char *)p);
        break;
    case 'i':
        orz_rt_put_int(*(const int *)p);
        break;
    case 'l':
        orz_rt_put_int(*(const long *)p);
        break;
    case 'p':
        orz_rt_put_addr(*(const long *)p);
        break;
    case 's': {
        const char *s = *(const char *const *)p;
        if (quote && s) out("\"", 1);
        orz_rt_put_str(s);
        if (quote && s) out("\"", 1);
        break;
    }
    case 'm':
        if (*(const void *const *)p == 0)
            outs("null");
        else
            put_value(desc + 1, p, quote);
        break;
    case '[': {
        const char *a = *(const char *const *)p;
        if (!a) {
            outs("null");
            break;
        }
        long n = ((const long *)a)[-1], size = elem_size(desc + 1);
        out("[", 1);
        for (long i = 0; i < n; i++) {
            if (i) out(", ", 2);
            put_value(desc + 1, a + i * size, 1);
        }
        out("]", 1);
        break;
    }
    }
}

// 配列を [1, 2, 3] の形で表示する。desc は要素の型の説明。
void orz_rt_put_array(const void *a, const char *desc) {
    char d[64];
    d[0] = '[';
    long i = 0;
    for (; desc[i] && i < 62; i++) d[i + 1] = desc[i];
    d[i + 1] = 0;
    put_value(d, &a, 0);
}

// ---------------- 入力とファイル ----------------

static long last_errno;

// 伸びるバッファ。古いものは捨てる（GC が入るまでは解放しない）。
struct buf {
    char *p;
    long len, cap;
};

static void buf_push(struct buf *b, const char *p, long n) {
    if (b->len + n > b->cap) {
        long cap = b->cap ? b->cap * 2 : 256;
        while (cap < b->len + n) cap *= 2;
        char *q = orz_rt_alloc(cap, 0);
        memcpy(q, b->p, b->len);
        b->p = q;
        b->cap = cap;
    }
    memcpy(b->p + b->len, p, n);
    b->len += n;
}

static char inbuf[4096];
static long inpos, inlen;
static int ineof;

// 1 行読む。末尾の改行（\n と \r\n）は除く。入力の終わりなら null。
char *orz_rt_read_line(void) {
    orz_rt_flush();
    struct buf line = {0, 0, 0};
    int got = 0;
    for (;;) {
        if (inpos == inlen) {
            if (ineof) break;
            long r = sys3(SYS_read, 0, (long)inbuf, sizeof inbuf);
            if (r == -EINTR) continue;
            if (r <= 0) {
                ineof = 1;
                break;
            }
            inpos = 0;
            inlen = r;
        }
        got = 1;
        char *start = inbuf + inpos, *end = inbuf + inlen, *nl = start;
        while (nl < end && *nl != '\n') nl++;
        buf_push(&line, start, nl - start);
        if (nl < end) {
            inpos = nl - inbuf + 1;
            if (line.len > 0 && line.p[line.len - 1] == '\r') line.len--;
            return str_from(line.p, line.len);
        }
        inpos = inlen;
    }
    if (!got) return 0;
    return str_from(line.p, line.len);
}

char *orz_rt_read_file(const char *path) {
    long fd = sys4(SYS_open, (long)path, O_RDONLY | O_CLOEXEC, 0, 0);
    if (fd < 0) {
        last_errno = -fd;
        return 0;
    }
    struct buf b = {0, 0, 0};
    char tmp[4096];
    for (;;) {
        long r = sys3(SYS_read, fd, (long)tmp, sizeof tmp);
        if (r == -EINTR) continue;
        if (r < 0) {
            last_errno = -r;
            sys3(SYS_close, fd, 0, 0);
            return 0;
        }
        if (r == 0) break;
        buf_push(&b, tmp, r);
    }
    sys3(SYS_close, fd, 0, 0);
    return str_from(b.p, b.len);
}

long orz_rt_write_file(const char *path, const char *text) {
    long fd = sys4(SYS_open, (long)path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0644, 0);
    if (fd < 0) {
        last_errno = -fd;
        return 0;
    }
    const char *p = text;
    long n = str_len(text);
    while (n > 0) {
        long r = sys3(SYS_write, fd, (long)p, n);
        if (r == -EINTR) continue;
        if (r < 0) {
            last_errno = -r;
            sys3(SYS_close, fd, 0, 0);
            return 0;
        }
        p += r;
        n -= r;
    }
    long r = sys3(SYS_close, fd, 0, 0);
    if (r < 0) {
        last_errno = -r;
        return 0;
    }
    return 1;
}

// 直前の失敗の理由。
char *orz_rt_last_error(void) {
    const char *m;
    switch (last_errno) {
    case 0:
        return str_empty.s;
    case 2:
        m = "ファイルやフォルダがありません";
        break;
    case 13:
        m = "権限がありません";
        break;
    case 17:
        m = "すでにあります";
        break;
    case 20:
        m = "パスの途中がフォルダではありません";
        break;
    case 21:
        m = "フォルダです（ファイルではありません）";
        break;
    case 24:
        m = "開いているファイルが多すぎます";
        break;
    case 28:
        m = "ディスクの空きがありません";
        break;
    case 30:
        m = "読み取り専用の場所です";
        break;
    case 36:
        m = "パスが長すぎます";
        break;
    default: {
        char a[24], *num = fmt_long(a, last_errno);
        char msg[64] = "エラー（番号 ";
        long l = cstrlen(msg), k = 0;
        while (num[k]) msg[l++] = num[k++];
        const char *tail = "）";
        for (k = 0; tail[k]; k++) msg[l++] = tail[k];
        return str_from(msg, l);
    }
    }
    return str_from(m, cstrlen(m));
}

// ---------------- 累乗 ----------------

// 整数の累乗。bits は 8・32・64。途中の 2 乗が溢れるなら、最後の結果も溢れる。
long orz_rt_pow(long base, long exp, long bits, const char *site, const char *overflow, const char *negexp) {
    if (exp < 0) orz_rt_panic(site, negexp);
    long lo, hi;
    if (bits == 8) {
        lo = 0;
        hi = 255;
    } else if (bits == 32) {
        lo = -2147483647L - 1;
        hi = 2147483647L;
    } else {
        lo = (long)(1UL << 63);
        hi = ~lo;
    }
    long r = 1;
    while (exp > 0) {
        if (exp & 1) {
            if (__builtin_mul_overflow(r, base, &r) || r < lo || r > hi) orz_rt_panic(site, overflow);
        }
        exp >>= 1;
        if (exp > 0) {
            if (__builtin_mul_overflow(base, base, &base) || base < lo || base > hi) orz_rt_panic(site, overflow);
        }
    }
    return r;
}

// ---------------- 起動 ----------------

static long g_argc;
static char **g_argv;

// コマンドライン引数を array:string にする。
void *orz_rt_args(void) {
    char **a = orz_rt_array_new(g_argc, 8, 0, 0);
    for (long i = 0; i < g_argc; i++) a[i] = str_from(g_argv[i], cstrlen(g_argv[i]));
    return a;
}

// スタックが溢れたとき（SIGSEGV）に、メッセージを出して止まる。
// 溢れたスタックでは何もできないので、別のスタック（sigaltstack）で受ける。
static void on_segv(int sig) {
    (void)sig;
    orz_rt_flush();
    err("実行時エラー: メモリの読み書きに失敗しました"
        "（再帰が深すぎてスタックが溢れたか、ポインタが不正な場所を指しています）\n");
    sys_exit(PANIC_EXIT_CODE);
}

// シグナルから戻るとき用（on_segv は戻らないので、実際には使われない）。
__asm__(".text\n"
        "orz_rt_sigreturn:\n"
        "\tmovl $15, %eax\n"
        "\tsyscall\n");
void orz_rt_sigreturn(void);

static char altstack[65536] __attribute__((aligned(16)));

struct kstack {
    void *sp;
    int flags;
    long size;
};

struct ksigaction {
    void (*handler)(int);
    unsigned long flags;
    void (*restorer)(void);
    unsigned long mask;
};

static void setup_signals(void) {
    static struct kstack ss = {altstack, 0, sizeof altstack};
    static struct ksigaction sa = {on_segv, 0x04000000 /* SA_RESTORER */ | 0x08000000 /* SA_ONSTACK */,
                                   orz_rt_sigreturn, 0};
    sys3(SYS_sigaltstack, (long)&ss, 0, 0);
    sys4(SYS_rt_sigaction, 11 /* SIGSEGV */, (long)&sa, 0, 8);
    sys4(SYS_rt_sigaction, 7 /* SIGBUS */, (long)&sa, 0, 8);
}

// コンパイラが出す。main を呼んで、終了コードを返す。
long orz_entry(void);

// _start から呼ばれる。sp は起動時のスタック（argc、argv… が並んでいる）。
__attribute__((noreturn)) void orz_rt_start(long *sp) {
    g_argc = sp[0];
    g_argv = (char **)(sp + 1);
    setup_signals();
    orz_rt_exit(orz_entry());
}
